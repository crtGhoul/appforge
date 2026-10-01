#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adblock;
mod launcher;
mod launcher_settings;
mod page_title;
mod store;
mod windows;

use adblock::AdblockState;
use launcher::{LauncherState, NativeProgram};
use launcher_settings::LauncherSettings;
use serde::Serialize;
use std::sync::Mutex;
use store::{Account, AppSettings, AppStore, WebApp};
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_autostart::ManagerExt;
use windows::WindowState;

#[tauri::command]
fn list_apps(store: State<'_, AppStore>) -> Result<Vec<WebApp>, String> {
    store.list()
}

#[tauri::command]
fn add_app(name: String, url: String, store: State<'_, AppStore>) -> Result<WebApp, String> {
    store.add_app(name, url)
}

#[tauri::command]
fn update_app(
    id: String,
    name: String,
    url: String,
    color: String,
    store: State<'_, AppStore>,
) -> Result<WebApp, String> {
    store.update_app(&id, name, url, color)
}

#[tauri::command]
fn remove_app(
    app: AppHandle,
    id: String,
    store: State<'_, AppStore>,
) -> Result<(), String> {
    // Windows lock the session files while a webview is alive, so close the
    // app's windows before the store deletes their session directories.
    windows::close_account_windows(&app, &id);
    store.remove_app(&id)
}

#[tauri::command]
fn update_app_settings(
    app: AppHandle,
    id: String,
    settings: AppSettings,
    store: State<'_, AppStore>,
) -> Result<AppSettings, String> {
    let updated = store.update_app_settings(&id, settings)?;
    // Push the adblock toggle to already-open windows; no rebuild needed.
    windows::set_app_adblock_enabled(&app, &id, updated.adblock_enabled);
    Ok(updated)
}

#[tauri::command]
fn add_account(
    app_id: String,
    label: String,
    color: Option<String>,
    store: State<'_, AppStore>,
) -> Result<Account, String> {
    store.add_account(&app_id, label, color)
}

#[tauri::command]
fn remove_account(
    app: AppHandle,
    app_id: String,
    account_id: String,
    store: State<'_, AppStore>,
) -> Result<(), String> {
    windows::close_account_window(&app, &app_id, &account_id);
    store.remove_account(&app_id, &account_id)
}

#[tauri::command]
fn open_account(
    app: AppHandle,
    store: State<'_, AppStore>,
    adblock: State<'_, AdblockState>,
    winstate: State<'_, WindowState>,
    app_id: String,
    account_id: String,
) -> Result<(), String> {
    windows::open_account(&app, &store, &adblock, &winstate, &app_id, &account_id)
}

#[tauri::command]
fn suspend_account(
    app: AppHandle,
    store: State<'_, AppStore>,
    app_id: String,
    account_id: String,
) -> Result<(), String> {
    windows::suspend_account_window(&app, &store, &app_id, &account_id)
}

#[derive(Serialize)]
struct PlatformInfo {
    os: String,
    /// True only where we hook requests at the network layer (Windows).
    network_adblock: bool,
    filter_lists_loaded: bool,
    filter_lists_updated_at: Option<u64>,
}

#[tauri::command]
fn platform_info(adblock: State<'_, AdblockState>) -> Result<PlatformInfo, String> {
    let (filter_lists_loaded, filter_lists_updated_at) = adblock.meta_snapshot();
    Ok(PlatformInfo {
        os: std::env::consts::OS.to_string(),
        network_adblock: cfg!(windows),
        filter_lists_loaded,
        filter_lists_updated_at,
    })
}

// ---------------------------------------------------------------------------
// Launcher commands
// ---------------------------------------------------------------------------

/// Programs found by the last scan (the startup scan runs in the background).
#[tauri::command]
fn list_programs(state: State<'_, LauncherState>) -> Vec<NativeProgram> {
    state.list()
}

/// Full rescan now; blocks a worker thread, not the UI. Returns the count.
#[tauri::command]
fn rescan_programs(state: State<'_, LauncherState>) -> usize {
    state.rescan()
}

/// Launch a program by id. The lookup is server-side, so the frontend can
/// never ask the backend to run an arbitrary path.
#[tauri::command]
fn launch_program(id: String, state: State<'_, LauncherState>) -> Result<(), String> {
    state.launch(&id)
}

/// Hide the main window without quitting (Escape with an empty search box).
#[tauri::command]
fn hide_library(app: AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("main") {
        w.hide().map_err(|e| e.to_string())
    } else {
        Ok(())
    }
}

/// Show the main window in library (management) view. The frontend listens
/// for the `appforge:show-library` event and switches views accordingly.
#[tauri::command]
fn show_library(app: AppHandle) -> Result<(), String> {
    show_library_view(&app)
}

/// Best-effort page title for the quick-add flow. The frontend falls back to
/// a prettified domain name when this errors.
#[tauri::command]
fn fetch_page_title(url: String) -> Result<String, String> {
    page_title::fetch_page_title(&url)
}

