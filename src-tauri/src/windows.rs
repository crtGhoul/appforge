//! Account windows: one isolated OS window per (app, account).
//!
//! Isolation mechanism: each account window is built with
//! `WebviewWindowBuilder::data_directory(account.session_dir)`. On Windows
//! that becomes the WebView2 user-data folder; on Linux the WebKitGTK data
//! dir. Never rely on the default data directory — it is shared and, on
//! Windows, lives next to the binary where it may not be writable.
//!
//! This module also owns popup policy (deny-by-default + contained OAuth
//! modals sharing the account's session), per-window activity tracking, and
//! the auto-suspend watcher (TrySuspend on Windows, no-op on Linux).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder, WindowEvent};

use crate::adblock::AdblockState;
use crate::store::AppStore;

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Window label for an account's main window. The suspend watcher and the
/// remove commands find windows by this prefix.
pub fn account_window_label(app_id: &str, account_id: &str) -> String {
    format!("acct-{app_id}-{account_id}")
}

struct TrackedWindow {
    app_id: String,
    last_active: u64,
    /// Flipped live by update_app_settings so open windows follow the toggle
    /// without a rebuild.
    adblock_enabled: Arc<AtomicBool>,
    /// Title without the suspend cue, restored on focus/resume (Windows).
    #[cfg(windows)]
    base_title: String,
    suspended: bool,
}

/// Live per-window bookkeeping, managed as Tauri state.
#[derive(Default)]
pub struct WindowState {
    inner: Mutex<HashMap<String, TrackedWindow>>,
}

/// Open an account's window, or focus it if it is already open. The window is
/// lazily created here — nothing exists until the user opens the account.
pub fn open_account(
    app: &AppHandle,
    store: &AppStore,
    adblock: &AdblockState,
    winstate: &WindowState,
    app_id: &str,
    account_id: &str,
) -> Result<(), String> {
    let web_app = store.get(app_id)?;
    let account = web_app
        .accounts
        .iter()
        .find(|a| a.id == account_id)
        .ok_or_else(|| "Account not found.".to_string())?;

    let label = account_window_label(app_id, account_id);
    if let Some(window) = app.get_webview_window(&label) {
        let _ = window.set_focus();
        return Ok(());
    }

    // The store validates URLs on write, so this only fails on hand-edited
    // apps.json — still no unwrap.
    let page_url: url::Url = web_app
        .url
        .parse()
        .map_err(|_| format!("App URL is not valid: {}", web_app.url))?;
    let title = format!("{} — {}", web_app.name, account.label);
    // The stored absolute path is the source of truth for the data directory.
    let session_dir = store.session_dir_for(app_id, account_id)?;

    let mut builder = WebviewWindowBuilder::new(app, &label, WebviewUrl::External(page_url))
        .data_directory(session_dir.clone())
        .title(&title)
        .inner_size(1200.0, 800.0)
        .center()
        .on_new_window(make_popup_handler(PopupContext {
            app: app.clone(),
            app_id: app_id.to_string(),
            account_id: account_id.to_string(),
            app_name: web_app.name.clone(),
            app_url: web_app.url.clone(),
            session_dir: session_dir.clone(),
            popup_policy: web_app.settings.popup_policy.clone(),
            popup_allowlist: web_app.settings.popup_allowlist.clone(),
        }));
    // Cosmetic filtering: engine-generated hide selectors injected before
    // first paint. Skipped entirely when no engine is loaded (fail open).
    let css = adblock.cosmetic_css_for(&web_app.url);
    if !css.is_empty() {
        builder = builder.initialization_script(cosmetic_init_script(&css));
    }
    let window = builder
        .build()
        .map_err(|e| format!("could not open account window: {e}"))?;

    let adblock_flag = Arc::new(AtomicBool::new(web_app.settings.adblock_enabled));
    {
        let mut tracked = winstate
            .inner
            .lock()
            .map_err(|e| format!("window state lock poisoned: {e}"))?;
        tracked.insert(
            label.clone(),
            TrackedWindow {
                app_id: app_id.to_string(),
                last_active: unix_secs(),
                adblock_enabled: adblock_flag.clone(),
                #[cfg(windows)]
                base_title: title,
                suspended: false,
            },
        );
    }

    // Focus in/out feeds the suspend watcher; focus also resumes a suspended
    // webview on Windows.
    let track_app = app.clone();
    let track_label = label.clone();
    window.on_window_event(move |event| {
        if let WindowEvent::Focused(focused) = event {
            touch_window(&track_app, &track_label, *focused);
        }
    });

    #[cfg(windows)]
    crate::adblock::attach_network_blocking(&window, adblock, adblock_flag);

    store.touch_account(app_id, account_id);
    Ok(())
}

