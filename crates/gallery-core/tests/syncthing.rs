//! The local Syncthing pause, against a fake Syncthing (wiremock) and a temp
//! `config.xml`. Nothing here may reach a real Syncthing: every client is
//! built from a config that points at the fake.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gallery_core::plan::Plan;
use gallery_core::syncthing::{
    Folder, PauseState, SyncPause, Syncthing, folders_for, load_state, plan_roots,
};
use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const KEY: &str = "SECRETKEY-9f3a";

struct Fake {
    rt: tokio::runtime::Runtime,
    server: MockServer,
}

impl Fake {
    fn new() -> Fake {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let server = rt.block_on(MockServer::start());
        Fake { rt, server }
    }
    fn mount(&self, m: Mock) {
        self.rt.block_on(m.mount(&self.server));
    }
    fn addr(&self) -> String {
        self.server.address().to_string()
    }
    fn folders(&self, folders: serde_json::Value) {
        self.mount(
            Mock::given(method("GET"))
                .and(path("/rest/config/folders"))
                .and(header("X-API-Key", KEY))
                .respond_with(ResponseTemplate::new(200).set_body_json(folders)),
        );
    }
    fn requests(&self) -> Vec<Request> {
        self.rt
            .block_on(self.server.received_requests())
            .unwrap_or_default()
    }
    fn patches(&self) -> Vec<(String, serde_json::Value)> {
        self.requests()
            .into_iter()
            .filter(|r| r.method.as_str() == "PATCH")
            .map(|r| {
                (
                    r.url.path().to_string(),
                    serde_json::from_slice(&r.body).unwrap(),
                )
            })
            .collect()
    }
}

fn config_xml(dir: &Path, address: &str) -> PathBuf {
    let p = dir.join("config.xml");
    std::fs::write(
        &p,
        format!(
            r#"<configuration version="37">
<folder id="x"><address>not-this</address></folder>
<gui enabled="true" tls="false" debugging="false">
    <address>{address}</address>
    <apikey>{KEY}</apikey>
    <theme>default</theme>
</gui>
</configuration>"#
        ),
    )
    .unwrap();
    p
}

fn plan(root: &Path, out: &Path) -> Plan {
    let mut p: Plan = serde_json::from_value(json!({
        "version": 1, "created": "2026-11-05T10:00:00+02:00", "side": "local",
        "content_presets": {}, "items": [], "review": [],
        "unmatched": [], "warnings": [], "unchanged": []
    }))
    .unwrap();
    p.items = serde_json::from_value(json!([{
        "source": root.join("a.jpg"), "source_root": root, "size": 1, "mtime_ns": 1,
        "target": "t", "via": {"kind": "mask"},
        "outputs": [{"dest": out.join("2026/a.jpg"), "op": "write", "steps": [], "trace": []}]
    }]))
    .unwrap();
    p
}

fn sync_for(fake: &Fake, tmp: &Path) -> SyncPause {
    let cfg = config_xml(tmp, &fake.addr());
    SyncPause {
        client: Syncthing::from_config(&cfg).unwrap(),
        state_path: tmp.join("state").join("sync-paused.json"),
    }
}

fn folder(id: &str, label: &str, path: &Path, paused: bool) -> serde_json::Value {
    json!({"id": id, "label": label, "path": path, "paused": paused})
}

#[test]
fn folders_match_roots_and_outputs_not_others() {
    let tmp = tempfile::tempdir().unwrap();
    let cam = tmp.path().join("Camera");
    let shots = tmp.path().join("Screenshots");
    let other = tmp.path().join("Docs");
    let out = tmp.path().join("Library");
    for d in [&cam, &shots, &other, &out] {
        std::fs::create_dir_all(d).unwrap();
    }
    let f = |id: &str, p: &Path| Folder {
        id: id.into(),
        label: id.into(),
        path: p.display().to_string(),
        paused: false,
    };
    let all = vec![
        f("cam", &cam),
        f("shots", &shots),
        f("docs", &other),
        f("lib", &out),
    ];
    // The source root inside Camera, the output dir inside Library (not yet existing).
    let p = plan(&cam.join("2026"), &out);
    let ids: Vec<String> = folders_for(&all, &plan_roots(&p))
        .into_iter()
        .map(|f| f.id)
        .collect();
    assert_eq!(ids, ["cam", "lib"]);
}

