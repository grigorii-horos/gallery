//! The `claude-code` AI backend: each file is one headless run of the
//! official Claude Code CLI (`claude -p`), signed in with the user's own
//! Claude subscription. Gallery never reads or stores that login; the CLI
//! does all of it. The API key variables are removed from the child's
//! environment, since in `-p` mode Claude Code bills a key when it sees one.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

use super::cache::{META_VERSION, Stored};
use crate::{Error, Result};

/// `[ai] backend` value that selects this backend.
pub const BACKEND: &str = "claude-code";

/// Variables that would make Claude Code bill an API key instead of the
/// subscription.
const KEY_VARS: [&str; 3] = [
    "ANTHROPIC_API_KEY",
    "CLAUDE_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
];

/// What one run gave: the answer (`None` when it was cut off) and the
/// run's token usage (the Messages API `usage` shape).
pub struct Run {
    pub stored: Option<Stored>,
    pub usage: Option<Value>,
}

/// What to ask in one run.
#[derive(Clone, Copy)]
pub struct Ask<'a> {
    pub model: &'a str,
    pub effort: &'a str,
    /// The classification prompt; replaces Claude Code's own.
    pub system: &'a str,
    pub schema: &'a Value,
    /// The JPEG thumbnail, base64.
    pub image_b64: &'a str,
    /// The text after the image ("This is a photo.").
    pub note: &'a str,
}

/// The `claude` executable, if it is installed.
pub fn find() -> Option<PathBuf> {
    crate::tools::find("claude")
}

/// One headless run: `system` replaces Claude Code's own system prompt, no
/// tools, no settings, MCP servers, skills or saved session; the thumbnail
/// goes in as an image block on stdin; the answer must match `schema`.
/// Runs in `dir` (an empty folder), so no project instructions are read.
pub fn classify(claude: &Path, dir: &Path, ask: &Ask<'_>) -> Result<Run> {
    let Ask {
        model,
        effort,
        system,
        schema,
        image_b64,
        note,
    } = *ask;
    let fail = |e: String| Error::Ai(crate::t!("ai.claude_code_failed", error = e));
    let mut cmd = Command::new(claude);
    cmd.current_dir(dir)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
        ])
        .arg("--verbose")
        .arg("--json-schema")
        .arg(schema.to_string())
        .arg("--system-prompt")
        .arg(system)
        .args(["--tools", ""])
        .args(["--model", model])
        .args(["--no-session-persistence", "--setting-sources", ""])
        .args(["--strict-mcp-config", "--disable-slash-commands"]);
    if !effort.is_empty() {
        cmd.args(["--effort", effort]);
    }
    for v in KEY_VARS {
        cmd.env_remove(v);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(e.to_string()))?;
    let message = json!({
        "type": "user",
        "message": {"role": "user", "content": [
            {"type": "image", "source": {"type": "base64", "media_type": "image/jpeg", "data": image_b64}},
            {"type": "text", "text": note},
        ]},
    });
    {
        let mut stdin = child.stdin.take().expect("piped stdin");
        writeln!(stdin, "{message}").map_err(|e| fail(e.to_string()))?;
        // Dropped here: end of input ends the run after this message.
    }
    let out = child.wait_with_output().map_err(|e| fail(e.to_string()))?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match parse(&stdout) {
        Some(r) => r,
        None => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let head: String = stderr.trim().chars().take(300).collect();
            Err(fail(if head.is_empty() {
                format!("exit {}", out.status)
            } else {
                head
            }))
        }
    }
}

/// The `result` event of a stream-json transcript; `None` when there is none.
fn parse(stdout: &str) -> Option<Result<Run>> {
    let result = stdout
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v.get("type").and_then(Value::as_str) == Some("result"))?;
    let usage = result.get("usage").cloned();
    if result.get("is_error").and_then(Value::as_bool) == Some(true) {
        let text = result
            .get("result")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| result.get("errors").map(Value::to_string))
            .or_else(|| result.get("subtype").map(Value::to_string))
            .unwrap_or_default();
        return Some(Err(Error::Ai(crate::t!(
            "ai.claude_code_failed",
            error = text
        ))));
    }
    if result.get("stop_reason").and_then(Value::as_str) == Some("refusal") {
        return Some(Ok(Run {
            stored: Some(Stored {
                category: super::client::NONE_CATEGORY.into(),
                reason: crate::t!("ai.refused"),
                confidence: "LOW".into(),
                tags: Vec::new(),
                description: String::new(),
                meta: META_VERSION,
            }),
            usage,
        }));
    }
    let Some(answer) = result.get("structured_output").filter(|v| !v.is_null()) else {
        // Ran out of room before answering: like a cut-off API answer.
        return Some(Ok(Run {
            stored: None,
            usage,
        }));
    };
    Some(
        serde_json::from_value::<Stored>(answer.clone())
            .map(|s| Run {
                stored: Some(Stored {
                    meta: META_VERSION,
                    ..s
                }),
                usage,
            })
            .map_err(|e| Error::Ai(crate::t!("ai.bad_answer", error = e))),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const INIT: &str = r#"{"type":"system","subtype":"init"}"#;

    #[test]
    fn the_answer_comes_from_structured_output() {
        let out = format!(
            "{INIT}\n{}\n",
            r#"{"type":"result","subtype":"success","is_error":false,"structured_output":{"category":"docs","reason":"r","confidence":"OK","tags":["a"],"description":"d"},"usage":{"input_tokens":2,"output_tokens":5}}"#
        );
        let r = parse(&out).unwrap().unwrap();
        let s = r.stored.unwrap();
        assert_eq!((s.category.as_str(), s.meta), ("docs", META_VERSION));
        assert_eq!(r.usage.unwrap()["output_tokens"], 5);
    }

    #[test]
    fn an_error_result_is_an_error_with_its_text() {
        let out = r#"{"type":"result","subtype":"success","is_error":true,"result":"Claude AI usage limit reached"}"#;
        let e = parse(out).unwrap().err().unwrap().to_string();
        assert!(e.contains("usage limit reached"), "{e}");
    }

    #[test]
    fn no_structured_output_is_a_cut_off_answer() {
        let out = r#"{"type":"result","subtype":"success","is_error":false,"result":""}"#;
        assert!(parse(out).unwrap().unwrap().stored.is_none());
    }

    #[test]
    fn no_result_event_is_none() {
        assert!(parse(INIT).is_none());
        assert!(parse("").is_none());
    }
}
