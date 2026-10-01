#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod adblock;
mod store;
mod windows;

use adblock::AdblockState;
use serde::Serialize;
use store::{Account, AppSettings, AppStore, WebApp};
use tauri::{AppHandle, Manager, State};
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

fn main() {
    tauri::Builder::default()
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

            windows::start_suspend_watcher(app.handle().clone());
            Ok(())
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
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
