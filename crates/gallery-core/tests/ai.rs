mod support;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gallery_core::Error;
use gallery_core::ai::{AiClassifier, system_prompt};
use gallery_core::config::AiSettings;
use gallery_core::route::{AiRequest, Classifier, Confidence, SourcePrompt};
use gallery_core::scan::FileEntry;
use gallery_core::tools::{Tools, find};
use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn tools() -> Tools {
    let p = |n: &str| find(n).unwrap_or_else(|| n.into());
    Tools {
        ffmpeg: p("ffmpeg"),
        ffprobe: p("ffprobe"),
        exiftool: p("exiftool"),
        avifenc: p("avifenc"),
        magick: p("magick"),
        optipng: None,
        tz: None,
    }
}

fn cats() -> Vec<(String, String)> {
    vec![
        ("Receipts".into(), "store receipts".into()),
        ("Boards".into(), "electronic boards".into()),
    ]
}

fn req(dir: &Path, name: &str, categories: Vec<(String, String)>) -> AiRequest {
    req_from(dir, name, categories, SourcePrompt::default())
}

fn req_from(
    dir: &Path,
    name: &str,
    categories: Vec<(String, String)>,
    prompt: SourcePrompt,
) -> AiRequest {
    let p = support::fixture_jpeg(dir, name, None, None);
    let m = std::fs::metadata(&p).unwrap();
    let mtime_ns = m
        .modified()
        .unwrap()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos() as i128;
    AiRequest {
        entry: FileEntry {
            source_id: "s".into(),
            rel: PathBuf::from(name),
            path: p,
            size: m.len(),
            mtime_ns,
        },
        categories,
        prompt,
    }
}

fn frame(prefix: Option<&str>, suffix: Option<&str>) -> SourcePrompt {
    SourcePrompt {
        prefix: prefix.map(String::from),
        suffix: suffix.map(String::from),
        ..Default::default()
    }
}

fn with_examples(lines: &[&str]) -> SourcePrompt {
    SourcePrompt {
        examples: lines.iter().map(|l| l.to_string()).collect(),
        ..frame(Some("camera"), Some("rule"))
    }
}

fn answer_body(category: &str, reason: &str, confidence: &str) -> Value {
    answer_body_with(json!({
        "category": category, "reason": reason, "confidence": confidence
    }))
}

fn answer_body_with(answer: Value) -> Value {
    json!({
        "stop_reason": "end_turn",
        "content": [{"type": "text", "text": answer.to_string()}],
        "usage": {"input_tokens": 10, "output_tokens": 5,
                  "cache_creation_input_tokens": 100, "cache_read_input_tokens": 0},
    })
}

fn classifier(base: &str, cache: &Path) -> AiClassifier {
    AiClassifier::with_api_key(
        &AiSettings::default(),
        tools(),
        cache.to_path_buf(),
        base.to_string(),
        Some("test-key".into()),
    )
    .unwrap()
}

