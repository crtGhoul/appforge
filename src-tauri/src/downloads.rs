//! In-app download manager for account windows.
//!
//! Clicking a download link inside an account window used to kick the user
//! out to their system browser. This module wires the webview's native
//! download flow (`WebviewWindowBuilder::on_download`) so the download
//! happens inside the account window's session — cookies and login stay
//! intact — and lands in the user's download folder.
//!
//! What it is: a download list with progress, open, show-in-folder, remove,
//! clear-finished, and a configurable destination folder. What it is NOT: a
//! file manager — there is no filesystem browsing, no moving, no deleting
//! files, and no cancel (listed as future work).
//!
//! Steady-state RAM is negligible: downloads live in a bounded in-memory
//! Vec (cap 100); a 500 ms poller thread only exists while a download is
//! active and exits on completion.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tauri::webview::{DownloadEvent, Webview};
use tauri::{AppHandle, Emitter, Manager, State};

/// Cap on in-memory download history; newest first.
const MAX_DOWNLOADS: usize = 100;

/// How often the poller thread samples the destination file size.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Timeout for the single best-effort HEAD request that learns the total size.
const HEAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Payload for `appmaka:download-progress` events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadEntry {
    pub id: String,
    pub filename: String,
    pub url: String,
    pub state: String, // "active" | "complete" | "failed"
    pub received_bytes: u64,
    pub total_bytes: Option<u64>,
    pub path: String,
}

/// Persisted settings, stored as `<app-data>/downloads.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DownloadSettings {
    #[serde(default = "default_download_dir")]
    download_dir: PathBuf,
}

impl Default for DownloadSettings {
    fn default() -> Self {
        Self {
            download_dir: default_download_dir(),
        }
    }
}

struct DownloadStore {
    downloads: Vec<DownloadEntry>,
    settings: DownloadSettings,
}

/// Managed state: `app.manage(DownloadState::load(app)?)`.
pub struct DownloadState {
    store: Mutex<DownloadStore>,
    settings_file: PathBuf,
}

fn default_download_dir() -> PathBuf {
    #[cfg(windows)]
    {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join("Downloads");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join("Downloads");
    }
    std::env::temp_dir()
}

impl DownloadState {
    /// Load settings from `<app-data>/downloads.json`, creating the download
    /// folder if it does not exist. Malformed or missing settings fall back
    /// to the default download folder (serde defaults).
    pub fn load(app: &AppHandle) -> Result<Self, String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("could not resolve app data dir: {e}"))?;
        fs::create_dir_all(&dir).map_err(|e| format!("could not create app data dir: {e}"))?;
        let settings_file = dir.join("downloads.json");

        let settings = match fs::read_to_string(&settings_file) {
            Ok(contents) => serde_json::from_str::<DownloadSettings>(&contents)
                .unwrap_or_default(),
            Err(_) => DownloadSettings::default(),
        };

        fs::create_dir_all(&settings.download_dir).map_err(|e| {
            format!(
                "could not create download folder {}: {e}",
                settings.download_dir.display()
            )
        })?;

        Ok(Self {
            store: Mutex::new(DownloadStore {
                downloads: Vec::new(),
                settings,
            }),
            settings_file,
        })
    }

    fn with_store<R>(
        &self,
        f: impl FnOnce(&mut DownloadStore) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut store = self
            .store
            .lock()
            .map_err(|e| format!("download state lock poisoned: {e}"))?;
        f(&mut store)
    }
}

// ---------------------------------------------------------------------------
// Download handler
// ---------------------------------------------------------------------------

static DOWNLOAD_COUNTER: AtomicU64 = AtomicU64::new(0);

fn next_download_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let n = DOWNLOAD_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("dl-{millis}-{n}")
}

/// Strip anything that could escape the download folder: keep only the file
/// name component, drop separators, reject ".." and empty names.
fn sanitize_filename(raw: &str) -> String {
    let base = Path::new(raw)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    // file_name() already strips directory separators and rejects "..";
    // filter again as belt-and-suspenders against odd input.
    let cleaned: String = base
        .chars()
        .filter(|c| !matches!(c, '/' | '\\' | '\0'))
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() || cleaned == "." {
        "download".to_string()
    } else {
        cleaned.to_string()
    }
}

