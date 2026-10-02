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

/** Summon hotkey + run-at-startup, stored in launcher.json on the backend. */
export interface LauncherSettings {
  hotkey: string;
  autostart: boolean;
}

/** Result of the preview_start command: a signed-in-later throwaway session. */
export interface PreviewStart {
  id: string;
  url: string;
}

/**
 * Result of add_app / preview_add. `created` is false when the site was
 * already in the library — the backend never creates duplicates; the UI
 * reveals the existing entry instead.
 */
export interface AddAppOutcome {
  app: WebApp;
  created: boolean;
}
