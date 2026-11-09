//! Anthropic Messages API classifier with prompt-cache warm-up and a jsonl
//! answer cache.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use serde_json::{Value, json};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use super::cache::{self, Cache, Stored};
use super::thumb;
use crate::config::AiSettings;
use crate::media::{MediaKind, kind_of};
use crate::progress::{Cancel, NoProgress, Progress, ProgressEvent, Stage};
use crate::route::{AiAnswer, AiMeta, AiRequest, Classifier, Confidence, SourcePrompt};
use crate::tools::Tools;
use crate::{Error, Result};

/// Enum value the model answers with when no category fits.
const NONE_CATEGORY: &str = "NONE";
/// The previous (Russian) spelling of [`NONE_CATEGORY`]; answers and cache
/// entries made before the prompt became English still carry it.
const LEGACY_NONE_CATEGORY: &str = "НИКАКАЯ";
const MAX_TOKENS: u32 = 4000;
/// At most this many keywords of an answer are kept.
const MAX_TAGS: usize = 10;
const MAX_RETRIES: u32 = 6;
const TIMEOUT: Duration = Duration::from_secs(120);
pub const DEFAULT_API_BASE: &str = "https://api.anthropic.com";
const DEFAULT_BACKOFF_MS: u64 = 500;

/// Token counters of all requests made by a classifier.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Requests that were answered (and so counted tokens).
    pub requests: u64,
    pub input: u64,
    pub output: u64,
    pub cache_write: u64,
    pub cache_read: u64,
}

impl Usage {
    /// Cost in USD, `None` for an unknown model.
    pub fn cost(&self, model: &str) -> Option<f64> {
        let p = price(model)?;
        let total = self.input as f64 * p.input
            + self.output as f64 * p.output
            + self.cache_write as f64 * p.cache_write
            + self.cache_read as f64 * p.cache_read;
        Some(total / 1e6)
    }
}

/// Price of a model in USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    /// Cache write (5 min).
    pub cache_write: f64,
    pub cache_read: f64,
}

/// The price table; `None` for an unknown model.
pub fn price(model: &str) -> Option<Price> {
    let (input, output, cache_write, cache_read) = match model {
        "claude-sonnet-5-5" => (2.00, 10.00, 2.50, 0.20),
        "claude-haiku-5-5" => (0.10, 0.50, 0.125, 0.01),
        "claude-opus-5-5" => (4.00, 20.00, 5.00, 0.20),
        _ => return None,
    };
    Some(Price {
        input,
        output,
        cache_write,
        cache_read,
    })
}

/// Whether a request's answer is in the cache (a hit costs nothing): the
/// same key and validity check as [`AiClassifier::classify_with`] uses.
pub fn is_cached(cache: &Cache, model: &str, req: &AiRequest) -> bool {
    let hash = cache::prompt_hash(&cache_prompt(&req.categories, &req.prompt));
    cache
        .get(&cache::key(&req.entry, model, &hash))
        .is_some_and(|s| to_answer(s, &req.categories).is_ok())
}

/// The cached answer for a request, if there is a valid one (no API call).
pub fn cached_answer(cache: &Cache, model: &str, req: &AiRequest) -> Option<AiAnswer> {
    let hash = cache::prompt_hash(&cache_prompt(&req.categories, &req.prompt));
    cache
        .get(&cache::key(&req.entry, model, &hash))
        .and_then(|s| to_answer(s, &req.categories).ok())
}

/// System prompt for one category set; categories are (name, description).
/// The source's prefix goes before the category list, its suffix after it.
pub fn system_prompt(categories: &[(String, String)], source: &SourcePrompt) -> String {
    build_prompt(categories, source, true)
}

/// The prompt the cache key is computed from: the same text without the
/// examples section and without the tags/description lines of the answer
/// format, so neither invalidates cached answers (an answer cached before
/// tags were asked for is used as is, without them).
pub fn cache_prompt(categories: &[(String, String)], source: &SourcePrompt) -> String {
    build_prompt(categories, source, false)
}