/// Pick a destination under `dir` that does not clobber an existing file:
/// `name (1).ext`, `name (2).ext`, ...
fn unique_destination(dir: &Path, filename: &str) -> PathBuf {
    let first = dir.join(filename);
    if !first.exists() {
        return first;
    }
    let stem = Path::new(filename)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| filename.to_string());
    let ext = Path::new(filename)
        .extension()
        .map(|s| format!(".{}", s.to_string_lossy()))
        .unwrap_or_default();
    let mut n: u32 = 1;
    loop {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
        n = n.saturating_add(1);
    }
}

/// One best-effort HEAD request to learn the total size for the progress
/// display. Never downloads the body; failure just leaves `total_bytes`
/// unknown ("12 MB so far" instead of "12 MB of 48 MB").
fn head_content_length(url: &str) -> Option<u64> {
    let resp = ureq::head(url).timeout(HEAD_TIMEOUT).call().ok()?;
    resp.header("content-length")?.parse::<u64>().ok()
}

fn emit_progress(app: &AppHandle, entry: &DownloadEntry) {
    let _ = app.emit("appmaka:download-progress", entry);
}

/// Poll the destination file size while the download is active. Runs on its
/// own thread and exits as soon as the download leaves "active" — no
/// persistent threads.
fn spawn_progress_poller(app: AppHandle, id: String, url: String, dest: PathBuf) {
    let thread_name = format!("appmaka-dl-poller-{id}");
    let _ = std::thread::Builder::new().name(thread_name).spawn(move || {
        // One best-effort HEAD up front; after this we only read file sizes.
        if let Some(total) = head_content_length(&url) {
            if let Some(state) = app.try_state::<DownloadState>() {
                let _ = state.with_store(|store| {
                    if let Some(entry) = store.downloads.iter_mut().find(|e| e.id == id) {
                        entry.total_bytes = Some(total);
                    }
                    Ok(())
                });
            }
        }
        loop {
            std::thread::sleep(POLL_INTERVAL);
            let received = fs::metadata(&dest).map(|m| m.len()).unwrap_or(0);
            let finished = match app.try_state::<DownloadState>() {
                Some(state) => {
                    let result = state.with_store(|store| {
                        if let Some(entry) = store.downloads.iter_mut().find(|e| e.id == id) {
                            entry.received_bytes = entry.received_bytes.max(received);
                            let active = entry.state == "active";
                            let entry = entry.clone();
                            Ok((entry, active))
                        } else {
                            Err("gone".to_string())
                        }
                    });
                    match result {
                        Ok((entry, active)) => {
                            if !active {
                                // The Finished handler already emitted the
                                // final state; no need to repeat it.
                                true
                            } else {
                                emit_progress(&app, &entry);
                                false
                            }
                        }
                        Err(_) => true,
                    }
                }
                None => true,
            };
            if finished {
                break;
            }
        }
    });
}

