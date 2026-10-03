//! Web-search window (v0.9.3): the launcher's `?query` command opens the
//! search-engine results in a contained in-app webview ("web app") instead
//! of the external browser.
//!
//! This is deliberately NOT an account: no store record, no session
//! persistence, no appearance in the library. Exactly one window exists at
//! a time (fixed label, reused across searches — the RAM discipline), its
//! WebView2/WebKit data dir is wiped when the window closes, and — like
//! every other site window — it loads external URLs only with
//! `withGlobalTauri` false, so no Tauri IPC is ever injected.

use std::path::{Path, PathBuf};
#[cfg(windows)]
use std::sync::atomic::AtomicBool;
#[cfg(windows)]
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, WindowEvent};

use crate::adblock::AdblockState;
use crate::launcher_settings::LauncherSettings;
use crate::windows::{cosmetic_init_script, NAV_KEYS_JS, TARGET_BLANK_SHIM_JS};

/// Fixed label: one search window at a time; a second search reuses it.
pub const SEARCH_WINDOW_LABEL: &str = "websearch";

/// Build the results URL for a query. Pure (unit-tested): the engine comes
/// from the launcher settings, defaulting to DuckDuckGo — the same two
/// engines and URL shapes the old external-browser path used.
fn build_search_url(engine: &str, query: &str) -> Result<url::Url, String> {
    let base = if engine == "google" {
        "https://www.google.com/search"
    } else {
        "https://duckduckgo.com/"
    };
    url::Url::parse_with_params(base, &[("q", query)])
        .map_err(|e| format!("Couldn't build the search URL: {e}"))
}

fn search_engine(app: &AppHandle) -> String {
    app.try_state::<Mutex<LauncherSettings>>()
        .and_then(|s| s.lock().ok().map(|s| s.search_engine.clone()))
        .unwrap_or_else(|| "duckduckgo".to_string())
}

fn search_data_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_data_dir()
        .map(|d| d.join("websearch"))
        .map_err(|e| format!("couldn't resolve the app data dir: {e}"))
}

/// Open the search window for `query`, or navigate the existing one.
/// Sync helper: window *creation* always happens on a dedicated spawned
/// thread (wry#583 — never build on an IPC thread or the main thread);
/// focusing/navigating an existing window is a quick op and safe inline.
pub fn open_search_window(
    app: &AppHandle,
    adblock: &AdblockState,
    query: &str,
) -> Result<(), String> {
    let query = query.trim();
    if query.is_empty() {
        return Err("Type something to search for.".to_string());
    }
    let page_url = build_search_url(&search_engine(app), query)?;

    if let Some(window) = app.get_webview_window(SEARCH_WINDOW_LABEL) {
        // Reuse: unminimize + show first (Windows can't focus a minimized
        // window with set_focus alone), retitle, then navigate.
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        let js = format!(
            "window.location.href={};",
            serde_json::to_string(page_url.as_str()).unwrap_or_default()
        );
        window
            .eval(&js)
            .map_err(|e| format!("Couldn't open the search: {e}"))?;
        let _ = window.set_title(query);
        return Ok(());
    }

    let window_app = app.clone();
    let adblock = adblock.clone();
    let title = query.to_string();
    let data_dir = search_data_dir(app)?;
    let _ = std::thread::Builder::new()
        .name("appmaka-websearch".to_string())
        .spawn(move || build_search_window(&window_app, &adblock, &page_url, &title, &data_dir));
    Ok(())
}

/// JS: `invoke("open_web_search", { query })`.
/// Async on purpose: window-creating commands are never synchronous
/// (Windows WebView2 deadlock, wry#583) — and creation itself still goes
/// through a dedicated thread via `open_search_window`.
#[tauri::command]
pub async fn open_web_search(app: AppHandle, query: String) -> Result<(), String> {
    let adblock = app
        .try_state::<AdblockState>()
        .as_deref()
        .cloned()
        .ok_or_else(|| "ad-blocker state not initialized".to_string())?;
    open_search_window(&app, &adblock, &query)
}