/// Wrap engine-generated hide CSS in a JSON-escaped <style> injection that
/// runs before the page's own scripts (initialization script timing).
/// Shared with the preview flow.
pub(crate) fn cosmetic_init_script(css: &str) -> String {
    // serde_json escaping keeps arbitrary selector text (quotes, backslashes)
    // from breaking out of the JS string literal.
    let json_css = serde_json::to_string(css).unwrap_or_else(|_| "\"\"".to_string());
    format!(
        "(function(){{try{{var css={json_css};\
        var s=document.createElement('style');\
        s.setAttribute('data-appforge','cosmetic');s.textContent=css;\
        var root=document.head||document.documentElement;\
        if(root){{root.appendChild(s);}}}}catch(e){{}}}})();"
    )
}

// ---------------------------------------------------------------------------
// Popup policy
// ---------------------------------------------------------------------------

static OAUTH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Everything the popup handler needs, captured by value (the handler is
/// `Fn`, so it can't borrow from the stack frame that creates the window).
/// Shared with the preview flow (`preview.rs`), which passes placeholder
/// ids since the app doesn't exist yet.
pub(crate) struct PopupContext {
    pub(crate) app: AppHandle,
    pub(crate) app_id: String,
    pub(crate) account_id: String,
    pub(crate) app_name: String,
    pub(crate) app_url: String,
    pub(crate) session_dir: PathBuf,
    pub(crate) popup_policy: String,
    pub(crate) popup_allowlist: Vec<String>,
}

/// Build the `on_new_window` handler for an account window.
///
/// Runs on a separate thread on Windows, so it must stay non-blocking: the
/// decision is made synchronously and any window creation is bounced to the
/// main thread via `run_on_main_thread`. The original request is always
/// denied — allowed popups are re-created as contained modals instead.
///
/// Also used by the preview flow with placeholder ids.
pub(crate) fn make_popup_handler(
    ctx: PopupContext,
) -> impl Fn(url::Url, tauri::webview::NewWindowFeatures) -> tauri::webview::NewWindowResponse<tauri::Wry>
       + Send
       + 'static {
    // Origin used by the OAuth modal's best-effort auto-close.
    let home_origin = app_origin(&ctx.app_url);
    move |url: url::Url, _features| {
        let allowed = if ctx.popup_policy == "allow" {
            true
        } else {
            // "block": only allowlisted hosts get a contained popup.
            let host = url.host_str().unwrap_or("").to_lowercase();
            ctx.popup_allowlist
                .iter()
                .any(|h| h.eq_ignore_ascii_case(&host))
        };
        if allowed {
            spawn_oauth_modal(
                &ctx.app,
                &url,
                &ctx.app_id,
                &ctx.account_id,
                &ctx.app_name,
                &home_origin,
                &ctx.session_dir,
            );
        }
        tauri::webview::NewWindowResponse::Deny
    }
}

/// `scheme://host[:port]` of the app's URL, for the modal auto-close check.
/// Built from parsed parts so no quote characters can sneak in.
fn app_origin(app_url: &str) -> String {
    url::Url::parse(app_url)
        .map(|u| {
            let host = u.host_str().unwrap_or("");
            match u.port() {
                Some(p) => format!("{}://{host}:{p}", u.scheme()),
                None => format!("{}://{host}", u.scheme()),
            }
        })
        .unwrap_or_default()
}

