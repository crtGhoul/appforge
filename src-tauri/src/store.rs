//! JSON persistence for the web-app library.
//!
//! Apps are stored as a JSON array in the Tauri app-data directory
//! (`apps.json`). If the file is corrupt it is backed up next to itself and
//! replaced with a fresh empty library instead of crashing the app.
//!
//! Each app owns one or more accounts; every account points at its own
//! session directory (`sessions/<app_id>/<account_id>/`), which becomes the
//! WebView2 user-data folder / WebKitGTK data dir for that account's window —
//! that directory *is* the session isolation. An app always keeps at least one
//! account; deleting the last one is refused (remove the app instead).

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Manager};

/// Fallback accent color for new apps/accounts (indigo).
pub const DEFAULT_COLOR: &str = "#6366f1";

/// Per-app behavior settings (stored inline on the app record).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    /// "block" = deny new-window requests except allowlisted hosts;
    /// "allow" = open every new-window request as a contained popup.
    /// Old records without it migrate to "block".
    #[serde(default = "default_popup_policy")]
    pub popup_policy: String,
    /// Hostnames (e.g. "accounts.google.com") allowed to open popups when the
    /// policy is "block" — the OAuth / "sign in with" escape hatch.
    /// Old records without it migrate to an empty list.
    #[serde(default)]
    pub popup_allowlist: Vec<String>,
    /// Network-level ad/tracker blocking for this app's windows. Windows-only
    /// at the network layer; cosmetic filtering works everywhere.
    /// Old records without it migrate to on.
    #[serde(default = "default_true")]
    pub adblock_enabled: bool,
    /// Idle minutes after which an unfocused account window is suspended.
    /// 0 = never. Old records without it migrate to 30.
    #[serde(default = "default_auto_suspend_minutes")]
    pub auto_suspend_minutes: u64,
    /// Idle minutes after which an unfocused account window is closed outright
    /// (the session dir survives, so reopening restores the login). 0 = never.
    /// Old records without it migrate to 30.
    #[serde(default = "default_auto_close_minutes")]
    pub auto_close_minutes: u32,
}

fn default_auto_close_minutes() -> u32 {
    30
}

fn default_popup_policy() -> String {
    "block".to_string()
}

fn default_true() -> bool {
    true
}

fn default_auto_suspend_minutes() -> u64 {
    30
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            popup_policy: "block".to_string(),
            popup_allowlist: Vec::new(),
            adblock_enabled: true,
            auto_suspend_minutes: 30,
            auto_close_minutes: 30,
        }
    }
}

