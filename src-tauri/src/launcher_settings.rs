//! Launcher settings: summon hotkey + run-at-startup, in one JSON file.
//!
//! The hotkey is stored as a string like `"Alt+Space"` and parsed by
//! tauri-plugin-global-shortcut when registered. When the user changes it,
//! the old binding is unregistered first; if the new one fails to register
//! (taken by another app, invalid), the old one is restored and the change is
//! reported as an error naming the hotkey — the launcher is never left with
//! no way to be summoned.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::{AppHandle, Manager};
use tauri_plugin_autostart::ManagerExt;
use tauri_plugin_global_shortcut::GlobalShortcutExt;

pub const DEFAULT_HOTKEY: &str = "Alt+Space";

/// Default translucency of the phone-folder launcher panel (matches the
/// original hard-coded CSS value). The user can make it more solid in the
/// launcher settings; 1.0 is fully opaque.
pub const DEFAULT_PANEL_OPACITY: f32 = 0.55;
/// Hard floor so the panel can never become unreadably faint.
pub const MIN_PANEL_OPACITY: f32 = 0.3;

fn default_opacity() -> f32 {
    DEFAULT_PANEL_OPACITY
}

/// Clamp to the usable range. NaN (which serde could never produce, but a
/// hand-edited file might) falls back to the default.
pub fn clamp_opacity(v: f32) -> f32 {
    if !v.is_finite() {
        DEFAULT_PANEL_OPACITY
    } else {
        v.clamp(MIN_PANEL_OPACITY, 1.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LauncherSettings {
    pub hotkey: String,
    pub autostart: bool,
    #[serde(default = "default_opacity")]
    pub panel_opacity: f32,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            hotkey: DEFAULT_HOTKEY.to_string(),
            autostart: false,
            panel_opacity: DEFAULT_PANEL_OPACITY,
        }
    }
}

fn settings_path(app: &AppHandle) -> Result<PathBuf, String> {
    Ok(app
        .path()
        .app_data_dir()
        .map_err(|e| format!("could not resolve app data dir: {e}"))?
        .join("launcher.json"))
}

pub fn load(app: &AppHandle) -> LauncherSettings {
    let merged = || -> Option<LauncherSettings> {
        let path = settings_path(app).ok()?;
        let raw = fs::read_to_string(path).ok()?;
        serde_json::from_str(&raw).ok()
    };
    let mut s = merged().unwrap_or_default();
    if s.hotkey.trim().is_empty() {
        s.hotkey = DEFAULT_HOTKEY.to_string();
    }
    s.panel_opacity = clamp_opacity(s.panel_opacity);
    s
}

pub fn save(app: &AppHandle, s: &LauncherSettings) -> Result<(), String> {
    let path = settings_path(app)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("could not create settings dir: {e}"))?;
    }
    let raw = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, raw).map_err(|e| format!("could not write launcher settings: {e}"))?;
    fs::rename(&tmp, &path).map_err(|e| format!("could not save launcher settings: {e}"))?;
    Ok(())
}

/// Register `hotkey` as the summon shortcut. Best-effort at startup: if it
/// fails (already taken, invalid), it is logged and the app keeps running.
pub fn register_hotkey(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    app.global_shortcut()
        .register(hotkey)
        .map_err(|e| format!("could not register {hotkey}: {e}"))
}

/// Swap the summon hotkey: unregister the old one, register the new one, and
/// roll back to the old one if the new registration fails.
pub fn set_hotkey(
    app: &AppHandle,
    settings: &mut LauncherSettings,
    new_hotkey: &str,
) -> Result<(), String> {
    let new_hotkey = new_hotkey.trim().to_string();
    if new_hotkey.is_empty() {
        return Err("Type a hotkey first, e.g. Alt+Space.".to_string());
    }
    if new_hotkey == settings.hotkey {
        return Ok(());
    }
    let old = settings.hotkey.clone();
    let _ = app.global_shortcut().unregister(old.as_str());
    if let Err(e) = app.global_shortcut().register(new_hotkey.as_str()) {
        // Roll back: never leave the user without a working summon key.
        let _ = app.global_shortcut().register(old.as_str());
        return Err(format!(
            "Couldn't use {new_hotkey} ({e}). Kept {old}."
        ));
    }
    settings.hotkey = new_hotkey;
    save(app, settings)
}

/// Set the launcher panel translucency (0.3..=1.0). Saved immediately so
/// the choice survives restarts.
pub fn set_panel_opacity(
    app: &AppHandle,
    settings: &mut LauncherSettings,
    opacity: f32,
) -> Result<(), String> {
    settings.panel_opacity = clamp_opacity(opacity);
    save(app, settings)
}

/// Toggle run-at-startup through the autostart plugin.
pub fn set_autostart(
    app: &AppHandle,
    settings: &mut LauncherSettings,
    enabled: bool,
) -> Result<(), String> {
    if enabled {
        app.autolaunch()
            .enable()
            .map_err(|e| format!("could not enable run at startup: {e}"))?;
    } else {
        app.autolaunch()
            .disable()
            .map_err(|e| format!("could not disable run at startup: {e}"))?;
    }
    settings.autostart = enabled;
    save(app, settings)
}