/// Mock server on its own runtime thread; the classifier blocks on its own.
fn server(rt: &tokio::runtime::Runtime, responder: impl Respond + 'static) -> MockServer {
    rt.block_on(async {
        let s = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(responder)
            .mount(&s)
            .await;
        s
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
}

fn received(rt: &tokio::runtime::Runtime, s: &MockServer) -> Vec<Request> {
    rt.block_on(s.received_requests()).unwrap()
}

#[test]
fn system_prompt_lists_categories() {
    let p = system_prompt(&cats(), &SourcePrompt::default());
    assert!(p.contains("- Receipts — store receipts"));
    assert!(p.contains("- Boards — electronic boards"));
    assert!(p.contains("NONE"));
    assert!(p.contains("no category fits"));
    assert!(p.contains("3-6 words"));
}

#[test]
fn system_prompt_without_source_texts_is_unchanged() {
    let expected = "You sort photos and videos into categories. \
Each request is one frame (a 512px preview; for videos, 3 frames in a row). \
The categories are below; there are no others.\n\
\n\
<categories>\n\
- Receipts — store receipts\n\
- Boards — electronic boards\n\
- NONE — no category fits; don't guess: \
a doubtful frame is better left uncategorized\n\
</categories>\n\
\n\
ANSWER FORMAT:\n\
- category — exactly one category name from the list (no description); \
NONE — no category fits;\n\
- reason — the reason in 3-6 words, written in English (\"Lidl receipt\", \"ESP32 board close-up\");\n\
- confidence — OK, or LOW if the frame sits on the border of two categories or you are unsure.\n\
- tags — 5-20 short keywords, lowercase, written in English: the concrete things \
visible in the frame (name each kind of object, food, plant, animal or product specifically, \
e.g. \"pineapple\", not just \"fruit\"), the kind of place and the activity;\n\
- description — 2-4 sentences in English describing what is in the frame concretely: \
the main objects by name, readable text or brands, the setting and what is happening. \
Say what you see, not a guess about the occasion.\n";
    assert_eq!(system_prompt(&cats(), &SourcePrompt::default()), expected);
    // Blank texts add nothing.
    assert_eq!(
        system_prompt(&cats(), &frame(Some(" \n"), Some(""))),
        expected
    );
}

#[test]
fn cache_prompt_is_the_prompt_before_tags_were_asked_for() {
    use gallery_core::ai::cache_prompt;
    // Exactly the text cache keys were made from before: asking for tags
    // and a description must not invalidate a single cached answer.
    let before = "You sort photos and videos into categories. \
Each request is one frame (a 512px preview; for videos, 3 frames in a row). \
The categories are below; there are no others.\n\
\n\
<categories>\n\
- Receipts — store receipts\n\
- Boards — electronic boards\n\
- NONE — no category fits; don't guess: \
a doubtful frame is better left uncategorized\n\
</categories>\n\
\n\
ANSWER FORMAT:\n\
- category — exactly one category name from the list (no description); \
NONE — no category fits;\n\
- reason — the reason in 3-6 words, written in English (\"Lidl receipt\", \"ESP32 board close-up\");\n\
- confidence — OK, or LOW if the frame sits on the border of two categories or you are unsure.\n";
    assert_eq!(cache_prompt(&cats(), &SourcePrompt::default()), before);
}

#[test]
fn system_prompt_frames_the_category_list_with_source_texts() {
    let (prefix, suffix) = ("Frames from the camera.", "A price tag goes by item.");
    let p = system_prompt(&cats(), &frame(Some(prefix), Some(suffix)));
    let at = |s: &str| p.find(s).unwrap_or_else(|| panic!("no `{s}` in {p}"));
    assert!(p.starts_with(prefix), "{p}");
    assert!(at(prefix) < at("You sort photos"), "{p}");
    assert!(at("</categories>") < at(suffix), "{p}");
    assert!(at(suffix) < at("ANSWER FORMAT"), "{p}");
}

#[test]
fn system_prompt_lists_user_corrections_after_the_suffix() {
    let p = system_prompt(
        &cats(),
        &with_examples(&[
            "receipt — AI chose Boards, correct is Receipts",
            "board — AI chose nothing, correct is Boards",
        ]),
    );
    let at = |s: &str| p.find(s).unwrap_or_else(|| panic!("no `{s}` in {p}"));
    assert!(at("rule") < at("The user's past corrections:"), "{p}");
    assert!(
        at("The user's past corrections:") < at("ANSWER FORMAT"),
        "{p}"
    );
    assert!(
        at("- receipt — AI chose Boards, correct is Receipts\n")
            < at("- board — AI chose nothing, correct is Boards\n"),
        "{p}"
    );
    // No corrections: no section.
    assert!(!system_prompt(&cats(), &with_examples(&[])).contains("The user's past corrections"));
}

#[test]
fn cache_key_ignores_the_examples() {
    use gallery_core::ai::{cache::prompt_hash, cache_prompt};
    let h = |lines: &[&str]| prompt_hash(&cache_prompt(&cats(), &with_examples(lines)));
    let one = ["receipt — AI chose Boards, correct is Receipts"];
    let two = [one[0], "board — AI chose nothing, correct is Boards"];
    assert_eq!(h(&one), h(&two), "a new correction keeps the key");
    assert_eq!(h(&[]), h(&one));
    // Everything else still counts.
    let other = SourcePrompt {
        suffix: Some("another rule".into()),
        ..with_examples(&one)
    };
    assert_ne!(h(&one), prompt_hash(&cache_prompt(&cats(), &other)));
    // The full prompt does carry them.
    assert_ne!(
        system_prompt(&cats(), &with_examples(&one)),
        system_prompt(&cats(), &with_examples(&[]))
    );
}

#[test]
fn examples_reach_new_requests_but_do_not_invalidate_cached_answers() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("Receipts", "Lidl receipt", "OK")),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let c = classifier(&s.uri(), &cache);
    let file = req(d.path(), "a.jpg", cats());
    let ask = |r: AiRequest| c.classify(vec![r]).unwrap();
    let with = |lines: &[&str]| AiRequest {
        prompt: with_examples(lines),
        ..file.clone()
    };
    ask(with(&[]));
    assert_eq!(received(&rt, &s).len(), 1);
    ask(with(&["receipt — AI chose Boards, correct is Receipts"]));
    assert_eq!(
        received(&rt, &s).len(),
        1,
        "cached answer survives the new example"
    );

    // A file the cache does not know is asked with the examples in the body.
    let other = req(d.path(), "b.jpg", cats());
    ask(AiRequest {
        prompt: with_examples(&["receipt — AI chose Boards, correct is Receipts"]),
        ..other
    });
    let reqs = received(&rt, &s);
    assert_eq!(reqs.len(), 2);
    let body: Value = serde_json::from_slice(&reqs[1].body).unwrap();
    let system = body["system"][0]["text"].as_str().unwrap();
    assert!(system.contains("The user's past corrections:"), "{system}");
    assert!(system.contains("correct is Receipts"));
}

