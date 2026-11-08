//! Thumbnails for the plan grid: made by the core into its cache directory
//! and handed to the webview as asset-protocol URLs. The asset protocol is
//! scoped to that directory only.

use std::path::Path;
use std::sync::{Condvar, Mutex};

/// URL of a local file under the Tauri asset protocol (what the JS
/// `convertFileSrc` returns).
pub fn asset_url(path: &Path) -> String {
    let enc = encode_uri_component(&path.to_string_lossy());
    if cfg!(any(windows, target_os = "android")) {
        format!("http://asset.localhost/{enc}")
    } else {
        format!("asset://localhost/{enc}")
    }
}

/// JavaScript `encodeURIComponent`: everything but `A-Z a-z 0-9 - _ . ! ~ * ' ( )`
/// is percent-encoded as UTF-8.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// A counting gate: at most `n` thumbnails are made at once, so scrolling
/// through thousands of files does not start thousands of ffmpeg runs.
pub struct Gate {
    free: Mutex<usize>,
    cv: Condvar,
}

impl Gate {
    pub fn new(n: usize) -> Self {
        Gate {
            free: Mutex::new(n.max(1)),
            cv: Condvar::new(),
        }
    }

    pub fn run<T>(&self, f: impl FnOnce() -> T) -> T {
        {
            let mut free = self.free.lock().expect("gate lock");
            while *free == 0 {
                free = self.cv.wait(free).expect("gate lock");
            }
            *free -= 1;
        }
        struct Release<'a>(&'a Gate);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                *self.0.free.lock().expect("gate lock") += 1;
                self.0.cv.notify_one();
            }
        }
        let _release = Release(self);
        f()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn asset_url_encodes_like_convert_file_src() {
        assert_eq!(
            asset_url(Path::new("/home/u/.cache/gallery/thumbs/ab12.jpg")),
            if cfg!(windows) {
                "http://asset.localhost/%2Fhome%2Fu%2F.cache%2Fgallery%2Fthumbs%2Fab12.jpg"
            } else {
                "asset://localhost/%2Fhome%2Fu%2F.cache%2Fgallery%2Fthumbs%2Fab12.jpg"
            }
        );
        assert_eq!(
            encode_uri_component("Фото 1 (2)&#.jpg"),
            "%D0%A4%D0%BE%D1%82%D0%BE%201%20(2)%26%23.jpg"
        );
    }

    #[test]
    fn gate_bounds_concurrency() {
        let gate = Arc::new(Gate::new(2));
        let now = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let hs: Vec<_> = (0..8)
            .map(|_| {
                let (gate, now, peak) = (gate.clone(), now.clone(), peak.clone());
                std::thread::spawn(move || {
                    gate.run(|| {
                        let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(n, Ordering::SeqCst);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        now.fetch_sub(1, Ordering::SeqCst);
                    })
                })
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert!(peak.load(Ordering::SeqCst) <= 2);
        assert_eq!(gate.run(|| 7), 7);
    }
}