impl AppSettings {
    /// Clamp user-supplied settings to sane values so a bad payload can never
    /// be persisted (e.g. an unknown popup policy falls back to "block").
    fn sanitized(mut self) -> Self {
        self.popup_policy = self.popup_policy.trim().to_lowercase();
        if self.popup_policy != "block" && self.popup_policy != "allow" {
            self.popup_policy = "block".to_string();
        }
        self.popup_allowlist = self
            .popup_allowlist
            .into_iter()
            .map(|h| h.trim().to_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        self
    }
}

/// One isolated session inside an app: label + color for the UI, and the
/// absolute path of its session directory (the WebView2 user-data folder).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub app_id: String,
    pub label: String,
    pub color: String,
    pub session_dir: String,
    /// Locally cached site thumbnail (og:image), shown on account tiles.
    /// Old records without it migrate via the serde default.
    #[serde(default)]
    pub thumbnail: Option<String>,
    /// Per-account popup policy override: "block" or "allow".
    /// `None` = inherit the app's `settings.popup_policy`. Old records
    /// without it migrate via the serde default.
    #[serde(default)]
    pub popup_policy: Option<String>,
    /// Old records without it migrate to 0 (treated as "long ago").
    #[serde(default)]
    pub last_opened: u64,
    #[serde(default)]
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebApp {
    pub id: String,
    pub name: String,
    pub url: String,
    pub icon: Option<String>,
    pub color: String,
    /// Old records without it migrate to the current default settings.
    #[serde(default)]
    pub settings: AppSettings,
    pub accounts: Vec<Account>,
    #[serde(default)]
    pub created_at: u64,
}

/// The v0 shape (before accounts/settings existed). Kept only so old
/// `apps.json` files migrate forward instead of being treated as corrupt.
#[derive(Debug, Deserialize)]
struct LegacyWebApp {
    id: String,
    name: String,
    url: String,
}

static ID_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn unix_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

fn new_id(prefix: &str) -> String {
    let n = ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{prefix}-{}-{n}", unix_millis())
}

/// Backend URL check. The frontend normalizes first; this is a second gate so
/// a bad value can never be persisted even if invoked directly. Shared with
/// the preview flow.
pub(crate) fn is_valid_url(url: &str) -> bool {
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

/// Normalize a user-supplied color to lowercase `#rrggbb`. Accepts the
/// leading `#` with or without it; rejects anything that is not exactly six
/// hex digits.
pub(crate) fn normalize_hex_color(raw: &str) -> Option<String> {
    let s = raw.trim().strip_prefix('#').unwrap_or(raw.trim());
    if s.len() == 6 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        Some(format!("#{}", s.to_lowercase()))
    } else {
        None
    }
}

/// Result of adding an app: either a fresh app, or the already-existing app
/// for the same site. Duplicates are never created. `added_account` is the
/// account that adopted the caller's session, when one was involved (the
/// preview flow); the quick-add form passes no session, so it is `None`
/// when the app already existed there.
#[derive(Debug, Clone, Serialize)]
pub struct AddAppOutcome {
    pub app: WebApp,
    pub created: bool,
    pub added_account: Option<Account>,
}

/// Canonical key for duplicate detection: lowercase scheme + host, default
/// ports dropped, trailing slashes trimmed, query/fragment ignored.
/// "https://muse.ai", "https://muse.ai/" and "https://muse.ai/?x=1" all map
/// to one entry; different hosts (or schemes) never collide.
fn normalize_url_key(raw: &str) -> String {
    let trimmed = raw.trim();
    match url::Url::parse(trimmed) {
        Ok(u) => {
            let scheme = u.scheme().to_lowercase();
            let host = u.host_str().unwrap_or("").to_lowercase();
            if host.is_empty() {
                return trimmed.to_lowercase();
            }
            let port = match (u.scheme(), u.port()) {
                ("http", Some(80)) | ("https", Some(443)) | (_, None) => String::new(),
                (_, Some(p)) => format!(":{p}"),
            };
            let path = u.path().trim_end_matches('/');
            format!("{scheme}://{host}{port}{path}")
        }
        Err(_) => trimmed.to_lowercase(),
    }
}

pub struct AppStore {
    path: PathBuf,
    data_dir: PathBuf,
    apps: Mutex<Vec<WebApp>>,
}

impl AppStore {
    pub fn load(app: &AppHandle) -> Result<Self, String> {
        let dir = app
            .path()
            .app_data_dir()
            .map_err(|e| format!("could not resolve app data dir: {e}"))?;
        fs::create_dir_all(&dir).map_err(|e| format!("could not create app data dir: {e}"))?;
        let path = dir.join("apps.json");

        let apps = match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<Vec<WebApp>>(&contents) {
                Ok(apps) => apps,
                // Not the current shape: maybe the pre-accounts v0 shape.
                Err(_) => match serde_json::from_str::<Vec<LegacyWebApp>>(&contents) {
                    Ok(legacy) => {
                        let migrated = Self::migrate_legacy(legacy, &dir)?;
                        // Persist the migrated shape so this only happens once.
                        let json = serde_json::to_string_pretty(&migrated)
                            .map_err(|e| format!("could not serialize migrated apps: {e}"))?;
                        let tmp = path.with_extension("json.tmp");
                        fs::write(&tmp, json)
                            .map_err(|e| format!("could not write apps.json: {e}"))?;
                        fs::rename(&tmp, &path)
                            .map_err(|e| format!("could not write apps.json: {e}"))?;
                        migrated
                    }
                    Err(_) => {
                        // Corrupt file: back it up beside itself, start fresh.
                        let backup =
                            dir.join(format!("apps.json.corrupt-{}.bak", unix_millis()));
                        let _ = fs::rename(&path, &backup);
                        Vec::new()
                    }
                },
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(format!("could not read apps.json: {e}")),
        };

        let store = Self {
            path,
            data_dir: dir,
            apps: Mutex::new(apps),
        };
        // The auto-created first account used to be labeled "Default", which
        // users found confusing — it is now "Main". Rename it silently on
        // load (the user can rename any account in the library anyway).
        store.normalize_default_labels()?;
        Ok(store)
    }