#[test]
fn system_prompt_differs_per_source_texts() {
    let a = system_prompt(&cats(), &frame(Some("camera"), Some("rule A")));
    let b = system_prompt(&cats(), &frame(Some("Partner"), Some("rule B")));
    assert_ne!(a, b);
    // Each slot alone changes the prompt too.
    let base = system_prompt(&cats(), &SourcePrompt::default());
    assert_ne!(system_prompt(&cats(), &frame(Some("x"), None)), base);
    assert_ne!(system_prompt(&cats(), &frame(None, Some("x"))), base);
}

#[test]
fn classify_cache_follows_source_suffix() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("Receipts", "Lidl receipt", "OK")),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let c = classifier(&s.uri(), &cache);
    // One file, made once: only the source texts change between the calls.
    let file = req(d.path(), "a.jpg", cats());
    let ask = |suffix: Option<&str>| {
        let r = AiRequest {
            prompt: frame(Some("camera"), suffix),
            ..file.clone()
        };
        c.classify(vec![r]).unwrap();
    };
    ask(Some("rule A"));
    assert_eq!(received(&rt, &s).len(), 1);
    ask(Some("rule A"));
    assert_eq!(received(&rt, &s).len(), 1, "same texts hit the cache");
    ask(Some("rule B"));
    assert_eq!(received(&rt, &s).len(), 2, "a new suffix misses the cache");
    let sent: Value = serde_json::from_slice(&received(&rt, &s)[1].body).unwrap();
    assert!(
        sent["system"][0]["text"]
            .as_str()
            .unwrap()
            .contains("rule B")
    );
    ask(None);
    assert_eq!(received(&rt, &s).len(), 3, "no suffix misses the cache");
}

/// Records each request's system prompt and start time; the first request
/// of every system prompt is slow, so a parallel start would show.
struct PerPrompt {
    seen: Arc<Mutex<Vec<(String, Instant)>>>,
}

impl Respond for PerPrompt {
    fn respond(&self, r: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&r.body).unwrap();
        let system = body["system"][0]["text"].as_str().unwrap().to_string();
        let mut seen = self.seen.lock().unwrap();
        let first = !seen.iter().any(|(s, _)| *s == system);
        seen.push((system, Instant::now()));
        let t = ResponseTemplate::new(200).set_body_json(answer_body("Receipts", "receipt", "OK"));
        if first {
            t.set_delay(Duration::from_millis(400))
        } else {
            t
        }
    }
}

