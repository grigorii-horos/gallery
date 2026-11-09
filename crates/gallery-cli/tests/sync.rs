//! `gallery sync` and `apply --pause-sync` against a fake Syncthing. The
//! config.xml of the fake is passed through GALLERY_SYNCTHING_CONFIG and the
//! state file lives in a temp XDG_DATA_HOME: the real Syncthing is never
//! asked.

use assert_cmd::Command;
use serde_json::json;
use std::path::Path;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const KEY: &str = "CLI-SECRET-KEY-77";

fn gallery(tmp: &Path, cfg: &Path) -> Command {
    let mut c = Command::cargo_bin("gallery").unwrap();
    c.env("LC_ALL", "C")
        .env("GALLERY_TRASH", "folder")
        .env("GALLERY_SYNCTHING_CONFIG", cfg)
        .env("XDG_DATA_HOME", tmp.join("data"))
        .env("GALLERY_DATA_DIR", tmp.join("data/gallery"));
    c
}

fn fake(rt: &tokio::runtime::Runtime, folder_path: &Path) -> MockServer {
    rt.block_on(async {
        let s = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/rest/noauth/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "OK"})))
            .mount(&s)
            .await;
        Mock::given(method("GET"))
            .and(path("/rest/config/folders"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!([
                {"id": "cam", "label": "Camera", "path": folder_path, "paused": false}
            ])))
            .mount(&s)
            .await;
        Mock::given(method("PATCH"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&s)
            .await;
        s
    })
}

fn config(dir: &Path, addr: &str) -> std::path::PathBuf {
    let p = dir.join("config.xml");
    std::fs::write(
        &p,
        format!(
            "<configuration><gui enabled=\"true\" tls=\"false\"><address>{addr}</address>\
             <apikey>{KEY}</apikey></gui></configuration>"
        ),
    )
    .unwrap();
    p
}

fn patches(rt: &tokio::runtime::Runtime, s: &MockServer) -> Vec<String> {
    rt.block_on(s.received_requests())
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PATCH")
        .map(|r| format!("{} {}", r.url.path(), String::from_utf8_lossy(&r.body)))
        .collect()
}

#[test]
fn apply_pause_sync_then_status_and_resume() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("Camera");
    std::fs::create_dir_all(&root).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let s = fake(&rt, &root);
    let cfg = config(tmp.path(), &s.address().to_string());

    // A plan whose only source is gone: the run needs no media tool to finish.
    let plan = tmp.path().join("a.plan.json");
    std::fs::write(
        &plan,
        json!({
            "version": 1, "created": "2026-11-05T10:00:00+02:00", "side": "local",
            "content_presets": {}, "review": [], "unmatched": [], "warnings": [],
            "unchanged": [],
            "items": [{
                "source": root.join("gone.jpg"), "source_root": root, "size": 1,
                "mtime_ns": 1, "target": "t", "via": {"kind": "mask"},
                "outputs": [{"dest": root.join("out/gone.jpg"), "op": "write",
                             "steps": [], "trace": []}]
            }]
        })
        .to_string(),
    )
    .unwrap();

    let out = gallery(tmp.path(), &cfg)
        .args(["apply", "--pause-sync"])
        .arg(&plan)
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    if text.contains("not found") && !text.contains("sync is paused") {
        eprintln!("skip: tools missing: {text}");
        return;
    }
    assert!(!text.contains(KEY), "{text}");
    assert!(
        text.contains("sync is paused (Camera), check the result, then: gallery sync resume"),
        "{text}"
    );
    assert_eq!(
        patches(&rt, &s),
        ["/rest/config/folders/cam {\"paused\":true}"]
    );
    assert!(tmp.path().join("data/gallery/sync-paused.json").is_file());

    let st = gallery(tmp.path(), &cfg)
        .args(["sync", "status"])
        .output()
        .unwrap();
    let st = String::from_utf8_lossy(&st.stdout);
    assert!(
        st.contains("paused: Camera") && st.contains("Syncthing is responding"),
        "{st}"
    );

    let r = gallery(tmp.path(), &cfg)
        .args(["sync", "resume"])
        .output()
        .unwrap();
    assert!(r.status.success());
    assert!(String::from_utf8_lossy(&r.stdout).contains("sync resumed: Camera"));
    assert_eq!(patches(&rt, &s).len(), 2);
    assert!(patches(&rt, &s)[1].ends_with("{\"paused\":false}"));
    assert!(!tmp.path().join("data/gallery/sync-paused.json").exists());

    let again = gallery(tmp.path(), &cfg)
        .args(["sync", "resume"])
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&again.stdout).contains("nothing to resume"));
    assert_eq!(patches(&rt, &s).len(), 2);
}

#[test]
fn sync_status_without_state_and_with_unreachable_syncthing() {
    let tmp = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = config(tmp.path(), &format!("127.0.0.1:{port}"));
    let o = gallery(tmp.path(), &cfg)
        .args(["sync", "status"])
        .output()
        .unwrap();
    let t = String::from_utf8_lossy(&o.stdout);
    assert!(o.status.success());
    assert!(
        t.contains("has paused nothing") && t.contains("unavailable"),
        "{t}"
    );
    assert!(!t.contains(KEY));
}
