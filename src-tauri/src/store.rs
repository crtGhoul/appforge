//! JSON persistence for the web-app library.
//!
//! Apps are stored as a JSON array in the Tauri app-data directory
//! (`apps.json`). If the file is corrupt it is backed up next to itself and
//! replaced with a fresh empty library instead of crashing the app.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebApp {
    pub id: String,
    pub name: String,
    pub url: String,
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn new_id() -> String {
    let n = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("app-{}-{}", unix_millis(), n)
}

/// Backend URL check. The frontend normalizes first; this is a second gate so
/// a bad value can never be persisted even if invoked directly.
fn is_valid_url(url: &str) -> bool {
    let url = url.trim();
    let rest = if let Some(r) = url.strip_prefix("https://") {
        r
    } else if let Some(r) = url.strip_prefix("http://") {
        r
    } else {
        return false;
    };
    // Take the authority part (up to /, ?, or #), strip userinfo and port.
    let authority = rest.split(&['/', '?', '#'][..]).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    // Require a real host: something with a dot, or localhost.
    !host.is_empty() && (host.contains('.') || host == "localhost")
}

pub struct AppStore {
    path: PathBuf,
    apps: Mutex<Vec<WebApp>>,
}

impl AppStore {
    pub fn load(app: &AppHandle) -> Result<Self, String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("could not resolve app data dir: {e}"))?;
        fs::create_dir_all(&dir)
            .map_err(|e| format!("could not create app data dir: {e}"))?;
        let path = dir.join("apps.json");

        let apps = match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Vec<WebApp>>(&contents) {
                Ok(apps) => apps,
                Err(_) => {
                    // Corrupt file: back it up beside itself, start fresh.
                    let backup =
                        dir.join(format!("apps.json.corrupt-{}.bak", unix_millis()));
                    let _ = fs::rename(&path, &backup);
                    Vec::new()
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("could not read apps.json: {e}")),
        };

        Ok(Self {
            path,
            apps: Mutex::new(apps),
        })
    }

    fn save(&self) -> Result<(), String> {
        let apps = self
            .apps
            .lock()
            .map_err(|e| format!("store lock poisoned: {e}"))?;
        let json = serde_json::to_string_pretty(&*apps).map_err(|e| e.to_string())?;
        // Write temp file first, then rename, so a crash can't leave a
        // half-written apps.json behind.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, json).map_err(|e| format!("could not write apps.json: {e}"))?;
        fs::rename(&tmp, &self.path).map_err(|e| format!("could not write apps.json: {e}"))?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<WebApp>, String> {
        self.apps
            .lock()
            .map(|apps| apps.clone())
            .map_err(|e| format!("store lock poisoned: {e}"))
    }

    pub fn add(&self, name: String, url: String) -> Result<WebApp, String> {
        let name = name.trim().to_string();
        let url = url.trim().to_string();
        if name.is_empty() {
            return Err("Name is required.".to_string());
        }
        if !is_valid_url(&url) {
            return Err("URL must start with http:// or https://.".to_string());
        }
        let app = WebApp {
            id: new_id(),
            name,
            url,
        };
        {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            apps.push(app.clone());
        }
        self.save()?;
        Ok(app)
    }

    pub fn remove(&self, id: &str) -> Result<(), String> {
        {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let before = apps.len();
            apps.retain(|a| a.id != id);
            if apps.len() == before {
                return Err("App not found.".to_string());
            }
        }
        self.save()
    }
}