/// Build the `on_download` closure for an account window's
/// `WebviewWindowBuilder` chain. The webview itself performs the download
/// (WebView2's DownloadStarting on Windows, WebKitGTK's download-started on
/// Linux), so the page's session and cookies are preserved.
pub fn make_download_handler(
    app: AppHandle,
) -> impl Fn(Webview<tauri::Wry>, DownloadEvent<'_>) -> bool + Send + Sync + 'static {
    move |_webview, event| match event {
        DownloadEvent::Requested { url, destination } => {
            let Some(state) = app.try_state::<DownloadState>() else {
                // DownloadState not managed yet; block rather than leak the
                // file to the webview's default location.
                return false;
            };
            let prepared = state.with_store(|store| {
                let dir = &store.settings.download_dir;
                if let Err(e) = fs::create_dir_all(dir) {
                    return Err(format!("could not create download folder: {e}"));
                }
                let suggested = destination
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "download".to_string());
                let filename = sanitize_filename(&suggested);
                let dest = unique_destination(dir, &filename);
                let entry = DownloadEntry {
                    id: next_download_id(),
                    filename: dest
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| filename.clone()),
                    url: url.as_str().to_string(),
                    state: "active".to_string(),
                    received_bytes: 0,
                    total_bytes: None,
                    path: dest.to_string_lossy().into_owned(),
                };
                // Newest first, bounded history.
                store.downloads.insert(0, entry.clone());
                store.downloads.truncate(MAX_DOWNLOADS);
                *destination = dest.clone();
                Ok(entry)
            });
            match prepared {
                Ok(entry) => {
                    emit_progress(&app, &entry);
                    let dest = PathBuf::from(&entry.path);
                    spawn_progress_poller(app.clone(), entry.id.clone(), entry.url.clone(), dest);
                    true
                }
                Err(_) => false,
            }
        }
        DownloadEvent::Finished { url, path, success } => {
            let Some(state) = app.try_state::<DownloadState>() else {
                return true;
            };
            let url_str = url.as_str();
            let result = state.with_store(|store| {
                // Newest first: match the most recent active entry for this URL.
                let entry = store
                    .downloads
                    .iter_mut()
                    .find(|e| e.state == "active" && e.url == url_str);
                match entry {
                    Some(entry) => {
                        entry.state = if success { "complete" } else { "failed" }.to_string();
                        // Prefer the final path reported by the platform.
                        if let Some(p) = path {
                            entry.path = p.to_string_lossy().into_owned();
                        }
                        // Refresh the final size so a finished entry reports
                        // the full byte count even if the last poll missed it.
                        if success {
                            if let Ok(meta) = fs::metadata(Path::new(&entry.path)) {
                                entry.received_bytes = meta.len();
                            }
                        }
                        Ok(entry.clone())
                    }
                    None => Err("no active entry for this download".to_string()),
                }
            });
            if let Ok(entry) = result {
                // Windows only: stamp the Mark of the Web. Browsers tag every
                // download with a Zone.Identifier stream carrying the source
                // URL so SmartScreen and Explorer's "this file came from the
                // internet" warnings keep working. Saving without it would be
                // a security regression versus every browser.
                #[cfg(windows)]
                if entry.state == "complete" {
                    write_zone_identifier(&entry.path, &entry.url);
                }
                emit_progress(&app, &entry);
            }
            true
        }
        // DownloadEvent is non-exhaustive; future variants default to the
        // webview's built-in behavior (allow).
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Mark of the Web (Windows only)
// ---------------------------------------------------------------------------

/// Write the `Zone.Identifier` alternate data stream next to a finished
/// download, exactly like Chrome/Edge/Firefox do: ZoneId=3 (internet zone)
/// with the source URL as HostUrl. SmartScreen and Explorer depend on it.
///
/// Best-effort by design: a failed ADS write must never fail or undo the
/// download itself. On Linux there is no equivalent marking, so this is a
/// no-op there.
#[cfg(windows)]
fn write_zone_identifier(path: &str, url: &str) {
    let ads_path = format!("{path}:Zone.Identifier");
    let content = format!("[ZoneTransfer]\r\nZoneId=3\r\nHostUrl={url}\r\n");
    let _ = std::fs::write(ads_path, content);
}

// ---------------------------------------------------------------------------
// Path validation
// ---------------------------------------------------------------------------

/// Resolve the stored download dir and return its canonical form for
/// starts_with checks.
fn canonical_download_dir(store: &DownloadStore) -> Result<PathBuf, String> {
    store
        .settings
        .download_dir
        .canonicalize()
        .map_err(|_| "The download folder is missing. Pick a new one below.".to_string())
}

/// Look up an entry and return its path, validated to sit inside the current
/// download folder. Fails closed: anything outside (or not resolvable) is
/// rejected. Never deletes anything — validation is read-only.
fn validated_entry_path(store: &DownloadStore, id: &str) -> Result<PathBuf, String> {
    let entry = store
        .downloads
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| "That download is not in the list.".to_string())?;
    let dir = canonical_download_dir(store)?;
    let path = PathBuf::from(&entry.path)
        .canonicalize()
        .map_err(|_| "That file is no longer on disk.".to_string())?;
    if !path.starts_with(&dir) {
        return Err("That file is outside the download folder.".to_string());
    }
    Ok(path)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// JS: `invoke("list_downloads")`
#[tauri::command]
pub fn list_downloads(state: State<DownloadState>) -> Vec<DownloadEntry> {
    state
        .with_store(|store| Ok(store.downloads.clone()))
        .unwrap_or_default()
}

/// Open the file with the OS default app. Read-only validation: the path
/// must resolve inside the download folder.
///
/// JS: `invoke("open_download", { id })`
#[tauri::command]
pub fn open_download(app: AppHandle, state: State<DownloadState>, id: String) -> Result<(), String> {
    let path = state.with_store(|store| validated_entry_path(store, &id))?;
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(path.to_string_lossy().into_owned(), None::<&str>)
        .map_err(|e| format!("Could not open the file: {e}"))
}

/// Reveal the file in the system file manager. Read-only validation.
///
/// JS: `invoke("show_in_folder", { id })`
#[tauri::command]
pub fn show_in_folder(app: AppHandle, state: State<DownloadState>, id: String) -> Result<(), String> {
    let path = state.with_store(|store| validated_entry_path(store, &id))?;
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(path)
        .map_err(|e| format!("Could not show the file: {e}"))
}

/// Remove the list entry only. The file on disk is never touched.
///
/// JS: `invoke("remove_download", { id })`
#[tauri::command]
pub fn remove_download(state: State<DownloadState>, id: String) -> Result<(), String> {
    state.with_store(|store| {
        let before = store.downloads.len();
        store.downloads.retain(|e| e.id != id);
        if store.downloads.len() == before {
            return Err("That download is not in the list.".to_string());
        }
        Ok(())
    })
}

/// Drop every entry that is not actively downloading.
///
/// JS: `invoke("clear_finished")`
#[tauri::command]
pub fn clear_finished(state: State<DownloadState>) -> Result<(), String> {
    state.with_store(|store| {
        store.downloads.retain(|e| e.state == "active");
        Ok(())
    })
}

/// JS: `invoke("get_download_dir")`
#[tauri::command]
pub fn get_download_dir(state: State<DownloadState>) -> Result<String, String> {
    state.with_store(|store| {
        Ok(store
            .settings
            .download_dir
            .to_string_lossy()
            .into_owned())
    })
}

/// Change the download folder. The path must exist and be a directory; the
/// new value is persisted to `<app-data>/downloads.json`.
///
/// JS: `invoke("set_download_dir", { path })`
#[tauri::command]
pub fn set_download_dir(
    state: State<DownloadState>,
    path: String,
) -> Result<(), String> {
    let path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        return Err("Pick a folder first.".to_string());
    }
    if !path.exists() {
        return Err("That folder does not exist.".to_string());
    }
    if !path.is_dir() {
        return Err("That is not a folder.".to_string());
    }
    let settings = state.with_store(|store| {
        store.settings.download_dir = path.clone();
        Ok(store.settings.clone())
    })?;
    // Persist outside the mutex so a disk failure does not leave the
    // in-memory value out of sync: re-read under a fresh lock on failure.
    if let Err(e) = persist_settings(&state.settings_file, &settings) {
        let _ = state.with_store(|store| {
            if let Ok(old) = fs::read_to_string(&state.settings_file) {
                if let Ok(parsed) = serde_json::from_str::<DownloadSettings>(&old) {
                    store.settings = parsed;
                }
            }
            Ok(())
        });
        return Err(format!("Could not save the download folder: {e}"));
    }
    Ok(())
}

fn persist_settings(file: &Path, settings: &DownloadSettings) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(settings).map_err(|e| format!("serialize error: {e}"))?;
    fs::write(file, json).map_err(|e| format!("write error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_drops_separators_and_dotdot() {
        assert_eq!(sanitize_filename("/etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("../../evil.exe"), "evil.exe");
        assert_eq!(sanitize_filename(".."), "download");
        assert_eq!(sanitize_filename(""), "download");
        assert_eq!(sanitize_filename("report (1).pdf"), "report (1).pdf");
        assert_eq!(sanitize_filename("."), "download");
    }

    #[test]
    fn dedup_avoids_clobbering() {
        let dir = std::env::temp_dir().join("appmaka-dl-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("a.txt"), b"x").unwrap();
        assert_eq!(unique_destination(&dir, "a.txt"), dir.join("a (1).txt"));
        fs::write(dir.join("a (1).txt"), b"x").unwrap();
        assert_eq!(unique_destination(&dir, "a.txt"), dir.join("a (2).txt"));
        assert_eq!(unique_destination(&dir, "fresh.bin"), dir.join("fresh.bin"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn settings_defaults_to_a_real_folder() {
        let s = DownloadSettings::default();
        assert!(s.download_dir.is_absolute());
    }
}