    /// Rename legacy "Default" account labels to "Main". Persists only when
    /// something actually changed.
    fn normalize_default_labels(&self) -> Result<(), String> {
        let changed = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let mut changed = false;
            for app in apps.iter_mut() {
                for account in app.accounts.iter_mut() {
                    if account.label == "Default" {
                        account.label = "Main".to_string();
                        changed = true;
                    }
                }
            }
            changed
        };
        if changed {
            self.save()?;
        }
        Ok(())
    }

    /// Give a v0 app (no settings/accounts) the current shape: default
    /// settings plus one "Main" account with a fresh session directory.
    fn migrate_legacy(legacy: Vec<LegacyWebApp>, data_dir: &Path) -> Result<Vec<WebApp>, String> {
        let now = unix_secs();
        let mut apps = Vec::with_capacity(legacy.len());
        for old in legacy {
            let account_id = new_id("acct");
            let session_dir = data_dir
                .join("sessions")
                .join(&old.id)
                .join(&account_id);
            fs::create_dir_all(&session_dir)
                .map_err(|e| format!("could not create session dir: {e}"))?;
            apps.push(WebApp {
                id: old.id.clone(),
                name: old.name,
                url: old.url,
                icon: None,
                color: DEFAULT_COLOR.to_string(),
                settings: AppSettings::default(),
                accounts: vec![Account {
                    id: account_id,
                    app_id: old.id,
                    label: "Main".to_string(),
                    color: DEFAULT_COLOR.to_string(),
                    session_dir: session_dir.to_string_lossy().into_owned(),
                    thumbnail: None,
                    popup_policy: None,
                    last_opened: 0,
                    created_at: now,
                }],
                created_at: now,
            });
        }
        Ok(apps)
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

    fn sessions_root(&self) -> PathBuf {
        self.data_dir.join("sessions")
    }

    fn favicons_dir(&self) -> PathBuf {
        self.data_dir.join("favicons")
    }

    /// Best-effort account thumbnail: the site's og:image, cached locally.
    /// Never fails — returns None when anything goes wrong, and the tiles
    /// keep their fallbacks. Called without the apps lock held.
    fn fetch_thumbnail(&self, url: &str) -> Option<String> {
        let page_url: url::Url = url.parse().ok()?;
        let path = crate::favicon::fetch_og_image(&page_url, &self.favicons_dir())?;
        Some(path.to_string_lossy().into_owned())
    }

    pub fn list(&self) -> Result<Vec<WebApp>, String> {
        self.apps
            .lock()
            .map(|apps| apps.clone())
            .map_err(|e| format!("store lock poisoned: {e}"))
    }

    pub fn get(&self, id: &str) -> Result<WebApp, String> {
        self.apps
            .lock()
            .map_err(|e| format!("store lock poisoned: {e}"))?
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| "App not found.".to_string())
    }

