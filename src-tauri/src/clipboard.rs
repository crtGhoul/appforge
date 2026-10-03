//! Clipboard history v1 (text only).
//!
//! A poll-based clipboard watcher records text copies into a local,
//! newest-first history stored in `<app-data>/clipboard.json`. Nothing is
//! ever transmitted anywhere — there is no network code in this module.
//!
//! Design notes:
//! - The watcher is a single tokio task ticking every 600ms. Each tick is
//!   one OS clipboard read plus a string compare — ~nothing when the
//!   clipboard hasn't changed. No new processes, no background services.
//! - v1 is TEXT ONLY. Empty/whitespace-only copies are ignored, and texts
//!   over 1 MiB are skipped (pasting megabytes through a history list is
//!   never what the user wants).
//! - Selecting an entry copies it back to the OS clipboard and closes the
//!   popup. v1 does NOT synthesize Ctrl+V into other apps — the user
//!   pastes normally after picking.
//! - The popup window is built on a dedicated thread, never inside a
//!   synchronous command and never on the main thread (same Windows
//!   WebView2 deadlock rule as every other window in this app).

use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::hotkeys::{record_binding_failure, set_binding, BindingStatus, HotkeyKind};

/// Registry binding id for the clipboard popup hotkey.
pub const BINDING_ID: &str = "clipboard:popup";
/// Window label for the clipboard popup.
pub const WINDOW_LABEL: &str = "clipboard";
/// Default global hotkey for the popup. Chosen to avoid PowerToys Run's
/// Alt+Space and Windows' own Win+V.
pub const DEFAULT_HOTKEY: &str = "Ctrl+Shift+V";
/// Default history cap (entries).
pub const DEFAULT_CAP: usize = 100;
/// Minimum/maximum configurable cap.
pub const MIN_CAP: usize = 10;
pub const MAX_CAP: usize = 1000;
/// Texts larger than this are never recorded.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// How much of an entry the list shows; the full text stays on disk and is
/// what gets copied back.
pub const PREVIEW_CHARS: usize = 500;
/// Watcher poll interval.
const POLL_INTERVAL: Duration = Duration::from_millis(600);

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

static ENTRY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One recorded copy. `text` is the full text; the list command truncates
/// to a preview.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardEntry {
    pub id: String,
    pub text: String,
    pub created_at_ms: u64,
}

/// What `list_clipboard` returns: newest first, text truncated to a
/// preview. Copying uses the id, so the full text never has to travel to
/// the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardListEntry {
    pub id: String,
    pub preview: String,
    pub chars: usize,
    pub truncated: bool,
    pub created_at_ms: u64,
}

/// Settings payload for the UI.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClipboardSettings {
    pub cap: usize,
    pub hotkey: String,
}

/// On-disk shape of clipboard.json. Unknown fields are ignored on load so
/// future versions can extend it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClipboardFile {
    #[serde(default)]
    entries: Vec<ClipboardEntry>,
    #[serde(default = "default_cap")]
    cap: usize,
    #[serde(default = "default_hotkey")]
    hotkey: String,
}

fn default_cap() -> usize {
    DEFAULT_CAP
}

fn default_hotkey() -> String {
    DEFAULT_HOTKEY.to_string()
}

impl Default for ClipboardFile {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
            cap: DEFAULT_CAP,
            hotkey: DEFAULT_HOTKEY.to_string(),
        }
    }
}

/// In-memory state. `last_seen` is the last clipboard text observed (or
/// written by us) — never persisted; it only suppresses re-recording.
struct ClipboardData {
    entries: VecDeque<ClipboardEntry>,
    cap: usize,
    hotkey: String,
    last_seen: Option<String>,
}

pub struct ClipboardState {
    inner: Mutex<ClipboardData>,
}

fn clipboard_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve app data dir: {e}"))?
        .join("clipboard.json"))
}

