/**
 * Shared types for the AppForge library window.
 *
 * Backend contract (implemented by the Rust side — do not extend).
 *
 * IMPORTANT — invoke argument naming: Tauri converts Rust `snake_case`
 * command parameters to `camelCase` for JavaScript. So a command declared as
 * `fn add_account(app_id: String, ...)` MUST be invoked as
 * `invoke("add_account", { appId: ... })` — passing `app_id` fails at
 * RUNTIME with "missing required key appId", and TypeScript cannot catch it
 * (invoke args are not type-checked against the Rust signature). Struct
 * fields (e.g. Account.app_id) are different: they follow serde and stay
 * snake_case in JSON.
 */

export interface AppSettings {
  popup_policy: "block" | "allow";
  popup_allowlist: string[];
  adblock_enabled: boolean;
  auto_suspend_minutes: number;
}

export interface Account {
  id: string;
  app_id: string;
  label: string;
  color: string;
  session_dir: string;
  /** Locally cached og:image thumbnail for the account tile (may be null). */
  thumbnail: string | null;
  last_opened: number;
  created_at: number;
}

export interface WebApp {
  id: string;
  name: string;
  url: string;
  icon: string | null;
  color: string;
  settings: AppSettings;
  accounts: Account[];
  created_at: number;
}

export interface PlatformInfo {
  os: string;
  network_adblock: boolean;
  filter_lists_loaded: boolean;
  filter_lists_updated_at: number | null;
}

/** A native installed program found by the Start Menu / .desktop scan. */
export interface NativeProgram {
  id: string;
  name: string;
  exe_path: string;
  icon_path: string | null;
}

/** Summon hotkey + run-at-startup + launcher panel look, stored in launcher.json on the backend. */
export interface LauncherSettings {
  hotkey: string;
  autostart: boolean;
  /** Launcher panel translucency, 0.3 (faint) .. 1.0 (solid). Backend always sends it (serde default). */
  panel_opacity: number;
}

/** Result of the preview_start command: a signed-in-later throwaway session. */
export interface PreviewStart {
  id: string;
  url: string;
}

/**
 * Result of add_app / preview_add. `created` is false when the site was
 * already in the library — the backend never creates duplicates. For the
 * preview flow, the signed-in session is always adopted: as the first
 * account of a new app, or as a brand-new account on the existing app
 * (`added_account`, never None there). The quick-add form passes no
 * session, so `added_account` is None when it hits an existing app.
 */
export interface AddAppOutcome {
  app: WebApp;
  created: boolean;
  added_account: Account | null;
}

/**
 * Result of the preview_add command, delivered on the
 * `appforge:preview-added` event. Serialized camelCase by the backend:
 * `addedAccount` is the account that adopted the preview's signed-in
 * session — the first account for a new app, or a new account when the
 * site was already in the library.
 */
export interface PreviewAddOutcome {
  app: WebApp;
  created: boolean;
  addedAccount: Account | null;
}