    /// Create the app *and* its first "Main" account in one step, so an app
    /// never exists without at least one session to open.
    ///
    /// If an app for the same site already exists (URL-normalized), no
    /// duplicate is created — the existing app is returned with
    /// `created: false` so the UI can reveal it instead.
    pub fn add_app(&self, name: String, url: String) -> Result<AddAppOutcome, String> {
        let name = name.trim().to_string();
        let url = url.trim().to_string();
        if name.is_empty() {
            return Err("Name is required.".to_string());
        }
        if !is_valid_url(&url) {
            return Err("URL must start with http:// or https://.".to_string());
        }
        if let Some(existing) = self.find_by_url(&url) {
            return Ok(AddAppOutcome {
                app: existing,
                created: false,
                added_account: None,
            });
        }
        let now = unix_secs();
        let app_id = new_id("app");
        let account_id = new_id("acct");
        let session_dir = self.sessions_root().join(&app_id).join(&account_id);
        fs::create_dir_all(&session_dir)
            .map_err(|e| format!("could not create session dir: {e}"))?;

        let app = WebApp {
            id: app_id.clone(),
            name,
            url,
            icon: None,
            color: DEFAULT_COLOR.to_string(),
            settings: AppSettings::default(),
            accounts: vec![Account {
                id: account_id,
                app_id,
                label: "Main".to_string(),
                color: DEFAULT_COLOR.to_string(),
                session_dir: session_dir.to_string_lossy().into_owned(),
                thumbnail: None,
                popup_policy: None,
                last_opened: 0,
                created_at: now,
            }],
            created_at: now,
        };
        {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            apps.push(app.clone());
        }
        self.save()?;
        Ok(AddAppOutcome { added_account: app.accounts.first().cloned(), app, created: true })
    }

    /// Find an app by normalized URL. Used to refuse duplicates.
    fn find_by_url(&self, url: &str) -> Option<WebApp> {
        let key = normalize_url_key(url);
        self.apps
            .lock()
            .ok()?
            .iter()
            .find(|a| normalize_url_key(&a.url) == key)
            .cloned()
    }

    /// Create the app *and* its first account from a preview session: the
    /// temp dir the user signed in to becomes the account's session dir, so
    /// the sign-in carries over. The caller closes the preview window first
    /// (WebView2 locks the dir while the webview lives); the move retries
    /// briefly and falls back to a fresh dir rather than failing the add.
    pub fn add_app_with_session(
        &self,
        name: String,
        url: String,
        label: String,
        temp_dir: &Path,
    ) -> Result<AddAppOutcome, String> {
        let name = name.trim().to_string();
        let url = url.trim().to_string();
        let label = label.trim().to_string();
        if name.is_empty() {
            return Err("Name is required.".to_string());
        }
        if !is_valid_url(&url) {
            return Err("URL must start with http:// or https://.".to_string());
        }
        // A preview of a site that's already in the library becomes another
        // account on the existing app: the signed-in temp dir is adopted as
        // the new account's session dir, never destroyed. (The quick-add
        // form keeps its old reveal-the-existing-app behavior — no session
        // is involved there.)
        if let Some(existing) = self.find_by_url(&url) {
            let now = unix_secs();
            let app_id = existing.id.clone();
            let account_id = new_id("acct");
            let session_dir = self.sessions_root().join(&app_id).join(&account_id);
            crate::preview::move_session_dir(temp_dir, &session_dir)?;
            // Best-effort thumbnail, fetched before the write lock.
            let thumbnail = self.fetch_thumbnail(&url);
            let account = Account {
                id: account_id,
                app_id: app_id.clone(),
                label: if label.is_empty() {
                    format!("Account {}", existing.accounts.len() + 1)
                } else {
                    label
                },
                color: DEFAULT_COLOR.to_string(),
                session_dir: session_dir.to_string_lossy().into_owned(),
                thumbnail,
                popup_policy: None,
                last_opened: 0,
                created_at: now,
            };
            let updated = {
                let mut apps = self
                    .apps
                    .lock()
                    .map_err(|e| format!("store lock poisoned: {e}"))?;
                let app = apps
                    .iter_mut()
                    .find(|a| a.id == app_id)
                    .ok_or_else(|| "App not found.".to_string())?;
                app.accounts.push(account.clone());
                app.clone()
            };
            self.save()?;
            return Ok(AddAppOutcome {
                app: updated,
                created: false,
                added_account: Some(account),
            });
        }
        let now = unix_secs();
        let app_id = new_id("app");
        let account_id = new_id("acct");
        let session_dir = self.sessions_root().join(&app_id).join(&account_id);
        crate::preview::move_session_dir(temp_dir, &session_dir)?;
        // Best-effort thumbnail, fetched before the write lock.
        let thumbnail = self.fetch_thumbnail(&url);

        let app = WebApp {
            id: app_id.clone(),
            name,
            url,
            icon: None,
            color: DEFAULT_COLOR.to_string(),
            settings: AppSettings::default(),
            accounts: vec![Account {
                id: account_id,
                app_id,
                label: if label.is_empty() {
                    "Main".to_string()
                } else {
                    label
                },
                color: DEFAULT_COLOR.to_string(),
                session_dir: session_dir.to_string_lossy().into_owned(),
                thumbnail,
                popup_policy: None,
                last_opened: 0,
                created_at: now,
            }],
            created_at: now,
        };
        {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            apps.push(app.clone());
        }
        self.save()?;
        Ok(AddAppOutcome { added_account: app.accounts.first().cloned(), app, created: true })
    }