/// Load persisted history, or start empty. A missing/corrupt file is just
/// "no history yet" — the watcher keeps working.
pub fn load(app: &AppHandle) -> ClipboardState {
    let file: ClipboardFile = clipboard_path(app)
        .ok()
        .and_then(|p| fs::read_to_string(p).ok())
        .and_then(|c| serde_json::from_str(&c).ok())
        .unwrap_or_default();
    let cap = file.cap.clamp(MIN_CAP, MAX_CAP);
    let mut entries: VecDeque<ClipboardEntry> = file.entries.into();
    entries.truncate(cap);
    ClipboardState {
        inner: Mutex::new(ClipboardData {
            entries,
            cap,
            hotkey: file.hotkey,
            last_seen: None,
        }),
    }
}

fn persist(app: &AppHandle, data: &ClipboardData) -> Result<(), String> {
    let path = clipboard_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("could not create app data dir: {e}"))?;
    }
    let file = ClipboardFile {
        entries: data.entries.iter().cloned().collect(),
        cap: data.cap,
        hotkey: data.hotkey.clone(),
    };
    let json =
        serde_json::to_string_pretty(&file).map_err(|e| format!("could not encode: {e}"))?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, json).map_err(|e| format!("could not write: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("could not save: {e}"))?;
    Ok(())
}

fn with_state<T>(app: &AppHandle, f: impl FnOnce(&mut ClipboardData) -> T) -> Result<T, String> {
    let state = app
        .try_state::<ClipboardState>()
        .ok_or_else(|| "clipboard state not initialized".to_string())?;
    let mut data = state
        .inner
        .lock()
        .map_err(|e| format!("clipboard state poisoned: {e}"))?;
    Ok(f(&mut data))
}

// ---------------------------------------------------------------------------
// Pure recording rules (unit-tested, no AppHandle needed).
// ---------------------------------------------------------------------------

/// Should this clipboard text become a history entry? Skips blanks,
/// oversized texts, and anything identical to what we last saw.
fn should_record(text: &str, last_seen: Option<&str>) -> bool {
    if text.trim().is_empty() {
        return false;
    }
    if text.len() > MAX_TEXT_BYTES {
        return false;
    }
    match last_seen {
        Some(last) => text != last,
        None => true,
    }
}

/// Push a new entry to the front, enforcing the cap. Pure for testing.
fn insert_entry(entries: &mut VecDeque<ClipboardEntry>, entry: ClipboardEntry, cap: usize) {
    entries.push_front(entry);
    entries.truncate(cap.max(1));
}

fn make_entry(text: String) -> ClipboardEntry {
    let n = ENTRY_COUNTER.fetch_add(1, Ordering::Relaxed);
    ClipboardEntry {
        id: format!("clip-{}-{n}", unix_millis()),
        text,
        created_at_ms: unix_millis(),
    }
}

/// Record one clipboard observation. Returns true when a new entry was
/// stored. Extracted so the watcher loop stays thin.
fn observe(app: &AppHandle, text: String) -> bool {
    let recorded = with_state(app, |data| {
        if !should_record(&text, data.last_seen.as_deref()) {
            // Still remember it: an unchanged clipboard must not be
            // re-examined as "new" later.
            data.last_seen = Some(text);
            return false;
        }
        data.last_seen = Some(text.clone());
        insert_entry(&mut data.entries, make_entry(text), data.cap);
        true
    });
    match recorded {
        Ok(true) => {
            let save_result = with_state(app, |data| persist(app, data));
            if let Err(e) = save_result.flatten() {
                eprintln!("clipboard: persist failed: {e}");
            }
            // The popup polls while open (push events proved unreliable for
            // secondary windows), so no emit is needed here.
            true
        }
        Ok(false) => false,
        Err(e) => {
            eprintln!("clipboard: observe failed: {e}");
            false
        }
    }
}

/// Background watcher: one OS clipboard read per tick plus a string
/// compare. Started once from main.rs setup.
pub fn start_watcher(app: AppHandle) {
    // Seed last_seen from whatever is already on the clipboard so
    // pre-existing content isn't recorded as new history.
    if let Ok(current) = app.clipboard().read_text() {
        let _ = with_state(&app, |data| {
            data.last_seen = Some(current);
        });
    }
    tauri::async_runtime::spawn(async move {
        let mut interval = tokio::time::interval(POLL_INTERVAL);
        loop {
            interval.tick().await;
            match app.clipboard().read_text() {
                Ok(text) => {
                    observe(&app, text);
                }
                Err(_) => {
                    // Clipboard unavailable right now (locked by another
                    // app, no text format, X11 owner gone). Next tick.
                }
            }
        }
    });
}

/// Register the saved popup hotkey at startup. Best-effort: a failure is
/// recorded in the binding status (the UI warns) and logged — startup
/// never crashes on a hotkey.
pub fn register_saved_hotkey(app: &AppHandle) {
    let hotkey = with_state(app, |data| data.hotkey.clone()).unwrap_or_default();
    let hotkey = hotkey.trim().to_string();
    if hotkey.is_empty() {
        return;
    }
    if let Err(e) = set_binding(app, BINDING_ID, HotkeyKind::Clipboard, &hotkey) {
        eprintln!("clipboard hotkey: {e}");
        record_binding_failure(app, BINDING_ID, hotkey, e);
    }
}

// ---------------------------------------------------------------------------
// Popup window.
// ---------------------------------------------------------------------------

/// Clamp a cursor-anchored popup origin inside a monitor rectangle.
/// Panic-free even when the window is bigger than the monitor.
/// Pure math — unit-tested.
fn clamp_popup_origin(
    cursor: (i32, i32),
    monitor: (i32, i32, i32, i32),
    window: (i32, i32),
) -> (i32, i32) {
    let (cursor_x, cursor_y) = cursor;
    let (mon_x, mon_y, mon_w, mon_h) = monitor;
    let (win_w, win_h) = window;
    let max_x = (mon_x + mon_w - win_w).max(mon_x);
    let max_y = (mon_y + mon_h - win_h).max(mon_y);
    let x = (cursor_x + 12).clamp(mon_x, max_x);
    let y = (cursor_y + 12).clamp(mon_y, max_y);
    (x, y)
}

fn position_clipboard_window(app: &AppHandle, w: &tauri::WebviewWindow) {
    use tauri::{PhysicalPosition, Position};
    const WIN_W: i32 = 440;
    const WIN_H: i32 = 520;
    if let Some(monitor) = crate::windows::cursor_monitor(app) {
        let mp = monitor.position();
        let ms = monitor.size();
        let (cx, cy) = app
            .cursor_position()
            .ok()
            .map(|p| (p.x as i32, p.y as i32))
            .unwrap_or((mp.x, mp.y));
        let (x, y) = clamp_popup_origin(
            (cx, cy),
            (mp.x, mp.y, ms.width as i32, ms.height as i32),
            (WIN_W, WIN_H),
        );
        if w
            .set_position(Position::Physical(PhysicalPosition::new(x, y)))
            .is_ok()
        {
            return;
        }
    }
    let _ = w.center();
}

fn build_clipboard_window(app: &AppHandle) -> Result<(), String> {
    if app.get_webview_window(WINDOW_LABEL).is_some() {
        return Ok(());
    }
    let win = WebviewWindowBuilder::new(app, WINDOW_LABEL, WebviewUrl::App("clipboard.html".into()))
        .title("Clipboard history")
        .inner_size(440.0, 520.0)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .focused(true)
        .build()
        .map_err(|e| format!("could not open clipboard history: {e}"))?;
    position_clipboard_window(app, &win);
    let _ = win.show();
    let _ = win.set_focus();
    Ok(())
}

/// Toggle the clipboard popup. Called from the global-shortcut handler in
/// main.rs (NOT a Tauri command): window creation happens on a dedicated
/// thread, never on a sync command thread or the main thread.
pub fn toggle_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        if w.is_visible().unwrap_or(false) {
            let _ = w.hide();
        } else {
            position_clipboard_window(app, &w);
            let _ = w.show();
            let _ = w.set_focus();
        }
        return;
    }
    let handle = app.clone();
    std::thread::Builder::new()
        .name("appmaka-clipboard-window".to_string())
        .spawn(move || {
            if let Err(e) = build_clipboard_window(&handle) {
                eprintln!("clipboard window: {e}");
            }
        })
        .ok();
}