#[test]
fn classify_groups_by_source_texts_with_own_warm_up() {
    require_tool!("magick");
    let rt = rt();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = server(&rt, PerPrompt { seen: seen.clone() });
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let camera = frame(Some("camera"), Some("rule A"));
    let partner = frame(Some("Partner"), Some("rule B"));
    // Same categories, two sources, interleaved.
    let batch = (0..6)
        .map(|i| {
            let f = if i % 2 == 0 { &camera } else { &partner };
            req_from(d.path(), &format!("{i}.jpg"), cats(), f.clone())
        })
        .collect();
    let r = c.classify(batch).unwrap();
    assert_eq!(r.len(), 6);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 6);
    let mut systems: Vec<&str> = seen.iter().map(|(s, _)| s.as_str()).collect();
    systems.dedup();
    systems.sort();
    systems.dedup();
    assert_eq!(systems.len(), 2, "one system prompt per source");
    for system in systems {
        let times: Vec<Instant> = seen
            .iter()
            .filter(|(s, _)| s == system)
            .map(|(_, t)| *t)
            .collect();
        assert_eq!(times.len(), 3);
        for later in &times[1..] {
            assert!(
                later.duration_since(times[0]) >= Duration::from_millis(350),
                "a request started before its prompt's warm-up finished"
            );
        }
    }
}

#[test]
fn classify_parses_structured_answer() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("Receipts", "Lidl receipt", "OK")),
    );
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let r = c.classify(vec![req(d.path(), "a.jpg", cats())]).unwrap();
    assert_eq!(r[0].category.as_deref(), Some("Receipts"));
    assert_eq!(r[0].reason, "Lidl receipt");
    assert_eq!(r[0].confidence, Confidence::Ok);

    let reqs = received(&rt, &s);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].headers.get("x-api-key").unwrap(), "test-key");
    assert_eq!(
        reqs[0].headers.get("anthropic-version").unwrap(),
        "2023-06-01"
    );
    let body: Value = serde_json::from_slice(&reqs[0].body).unwrap();
    assert_eq!(body["max_tokens"], 16000);
    assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
    assert_eq!(body["output_config"]["effort"], "low");
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(
        body["output_config"]["format"]["schema"]["properties"]["category"]["enum"],
        json!(["Receipts", "Boards", "NONE"])
    );
    assert_eq!(
        body["messages"][0]["content"][0]["source"]["media_type"],
        "image/jpeg"
    );
    assert_eq!(
        body["messages"][0]["content"][1]["text"],
        "This is a photo."
    );

    let u = c.usage();
    assert_eq!(
        (u.input, u.output, u.cache_write, u.cache_read),
        (10, 5, 100, 0)
    );
}

#[test]
fn classify_none_category_maps_to_none() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("NONE", "landscape", "OK")),
    );
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let r = c.classify(vec![req(d.path(), "a.jpg", cats())]).unwrap();
    assert_eq!(r[0].category, None);
}

#[test]
fn classify_legacy_none_category_still_maps_to_none() {
    require_tool!("magick");
    let rt = rt();
    // The answer an older prompt (or a cache entry written by it) carries.
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("НИКАКАЯ", "landscape", "OK")),
    );
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let r = c.classify(vec![req(d.path(), "a.jpg", cats())]).unwrap();
    assert_eq!(r[0].category, None);
}

#[test]
fn system_prompt_asks_for_the_reason_in_the_configured_language() {
    let with = |language: &str| SourcePrompt {
        language: language.into(),
        ..Default::default()
    };
    let ru = system_prompt(&cats(), &with("ru"));
    assert!(ru.contains("written in ru"), "{ru}");
    // Blank means the default, English.
    let en = system_prompt(&cats(), &SourcePrompt::default());
    assert!(en.contains("written in English"), "{en}");
    assert_eq!(system_prompt(&cats(), &with("  ")), en);
    assert_ne!(ru, en);
}

#[test]
fn classify_uses_cache_second_time() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("Boards", "ESP32 board", "LOW")),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let batch = vec![req(d.path(), "a.jpg", cats())];
    let first = classifier(&s.uri(), &cache)
        .classify(batch.clone())
        .unwrap();
    assert_eq!(received(&rt, &s).len(), 1);
    assert_eq!(std::fs::read_to_string(&cache).unwrap().lines().count(), 1);

    let second = classifier(&s.uri(), &cache).classify(batch).unwrap();
    assert_eq!(received(&rt, &s).len(), 1, "second run must not hit HTTP");
    assert_eq!(second[0].category, first[0].category);
    assert_eq!(second[0].confidence, Confidence::Low);
}

