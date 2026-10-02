import { useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type {
  Account,
  LauncherSettings,
  NativeProgram,
  WebApp,
} from "./types";

function errMsg(err: unknown): string {
  return typeof err === "string" ? err : "Something went wrong.";
}

function hostOf(appUrl: string): string {
  try {
    return new URL(appUrl).hostname;
  } catch {
    return appUrl;
  }
}

/**
 * Subsequence fuzzy score. Higher is better, 0 means no match. Rewards
 * word-start matches and consecutive runs; shorter names win ties.
 */
export function fuzzyScore(query: string, text: string): number {
  const q = query.toLowerCase().trim();
  const t = text.toLowerCase();
  if (!q) return 0;
  let score = 0;
  let ti = 0;
  let lastMatch = -2;
  for (let qi = 0; qi < q.length; qi++) {
    const found = t.indexOf(q[qi], ti);
    if (found === -1) return 0;
    if (found === 0 || t[found - 1] === " " || t[found - 1] === "-" || t[found - 1] === "_") {
      score += 3;
    } else if (found === lastMatch + 1) {
      score += 2;
    } else {
      score += 1;
    }
    lastMatch = found;
    ti = found + 1;
  }
  score += Math.max(0, 20 - t.length);
  return score;
}

export type SearchResult =
  | { kind: "account"; id: string; title: string; context: string; score: number; app: WebApp; account: Account }
  | { kind: "app"; id: string; title: string; context: string; score: number; app: WebApp }
  | { kind: "program"; id: string; title: string; context: string; score: number; program: NativeProgram };

/**
 * Columns in the launcher folder grid. Must match `grid-template-columns`
 * in App.css — keyboard navigation moves by this many rows per Up/Down.
 */
export const GRID_COLUMNS = 6;

/** Max tiles shown when browsing with an empty query. Search caps at 25. */
const BROWSE_LIMIT = 48;

/**
 * Everything, for the phone-folder grid when no query is typed: web apps
 * (with their accounts right after each app), then installed programs —
 * alphabetical, capped. Like opening a folder on a phone home screen.
 */
export function browseAll(apps: WebApp[], programs: NativeProgram[]): SearchResult[] {
  const items: SearchResult[] = [];
  const sortedApps = [...apps].sort((a, b) => a.name.localeCompare(b.name));
  for (const app of sortedApps) {
    items.push({
      kind: "app",
      id: `app:${app.id}`,
      title: app.name,
      context: hostOf(app.url),
      score: 0,
      app,
    });
    const sortedAccounts = [...app.accounts].sort((a, b) =>
      a.label.localeCompare(b.label)
    );
    for (const account of sortedAccounts) {
      items.push({
        kind: "account",
        id: `account:${account.id}`,
        // App name first (the "Default" account label used to be the big
        // text, which confused users); the account label is the sublabel.
        title: app.name,
        context: account.label,
        score: 0,
        app,
        account,
      });
    }
  }
  const sortedPrograms = [...programs].sort((a, b) => a.name.localeCompare(b.name));
  for (const program of sortedPrograms) {
    items.push({
      kind: "program",
      id: `program:${program.id}`,
      title: program.name,
      context: "",
      score: 0,
      program,
    });
  }
  return items.slice(0, BROWSE_LIMIT);
}

/**
 * One ranked result list across accounts, web apps, and native programs.
 * Apps only appear as their own row when the query matches the app itself
 * (not just its accounts) — keeps the list short.
 */
export function buildResults(
  query: string,
  apps: WebApp[],
  programs: NativeProgram[]
): SearchResult[] {
  const q = query.trim();
  if (!q) return [];
  const results: SearchResult[] = [];

  for (const app of apps) {
    const appScore = Math.max(fuzzyScore(q, app.name), fuzzyScore(q, hostOf(app.url)));
    if (appScore > 0) {
      results.push({
        kind: "app",
        id: `app:${app.id}`,
        title: app.name,
        context: hostOf(app.url),
        score: appScore,
        app,
      });
    }
    for (const account of app.accounts) {
      const score = Math.max(fuzzyScore(q, account.label), fuzzyScore(q, `${account.label} ${app.name}`));
      if (score > 0) {
        results.push({
          kind: "account",
          id: `account:${account.id}`,
          // Tile shows the app name big, the account label small — the
          // account label still participates in matching via the score.
          title: app.name,
          context: account.label,
          score: score + 1, // accounts edge out the bare app row
          app,
          account,
        });
      }
    }
  }

  for (const program of programs) {
    const score = fuzzyScore(q, program.name);
    if (score > 0) {
      results.push({
        kind: "program",
        id: `program:${program.id}`,
        title: program.name,
        context: "Program",
        score,
        program,
      });
    }
  }

  return results.sort((a, b) => b.score - a.score).slice(0, 25);
}

/**
 * Phone-folder grid: one tile per result — icon with the name underneath.
 * Accounts show their app's icon with the account label (and the app name
 * as a quiet sub-label); programs show their extracted .exe icon.
 */
export function IconGrid({
  items,
  activeIndex,
  onHover,
  onActivate,
  renderIcon,
}: {
  items: SearchResult[];
  activeIndex: number;
  onHover: (i: number) => void;
  onActivate: (r: SearchResult) => void;
  renderIcon: (r: SearchResult) => React.ReactNode;
}) {
  return (
    <div className="icon-grid" role="listbox" aria-label="Apps and programs">
      {items.map((r, i) => (
        <button
          key={r.id}
          type="button"
          role="option"
          aria-selected={i === activeIndex}
          className={`icon-tile${i === activeIndex ? " is-active" : ""}`}
          onMouseEnter={() => onHover(i)}
          onClick={() => onActivate(r)}
        >
          <span className="tile-icon">{renderIcon(r)}</span>
          <span className="tile-label">{r.title}</span>
          {r.kind === "account" && <span className="tile-sub">{r.context}</span>}
        </button>
      ))}
    </div>
  );
}

export function ProgramIcon({ program }: { program: NativeProgram }) {
  const [failed, setFailed] = useState(false);
  if (!program.icon_path || failed) {
    return (
      <span className="app-icon-fallback" aria-hidden="true">
        {program.name.charAt(0).toUpperCase() || "?"}
      </span>
    );
  }
  return (
    <img
      className="app-icon"
      src={convertFileSrc(program.icon_path)}
      alt=""
      loading="lazy"
      onError={() => setFailed(true)}
    />
  );
}

export function SearchBar({
  query,
  onQuery,
  onKeyDown,
  inputRef,
}: {
  query: string;
  onQuery: (q: string) => void;
  onKeyDown: (e: React.KeyboardEvent) => void;
  inputRef: React.RefObject<HTMLInputElement | null>;
}) {
  return (
    <div className="search-wrap">
      <input
        ref={inputRef}
        className="search-input"
        type="search"
        value={query}
        onChange={(e) => onQuery(e.target.value)}
        onKeyDown={onKeyDown}
        placeholder="Search apps, accounts, and programs…"
        aria-label="Search apps, accounts, and programs"
        autoComplete="off"
        spellCheck={false}
      />
      <span className="search-hint" aria-hidden="true">
        Arrow keys move · Enter opens · Esc clears, then hides
      </span>
    </div>
  );
}

/**
 * Launcher preferences: summon hotkey, run at startup, program rescan.
 * The hotkey change is validated live — if the new key can't be registered
 * (already taken), the old one stays and the error names the key.
 */
export function LauncherSettingsPanel({
  settings,
  onSaved,
  programsCount,
  onProgramsRefreshed,
  onError,
}: {
  settings: LauncherSettings;
  onSaved: (s: LauncherSettings) => void;
  programsCount: number;
  onProgramsRefreshed: (programs: NativeProgram[]) => void;
  onError: (msg: string) => void;
}) {
  const [hotkey, setHotkey] = useState(settings.hotkey);
  const [autostart, setAutostart] = useState(settings.autostart);
  const [saving, setSaving] = useState(false);
  const [rescanning, setRescanning] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  async function handleHotkeySave(e: React.FormEvent) {
    e.preventDefault();
    setFormError(null);
    setSaving(true);
    try {
      await invoke("set_hotkey", { hotkey: hotkey.trim() });
      const updated = await invoke<LauncherSettings>("get_launcher_settings");
      onSaved(updated);
      setHotkey(updated.hotkey);
    } catch (err) {
      const msg = errMsg(err);
      setFormError(msg);
      onError(msg);
    } finally {
      setSaving(false);
    }
  }

  async function handleAutostart(checked: boolean) {
    setFormError(null);
    try {
      await invoke("set_autostart", { enabled: checked });
      setAutostart(checked);
      onSaved({ ...settings, hotkey, autostart: checked });
    } catch (err) {
      const msg = errMsg(err);
      setFormError(msg);
      onError(msg);
    }
  }

  async function handleRescan() {
    setFormError(null);
    setRescanning(true);
    try {
      await invoke<number>("rescan_programs");
      const list = await invoke<NativeProgram[]>("list_programs");
      onProgramsRefreshed(list);
    } catch (err) {
      const msg = errMsg(err);
      setFormError(msg);
      onError(msg);
    } finally {
      setRescanning(false);
    }
  }

  return (
    <div className="launcher-settings">
      <form className="inline-form" onSubmit={(e) => void handleHotkeySave(e)}>
        <label>
          <span>Summon hotkey</span>
          <input
            value={hotkey}
            onChange={(e) => setHotkey(e.target.value)}
            placeholder="Alt+Space"
            maxLength={40}
            autoComplete="off"
            spellCheck={false}
          />
        </label>
        <button type="submit" disabled={saving}>
          {saving ? "Saving…" : "Set hotkey"}
        </button>
        <span className="help">
          Press it anywhere to show or hide AppForge. Examples: Alt+Space, Ctrl+Alt+A.
        </span>
      </form>

      <label className="check-row">
        <input
          type="checkbox"
          checked={autostart}
          onChange={(e) => void handleAutostart(e.target.checked)}
        />
        <span>Run AppForge when I sign in</span>
      </label>

      <div className="inline-form">
        <button type="button" onClick={() => void handleRescan()} disabled={rescanning}>
          {rescanning ? "Scanning…" : "Rescan programs"}
        </button>
        <span className="help">
          {programsCount} installed {programsCount === 1 ? "program" : "programs"} indexed from
          the Start Menu and Desktop. Microsoft Store apps aren't listed yet.
        </span>
      </div>

      {formError && (
        <p className="form-error" role="alert">
          {formError}
        </p>
      )}
    </div>
  );
}