fn build_prompt(categories: &[(String, String)], source: &SourcePrompt, full: bool) -> String {
    let mut list = String::new();
    for (name, desc) in categories {
        list.push_str(&format!("- {name} — {desc}\n"));
    }
    // A slot is its trimmed text and a blank line; blank text adds nothing.
    let slot = |t: &Option<String>| match t.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => format!("{t}\n\n"),
        _ => String::new(),
    };
    let (prefix, suffix) = (slot(&source.prefix), slot(&source.suffix));
    // The user's corrections follow the suffix; none means no section.
    let examples = if !full || source.examples.is_empty() {
        String::new()
    } else {
        let lines: String = source.examples.iter().map(|l| format!("- {l}\n")).collect();
        format!("The user's past corrections:\n{lines}\n")
    };
    // Unset: the language of the interface (the CLI's or the GUI's locale).
    let language = match source.language.trim() {
        "" => crate::i18n::language_name(),
        l => l,
    };
    // Not part of the cache key: see `cache_prompt`.
    let meta = if full {
        format!(
            "- tags — 3-10 short keywords for what the frame shows, lowercase, written in {language};\n\
- description — one sentence describing the frame, written in {language}.\n"
        )
    } else {
        String::new()
    };
    format!(
        "{prefix}You sort photos and videos into categories. \
Each request is one frame (a 512px preview; for videos, 3 frames in a row). \
The categories are below; there are no others.\n\
\n\
<categories>\n\
{list}- {NONE_CATEGORY} — no category fits; don't guess: \
a doubtful frame is better left uncategorized\n\
</categories>\n\
\n\
{suffix}{examples}ANSWER FORMAT:\n\
- category — exactly one category name from the list (no description); \
{NONE_CATEGORY} — no category fits;\n\
- reason — the reason in 3-6 words, written in {language} (\"Lidl receipt\", \"ESP32 board close-up\");\n\
- confidence — OK, or LOW if the frame sits on the border of two categories or you are unsure.\n{meta}"
    )
}

fn schema(categories: &[(String, String)]) -> Value {
    let mut names: Vec<&str> = categories.iter().map(|(n, _)| n.as_str()).collect();
    names.push(NONE_CATEGORY);
    json!({
        "type": "object",
        "properties": {
            "category": {"type": "string", "enum": names},
            "reason": {"type": "string"},
            "confidence": {"type": "string", "enum": ["OK", "LOW"]},
            "tags": {"type": "array", "items": {"type": "string"}},
            "description": {"type": "string"},
        },
        "required": ["category", "reason", "confidence"],
        "additionalProperties": false,
    })
}

struct Shared {
    settings: AiSettings,
    tools: Tools,
    api_base: String,
    api_key: Option<String>,
    backoff_ms: u64,
    http: reqwest::Client,
    usage: Mutex<Usage>,
}

/// Requests sharing one system prompt: one category set and the same source
/// texts.
struct GroupCtx {
    categories: Vec<(String, String)>,
    system: String,
    schema: Value,
}

struct Job {
    idx: usize,
    path: PathBuf,
    key: String,
}

type Group = (Arc<GroupCtx>, String, Vec<Job>);

pub struct AiClassifier {
    shared: Arc<Shared>,
    cache_path: PathBuf,
}

impl AiClassifier {
    /// API key from `ANTHROPIC_API_KEY`, else `CLAUDE_API_KEY`, else the
    /// private key file (see [`super::key`]). A missing key only fails when a
    /// request actually needs the network.
    pub fn new(
        settings: &AiSettings,
        tools: Tools,
        cache_path: PathBuf,
        api_base: String,
    ) -> Result<Self> {
        let found = super::key::lookup();
        if let Some(w) = &found.warning {
            eprintln!("warning: {w}");
        }
        Self::with_api_key(settings, tools, cache_path, api_base, found.into_key())
    }

    pub fn with_api_key(
        settings: &AiSettings,
        tools: Tools,
        cache_path: PathBuf,
        api_base: String,
        api_key: Option<String>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|e| Error::Ai(crate::t!("ai.http_client", error = e)))?;
        Ok(AiClassifier {
            shared: Arc::new(Shared {
                settings: settings.clone(),
                tools,
                api_base: api_base.trim_end_matches('/').to_string(),
                api_key,
                backoff_ms: DEFAULT_BACKOFF_MS,
                http,
                usage: Mutex::new(Usage::default()),
            }),
            cache_path,
        })
    }

    /// Production setup: key from env, default API base and cache path.
    pub fn from_settings(settings: &AiSettings, tools: Tools) -> Result<Self> {
        let cache =
            Self::default_cache_path().ok_or_else(|| Error::Ai(crate::t!("ai.no_cache_dir")))?;
        Self::new(settings, tools, cache, DEFAULT_API_BASE.to_string())
    }

    /// Base delay of the retry backoff (default 500 ms); tests shorten it.
    pub fn with_backoff_ms(mut self, ms: u64) -> Self {
        Arc::get_mut(&mut self.shared)
            .expect("classifier not shared yet")
            .backoff_ms = ms;
        self
    }

    /// Default answer cache: `<user cache dir>/gallery/ai-cache.jsonl`.
    pub fn default_cache_path() -> Option<PathBuf> {
        crate::paths::cache_dir().map(|d| d.join("ai-cache.jsonl"))
    }

    pub fn usage(&self) -> Usage {
        *self.shared.usage.lock().expect("usage lock")
    }
}