/// Show the main window in library (management) view and tell the frontend
/// to switch to it.
fn show_library_view(app: &AppHandle) -> Result<(), String> {
    if let Some(w) = app.get_webview_window("main") {
        w.show().map_err(|e| e.to_string())?;
        w.set_focus().map_err(|e| e.to_string())?;
        app.emit("appforge:show-library", ())
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn get_launcher_settings(app: AppHandle) -> LauncherSettings {
    app.state::<Mutex<LauncherSettings>>()
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default()
}

#[tauri::command]
fn set_hotkey(app: AppHandle, hotkey: String) -> Result<(), String> {
    let state = app.state::<Mutex<LauncherSettings>>();
    let mut settings = state
        .lock()
        .map_err(|e| format!("settings state poisoned: {e}"))?;
    launcher_settings::set_hotkey(&app, &mut settings, &hotkey)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let state = app.state::<Mutex<LauncherSettings>>();
    let mut settings = state
        .lock()
        .map_err(|e| format!("settings state poisoned: {e}"))?;
    launcher_settings::set_autostart(&app, &mut settings, enabled)
}

/// Alt+Space (or the user's chosen key) toggles the window. Showing always
/// lands on the launcher (spotlight) view — the frontend resets via the
/// `appforge:show-launcher` event. Hiding is just hiding.
fn toggle_main_window(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let visible = w.is_visible().unwrap_or(false);
        if visible {
            let _ = w.hide();
        } else {
            let _ = w.center();
            let _ = w.show();
            let _ = w.set_focus();
            let _ = app.emit("appforge:show-launcher", ());
        }
    }
}

/// Build the tray icon: left-click toggles the launcher overlay, the menu
/// offers Show library / Rescan programs / Quit. Missing entirely on Linux
/// desktops without a tray (Wayland GNOME) — the app still works, just
/// without the icon.
fn build_tray(app: &mut tauri::App) -> Result<(), String> {
    use tauri::menu::{Menu, MenuItem};
    use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};

    let show = MenuItem::with_id(app.handle(), "tray-show", "Show library", true, None::<&str>)
        .map_err(|e| e.to_string())?;
    let rescan = MenuItem::with_id(
        app.handle(),
        "tray-rescan",
        "Rescan programs",
        true,
        None::<&str>,
    )
    .map_err(|e| e.to_string())?;
    let quit =
        MenuItem::with_id(app.handle(), "tray-quit", "Quit", true, None::<&str>)
            .map_err(|e| e.to_string())?;
    let menu = Menu::with_items(app.handle(), &[&show, &rescan, &quit])
        .map_err(|e| e.to_string())?;

    let mut builder = TrayIconBuilder::with_id("appforge")
        .tooltip("AppForge")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "tray-show" => {
                let _ = show_library_view(app);
            }
            "tray-rescan" => {
                let handle = app.clone();
                std::thread::Builder::new()
                    .name("appforge-tray-rescan".to_string())
                    .spawn(move || {
                        if let Some(state) = handle.try_state::<LauncherState>() {
                            state.rescan();
                        }
                    })
                    .ok();
            }
            "tray-quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                toggle_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    builder.build(app).map_err(|e| e.to_string())?;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state
                        == tauri_plugin_global_shortcut::ShortcutState::Pressed
                    {
                        toggle_main_window(app);
                    }
                })
                .build(),
        )
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let store =
                AppStore::load(app.handle()).map_err(std::io::Error::other)?;
            let adblock =
                AdblockState::new(app.handle()).map_err(std::io::Error::other)?;

            app.manage(store);
            app.manage(WindowState::default());

            // Filter lists download + engine compile happen on a background
            // thread so startup never waits on the network.
            let adblock_bg = adblock.clone();
            app.manage(adblock);
            std::thread::Builder::new()
                .name("appforge-adblock".to_string())
                .spawn(move || adblock_bg.refresh_loop())
                .map_err(std::io::Error::other)?;

            // --- launcher ---
            let launcher_state =
                LauncherState::new(app.handle()).map_err(std::io::Error::other)?;
            app.manage(launcher_state);
            let settings = launcher_settings::load(app.handle());
            if let Err(e) =
                launcher_settings::register_hotkey(app.handle(), &settings.hotkey)
            {
                eprintln!("launcher hotkey: {e}");
            }
            if settings.autostart {
                if let Err(e) = app.handle().autolaunch().enable() {
                    eprintln!("launcher autostart: {e}");
                }
            }
            app.manage(Mutex::new(settings));
            // First program scan runs in the background; results land in the
            // cache and are picked up by list_programs.
            {
                let handle = app.handle().clone();
                std::thread::Builder::new()
                    .name("appforge-program-scan".to_string())
                    .spawn(move || {
                        if let Some(state) = handle.try_state::<LauncherState>() {
                            state.rescan();
                        }
                    })
                    .map_err(std::io::Error::other)?;
            }

            // Tray icon. Missing on tray-less Linux desktops; that is fine.
            if let Err(e) = build_tray(app) {
                eprintln!("tray: {e}");
            }

            windows::start_suspend_watcher(app.handle().clone());
            Ok(())
        })
        // Closing the main window hides it to the tray; Quit is via the
        // tray menu. The launcher is always one hotkey away.
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_apps,
            add_app,
            update_app,
            remove_app,
            update_app_settings,
            add_account,
            remove_account,
            open_account,
            suspend_account,
            platform_info,
            list_programs,
            rescan_programs,
            launch_program,
            hide_library,
            show_library,
            fetch_page_title,
            get_launcher_settings,
            set_hotkey,
            set_autostart,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