    pub fn update_app(        &self,
        id: &str,
        name: String,
        url: String,
        color: String,
    ) -> Result<WebApp, String> {
        let name = name.trim().to_string();
        let url = url.trim().to_string();
        let color = normalize_hex_color(&color)
            .ok_or_else(|| "Color must be a hex color like #6366f1.".to_string())?;
        if name.is_empty() {
            return Err("Name is required.".to_string());
        }
        if name.chars().count() > 80 {
            return Err("Name is too long (80 characters max).".to_string());
        }
        if !is_valid_url(&url) {
            return Err("URL must start with http:// or https://.".to_string());
        }
        let updated = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let app = apps
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or_else(|| "App not found.".to_string())?;
            app.name = name;
            app.url = url;
            app.color = color;
            app.clone()
        };
        self.save()?;
        Ok(updated)
    }

    /// Rename an app (name only — URL/color/settings are untouched).
    pub fn rename_app(&self, id: &str, name: String) -> Result<WebApp, String> {
        let name = name.trim().to_string();
        if name.is_empty() {
            return Err("Name is required.".to_string());
        }
        let updated = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let app = apps
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or_else(|| "App not found.".to_string())?;
            app.name = name;
            app.clone()
        };
        self.save()?;
        Ok(updated)
    }

    /// Rename an account (label only — the session dir is untouched).
    pub fn rename_account(
        &self,
        app_id: &str,
        account_id: &str,
        label: String,
    ) -> Result<Account, String> {
        let label = label.trim().to_string();
        if label.is_empty() {
            return Err("Account label is required.".to_string());
        }
        let updated = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let account = apps
                .iter_mut()
                .find(|a| a.id == app_id)
                .and_then(|a| a.accounts.iter_mut().find(|ac| ac.id == account_id))
                .ok_or_else(|| "Account not found.".to_string())?;
            account.label = label;
            account.clone()
        };
        self.save()?;
        Ok(updated)
    }

    /// Edit an account's label, color, and per-account popup policy override.
    /// `popup_policy` is `None` = inherit the app's setting, `Some("block")`
    /// or `Some("allow")` = override. Anything else is rejected so a bad
    /// value can never be persisted even if invoked directly.
    pub fn update_account(
        &self,
        app_id: &str,
        account_id: &str,
        label: String,
        color: String,
        popup_policy: Option<String>,
    ) -> Result<WebApp, String> {
        let label = label.trim().to_string();
        if label.is_empty() {
            return Err("Account label is required.".to_string());
        }
        if label.chars().count() > 60 {
            return Err("Account label is too long (60 characters max).".to_string());
        }
        let color = normalize_hex_color(&color)
            .ok_or_else(|| "Color must be a hex color like #6366f1.".to_string())?;
        let popup_policy = match popup_policy {
            None => None,
            Some(p) => {
                let p = p.trim().to_lowercase();
                if p != "block" && p != "allow" {
                    return Err("Popup policy must be \"block\" or \"allow\".".to_string());
                }
                Some(p)
            }
        };
        let updated = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let app = apps
                .iter_mut()
                .find(|a| a.id == app_id)
                .ok_or_else(|| "App not found.".to_string())?;
            let account = app
                .accounts
                .iter_mut()
                .find(|ac| ac.id == account_id)
                .ok_or_else(|| "Account not found.".to_string())?;
            account.label = label;
            account.color = color;
            account.popup_policy = popup_policy;
            app.clone()
        };
        self.save()?;
        Ok(updated)
    }

    /// Set (or clear) the cached favicon path for an app. Written by the
    /// `fetch_favicon` command after it downloads the site's logo.
    pub fn set_app_icon(&self, id: &str, icon: Option<String>) -> Result<WebApp, String> {
        let updated = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let app = apps
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or_else(|| "App not found.".to_string())?;
            app.icon = icon;
            app.clone()
        };
        self.save()?;
        Ok(updated)
    }

    /// Remove the app record and delete all of its session data. The caller
    /// closes the app's windows first; dir deletion is best-effort (a locked
    /// file on Windows must not block removing the record).
    pub fn remove_app(&self, id: &str) -> Result<(), String> {
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
        let _ = fs::remove_dir_all(self.sessions_root().join(id));
        self.save()
    }

    pub fn update_app_settings(
        &self,
        id: &str,
        settings: AppSettings,
    ) -> Result<AppSettings, String> {
        let settings = settings.sanitized();
        {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let app = apps
                .iter_mut()
                .find(|a| a.id == id)
                .ok_or_else(|| "App not found.".to_string())?;
            app.settings = settings.clone();
        }
        self.save()?;
        Ok(settings)
    }

    pub fn add_account(
        &self,
        app_id: &str,
        label: String,
        color: Option<String>,
    ) -> Result<Account, String> {
        let label = label.trim().to_string();
        if label.is_empty() {
            return Err("Account label is required.".to_string());
        }
        // Best-effort thumbnail: resolve the URL and fetch before the write
        // lock, so a slow site can't hold it.
        let app_url = {
            let apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            apps
                .iter()
                .find(|a| a.id == app_id)
                .map(|a| a.url.clone())
                .ok_or_else(|| "App not found.".to_string())?
        };
        let thumbnail = self.fetch_thumbnail(&app_url);

        let now = unix_secs();
        let account_id = new_id("acct");
        let session_dir = self.sessions_root().join(app_id).join(&account_id);
        fs::create_dir_all(&session_dir)
            .map_err(|e| format!("could not create session dir: {e}"))?;

        let account = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let app = apps
                .iter_mut()
                .find(|a| a.id == app_id)
                .ok_or_else(|| "App not found.".to_string())?;
            let account = Account {
                id: account_id,
                app_id: app_id.to_string(),
                label,
                color: color
                    .map(|c| c.trim().to_string())
                    .filter(|c| !c.is_empty())
                    .unwrap_or_else(|| app.color.clone()),
                session_dir: session_dir.to_string_lossy().into_owned(),
                thumbnail,
                popup_policy: None,
                last_opened: 0,
                created_at: now,
            };
            app.accounts.push(account.clone());
            account
        };
        self.save()?;
        Ok(account)
    }

    /// The caller closes the account's window first (a live WebView2 session
    /// keeps files locked on Windows, so dir deletion is best-effort).
    pub fn remove_account(&self, app_id: &str, account_id: &str) -> Result<(), String> {
        let session_dir = {
            let mut apps = self
                .apps
                .lock()
                .map_err(|e| format!("store lock poisoned: {e}"))?;
            let app = apps
                .iter_mut()
                .find(|a| a.id == app_id)
                .ok_or_else(|| "App not found.".to_string())?;
            if app.accounts.len() <= 1 {
                return Err(
                    "An app must keep at least one account — remove the app instead.".to_string(),
                );
            }
            let pos = app
                .accounts
                .iter()
                .position(|a| a.id == account_id)
                .ok_or_else(|| "Account not found.".to_string())?;
            app.accounts.remove(pos).session_dir
        };
        let _ = fs::remove_dir_all(session_dir);
        self.save()
    }

    /// Wipe an account's session directory and recreate it empty — a full
    /// sign-out. The account record, its app, and every other account are
    /// untouched. The caller closes the account's window first: Windows
    /// locks session files while the webview is alive, so a live window
    /// would leave stale files behind.
    /// (Interim allow: called by the `forget_login` command, which the
    /// coordinator registers in main.rs.)
    #[allow(dead_code)]
    pub fn forget_account_session(&self, app_id: &str, account_id: &str) -> Result<(), String> {
        // The stored absolute path is the source of truth, so only this
        // account's directory is ever touched.
        let session_dir = self.session_dir_for(app_id, account_id)?;
        let _ = fs::remove_dir_all(&session_dir);
        fs::create_dir_all(&session_dir)
            .map_err(|e| format!("could not recreate session dir: {e}"))?;
        Ok(())
    }

    /// Record that an account's window was opened/focused (MRU ordering).
    /// Best-effort: a failed write must not break opening the window.
    pub fn touch_account(&self, app_id: &str, account_id: &str) {
        let touched = (|| -> Result<(), String> {
            {
                let mut apps = self
                    .apps
                    .lock()
                    .map_err(|e| format!("store lock poisoned: {e}"))?;
                let account = apps
                    .iter_mut()
                    .find(|a| a.id == app_id)
                    .and_then(|a| a.accounts.iter_mut().find(|ac| ac.id == account_id))
                    .ok_or_else(|| "Account not found.".to_string())?;
                account.last_opened = unix_secs();
            }
            self.save()
        })();
        let _ = touched;
    }

    /// Absolute session-dir path for an account. The caller (window opener)
    /// needs it before any window exists, so this is separate from the record.
    pub fn session_dir_for(&self, app_id: &str, account_id: &str) -> Result<PathBuf, String> {
        self.apps
            .lock()
            .map_err(|e| format!("store lock poisoned: {e}"))?
            .iter()
            .find(|a| a.id == app_id)
            .and_then(|a| a.accounts.iter().find(|ac| ac.id == account_id))
            .map(|ac| PathBuf::from(&ac.session_dir))
            .ok_or_else(|| "Account not found.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_settings_migrates_missing_fields() {
        // A hand-edited apps.json missing newer settings fields still parses.
        let s: AppSettings = serde_json::from_str(r#"{"popup_policy":"allow"}"#).unwrap();
        assert_eq!(s.popup_policy, "allow");
        assert!(s.popup_allowlist.is_empty());
        assert!(s.adblock_enabled);
        assert_eq!(s.auto_suspend_minutes, 30);
        assert_eq!(s.auto_close_minutes, 30);
    }

    #[test]
    fn webapp_migrates_missing_settings_and_timestamps() {
        let json = r##"{
            "id": "app-1", "name": "Test", "url": "https://example.com",
            "icon": null, "color": "#ffffff", "accounts": []
        }"##;
        let app: WebApp = serde_json::from_str(json).unwrap();
        assert_eq!(app.settings.popup_policy, "block");
        assert_eq!(app.created_at, 0);
    }

    #[test]
    fn account_migrates_missing_optional_fields() {
        let json = r##"{
            "id": "acct-1", "app_id": "app-1", "label": "Main",
            "color": "#ffffff", "session_dir": "/tmp/sess"
        }"##;
        let acct: Account = serde_json::from_str(json).unwrap();
        assert!(acct.thumbnail.is_none());
        assert!(acct.popup_policy.is_none());
        assert_eq!(acct.last_opened, 0);
        assert_eq!(acct.created_at, 0);
    }

    #[test]
    fn partial_launcher_style_partial_apps_json_parses() {
        // Missing core identity fields still fail loudly (the corrupt-backup
        // path), rather than silently becoming a broken record.
        let bad: Result<WebApp, _> = serde_json::from_str(r#"{"name":"No id"}"#);
        assert!(bad.is_err());
    }
}