#[test]
fn classify_refusal_is_low() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200)
            .set_body_json(json!({"stop_reason": "refusal", "content": [], "usage": {}})),
    );
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let r = c.classify(vec![req(d.path(), "a.jpg", cats())]).unwrap();
    assert_eq!(r[0].category, None);
    assert_eq!(r[0].confidence, Confidence::Low);
    assert_eq!(r[0].reason, "model refused");
}

#[test]
fn an_answer_cut_off_at_max_tokens_goes_to_review_and_is_not_cached() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200)
            .set_body_json(json!({"stop_reason": "max_tokens", "content": [], "usage": {}})),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let c = classifier(&s.uri(), &cache);
    let r = c
        .classify(vec![
            req(d.path(), "a.jpg", cats()),
            req(d.path(), "b.jpg", cats()),
        ])
        .unwrap();
    assert_eq!(r.len(), 2);
    for a in &r {
        assert_eq!(a.category, None);
        assert_eq!(a.confidence, Confidence::Low);
        assert!(a.reason.contains("max_tokens"), "{}", a.reason);
    }
    assert!(
        std::fs::read_to_string(&cache)
            .unwrap_or_default()
            .is_empty()
    );
}

#[test]
fn classify_401_aborts() {
    require_tool!("magick");
    let rt = rt();
    let s = server(&rt, ResponseTemplate::new(401).set_body_string("{}"));
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let batch = (0..3)
        .map(|i| req(d.path(), &format!("{i}.jpg"), cats()))
        .collect();
    let e = c.classify(batch).unwrap_err();
    assert!(matches!(e, Error::Ai(m) if m.contains("API key rejected")));
    assert_eq!(
        received(&rt, &s).len(),
        1,
        "no retries, no further requests"
    );
}

struct Timed {
    seen: Arc<Mutex<Vec<Instant>>>,
}

impl Respond for Timed {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let mut seen = self.seen.lock().unwrap();
        let first = seen.is_empty();
        seen.push(Instant::now());
        let t = ResponseTemplate::new(200).set_body_json(answer_body("Receipts", "receipt", "OK"));
        if first {
            t.set_delay(Duration::from_millis(400))
        } else {
            t
        }
    }
}

#[test]
fn classify_warms_cache_first() {
    require_tool!("magick");
    let rt = rt();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s = server(&rt, Timed { seen: seen.clone() });
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let batch = (0..3)
        .map(|i| req(d.path(), &format!("{i}.jpg"), cats()))
        .collect();
    c.classify(batch).unwrap();
    let t = seen.lock().unwrap().clone();
    assert_eq!(t.len(), 3);
    for later in &t[1..] {
        assert!(
            later.duration_since(t[0]) >= Duration::from_millis(350),
            "later request started before the first finished"
        );
    }
}

#[test]
fn classify_keeps_input_order_across_category_sets() {
    require_tool!("magick");
    let rt = rt();
    let resp = |req: &Request| {
        // Answer with the first category name found in the system prompt.
        let body: Value = serde_json::from_slice(&req.body).unwrap();
        let name = body["output_config"]["format"]["schema"]["properties"]["category"]["enum"][0]
            .as_str()
            .unwrap()
            .to_string();
        ResponseTemplate::new(200).set_body_json(answer_body(&name, "ok", "OK"))
    };
    let s = server(&rt, resp);
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let other = vec![("Other".to_string(), "x".to_string())];
    let batch = vec![
        req(d.path(), "0.jpg", cats()),
        req(d.path(), "1.jpg", other.clone()),
        req(d.path(), "2.jpg", cats()),
    ];
    let r = c.classify(batch).unwrap();
    let got: Vec<_> = r.iter().map(|a| a.category.clone().unwrap()).collect();
    assert_eq!(got, ["Receipts", "Other", "Receipts"]);
    assert_eq!(received(&rt, &s).len(), 3);
}

