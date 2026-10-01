//! Preview sign-in flow: open a site in a throwaway session, sign in on the
//! real page, then adopt the session as a new app's first account.
//!
//! The preview is one Tauri window holding two webviews: a slim header strip
//! (our own UI — the URL, an optional account label, Add/Discard buttons)
//! above the site webview. Buttons are never injected into the site's DOM,
//! and the header is the only webview in that window allowed to invoke
//! commands (see `capabilities/preview.json`).
//!
//! The site webview gets a *temporary* session directory,
//! `sessions/.preview-<id>/`, which is either moved into place as the new
//! account's session dir ("Add as app") or deleted ("Discard", the window
//! closed by hand, or left behind by a crash and swept at startup).
//!
//! Windows file-lock ordering: WebView2 locks the user-data dir while the
//! webview lives, so "Add as app" closes the preview window *before* moving
//! the directory, with a short retry loop in case teardown lags behind.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
#[cfg(windows)]
use std::sync::{atomic::AtomicBool, Arc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tauri::{
    AppHandle, Emitter, Manager, PhysicalPosition, PhysicalSize, Webview, WebviewBuilder,
    WebviewUrl, Window, WindowBuilder, WindowEvent,
};

use crate::adblock::AdblockState;
use crate::store::{AppStore, WebApp};
use crate::windows::{self, PopupContext};

/// Header strip height in logical pixels.
const HEADER_H: f64 = 64.0;

/// How long "Add as app" waits for WebView2 to release the temp dir.
const MOVE_RETRIES: u32 = 30;
const MOVE_RETRY_DELAY: Duration = Duration::from_millis(100);

/// Event the library window listens for so it can pick up the new app.
const PREVIEW_ADDED_EVENT: &str = "appforge:preview-added";

#[derive(Debug, Clone)]
struct PreviewSession {
    url: String,
    session_dir: PathBuf,
    /// Latest non-empty document.title seen in the site webview.
    title: Option<String>,
}

/// Live preview sessions, managed as Tauri state.
#[derive(Default)]
pub struct PreviewState {
    inner: Mutex<HashMap<String, PreviewSession>>,
}

/// Returned by `preview_start` so the frontend can report status.
#[derive(Serialize)]
pub struct PreviewStart {
    pub id: String,
    pub url: String,
}

static PREVIEW_COUNTER: AtomicU64 = AtomicU64::new(0);

fn new_preview_id() -> String {
    let n = PREVIEW_COUNTER.fetch_add(1, Ordering::Relaxed);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}-{n}")
}

fn preview_window_label(id: &str) -> String {
    format!("preview-{id}")
}

fn take_session(app: &AppHandle, preview_id: &str) -> Option<PreviewSession> {
    let state = app.try_state::<PreviewState>()?;
    let mut sessions = state.inner.lock().ok()?;
    sessions.remove(preview_id)
}

fn close_preview_window(app: &AppHandle, preview_id: &str) {
    if let Some(window) = app.get_window(&preview_window_label(preview_id)) {
        let _ = window.close();
    }
}

/// Position the header strip on top and the site webview below it, in
/// physical pixels (the window's inner size is physical).
fn layout_preview_views(window: &Window, site: &Webview, header: &Webview, header_h: u32) {
    if let Ok(size) = window.inner_size() {
        let _ = header.set_position(PhysicalPosition::new(0, 0));
        let _ = header.set_size(PhysicalSize::new(size.width, header_h));
        let _ = site.set_position(PhysicalPosition::new(0, header_h as i32));
        let _ = site.set_size(PhysicalSize::new(
            size.width,
            size.height.saturating_sub(header_h),
        ));
    }
}