pub(crate) fn to_answer(stored: &Stored, categories: &[(String, String)]) -> Result<AiAnswer> {
    let category = if stored.category == NONE_CATEGORY || stored.category == LEGACY_NONE_CATEGORY {
        None
    } else if categories.iter().any(|(n, _)| *n == stored.category) {
        Some(stored.category.clone())
    } else {
        return Err(Error::Ai(crate::t!(
            "ai.unknown_category",
            name = stored.category
        )));
    };
    Ok(AiAnswer {
        category,
        reason: one_line(&stored.reason),
        confidence: if stored.confidence == "OK" {
            Confidence::Ok
        } else {
            Confidence::Low
        },
        meta: to_meta(stored),
    })
}

/// Whitespace runs (newlines too) collapsed to single spaces, trimmed.
fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The answer's keywords (one line each, lowercase, no blanks or repeats, at
/// most [`MAX_TAGS`]) and description; `None` when neither is left.
fn to_meta(stored: &Stored) -> Option<AiMeta> {
    let mut tags: Vec<String> = Vec::new();
    for t in &stored.tags {
        let t = one_line(t).to_lowercase();
        if !t.is_empty() && !tags.contains(&t) && tags.len() < MAX_TAGS {
            tags.push(t);
        }
    }
    let description = one_line(&stored.description);
    (!tags.is_empty() || !description.is_empty()).then_some(AiMeta { tags, description })
}

fn backoff(base_ms: u64, attempt: u32) -> Duration {
    Duration::from_millis((base_ms << attempt.min(4)).min(8000))
}

impl Shared {
    fn add_usage(&self, u: &Value) {
        let n = |k: &str| u.get(k).and_then(Value::as_u64).unwrap_or(0);
        let mut t = self.usage.lock().expect("usage lock");
        t.requests += 1;
        t.input += n("input_tokens");
        t.output += n("output_tokens");
        t.cache_write += n("cache_creation_input_tokens");
        t.cache_read += n("cache_read_input_tokens");
    }

    async fn request(&self, ctx: &GroupCtx, image_b64: &str, video: bool) -> Result<Stored> {
        let key = self
            .api_key
            .as_deref()
            .ok_or_else(|| Error::Ai(crate::t!("ai.no_key")))?;
        let note = if video {
            "This is a video: three frames from the start, middle and end in one row."
        } else {
            "This is a photo."
        };
        let body = json!({
            "model": self.settings.model,
            "max_tokens": MAX_TOKENS,
            "system": [{"type": "text", "text": ctx.system, "cache_control": {"type": "ephemeral"}}],
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": image_b64}},
                {"type": "text", "text": note},
            ]}],
            "output_config": {
                "effort": self.settings.effort,
                "format": {"type": "json_schema", "schema": ctx.schema},
            },
        })
        .to_string();
        let url = format!("{}/v1/messages", self.api_base);

        for attempt in 0..=MAX_RETRIES {
            let sent = self
                .http
                .post(&url)
                .header("x-api-key", key)
                .header("anthropic-version", "2023-06-01")
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await;
            let resp = match sent {
                Ok(r) => r,
                Err(e) if attempt == MAX_RETRIES => {
                    return Err(Error::Ai(crate::t!("ai.request_failed", error = e)));
                }
                Err(_) => {
                    tokio::time::sleep(backoff(self.backoff_ms, attempt)).await;
                    continue;
                }
            };
            let status = resp.status();
            if status.as_u16() == 401 {
                return Err(Error::Ai(crate::t!("ai.key_rejected")));
            }
            if status.as_u16() == 429 || status.is_server_error() {
                if attempt == MAX_RETRIES {
                    return Err(Error::Ai(crate::t!("ai.api_retries", status = status)));
                }
                tokio::time::sleep(backoff(self.backoff_ms, attempt)).await;
                continue;
            }
            let text = resp
                .text()
                .await
                .map_err(|e| Error::Ai(crate::t!("ai.reading_response", error = e)))?;
            if !status.is_success() {
                let head: String = text.chars().take(300).collect();
                return Err(Error::Ai(crate::t!(
                    "ai.api_error",
                    status = status,
                    head = head
                )));
            }
            let v: Value = serde_json::from_str(&text)
                .map_err(|e| Error::Ai(crate::t!("ai.bad_response", error = e)))?;
            return self.parse(&v);
        }
        unreachable!("loop returns on the last attempt")
    }

    fn parse(&self, v: &Value) -> Result<Stored> {
        if let Some(u) = v.get("usage") {
            self.add_usage(u);
        }
        match v.get("stop_reason").and_then(Value::as_str) {
            Some("refusal") => {
                return Ok(Stored {
                    category: NONE_CATEGORY.into(),
                    reason: crate::t!("ai.refused"),
                    confidence: "LOW".into(),
                    tags: Vec::new(),
                    description: String::new(),
                });
            }
            Some("max_tokens") => return Err(Error::Ai(crate::t!("ai.truncated"))),
            _ => {}
        }
        let text = v
            .get("content")
            .and_then(Value::as_array)
            .and_then(|blocks| {
                blocks
                    .iter()
                    .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            })
            .and_then(|b| b.get("text"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Ai(crate::t!("ai.no_text")))?;
        serde_json::from_str(text).map_err(|e| Error::Ai(crate::t!("ai.bad_answer", error = e)))
    }
}