#[test]
fn classify_undecodable_file_is_low_and_not_cached() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("Receipts", "receipt", "OK")),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let c = classifier(&s.uri(), &cache);
    let mut batch = vec![
        req(d.path(), "0.jpg", cats()),
        req(d.path(), "2.jpg", cats()),
        req(d.path(), "3.jpg", cats()),
    ];
    let bad = d.path().join("1.jpg");
    std::fs::write(&bad, b"\x00not an image at all\xff\xd8garbage").unwrap();
    let mut bad_req = batch[0].clone();
    bad_req.entry.path = bad.clone();
    bad_req.entry.rel = PathBuf::from("1.jpg");
    bad_req.entry.size = std::fs::metadata(&bad).unwrap().len();
    batch.insert(1, bad_req);

    let r = c.classify(batch).unwrap();
    assert_eq!(r.len(), 4);
    assert_eq!(r[1].category, None);
    assert_eq!(r[1].confidence, Confidence::Low);
    assert_eq!(r[1].reason, "could not make a preview");
    for i in [0, 2, 3] {
        assert_eq!(r[i].category.as_deref(), Some("Receipts"));
    }
    assert_eq!(received(&rt, &s).len(), 3);
    let lines = std::fs::read_to_string(&cache).unwrap();
    assert_eq!(lines.lines().count(), 3);
    assert!(!lines.contains("1.jpg"));
}

struct Flaky {
    n: Arc<Mutex<usize>>,
    fail_status: u16,
    fail_calls: Vec<usize>,
}

impl Respond for Flaky {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let mut n = self.n.lock().unwrap();
        *n += 1;
        if self.fail_calls.contains(&*n) {
            ResponseTemplate::new(self.fail_status).set_body_string("{}")
        } else {
            ResponseTemplate::new(200).set_body_json(answer_body("Receipts", "receipt", "OK"))
        }
    }
}

#[test]
fn classify_retries_429_then_succeeds() {
    require_tool!("magick");
    let rt = rt();
    let n = Arc::new(Mutex::new(0));
    let s = server(
        &rt,
        Flaky {
            n: n.clone(),
            fail_status: 429,
            fail_calls: vec![1, 2],
        },
    );
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl")).with_backoff_ms(1);
    let r = c.classify(vec![req(d.path(), "a.jpg", cats())]).unwrap();
    assert_eq!(r[0].category.as_deref(), Some("Receipts"));
    assert_eq!(*n.lock().unwrap(), 3);
}

#[test]
fn classify_401_in_parallel_phase_aborts() {
    require_tool!("magick");
    let rt = rt();
    let n = Arc::new(Mutex::new(0));
    // Call 1 (warm-up) succeeds; the rest get 401.
    let s = server(
        &rt,
        Flaky {
            n: n.clone(),
            fail_status: 401,
            fail_calls: vec![2, 3, 4],
        },
    );
    let d = tempfile::tempdir().unwrap();
    let c = classifier(&s.uri(), &d.path().join("cache.jsonl"));
    let batch = (0..4)
        .map(|i| req(d.path(), &format!("{i}.jpg"), cats()))
        .collect();
    let e = c.classify(batch).unwrap_err();
    assert!(matches!(e, Error::Ai(m) if m.contains("API key rejected")));
}

#[test]
fn classify_with_reports_each_answer_and_stops_on_cancel() {
    use gallery_core::progress::{Cancel, ProgressEvent, Stage};
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body("Boards", "board", "HIGH")),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let batch: Vec<AiRequest> = ["a.jpg", "b.jpg", "c.jpg"]
        .iter()
        .map(|n| req(d.path(), n, cats()))
        .collect();

    // Cancelled before the start: no request is sent.
    let cancel = Cancel::new();
    cancel.cancel();
    let r = classifier(&s.uri(), &cache).classify_with(
        batch.clone(),
        &gallery_core::progress::NoProgress,
        &cancel,
    );
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert_eq!(received(&rt, &s).len(), 0);

    // Cancelled once the first (warm-up) answer is in: nothing else starts,
    // and the paid answer stays in the cache.
    let cancel = Cancel::new();
    let c = cancel.clone();
    let stop = move |e: ProgressEvent| {
        if e.done >= 1 {
            c.cancel();
        }
    };
    let r = classifier(&s.uri(), &cache).classify_with(batch.clone(), &stop, &cancel);
    assert!(matches!(r, Err(Error::Cancelled)), "{r:?}");
    assert_eq!(received(&rt, &s).len(), 1);
    assert_eq!(std::fs::read_to_string(&cache).unwrap().lines().count(), 1);

    // A full run: the cached answer counts at once, the others one by one.
    let evs = Mutex::new(Vec::new());
    let rec = |e: ProgressEvent| evs.lock().unwrap().push(e);
    let out = classifier(&s.uri(), &cache)
        .classify_with(batch, &rec, &Cancel::new())
        .unwrap();
    assert_eq!(out.len(), 3);
    let evs = evs.into_inner().unwrap();
    assert!(evs.iter().all(|e| e.stage == Stage::Ai && e.total == 3));
    let dones: Vec<usize> = evs.iter().map(|e| e.done).collect();
    assert_eq!(dones, [1, 2, 3]);
}