/// Open the preview window for `url`. The caller signs in on the real site;
/// nothing is persisted until "Add as app".
pub fn start_preview(
    app: &AppHandle,
    adblock: &AdblockState,
    url: &str,
) -> Result<PreviewStart, String> {
    let url = url.trim().to_string();
    if !crate::store::is_valid_url(&url) {
        return Err("URL must start with http:// or https://.".to_string());
    }
    // The store validates URLs on write, so this only fails on a hand-built
    // string that passed the looser check above — still no unwrap.
    let page_url: url::Url = url
        .parse()
        .map_err(|_| format!("App URL is not valid: {url}"))?;

    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve app data dir: {e}"))?;
    let id = new_preview_id();
    let session_dir = data_dir.join("sessions").join(format!(".preview-{id}"));
    std::fs::create_dir_all(&session_dir)
        .map_err(|e| format!("could not create preview session dir: {e}"))?;

    // Build the window first; the session is only registered once the window
    // and both webviews exist, so a half-built preview can never leak state.
    let window = WindowBuilder::new(app, preview_window_label(&id))
        .title("AppForge Preview — sign in")
        .inner_size(1200.0, 860.0)
        .center()
        .build()
        .map_err(|e| format!("could not open preview window: {e}"))?;
    // Clean up the temp dir if anything below fails.
    let build_result: Result<(Webview, Webview, u32), String> = (|| {
        let scale = window.scale_factor().unwrap_or(1.0);
        let header_h = (HEADER_H * scale).round() as u32;

        // Site webview: the real page in the throwaway session. Popups are
        // allowed as contained modals (never real windows) bound to this same
        // temp session — sign-in flows often use them, and the session is
        // discarded unless the user clicks "Add as app".
        let app_for_title = app.clone();
        let title_id = id.clone();
        let mut site_builder =
            WebviewBuilder::new(format!("preview-{id}-site"), WebviewUrl::External(page_url))
                .data_directory(session_dir.clone())
                .on_new_window(windows::make_popup_handler(PopupContext {
                    app: app.clone(),
                    app_id: format!("preview-{id}"),
                    account_id: "preview".to_string(),
                    app_name: "Preview".to_string(),
                    app_url: url.clone(),
                    session_dir: session_dir.clone(),
                    popup_policy: "allow".to_string(),
                    popup_allowlist: Vec::new(),
                }))
                .on_document_title_changed(move |_webview: Webview, title: String| {
                    let title = title.trim().to_string();
                    if title.is_empty() {
                        return;
                    }
                    if let Some(state) = app_for_title.try_state::<PreviewState>() {
                        if let Ok(mut sessions) = state.inner.lock() {
                            if let Some(s) = sessions.get_mut(&title_id) {
                                s.title = Some(title.chars().take(160).collect());
                            }
                        }
                    }
                });
        let css = adblock.cosmetic_css_for(&url);
        if !css.is_empty() {
            site_builder = site_builder.initialization_script(windows::cosmetic_init_script(&css));
        }
        let site = window
            .add_child(
                site_builder,
                PhysicalPosition::new(0, header_h as i32),
                PhysicalSize::new(1200, 860),
            )
            .map_err(|e| format!("could not build preview site view: {e}"))?;

        // Header webview: our own UI. The preview id/url are injected as
        // globals — the page itself is static and carries no per-preview
        // markup.
        let json_id = serde_json::to_string(&id).unwrap_or_else(|_| "\"\"".to_string());
        let json_url = serde_json::to_string(&url).unwrap_or_else(|_| "\"\"".to_string());
        let header_builder = WebviewBuilder::new(
            format!("preview-{id}-header"),
            WebviewUrl::App("preview-header.html".into()),
        )
        .initialization_script(format!(
            "window.__APPFORGE_PREVIEW_ID__={json_id};\
             window.__APPFORGE_PREVIEW_URL__={json_url};"
        ));
        let header = window
            .add_child(
                header_builder,
                PhysicalPosition::new(0, 0),
                PhysicalSize::new(1200, header_h),
            )
            .map_err(|e| format!("could not build preview header: {e}"))?;

        Ok((site, header, header_h))
    })();
    let (site, header, header_h) = match build_result {
        Ok(views) => views,
        Err(e) => {
            let _ = window.close();
            let _ = std::fs::remove_dir_all(&session_dir);
            return Err(e);
        }
    };

    layout_preview_views(&window, &site, &header, header_h);

    // Keep the two webviews glued to the window frame on resize.
    // Webview handles are Arc-backed, so the closure gets its own clones and
    // the originals stay usable for the adblock hookup below.
    let win_r = window.clone();
    let site_r = site.clone();
    let header_r = header.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Resized(_) = event {
            layout_preview_views(&win_r, &site_r, &header_r, header_h);
        }
    });

    // Previews always get ad blocking; there are no per-app settings yet.
    #[cfg(windows)]
    crate::adblock::attach_network_blocking_to_webview(
        &site,
        adblock,
        Arc::new(AtomicBool::new(true)),
    );

    {
        let state = app
            .try_state::<PreviewState>()
            .ok_or_else(|| "preview state not initialized.".to_string())?;
        let mut sessions = state
            .inner
            .lock()
            .map_err(|e| format!("preview state lock poisoned: {e}"))?;
        sessions.insert(
            id.clone(),
            PreviewSession {
                url: url.clone(),
                session_dir,
                title: None,
            },
        );
    }

    Ok(PreviewStart { id, url })
}