#[test]
fn folder_path_with_tilde_and_symlink_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    std::fs::create_dir_all(&real).unwrap();
    let link = tmp.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let f = Folder {
        id: "r".into(),
        label: String::new(),
        path: real.display().to_string(),
        paused: false,
    };
    assert_eq!(
        folders_for(std::slice::from_ref(&f), &[link.join("sub")]).len(),
        1
    );
    assert_eq!(f.name(), "r");
}

#[test]
fn pauses_only_not_already_paused_and_writes_state_first() {
    let fake = Fake::new();
    let tmp = tempfile::tempdir().unwrap();
    let (cam, shots) = (tmp.path().join("Camera"), tmp.path().join("Shots"));
    std::fs::create_dir_all(&cam).unwrap();
    std::fs::create_dir_all(&shots).unwrap();
    fake.folders(json!([
        folder("cam", "Camera", &cam, false),
        folder("shots", "Screenshots", &shots, true),
    ]));
    let sync = sync_for(&fake, tmp.path());
    let state_seen = Arc::new(AtomicBool::new(false));
    struct Check(PathBuf, Arc<AtomicBool>);
    impl Respond for Check {
        fn respond(&self, _: &Request) -> ResponseTemplate {
            // The state file must exist before the PATCH arrives.
            self.1.store(self.0.exists(), Ordering::SeqCst);
            ResponseTemplate::new(200)
        }
    }
    fake.mount(
        Mock::given(method("PATCH"))
            .and(path("/rest/config/folders/cam"))
            .and(header("X-API-Key", KEY))
            .and(body_json(json!({"paused": true})))
            .respond_with(Check(sync.state_path.clone(), state_seen.clone()))
            .expect(1),
    );
    let p = plan(&cam, &shots);
    let st = sync
        .pause_for(&tmp.path().join("x.plan.json"), &p)
        .unwrap()
        .unwrap();
    assert!(
        state_seen.load(Ordering::SeqCst),
        "state written after PATCH"
    );
    assert_eq!(st.folders.len(), 1);
    assert_eq!(st.folders[0].id, "cam");
    assert_eq!(fake.patches().len(), 1);
    let on_disk = load_state(&sync.state_path).unwrap().unwrap();
    assert_eq!(on_disk, st);
    assert!(
        on_disk.report.ends_with("x.plan.report.json"),
        "{}",
        on_disk.report
    );
}

#[test]
fn resume_resumes_only_ours_and_clears_state() {
    let fake = Fake::new();
    let tmp = tempfile::tempdir().unwrap();
    let cam = tmp.path().join("Camera");
    std::fs::create_dir_all(&cam).unwrap();
    fake.folders(json!([folder("cam", "Camera", &cam, false)]));
    fake.mount(
        Mock::given(method("PATCH"))
            .and(path("/rest/config/folders/cam"))
            .respond_with(ResponseTemplate::new(200)),
    );
    let sync = sync_for(&fake, tmp.path());
    sync.pause_for(&tmp.path().join("p.plan.json"), &plan(&cam, &cam))
        .unwrap();
    let out = sync.resume().unwrap();
    assert_eq!(out.resumed, ["Camera"]);
    assert!(out.failed.is_empty());
    let patches = fake.patches();
    assert_eq!(patches.len(), 2);
    assert_eq!(
        patches[1],
        (
            "/rest/config/folders/cam".to_string(),
            json!({"paused": false})
        )
    );
    assert!(!sync.state_path.exists());
    // Nothing more to resume.
    assert_eq!(sync.resume().unwrap().resumed.len(), 0);
    assert_eq!(fake.patches().len(), 2);
}

