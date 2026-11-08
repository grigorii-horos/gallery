//! Pausing the local Syncthing folders a run touches.
//!
//! Results reach the phones through Syncthing; a bad run would spread before
//! anyone looks at it. So a run can pause the Syncthing folders it touches
//! and leave them paused until the user says the result is fine.
//!
//! Rules:
//! - only the LOCAL instance, found in its own `config.xml` (GUI address on a
//!   loopback host and API key); the key is read, never logged or returned;
//! - only folders that were not paused already are paused, and only those
//!   are resumed later;
//! - the state file is written BEFORE the first pause, so a crash never
//!   leaves folders paused with nobody knowing.
//!
//! `GALLERY_SYNCTHING_CONFIG` names the `config.xml` to use instead of the
//! default places (for tests: a fake Syncthing). When set it is the only
//! place looked at. Debug builds only: a release build never honours it, so
//! the environment cannot point the real app at another Syncthing.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::plan::Plan;
use crate::{Error, Result};

/// Env var naming the `config.xml` to read (tests).
pub const CONFIG_ENV: &str = "GALLERY_SYNCTHING_CONFIG";

const TIMEOUT: Duration = Duration::from_secs(3);

fn err(msg: impl Into<String>) -> Error {
    Error::Sync(msg.into())
}

fn block<F: Future>(f: F) -> Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(rt.block_on(f))
}

/// A folder of the local Syncthing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Folder {
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub paused: bool,
}

impl Folder {
    /// The label, or the id of an unlabelled folder.
    pub fn name(&self) -> &str {
        if self.label.is_empty() {
            &self.id
        } else {
            &self.label
        }
    }
}

/// Client of the local Syncthing REST API.
#[derive(Clone)]
pub struct Syncthing {
    base: String,
    key: String,
    tls: bool,
}

impl std::fmt::Debug for Syncthing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Syncthing")
            .field("base", &self.base)
            .field("key", &"<hidden>")
            .finish()
    }
}

/// What `config.xml` says about the GUI: `(tls, address, apikey)`.
fn parse_gui(xml: &str) -> Result<(bool, String, String)> {
    let start = xml
        .find("<gui")
        .ok_or_else(|| err(crate::t!("sync.no_gui")))?;
    let rest = &xml[start..];
    let head_end = rest
        .find('>')
        .ok_or_else(|| err(crate::t!("sync.corrupt")))?;
    let head = &rest[..head_end];
    let body = match rest.find("</gui>") {
        Some(e) if e > head_end => &rest[head_end..e],
        _ => "",
    };
    let tag = |name: &str| -> Option<String> {
        let open = format!("<{name}>");
        let s = body.find(&open)? + open.len();
        let e = body[s..].find(&format!("</{name}>"))?;
        Some(unescape(body[s..s + e].trim()))
    };
    let tls = head.contains("tls=\"true\"");
    let address = tag("address").ok_or_else(|| err(crate::t!("sync.no_address")))?;
    let key = tag("apikey")
        .filter(|k| !k.is_empty())
        .ok_or_else(|| err(crate::t!("sync.no_key")))?;
    Ok((tls, address, key))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// `host:port` of a loopback address; anything else is refused: Gallery
/// never talks to a Syncthing that is not on this machine. `0.0.0.0` and
/// `::` (listen everywhere) are reached through the loopback.
fn loopback_base(address: &str, tls: bool) -> Result<String> {
    let scheme = if tls { "https" } else { "http" };
    let (host, port) = address
        .rsplit_once(':')
        .ok_or_else(|| err(crate::t!("sync.no_port")))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let host = match host {
        "0.0.0.0" | "::" | "" => "127.0.0.1",
        h => h,
    };
    let ok = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback());
    if !ok || port.parse::<u16>().is_err() {
        return Err(err(crate::t!("sync.not_local")));
    }
    let host = if host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_string()
    };
    Ok(format!("{scheme}://{host}:{port}"))
}

/// Where Syncthing keeps `config.xml` by default, most likely first.
fn default_config_paths() -> Vec<PathBuf> {
    let mut v = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if let Some(s) = std::env::var_os("XDG_STATE_HOME").filter(|s| !s.is_empty()) {
        v.push(PathBuf::from(s).join("syncthing"));
    } else if let Some(h) = &home {
        v.push(h.join(".local/state/syncthing"));
    }
    if let Some(c) = dirs::config_dir() {
        v.push(c.join("syncthing"));
    }
    if let Some(h) = &home {
        v.push(h.join(".config/syncthing"));
    }
    v.into_iter().map(|d| d.join("config.xml")).collect()
}

impl Syncthing {
    /// Read the GUI address and key from `config.xml` (read-only).
    pub fn from_config(path: &Path) -> Result<Syncthing> {
        let xml =
            std::fs::read_to_string(path).map_err(|e| err(format!("{}: {e}", path.display())))?;
        let (tls, address, key) = parse_gui(&xml)?;
        Ok(Syncthing {
            base: loopback_base(&address, tls)?,
            key,
            tls,
        })
    }