/// Result of one job; `key` is `None` for answers that must not be cached.
type JobResult = (usize, Option<String>, Stored);

/// The thumbnail of `path`, made in the job's own `dir`, which is removed
/// once read: a run has thousands of jobs and the temp dir is often tmpfs.
fn thumb_bytes(tools: &Tools, path: &Path, kind: MediaKind, dir: &Path) -> Result<Vec<u8>> {
    let bytes = thumb::make(tools, path, kind, dir).and_then(|p| Ok(std::fs::read(p)?));
    let _ = std::fs::remove_dir_all(dir);
    bytes
}

async fn run_job(
    shared: Arc<Shared>,
    ctx: Arc<GroupCtx>,
    job: Job,
    thumb_dir: PathBuf,
) -> Result<JobResult> {
    let kind = kind_of(&job.path);
    let video = kind == MediaKind::Video;
    let path = job.path.clone();
    let tools = shared.tools.clone();
    let bytes = tokio::task::spawn_blocking(move || thumb_bytes(&tools, &path, kind, &thumb_dir))
        .await
        .map_err(|e| Error::Ai(crate::t!("ai.thumbnail_task", error = e)))?;
    let bytes = match bytes {
        Ok(b) => b,
        // A file that cannot be decoded must not fail the batch (nor be
        // cached, so a fixed file is retried next time).
        Err(_) => {
            let stored = Stored {
                category: NONE_CATEGORY.into(),
                reason: crate::t!("ai.no_preview"),
                confidence: "LOW".into(),
                tags: Vec::new(),
                description: String::new(),
            };
            return Ok((job.idx, None, stored));
        }
    };
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    let stored = shared.request(&ctx, &b64, video).await?;
    // Validate now so a bad answer fails the run instead of poisoning the cache.
    to_answer(&stored, &ctx.categories)?;
    Ok((job.idx, Some(job.key), stored))
}

impl Classifier for AiClassifier {
    fn classify(&self, batch: Vec<AiRequest>) -> Result<Vec<AiAnswer>> {
        self.classify_with(batch, &NoProgress, &Cancel::new())
    }

    /// One [`Stage::Ai`] event for the cached answers, then one per answer
    /// received. Once cancelled no new request starts; requests in flight
    /// finish and their (paid) answers still go to the cache; then
    /// `Error::Cancelled`.
    fn classify_with(
        &self,
        batch: Vec<AiRequest>,
        progress: &dyn Progress,
        cancel: &Cancel,
    ) -> Result<Vec<AiAnswer>> {
        cancel.check()?;
        let total = batch.len();
        let model = &self.shared.settings.model;
        let mut cache = Cache::load(&self.cache_path);
        let mut answers: Vec<Option<AiAnswer>> = vec![None; batch.len()];
        let mut groups: Vec<Group> = Vec::new();

        for (idx, req) in batch.iter().enumerate() {
            // Grouped by the whole prompt text, so each group warms its own
            // prompt cache and its hash covers the source texts.
            let system = system_prompt(&req.categories, &req.prompt);
            let gi = match groups.iter().position(|g| g.0.system == system) {
                Some(gi) => gi,
                None => {
                    let hash = cache::prompt_hash(&cache_prompt(&req.categories, &req.prompt));
                    groups.push((
                        Arc::new(GroupCtx {
                            schema: schema(&req.categories),
                            categories: req.categories.clone(),
                            system,
                        }),
                        hash,
                        Vec::new(),
                    ));
                    groups.len() - 1
                }
            };
            let key = cache::key(&req.entry, model, &groups[gi].1);
            let hit = cache
                .get(&key)
                .and_then(|s| to_answer(s, &req.categories).ok());
            match hit {
                Some(a) => answers[idx] = Some(a),
                None => groups[gi].2.push(Job {
                    idx,
                    path: req.entry.path.clone(),
                    key,
                }),
            }
        }

        let hits = answers.iter().filter(|a| a.is_some()).count();
        if hits > 0 {
            progress.step(ProgressEvent {
                stage: Stage::Ai,
                done: hits,
                total,
                current: None,
            });
        }
        if groups.iter().any(|g| !g.2.is_empty()) {
            let tmp = tempfile::tempdir()?;
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()?;
            let count = Count {
                progress,
                cancel,
                done: hits,
                total,
            };
            rt.block_on(self.run_groups(groups, tmp.path(), &mut cache, &mut answers, count))?;
        }

        Ok(answers
            .into_iter()
            .map(|a| a.expect("every request answered"))
            .collect())
    }
}