#[test]
fn second_run_keeps_the_first_runs_folders_in_the_state() {
    let fake = Fake::new();
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("A"), tmp.path().join("B"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    // A is paused already (by the first run), B is new.
    fake.folders(json!([
        folder("a", "A", &a, true),
        folder("b", "B", &b, false)
    ]));
    fake.mount(Mock::given(method("PATCH")).respond_with(ResponseTemplate::new(200)));
    let sync = sync_for(&fake, tmp.path());
    let first = PauseState {
        folders: vec![gallery_core::syncthing::PausedFolder {
            id: "a".into(),
            label: "A".into(),
        }],
        plan: "p1".into(),
        report: "r1".into(),
        paused_at: "t".into(),
    };
    std::fs::create_dir_all(sync.state_path.parent().unwrap()).unwrap();
    std::fs::write(&sync.state_path, serde_json::to_string(&first).unwrap()).unwrap();
    let mut p = plan(&a, &b);
    p.items[0].source_root = a.clone();
    let st = sync
        .pause_for(&tmp.path().join("p2.plan.json"), &p)
        .unwrap()
        .unwrap();
    let ids: Vec<_> = st.folders.iter().map(|f| f.id.as_str()).collect();
    assert_eq!(ids, ["a", "b"]);
    assert_eq!(fake.patches().len(), 1, "only B is paused now");
}

#[test]
fn unreachable_syncthing_is_an_error_without_the_key_and_no_state() {
    let tmp = tempfile::tempdir().unwrap();
    // A port nobody listens on: bind, note it, drop.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let cfg = config_xml(tmp.path(), &format!("127.0.0.1:{port}"));
    let sync = SyncPause {
        client: Syncthing::from_config(&cfg).unwrap(),
        state_path: tmp.path().join("s.json"),
    };
    let e = sync
        .pause_for(
            &tmp.path().join("p.plan.json"),
            &plan(tmp.path(), tmp.path()),
        )
        .unwrap_err()
        .to_string();
    assert!(!e.contains(KEY), "{e}");
    assert!(!sync.state_path.exists());
    let c = sync.check(&plan(tmp.path(), tmp.path()));
    assert!(!c.reachable);
    assert!(!format!("{c:?}{:?}", sync.client).contains(KEY));
}

#[test]
fn server_errors_never_carry_the_key() {
    let fake = Fake::new();
    let tmp = tempfile::tempdir().unwrap();
    fake.mount(
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(500).set_body_string(KEY)),
    );
    let sync = sync_for(&fake, tmp.path());
    let e = sync.client.folders().unwrap_err().to_string();
    assert!(e.contains("500") && !e.contains(KEY), "{e}");
    assert!(sync.client.health().is_err());
}

#[test]
fn health_and_check_report_reachable_with_labels() {
    let fake = Fake::new();
    let tmp = tempfile::tempdir().unwrap();
    let cam = tmp.path().join("Camera");
    std::fs::create_dir_all(&cam).unwrap();
    fake.mount(
        Mock::given(method("GET"))
            .and(path("/rest/noauth/health"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"status": "OK"}))),
    );
    fake.folders(json!([folder("cam", "Camera", &cam, false)]));
    let sync = sync_for(&fake, tmp.path());
    let c = sync.check(&plan(&cam, &cam));
    assert!(c.reachable, "{c:?}");
    assert_eq!(c.folders, ["Camera"]);
    assert!(fake.patches().is_empty(), "a check pauses nothing");
}

#[test]
fn only_loopback_gui_addresses_are_used() {
    let tmp = tempfile::tempdir().unwrap();
    for bad in ["192.168.1.5:8384", "example.com:8384"] {
        let cfg = config_xml(tmp.path(), bad);
        let e = Syncthing::from_config(&cfg).unwrap_err().to_string();
        assert!(e.contains("not listening on this computer"), "{e}");
    }
    let cfg = config_xml(tmp.path(), "0.0.0.0:8384");
    assert!(Syncthing::from_config(&cfg).is_ok());
    assert!(Syncthing::from_config(&tmp.path().join("none.xml")).is_err());
}