/// Abandon a preview: close the window, drop the session, delete the temp
/// dir. The temp dir deletion is best-effort (a lagging WebView2 teardown
/// must not fail the command); leftovers are swept at startup.
pub fn discard_preview(app: &AppHandle, preview_id: &str) -> Result<(), String> {
    let session = take_session(app, preview_id);
    // Close first so WebView2 releases its file locks, then delete.
    close_preview_window(app, preview_id);
    if let Some(s) = session {
        let _ = std::fs::remove_dir_all(&s.session_dir);
    }
    Ok(())
}

/// Turn a preview into a real app: the temp session dir becomes the new
/// app's first account session dir, preserving the sign-in the user just
/// completed. The preview window is closed *before* the move so WebView2
/// releases its locks on the directory.
pub fn add_preview_as_app(
    app: &AppHandle,
    store: &AppStore,
    preview_id: &str,
    label: Option<String>,
) -> Result<WebApp, String> {
    let session = take_session(app, preview_id).ok_or_else(|| "Preview not found.".to_string())?;
    let label = label.unwrap_or_default().trim().to_string();

    // Name: the live document.title wins (it sees JS-rendered titles), then
    // the plain HTTP title fetch, then a prettified domain.
    let name = match session.title.as_deref().map(str::trim) {
        Some(t) if !t.is_empty() => t.to_string(),
        _ => match crate::page_title::fetch_page_title(&session.url) {
            Ok(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => prettified_domain(&session.url),
        },
    };

    close_preview_window(app, preview_id);
    let created = store.add_app_with_session(name, session.url, label, &session.session_dir)?;

    // The library window picks the new app up and opens its first account.
    let _ = app.emit(PREVIEW_ADDED_EVENT, &created);
    Ok(created)
}

/// Best-effort cleanup when the user closes the preview window by hand (the
/// X button). `preview_add`/`preview_discard` already removed their state
/// entries, so those paths no-op here.
pub fn window_closed(app: &AppHandle, label: &str) {
    let Some(id) = label.strip_prefix("preview-") else {
        return;
    };
    if let Some(session) = take_session(app, id) {
        let _ = std::fs::remove_dir_all(&session.session_dir);
    }
}

/// Delete leftover `.preview-*` temp dirs (crash safety). Best-effort.
pub fn cleanup_stale_previews(app: &AppHandle) {
    let Ok(data_dir) = app.path().app_data_dir() else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(data_dir.join("sessions")) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(".preview-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Move a preview temp dir into its final home. Retries briefly because the
/// WebView2 teardown can lag behind the window close on Windows. Falls back
/// to a fresh empty dir (fail-open: the app is still created, the user just
/// signs in again) rather than failing the whole add.
pub(crate) fn move_session_dir(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create session dir: {e}"))?;
    }
    if !src.exists() {
        // Nothing to move (already cleaned up); start fresh.
        std::fs::create_dir_all(dst).map_err(|e| format!("could not create session dir: {e}"))?;
        return Ok(());
    }
    let mut last_err = String::new();
    for _ in 0..MOVE_RETRIES {
        match std::fs::rename(src, dst) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e.to_string();
                std::thread::sleep(MOVE_RETRY_DELAY);
            }
        }
    }
    eprintln!("[appforge] preview session move failed after retries: {last_err}; starting with a fresh session");
    std::fs::create_dir_all(dst).map_err(|e| format!("could not create session dir: {e}"))?;
    Ok(())
}

fn prettified_domain(raw_url: &str) -> String {
    let host = url::Url::parse(raw_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let mut chars = host.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => raw_url.to_string(),
    }
}