    /// The local Syncthing: `$GALLERY_SYNCTHING_CONFIG`, else the first
    /// `config.xml` in the default places.
    pub fn discover() -> Result<Syncthing> {
        #[cfg(debug_assertions)]
        if let Some(p) = std::env::var_os(CONFIG_ENV).filter(|p| !p.is_empty()) {
            return Syncthing::from_config(Path::new(&p));
        }
        let found = default_config_paths().into_iter().find(|p| p.is_file());
        match found {
            Some(p) => Syncthing::from_config(&p),
            None => Err(err(crate::t!("sync.config_not_found"))),
        }
    }

    fn client(&self) -> Result<reqwest::Client> {
        reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            // The GUI's own certificate is self-signed; the host is loopback.
            .danger_accept_invalid_certs(self.tls)
            .build()
            .map_err(|_| err(crate::t!("sync.http_client")))
    }

    /// An error that names what failed, never the request (it holds the key).
    fn fail(&self, what: &str, e: &reqwest::Error) -> Error {
        let why = if e.is_timeout() {
            crate::t!("sync.no_response")
        } else if e.is_connect() {
            crate::t!("sync.not_responding")
        } else {
            crate::t!("sync.exchange_error")
        };
        err(format!("({}): {what}: {why}", self.base))
    }

    fn status_error(&self, what: &str, s: reqwest::StatusCode) -> Error {
        err(format!("({}): {what}: HTTP {}", self.base, s.as_u16()))
    }

    /// `/rest/noauth/health`: whether the instance answers.
    pub fn health(&self) -> Result<()> {
        block(async {
            let r = self
                .client()?
                .get(format!("{}/rest/noauth/health", self.base))
                .send()
                .await
                .map_err(|e| self.fail(&crate::t!("sync.what_check"), &e))?;
            if r.status().is_success() {
                Ok(())
            } else {
                Err(self.status_error(&crate::t!("sync.what_check"), r.status()))
            }
        })?
    }

    /// All folders of the instance.
    pub fn folders(&self) -> Result<Vec<Folder>> {
        block(async {
            let r = self
                .client()?
                .get(format!("{}/rest/config/folders", self.base))
                .header("X-API-Key", &self.key)
                .send()
                .await
                .map_err(|e| self.fail(&crate::t!("sync.what_folder_list"), &e))?;
            if !r.status().is_success() {
                return Err(self.status_error(&crate::t!("sync.what_folder_list"), r.status()));
            }
            r.json::<Vec<Folder>>()
                .await
                .map_err(|_| err(crate::t!("sync.folder_list_parse")))
        })?
    }

    /// Pause or resume one folder.
    pub fn set_paused(&self, id: &str, paused: bool) -> Result<()> {
        block(async {
            let url = format!("{}/rest/config/folders/{}", self.base, encode(id));
            let r = self
                .client()?
                .patch(url)
                .header("X-API-Key", &self.key)
                .json(&serde_json::json!({ "paused": paused }))
                .send()
                .await
                .map_err(|e| self.fail(&crate::t!("sync.what_folder"), &e))?;
            if r.status().is_success() {
                Ok(())
            } else {
                Err(self.status_error(&crate::t!("sync.what_folder"), r.status()))
            }
        })?
    }
}

/// Percent-encode a folder id for a URL path segment.
fn encode(id: &str) -> String {
    let mut out = String::new();
    for b in id.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// `p` made absolute: `~` expanded, the longest existing ancestor
/// canonicalized (symlinks resolved), the rest appended as it is.
fn canon(p: &Path) -> PathBuf {
    let p = crate::config::expand_tilde(p);
    let mut rest = Vec::new();
    let mut cur = p.as_path();
    loop {
        if let Ok(c) = cur.canonicalize() {
            return rest.iter().rev().fold(c, |acc, n| acc.join(n));
        }
        match (cur.parent(), cur.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_owned());
                cur = parent;
            }
            _ => return p,
        }
    }
}

/// The directories a plan touches: every source root and every output's
/// folder.
pub fn plan_roots(plan: &Plan) -> Vec<PathBuf> {
    let mut set = BTreeSet::new();
    for it in &plan.items {
        set.insert(it.source_root.clone());
        for o in &it.outputs {
            if let Some(d) = o.dest.parent() {
                set.insert(d.to_path_buf());
            }
        }
    }
    set.into_iter().collect()
}

/// The folders whose path contains one of `roots`, or lies inside one (a
/// source root that holds a synced folder runs files through it too).
pub fn folders_for(folders: &[Folder], roots: &[PathBuf]) -> Vec<Folder> {
    let roots: Vec<PathBuf> = roots.iter().map(|r| canon(r)).collect();
    folders
        .iter()
        .filter(|f| {
            if f.path.is_empty() {
                return false;
            }
            let fp = canon(Path::new(&f.path));
            roots
                .iter()
                .any(|r| r.starts_with(&fp) || fp.starts_with(r))
        })
        .cloned()
        .collect()
}

/// A folder Gallery paused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PausedFolder {
    pub id: String,
    pub label: String,
}