/// Open an allowlisted popup as a small modal bound to the SAME session
/// directory, so an OAuth login lands in the right account's cookie jar.
/// The modal self-closes (best-effort) when navigation returns to the app's
/// origin; the user can always close it by hand.
fn spawn_oauth_modal(
    app: &AppHandle,
    url: &url::Url,
    app_id: &str,
    account_id: &str,
    app_name: &str,
    home_origin: &str,
    session_dir: &Path,
) {
    let n = OAUTH_COUNTER.fetch_add(1, Ordering::Relaxed);
    let label = format!("oauth-{n}");
    // run_on_main_thread needs 'static: clone every borrow up front.
    let app = app.clone();
    let app_id = app_id.to_string();
    let account_id = account_id.to_string();
    let url = url.clone();
    let session_dir = session_dir.to_path_buf();
    let home_origin = home_origin.to_string();
    let title = format!("{app_name} — sign-in");
    // The closure moves its captures; the run_on_main_thread call itself only
    // borrows, so hand the closure its own clone.
    let modal_app = app.clone();
    let _ = app.run_on_main_thread(move || {
        let json_home = serde_json::to_string(&home_origin).unwrap_or_else(|_| "\"\"".to_string());
        let autoclose = format!(
            "(function(){{var home={json_home};\
            var t=setInterval(function(){{try{{\
            if(window.location.origin===home){{window.close();clearInterval(t);}}\
            }}catch(e){{}}}},1500);}})();"
        );
        // Nested popups inside the modal are denied outright: an OAuth flow
        // that needs a second popup is rare, and this prevents modal loops.
        let _ = WebviewWindowBuilder::new(
            &modal_app,
            &label,
            WebviewUrl::External(url.clone()),
        )
        .data_directory(session_dir.clone())
        .title(&title)
        .inner_size(640.0, 720.0)
        .center()
        .initialization_script(autoclose)
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .build()
        .map_err(|e| eprintln!("[appforge] oauth modal failed for {app_id}/{account_id}: {e}"));
    });
}

// ---------------------------------------------------------------------------
// Focus tracking / suspend
// ---------------------------------------------------------------------------

fn touch_window(app: &AppHandle, label: &str, focused: bool) {
    if let Some(winstate) = app.try_state::<WindowState>() {
        if let Ok(mut tracked) = winstate.inner.lock() {
            if let Some(t) = tracked.get_mut(label) {
                t.last_active = unix_secs();
                if focused {
                    t.suspended = false;
                }
            }
        }
    }
    #[cfg(windows)]
    if focused {
        resume_window(app, label);
    }
}

/// Restore the pre-suspend title and wake the renderer. WebView2 auto-resumes
/// a suspended page when its controller becomes visible again; the explicit
/// Resume() covers the case where visibility didn't flip.
#[cfg(windows)]
fn resume_window(app: &AppHandle, label: &str) {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
    use windows_core::Interface;

    let Some(window) = app.get_webview_window(label) else {
        return;
    };
    if let Some(winstate) = app.try_state::<WindowState>() {
        if let Ok(tracked) = winstate.inner.lock() {
            if let Some(t) = tracked.get(label) {
                let _ = window.set_title(&t.base_title);
            }
        }
    }
    let _ = window.with_webview(|platform| unsafe {
        if let Ok(core) = platform.controller().CoreWebView2() {
            if let Ok(core3) = core.cast::<ICoreWebView2_3>() {
                let _ = core3.Resume();
            }
        }
    });
}

/// Background watchdog: every 60s, suspend account windows idle longer than
/// their app's `auto_suspend_minutes` (0 = never). Suspended windows keep
/// their session directory, so reopening/focusing resumes the session.
pub fn start_suspend_watcher(app: AppHandle) {
    let _ = std::thread::Builder::new()
        .name("appforge-suspend".to_string())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(60));
            suspend_idle_windows(&app);
        });
}

fn suspend_idle_windows(app: &AppHandle) {
    let now = unix_secs();
    // Snapshot under the lock; the actual suspend calls happen outside it.
    let tracked: Vec<(String, String, u64, bool)> = match app.try_state::<WindowState>() {
        Some(winstate) => match winstate.inner.lock() {
            Ok(map) => map
                .iter()
                .map(|(label, t)| {
                    (
                        label.clone(),
                        t.app_id.clone(),
                        t.last_active,
                        t.suspended,
                    )
                })
                .collect(),
            Err(_) => return,
        },
        None => return,
    };
    let Some(store) = app.try_state::<AppStore>() else {
        return;
    };
    for (label, app_id, last_active, suspended) in tracked {
        if suspended {
            continue;
        }
        let minutes = match store.get(&app_id) {
            Ok(a) => a.settings.auto_suspend_minutes,
            Err(_) => continue, // app deleted under us; its windows are being closed
        };
        if minutes == 0 {
            continue;
        }
        if now.saturating_sub(last_active) < minutes * 60 {
            continue;
        }
        let Some(window) = app.get_webview_window(&label) else {
            continue;
        };
        // Fail closed: if focus state is unknown, don't suspend.
        if window.is_focused().unwrap_or(true) {
            continue;
        }
        suspend_one(app, &label, &window);
    }
}

