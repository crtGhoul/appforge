#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod store;

use store::{AppStore, WebApp};
use tauri::{Manager, State};

#[tauri::command]
fn list_apps(store: State<'_, AppStore>) -> Result<Vec<WebApp>, String> {
    store.list()
}

#[tauri::command]
fn add_app(
    name: String,
    url: String,
    store: State<'_, AppStore>,
) -> Result<WebApp, String> {
    store.add(name, url)
}

#[tauri::command]
fn remove_app(id: String, store: State<'_, AppStore>) -> Result<(), String> {
    store.remove(&id)
}

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let store = AppStore::load(app.handle())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
            app.manage(store);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![list_apps, add_app, remove_app])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