#[test]
fn classify_asks_for_tags_and_reads_them() {
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body_with(json!({
            "category": "Receipts", "reason": "Lidl receipt", "confidence": "OK",
            "tags": ["Receipt", " lidl ", "receipt", "", "shop\nfood"],
            "description": "A  Lidl receipt\non a table.",
        }))),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let file = req(d.path(), "a.jpg", cats());
    let r = classifier(&s.uri(), &cache)
        .classify(vec![file.clone()])
        .unwrap();
    let meta = r[0].meta.clone().expect("tags and description");
    // Lowercase, one line, no blanks or repeats.
    assert_eq!(meta.tags, ["receipt", "lidl", "shop food"]);
    assert_eq!(meta.description, "A Lidl receipt on a table.");

    let body: Value = serde_json::from_slice(&received(&rt, &s)[0].body).unwrap();
    let schema = &body["output_config"]["format"]["schema"];
    assert_eq!(schema["properties"]["tags"]["type"], "array");
    assert_eq!(schema["properties"]["tags"]["items"]["type"], "string");
    assert_eq!(schema["properties"]["description"]["type"], "string");
    // Required: left optional, the model always left the description out.
    assert_eq!(
        schema["required"],
        json!(["category", "reason", "confidence", "tags", "description"])
    );
    assert_eq!(schema["additionalProperties"], json!(false));
    let system = body["system"][0]["text"].as_str().unwrap();
    assert!(system.contains("- tags — 5-20 short keywords"), "{system}");

    // The cache keeps them: the next run has them without a request.
    let again = classifier(&s.uri(), &cache).classify(vec![file]).unwrap();
    assert_eq!(received(&rt, &s).len(), 1);
    assert_eq!(again[0].meta.as_ref(), Some(&meta));
}

#[test]
fn answer_cached_before_meta_version_is_asked_again_and_replaced() {
    use gallery_core::ai::cache::{key, prompt_hash};
    use gallery_core::ai::cache_prompt;
    use gallery_core::ai::client::{cached_answer, is_cached};
    require_tool!("magick");
    let rt = rt();
    let s = server(
        &rt,
        ResponseTemplate::new(200).set_body_json(answer_body_with(json!({
            "category": "Boards", "reason": "new answer", "confidence": "OK",
            "tags": ["board"], "description": "A board on a desk.",
        }))),
    );
    let d = tempfile::tempdir().unwrap();
    let cache = d.path().join("cache.jsonl");
    let file = req(d.path(), "a.jpg", cats());
    // A row exactly as an older build wrote it: no tags, no description, no meta.
    let model = AiSettings::default().model;
    let k = key(
        &file.entry,
        &model,
        &prompt_hash(&cache_prompt(&cats(), &SourcePrompt::default())),
    );
    let row = json!({"key": k, "result": {
        "category": "Receipts", "reason": "old answer", "confidence": "OK"
    }});
    std::fs::write(&cache, format!("{row}\n")).unwrap();
    let old = gallery_core::ai::cache::Cache::load(&cache);
    assert!(!is_cached(&old, &model, &file));
    assert!(cached_answer(&old, &model, &file).is_none());

    let r = classifier(&s.uri(), &cache)
        .classify(vec![file.clone()])
        .unwrap();
    assert_eq!(received(&rt, &s).len(), 1, "asked again");
    assert_eq!(r[0].reason, "new answer");
    let loaded = gallery_core::ai::cache::Cache::load(&cache);
    assert!(is_cached(&loaded, &model, &file));
    let hit = cached_answer(&loaded, &model, &file).expect("replaced");
    assert_eq!(hit.reason, "new answer");
    assert_eq!(hit.meta.unwrap().description, "A board on a desk.");
    let text = std::fs::read_to_string(&cache).unwrap();
    assert!(
        text.lines().last().unwrap().contains("\"meta\":2"),
        "{text}"
    );

    // Now current: not asked a third time.
    classifier(&s.uri(), &cache).classify(vec![file]).unwrap();
    assert_eq!(received(&rt, &s).len(), 1);
}