/// The state file: what Gallery paused and has not resumed yet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PauseState {
    pub folders: Vec<PausedFolder>,
    /// The plan whose run paused them (the latest one).
    pub plan: String,
    pub report: String,
    /// RFC 3339.
    pub paused_at: String,
}

/// `<user data dir>/gallery/sync-paused.json`.
pub fn default_state_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("gallery").join("sync-paused.json"))
}

/// The state file; a missing one is `None`. An unreadable one is an error:
/// it is never silently dropped.
pub fn load_state(path: &Path) -> Result<Option<PauseState>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| err(format!("{}: {e}", path.display())))
}

fn save_state(path: &Path, st: &PauseState) -> Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    std::io::Write::write_all(
        &mut tmp,
        serde_json::to_string_pretty(st)
            .map_err(|e| err(e.to_string()))?
            .as_bytes(),
    )?;
    tmp.persist(path).map_err(|e| Error::Io(e.error))?;
    Ok(())
}

/// What the run screen asks before a run: can Syncthing be paused, and
/// which folders would be.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SyncCheck {
    pub reachable: bool,
    /// Why `reachable` is false.
    pub reason: Option<String>,
    /// Labels of the folders the plan touches that are not paused yet.
    pub folders: Vec<String>,
}

/// What a resume did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ResumeOutcome {
    /// Labels of the folders that run again.
    pub resumed: Vec<String>,
    /// Folders that could not be resumed (they stay in the state file).
    pub failed: Vec<String>,
}

/// A Syncthing to pause, and where to remember what was paused.
#[derive(Debug, Clone)]
pub struct SyncPause {
    pub client: Syncthing,
    pub state_path: PathBuf,
}

impl SyncPause {
    /// The local Syncthing with the default state file.
    pub fn discover() -> Result<SyncPause> {
        let state_path = default_state_path().ok_or_else(|| err(crate::t!("sync.no_data_dir")))?;
        Ok(SyncPause {
            client: Syncthing::discover()?,
            state_path,
        })
    }

    /// The not yet paused folders `plan` touches.
    pub fn folders_to_pause(&self, plan: &Plan) -> Result<Vec<Folder>> {
        let all = self.client.folders()?;
        Ok(folders_for(&all, &plan_roots(plan))
            .into_iter()
            .filter(|f| !f.paused)
            .collect())
    }

    /// [`SyncPause::folders_to_pause`] for the run screen.
    pub fn check(&self, plan: &Plan) -> SyncCheck {
        let r = self
            .client
            .health()
            .and_then(|()| self.folders_to_pause(plan));
        match r {
            Ok(f) => SyncCheck {
                reachable: true,
                reason: None,
                folders: f.iter().map(|f| f.name().to_string()).collect(),
            },
            Err(e) => SyncCheck {
                reachable: false,
                reason: Some(e.to_string()),
                folders: vec![],
            },
        }
    }

    /// Pause the folders `plan` touches that run now. The state file (these
    /// folders added to what an earlier run paused) is written first. Returns
    /// the state, `None` when nothing is paused by Gallery.
    pub fn pause_for(&self, plan_path: &Path, plan: &Plan) -> Result<Option<PauseState>> {
        let todo = self.folders_to_pause(plan)?;
        let old = load_state(&self.state_path)?;
        if todo.is_empty() {
            return Ok(old);
        }
        let mut folders = old.map(|s| s.folders).unwrap_or_default();
        for f in &todo {
            if !folders.iter().any(|p| p.id == f.id) {
                folders.push(PausedFolder {
                    id: f.id.clone(),
                    label: f.name().to_string(),
                });
            }
        }
        let st = PauseState {
            folders,
            plan: plan_path.display().to_string(),
            report: crate::apply::report_path(plan_path).display().to_string(),
            paused_at: chrono::Local::now().to_rfc3339(),
        };
        save_state(&self.state_path, &st)?;
        let mut failed = Vec::new();
        for f in &todo {
            if self.client.set_paused(&f.id, true).is_err() {
                failed.push(f.name().to_string());
            }
        }
        if !failed.is_empty() {
            return Err(err(crate::t!(
                "sync.could_not_pause",
                folders = failed.join(", ")
            )));
        }
        Ok(Some(st))
    }

    /// Resume what Gallery paused. A folder that is gone counts as resumed;
    /// one that fails stays in the state file for the next try.
    pub fn resume(&self) -> Result<ResumeOutcome> {
        let mut out = ResumeOutcome::default();
        let Some(mut st) = load_state(&self.state_path)? else {
            return Ok(out);
        };
        let mut left = Vec::new();
        for f in std::mem::take(&mut st.folders) {
            match self.client.set_paused(&f.id, false) {
                Ok(()) => out.resumed.push(f.label),
                Err(e) if e.to_string().ends_with("HTTP 404") => out.resumed.push(f.label),
                Err(_) => {
                    out.failed.push(f.label.clone());
                    left.push(f);
                }
            }
        }
        if left.is_empty() {
            match std::fs::remove_file(&self.state_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        } else {
            st.folders = left;
            save_state(&self.state_path, &st)?;
        }
        Ok(out)
    }
}