/// Build the contained search window on a dedicated thread. Mirrors
/// `windows::spawn_contained_window`: external URL only, nested popups
/// denied outright, in-app downloads, Alt+Left/Right nav, cosmetic ad
/// hiding + (Windows) network blocking seeded from the app default (on).
fn build_search_window(
    app: &AppHandle,
    adblock: &AdblockState,
    url: &url::Url,
    title: &str,
    data_dir: &Path,
) {
    let mut builder = WebviewWindowBuilder::new(app, SEARCH_WINDOW_LABEL, WebviewUrl::External(url.clone()))
        .data_directory(data_dir.to_path_buf())
        .title(title)
        .inner_size(1200.0, 800.0)
        .center()
        // A search page's popups stay dead: same posture as OAuth modals.
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        // Downloads from a search page stay in-app (v0.7.0 manager) instead
        // of kicking out to the system browser.
        .on_download(crate::downloads::make_download_handler(app.clone()));
    // target=_blank shim (WebKitGTK drops the clicks otherwise) + Alt+Left/
    // Alt+Right history nav: bare webviews have no chrome.
    builder = builder.initialization_script(TARGET_BLANK_SHIM_JS);
    builder = builder.initialization_script(NAV_KEYS_JS);
    let css = adblock.cosmetic_css_for(url.as_str());
    if !css.is_empty() {
        builder = builder.initialization_script(cosmetic_init_script(&css));
    }
    // Not an account, so no per-account override: the app default (on).
    // The flag only exists where network blocking does (Windows).
    #[cfg(windows)]
    let adblock_flag = Arc::new(AtomicBool::new(true));
    match builder.build() {
        Ok(window) => {
            #[cfg(windows)]
            crate::adblock::attach_network_blocking(&window, adblock, adblock_flag);
            // No saved profile: wipe the search data dir when the window
            // closes. Delayed + guarded — a fast reopen recreates the dir,
            // and the guard skips the wipe while a search window is alive.
            let wipe_app = app.clone();
            let wipe_dir = data_dir.to_path_buf();
            window.on_window_event(move |event| {
                if matches!(event, WindowEvent::Destroyed) {
                    // Cloned per event: the handler is Fn, called for every
                    // window event, so nothing may move out of it.
                    let wipe_app = wipe_app.clone();
                    let wipe_dir = wipe_dir.clone();
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_secs(5));
                        if wipe_app.get_webview_window(SEARCH_WINDOW_LABEL).is_none() {
                            let _ = std::fs::remove_dir_all(&wipe_dir);
                        }
                    });
                }
            });
        }
        Err(e) => eprintln!("[appmaka] websearch window failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_url_shapes() {
        let ddg = build_search_url("duckduckgo", "hello world").unwrap();
        assert_eq!(ddg.host_str(), Some("duckduckgo.com"));
        assert_eq!(
            ddg.query_pairs().find(|(k, _)| k == "q").map(|(_, v)| v.into_owned()),
            Some("hello world".to_string())
        );
        let g = build_search_url("google", "a/b?c=d").unwrap();
        assert_eq!(g.host_str(), Some("www.google.com"));
        assert_eq!(
            g.query_pairs().find(|(k, _)| k == "q").map(|(_, v)| v.into_owned()),
            Some("a/b?c=d".to_string())
        );
    }

    #[test]
    fn unknown_engine_falls_back_to_duckduckgo() {
        let u = build_search_url("bing", "x").unwrap();
        assert_eq!(u.host_str(), Some("duckduckgo.com"));
    }

    #[test]
    fn search_window_label_is_fixed_for_reuse() {
        // The whole one-window discipline hangs on this label never varying.
        assert_eq!(SEARCH_WINDOW_LABEL, "websearch");
    }
}