/// A stand-in `claude` that records its arguments, environment and stdin
/// and prints a stream-json transcript ending in `result`.
#[cfg(unix)]
fn fake_claude(dir: &Path, result: &Value) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let script = dir.join("claude");
    let log = dir.join("claude.log");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{log}.args'\n\
             env > '{log}.env'\ncat > '{log}.stdin'\n\
             echo '{{\"type\":\"system\",\"subtype\":\"init\"}}'\necho '{result}'\n",
            log = log.display(),
            result = result,
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script
}

#[cfg(unix)]
#[test]
fn claude_code_backend_runs_the_cli_without_the_api_key() {
    require_tool!("magick");
    let d = tempfile::tempdir().unwrap();
    let answer = json!({"category": "Receipts", "reason": "a receipt", "confidence": "OK",
                        "tags": ["receipt"], "description": "A shop receipt."});
    let claude = fake_claude(
        d.path(),
        &json!({"type": "result", "subtype": "success", "is_error": false,
                "structured_output": answer,
                "usage": {"input_tokens": 3, "output_tokens": 7}}),
    );
    let settings = AiSettings {
        backend: "claude-code".into(),
        ..AiSettings::default()
    };
    let cache = d.path().join("cache.jsonl");
    // An API key in the environment must not reach the CLI (it would bill it).
    let c = AiClassifier::with_api_key(
        &settings,
        tools(),
        cache.clone(),
        "http://127.0.0.1:9".into(),
        Some("test-key".into()),
    )
    .unwrap()
    .with_claude(claude);
    let file = req(d.path(), "a.jpg", cats());
    let a = c.classify(vec![file.clone()]).unwrap();
    assert_eq!(a[0].category.as_deref(), Some("Receipts"));
    assert_eq!(a[0].confidence, Confidence::Ok);
    assert_eq!(c.usage().output, 7);

    let log =
        |ext: &str| std::fs::read_to_string(d.path().join(format!("claude.log.{ext}"))).unwrap();
    let args = log("args");
    for flag in [
        "-p",
        "--json-schema",
        "--system-prompt",
        "--no-session-persistence",
        "stream-json",
    ] {
        assert!(args.lines().any(|l| l == flag), "{flag} missing: {args}");
    }
    let stdin: Value = serde_json::from_str(log("stdin").trim()).unwrap();
    assert_eq!(stdin["message"]["content"][0]["type"], "image");
    // The answer is stored like an API answer: asked once.
    std::fs::remove_file(d.path().join("claude.log.stdin")).unwrap();
    c.classify(vec![file]).unwrap();
    assert!(!d.path().join("claude.log.stdin").exists(), "asked again");
    assert_eq!(std::fs::read_to_string(&cache).unwrap().lines().count(), 1);
}

#[cfg(unix)]
#[test]
fn claude_code_backend_error_stops_the_run_with_its_text() {
    require_tool!("magick");
    let d = tempfile::tempdir().unwrap();
    let claude = fake_claude(
        d.path(),
        &json!({"type": "result", "subtype": "success", "is_error": true,
                "result": "Claude AI usage limit reached"}),
    );
    let settings = AiSettings {
        backend: "claude-code".into(),
        ..AiSettings::default()
    };
    let c = AiClassifier::with_api_key(
        &settings,
        tools(),
        d.path().join("cache.jsonl"),
        "http://127.0.0.1:9".into(),
        None,
    )
    .unwrap()
    .with_claude(claude);
    let e = c
        .classify(vec![req(d.path(), "a.jpg", cats())])
        .unwrap_err()
        .to_string();
    assert!(e.contains("usage limit reached"), "{e}");
}