/// Progress and cancellation of one classify run.
struct Count<'a> {
    progress: &'a dyn Progress,
    cancel: &'a Cancel,
    done: usize,
    total: usize,
}

impl AiClassifier {
    async fn run_groups(
        &self,
        groups: Vec<Group>,
        tmp: &Path,
        cache: &mut Cache,
        answers: &mut [Option<AiAnswer>],
        mut count: Count<'_>,
    ) -> Result<()> {
        let jobs_limit = self.shared.settings.jobs.max(1);
        let cancel = count.cancel.clone();
        let mut record = |r: JobResult, ctx: &GroupCtx| -> Result<()> {
            let (idx, key, stored) = r;
            answers[idx] = Some(to_answer(&stored, &ctx.categories)?);
            if let Some(key) = key {
                cache.put(key, stored)?;
            }
            count.done += 1;
            count.progress.step(ProgressEvent {
                stage: Stage::Ai,
                done: count.done,
                total: count.total,
                current: None,
            });
            Ok(())
        };
        for (ctx, _, jobs) in groups {
            let mut jobs = jobs.into_iter();
            let Some(first) = jobs.next() else { continue };
            cancel.check()?;
            // The first request runs alone so it writes the system prompt to
            // the prompt cache; parallel starters would each write it again.
            let dir = tmp.join(first.idx.to_string());
            let r = run_job(self.shared.clone(), ctx.clone(), first, dir).await?;
            record(r, &ctx)?;

            let sem = Arc::new(Semaphore::new(jobs_limit));
            let mut set = JoinSet::new();
            for job in jobs {
                let (shared, ctx, sem) = (self.shared.clone(), ctx.clone(), sem.clone());
                let cancel = cancel.clone();
                let dir = tmp.join(job.idx.to_string());
                set.spawn(async move {
                    let _permit = sem.acquire_owned().await.expect("semaphore open");
                    // A cancel stops jobs that have not started yet; started
                    // ones finish, so their paid answers reach the cache.
                    if cancel.is_cancelled() {
                        return Ok(None);
                    }
                    run_job(shared, ctx, job, dir).await.map(Some)
                });
            }
            while let Some(joined) = set.join_next().await {
                let r =
                    joined.map_err(|e| Error::Ai(crate::t!("ai.classify_task", error = e)))??;
                if let Some(r) = r {
                    record(r, &ctx)?;
                }
            }
            cancel.check()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thumbnail_is_deleted_once_read() {
        let Some(magick) = crate::tools::find("magick") else {
            eprintln!("skip: magick not found");
            return;
        };
        let p = PathBuf::from("/nonexistent/tool");
        let tools = Tools {
            ffmpeg: p.clone(),
            ffprobe: p.clone(),
            exiftool: p.clone(),
            avifenc: p,
            magick: magick.clone(),
            optipng: None,
            tz: None,
        };
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("a.png");
        let made = std::process::Command::new(&magick)
            .args(["-size", "64x64", "xc:red"])
            .arg(&src)
            .status()
            .unwrap();
        assert!(made.success());
        let dir = d.path().join("job");
        let bytes = thumb_bytes(&tools, &src, MediaKind::Photo, &dir).unwrap();
        assert!(!bytes.is_empty());
        // Thousands of jobs share one temp dir (often tmpfs): nothing stays.
        assert!(
            !dir.exists(),
            "{:?}",
            std::fs::read_dir(&dir).ok().map(|r| r.count())
        );
        // A failed thumbnail leaves nothing either.
        assert!(
            thumb_bytes(
                &tools,
                &d.path().join("missing.png"),
                MediaKind::Photo,
                &dir
            )
            .is_err()
        );
        assert!(!dir.exists());
    }
}
