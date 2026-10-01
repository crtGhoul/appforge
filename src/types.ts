/**
 * Shared types for the AppForge library window.
 *
 * Backend contract (implemented by the Rust side — do not extend).
 * All commands are invoked by their exact names with snake_case args.
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