/// Suspend one window immediately (the `suspend_account` command path).
/// Validates the account first so typos fail loudly; a closed window is a
/// no-op success.
pub fn suspend_account_window(
    app: &AppHandle,
    store: &AppStore,
    app_id: &str,
    account_id: &str,
) -> Result<(), String> {
    let web_app = store.get(app_id)?;
    if !web_app.accounts.iter().any(|a| a.id == account_id) {
        return Err("Account not found.".to_string());
    }
    let label = account_window_label(app_id, account_id);
    let Some(window) = app.get_webview_window(&label) else {
        return Ok(());
    };
    if window.is_focused().unwrap_or(true) {
        return Ok(());
    }
    suspend_one(app, &label, &window);
    Ok(())
}

#[cfg(windows)]
fn suspend_one(app: &AppHandle, label: &str, window: &WebviewWindow) {
    // WebView2 refuses TrySuspend while the controller is visible
    // (ERROR_INVALID_STATE), so only suspend hidden windows.
    if !webview_hidden(window) {
        return;
    }
    if try_suspend_webview(window) {
        if let Some(winstate) = app.try_state::<WindowState>() {
            if let Ok(mut tracked) = winstate.inner.lock() {
                if let Some(t) = tracked.get_mut(label) {
                    t.suspended = true;
                    // Visible cue that this window is asleep.
                    let _ = window.set_title(&format!("💤 {}", t.base_title));
                }
            }
        }
    }
}

#[cfg(not(windows))]
fn suspend_one(_app: &AppHandle, _label: &str, _window: &WebviewWindow) {
    // No-op: TrySuspend is a WebView2-only API. Linux keeps the webview alive;
    // closing the window is the reclaim path there.
}

/// True when the WebView2 controller reports itself not visible.
#[cfg(windows)]
fn webview_hidden(window: &WebviewWindow) -> bool {
    use windows_core::BOOL;
    let hidden = Arc::new(AtomicBool::new(false));
    let out = hidden.clone();
    let _ = window.with_webview(move |platform| unsafe {
        let mut visible = BOOL(0);
        if platform.controller().IsVisible(&mut visible).is_ok() {
            out.store(!visible.as_bool(), Ordering::Relaxed);
        }
    });
    hidden.load(Ordering::Relaxed)
}

/// Best-effort TrySuspend. Returns whether WebView2 accepted the suspend.
#[cfg(windows)]
fn try_suspend_webview(window: &WebviewWindow) -> bool {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
    use windows_core::Interface;
    let done = Arc::new(AtomicBool::new(false));
    let out = done.clone();
    let _ = window.with_webview(move |platform| unsafe {
        if let Ok(core) = platform.controller().CoreWebView2() {
            // ICoreWebView2 -> ICoreWebView2_3 via QueryInterface.
            if let Ok(core3) = core.cast::<ICoreWebView2_3>() {
                // No completion handler: suspension is fire-and-forget, and
                // the title cue is applied by the caller on success.
                if core3.TrySuspend(None).is_ok() {
                    out.store(true, Ordering::Relaxed);
                }
            }
        }
    });
    done.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Window lifecycle helpers for the commands
// ---------------------------------------------------------------------------

fn close_tracked_window(app: &AppHandle, label: &str) {
    if let Some(window) = app.get_webview_window(label) {
        let _ = window.close();
    }
    if let Some(winstate) = app.try_state::<WindowState>() {
        if let Ok(mut tracked) = winstate.inner.lock() {
            tracked.remove(label);
        }
    }
}

/// Close every open window belonging to an app (used before remove_app).
pub fn close_account_windows(app: &AppHandle, app_id: &str) {
    let prefix = format!("acct-{app_id}-");
    let labels: Vec<String> = app
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with(&prefix))
        .cloned()
        .collect();
    for label in labels {
        close_tracked_window(app, &label);
    }
}

/// Close one account's window if open (used before remove_account).
pub fn close_account_window(app: &AppHandle, app_id: &str, account_id: &str) {
    close_tracked_window(app, &account_window_label(app_id, account_id));
}

/// Push a settings change to already-open windows of an app without rebuilds.
pub fn set_app_adblock_enabled(app: &AppHandle, app_id: &str, enabled: bool) {
    if let Some(winstate) = app.try_state::<WindowState>() {
        if let Ok(tracked) = winstate.inner.lock() {
            for t in tracked.values().filter(|t| t.app_id == app_id) {
                t.adblock_enabled.store(enabled, Ordering::Relaxed);
            }
        }
    }
}