// ---------------------------------------------------------------------------
// Tauri commands. JS: invoke("list_clipboard") etc. (camelCase args).
// ---------------------------------------------------------------------------

/// Hide the popup. The frontend calls this instead of window.hide() from JS:
/// the Rust-side hide is the path proven to work for this window.
/// JS: `invoke("hide_clipboard_popup")`.
#[tauri::command]
pub fn hide_clipboard_popup(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(WINDOW_LABEL) {
        w.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Newest-first history, texts truncated to previews.
/// JS: `invoke("list_clipboard")`.
#[tauri::command]
pub fn list_clipboard(app: AppHandle) -> Result<Vec<ClipboardListEntry>, String> {
    with_state(&app, |data| {
        data.entries
            .iter()
            .map(|e| {
                let chars = e.text.chars().count();
                let truncated = chars > PREVIEW_CHARS;
                let preview: String = if truncated {
                    e.text.chars().take(PREVIEW_CHARS).collect()
                } else {
                    e.text.clone()
                };
                ClipboardListEntry {
                    id: e.id.clone(),
                    preview,
                    chars,
                    truncated,
                    created_at_ms: e.created_at_ms,
                }
            })
            .collect()
    })
}

/// Copy an entry back to the OS clipboard (v1 contract: the user pastes
/// from there; we don't synthesize keystrokes into other apps).
/// JS: `invoke("copy_clipboard_entry", { entryId })`.
#[tauri::command]
pub fn copy_clipboard_entry(app: AppHandle, entry_id: String) -> Result<(), String> {
    let text = with_state(&app, |data| {
        data.entries
            .iter()
            .find(|e| e.id == entry_id)
            .map(|e| e.text.clone())
    })?
    .ok_or_else(|| "That clipboard entry is gone.".to_string())?;
    app.clipboard()
        .write_text(text.clone())
        .map_err(|e| format!("Couldn't write to the clipboard: {e}"))?;
    // Remember what we just wrote so the watcher doesn't re-record it as
    // a new copy.
    let _ = with_state(&app, |data| {
        data.last_seen = Some(text);
    });
    Ok(())
}

/// Empty the history (the OS clipboard is untouched).
/// JS: `invoke("clear_clipboard")`.
#[tauri::command]
pub fn clear_clipboard(app: AppHandle) -> Result<(), String> {
    with_state(&app, |data| {
        data.entries.clear();
        data.last_seen = None;
        persist(&app, data)
    })?
}

/// Current cap + hotkey for the Settings UI.
/// JS: `invoke("get_clipboard_settings")`.
#[tauri::command]
pub fn get_clipboard_settings(app: AppHandle) -> Result<ClipboardSettings, String> {
    with_state(&app, |data| ClipboardSettings {
        cap: data.cap,
        hotkey: data.hotkey.clone(),
    })
}

/// Change the history cap (10–1000). Truncates immediately.
/// JS: `invoke("set_clipboard_cap", { cap })`.
#[tauri::command]
pub fn set_clipboard_cap(app: AppHandle, cap: usize) -> Result<usize, String> {
    if !(MIN_CAP..=MAX_CAP).contains(&cap) {
        return Err(format!("Keep the history size between {MIN_CAP} and {MAX_CAP}."));
    }
    with_state(&app, |data| {
        data.cap = cap;
        data.entries.truncate(cap);
        persist(&app, data).map(|_| cap)
    })?
}

/// Change the popup hotkey. Goes through the shared registry so conflicts
/// (another AppMaka shortcut, or the launcher summon key) come back as
/// plain-language errors. Empty clears the binding.
/// JS: `invoke("set_clipboard_hotkey", { hotkey })`.
#[tauri::command]
pub fn set_clipboard_hotkey(app: AppHandle, hotkey: String) -> Result<String, String> {
    let normalized = hotkey.trim().to_string();
    set_binding(&app, BINDING_ID, HotkeyKind::Clipboard, &normalized)?;
    with_state(&app, |data| {
        data.hotkey = normalized.clone();
        persist(&app, data).map(|_| normalized)
    })?
}

/// Registration status of the popup hotkey, for the conflict warning UI.
/// JS: `invoke("clipboard_hotkey_status")`.
#[tauri::command]
pub fn clipboard_hotkey_status(app: AppHandle) -> Result<BindingStatus, String> {
    if let Some(status) = crate::hotkeys::binding_status(&app, BINDING_ID) {
        return Ok(status);
    }
    let hotkey = with_state(&app, |data| data.hotkey.clone()).unwrap_or_default();
    Ok(BindingStatus {
        hotkey,
        registered: false,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_record_rules() {
        assert!(!should_record("", None));
        assert!(!should_record("   \n  ", None));
        assert!(!should_record("hello", Some("hello")));
        assert!(should_record("hello", Some("bye")));
        assert!(should_record("hello", None));
        let big = "x".repeat(MAX_TEXT_BYTES + 1);
        assert!(!should_record(&big, None));
        let exactly = "x".repeat(MAX_TEXT_BYTES);
        assert!(should_record(&exactly, None));
    }

    #[test]
    fn insert_entry_pushes_front_and_caps() {
        let mut entries = VecDeque::new();
        for i in 0..5 {
            insert_entry(
                &mut entries,
                ClipboardEntry {
                    id: format!("clip-{i}"),
                    text: format!("text {i}"),
                    created_at_ms: i,
                },
                3,
            );
        }
        assert_eq!(entries.len(), 3);
        // Newest first.
        assert_eq!(entries[0].id, "clip-4");
        assert_eq!(entries[2].id, "clip-2");
    }

    #[test]
    fn clamp_popup_origin_cases() {
        // Cursor-anchored with 12px offset, inside a 1920x1080 monitor.
        assert_eq!(
            clamp_popup_origin((100, 100), (0, 0, 1920, 1080), (440, 520)),
            (112, 112)
        );
        // Near the right edge: clamped so the window stays on-screen.
        assert_eq!(
            clamp_popup_origin((1900, 100), (0, 0, 1920, 1080), (440, 520)),
            (1480, 112)
        );
        // Near the bottom edge: clamped upward (600 - 520 = 80).
        assert_eq!(
            clamp_popup_origin((100, 580), (0, 0, 1920, 600), (440, 520)),
            (112, 80)
        );
        // Monitor with a negative origin (multi-monitor X11).
        assert_eq!(
            clamp_popup_origin((-1900, 100), (-1920, 0, 1920, 1080), (440, 520)),
            (-1888, 112)
        );
        // Window taller than the monitor: pinned to the monitor origin,
        // never panics.
        assert_eq!(
            clamp_popup_origin((100, 100), (0, 0, 300, 200), (440, 520)),
            (0, 0)
        );
    }

    #[test]
    fn file_round_trip_with_defaults() {
        // Old/minimal files deserialize through defaults.
        let f: ClipboardFile = serde_json::from_str("{}").unwrap();
        assert_eq!(f.cap, DEFAULT_CAP);
        assert_eq!(f.hotkey, DEFAULT_HOTKEY);
        assert!(f.entries.is_empty());

        let full = ClipboardFile {
            entries: vec![ClipboardEntry {
                id: "clip-1".to_string(),
                text: "hi".to_string(),
                created_at_ms: 42,
            }],
            cap: 50,
            hotkey: "Ctrl+Shift+X".to_string(),
        };
        let encoded = serde_json::to_string(&full).unwrap();
        let decoded: ClipboardFile = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.entries.len(), 1);
        assert_eq!(decoded.entries[0].text, "hi");
        assert_eq!(decoded.cap, 50);
        assert_eq!(decoded.hotkey, "Ctrl+Shift+X");
    }
}
