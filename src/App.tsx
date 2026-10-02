import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import "./App.css";
import {
  GRID_COLUMNS,
  IconGrid,
  LauncherSettingsPanel,
  ProgramIcon,
  SearchBar,
  browseAll,
  buildResults,
} from "./Launcher";
import type { SearchResult } from "./Launcher";
import type {
  Account,
  AddAppOutcome,
  AppSettings,
  LauncherSettings,
  NativeProgram,
  PlatformInfo,
  PreviewStart,
  WebApp,
} from "./types";

const DEFAULT_SETTINGS: AppSettings = {
  popup_policy: "block",
  popup_allowlist: [],
  adblock_enabled: true,
  auto_suspend_minutes: 30,
};

const DEFAULT_COLOR = "#64748b";

function isValidHexColor(s: string | null | undefined): s is string {
  return typeof s === "string" && /^#[0-9a-fA-F]{6}$/.test(s);
}

function safeColor(s: string | null | undefined): string {
  return isValidHexColor(s) ? s : DEFAULT_COLOR;
}

/**
 * Normalize a user-typed URL. Adds https:// when no scheme is present and
 * rejects anything that is not a valid http(s) URL. Returns null when invalid.
 */
function normalizeUrl(input: string): string | null {
  let s = input.trim();
  if (!s) return null;
  if (!/^[a-zA-Z][a-zA-Z0-9+.-]*:\/\//.test(s)) {
    s = "https://" + s;
  }
  try {
    const u = new URL(s);
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    if (!u.hostname) return null;
    return u.toString();
  } catch {
    return null;
  }
}

function faviconUrl(appUrl: string): string | null {
  try {
    return new URL("/favicon.ico", appUrl).toString();
  } catch {
    return null;
  }
}

function hostOf(appUrl: string): string {
  try {
    return new URL(appUrl).hostname;
  } catch {
    return appUrl;
  }
}

function errMsg(err: unknown): string {
  return typeof err === "string" ? err : "Something went wrong.";
}

/**
 * Inline rename: click the text to edit it, Enter to save, Esc to cancel,
 * clicking away saves too. Empty input reverts instead of saving blank.
 */
function InlineEdit({
  value,
  onSave,
  className,
  maxLength,
}: {
  value: string;
  onSave: (next: string) => void;
  className?: string;
  maxLength?: number;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);

  function start() {
    setDraft(value);
    setEditing(true);
  }

  function commit() {
    const clean = draft.trim();
    setEditing(false);
    if (clean && clean !== value) onSave(clean);
  }

  function cancel() {
    setEditing(false);
    setDraft(value);
  }

  if (!editing) {
    return (
      <span
        className={`inline-edit${className ? ` ${className}` : ""}`}
        onClick={start}
        title="Click to rename"
        role="button"
        tabIndex={0}
        onKeyDown={(e) => {
          if (e.key === "Enter") start();
        }}
      >
        {value}
      </span>
    );
  }
  return (
    <input
      className="inline-edit-input"
      value={draft}
      maxLength={maxLength ?? 80}
      autoFocus
      onFocus={(e) => e.target.select()}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={commit}
      onKeyDown={(e) => {
        if (e.key === "Enter") commit();
        else if (e.key === "Escape") cancel();
      }}
      aria-label="Rename"
    />
  );
}

/**
 * Quick-add: paste a URL, the app is named from the page title (best-effort;
 * falls back to a prettified domain when the title can't be fetched). The
 * manual name+URL form stays available as a secondary path in the library.
 */
function QuickAddForm({
  onAdded,
  onError,
}: {
  onAdded: (outcome: AddAppOutcome) => void;
  onError: (msg: string) => void;
}) {
  const [quickUrl, setQuickUrl] = useState("");
  const [quickAdding, setQuickAdding] = useState(false);
  const [quickError, setQuickError] = useState<string | null>(null);

  function prettifiedDomain(rawUrl: string): string {
    try {
      const host = new URL(rawUrl).hostname.replace(/^www\./, "");
      if (!host) return rawUrl;
      return host.charAt(0).toUpperCase() + host.slice(1);
    } catch {
      return rawUrl;
    }
  }

  async function handleQuickAdd(e: React.FormEvent) {
    e.preventDefault();
    setQuickError(null);
    const cleanUrl = normalizeUrl(quickUrl);
    if (!cleanUrl) {
      setQuickError("Enter a valid URL, e.g. https://example.com");
      return;
    }
    setQuickAdding(true);
    try {
      let name: string;
      try {
        const title = await invoke<string>("fetch_page_title", { url: cleanUrl });
        name = title.trim() || prettifiedDomain(cleanUrl);
      } catch {
        name = prettifiedDomain(cleanUrl);
      }
      const created = await invoke<AddAppOutcome>("add_app", { name, url: cleanUrl });
      onAdded(created);
      setQuickUrl("");
    } catch (err) {
      const msg = errMsg(err);
      setQuickError(msg);
      onError(msg);
    } finally {
      setQuickAdding(false);
    }
  }

  return (
    <form className="quick-add-form" onSubmit={(e) => void handleQuickAdd(e)}>
      <input
        value={quickUrl}
        onChange={(e) => setQuickUrl(e.target.value)}
        placeholder="Paste a website URL, e.g. https://mail.google.com"
        inputMode="url"
        autoComplete="off"
        aria-label="Website URL"
      />
      <button type="submit" disabled={quickAdding}>
        {quickAdding ? "Adding…" : "Add app"}
      </button>
      {quickError && (
        <p className="form-error" role="alert">
          {quickError}
        </p>
      )}
    </form>
  );
}

/**
 * Preview & sign in: for sites that need a login, open the real site in a
 * throwaway preview window. The user signs in there directly (we never touch
 * credentials), then clicks "Add as app" in the preview's header — the app
 * is created with its first account already signed in. "Discard" (or closing
 * the preview window) throws the session away.
 */
function PreviewSignInForm({ onError }: { onError: (msg: string) => void }) {
  const [open, setOpen] = useState(false);
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);
  const [previewOpen, setPreviewOpen] = useState(false);

  async function handleOpenPreview(e: React.FormEvent) {
    e.preventDefault();
    setFormError(null);
    const cleanUrl = normalizeUrl(url);
    if (!cleanUrl) {
      setFormError("Enter a valid URL, e.g. https://example.com");
      return;
    }
    setBusy(true);
    try {
      // Tauri exposes Rust snake_case params as camelCase to JS.
      await invoke<PreviewStart>("preview_start", { url: cleanUrl });
      setPreviewOpen(true);
      setUrl("");
    } catch (err) {
      const msg = errMsg(err);
      setFormError(msg);
      onError(msg);
    } finally {
      setBusy(false);
    }
  }

  if (!open) {
    return (
      <button className="text-button" onClick={() => setOpen(true)}>
        Preview &amp; sign in instead
      </button>
    );
  }

  return (
    <div className="preview-signin">
      <form className="quick-add-form" onSubmit={(e) => void handleOpenPreview(e)}>
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          placeholder="Paste a website URL to preview & sign in"
          inputMode="url"
          autoComplete="off"
          aria-label="Website URL to preview"
        />
        <button type="submit" disabled={busy}>
          {busy ? "Opening…" : "Open preview"}
        </button>
        {formError && (
          <p className="form-error" role="alert">
            {formError}
          </p>
        )}
      </form>
      {previewOpen && (
        <p className="muted small">
          Preview opened — sign in on the real site, then click “Add to the
          Forge” in its header. Nothing is kept until you do.
        </p>
      )}
      <p className="muted small">
        Best for sites that need a login. For sites that don’t, the quick add
        above is faster.
      </p>
    </div>
  );
}

/** "opened 5 minutes ago" / "opened 2 days ago" / "never opened" for last_opened (0 = never). */
function openedLabel(lastOpened: number): string {
  if (!lastOpened) return "never opened";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - lastOpened));
  if (seconds < 60) return "opened just now";
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `opened ${minutes} minute${minutes === 1 ? "" : "s"} ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `opened ${hours} hour${hours === 1 ? "" : "s"} ago`;
  const days = Math.floor(hours / 24);
  return `opened ${days} day${days === 1 ? "" : "s"} ago`;
}

function AppIcon({ app }: { app: WebApp }) {
  const [failed, setFailed] = useState(false);
  // `app.icon` is a locally cached logo file (absolute path); fall back to
  // the live /favicon.ico, then to a letter tile when all fetching fails.
  const src = !failed
    ? app.icon
      ? convertFileSrc(app.icon)
      : faviconUrl(app.url)
    : null;
  if (!src) {
    return (
      <span
        className="app-icon-fallback"
        aria-hidden="true"
        style={{ backgroundColor: `${safeColor(app.color)}22`, color: safeColor(app.color) }}
      >
        {app.name.charAt(0).toUpperCase() || "?"}
      </span>
    );
  }
  return (
    <img
      className="app-icon"
      src={src}
      alt=""
      loading="lazy"
      onError={() => setFailed(true)}
    />
  );
}

function AccountRow({
  account,
  onOpen,
  onSuspend,
  onRemove,
  onRename,
}: {
  account: Account;
  onOpen: () => void;
  onSuspend: () => void;
  onRemove: () => void;
  onRename: (label: string) => void;
}) {
  return (
    <li className="account-row">
      <span
        className="dot"
        aria-hidden="true"
        style={{ backgroundColor: safeColor(account.color) }}
      />
      <div className="account-meta">
        <InlineEdit
          value={account.label}
          onSave={onRename}
          className="account-label"
          maxLength={60}
        />
        <span className="account-opened">{openedLabel(account.last_opened)}</span>
      </div>
      <div className="app-actions">
        <button onClick={onOpen}>Open</button>
        <button onClick={onSuspend}>Suspend</button>
        <button className="danger" onClick={onRemove}>
          Remove
        </button>
      </div>
    </li>
  );
}

function AddAccountForm({
  app,
  onAdded,
  onError,
}: {
  app: WebApp;
  onAdded: (account: Account) => void;
  onError: (msg: string) => void;
}) {
  const [label, setLabel] = useState("");
  const [color, setColor] = useState(safeColor(app.color));
  const [adding, setAdding] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  async function handleAdd(e: React.FormEvent) {
    e.preventDefault();
    setFormError(null);
    const clean = label.trim();
    if (!clean) {
      setFormError("Give the account a label.");
      return;
    }
    setAdding(true);
    try {
      const account = await invoke<Account>("add_account", {
        // Tauri exposes Rust snake_case params as camelCase to JS.
        appId: app.id,
        label: clean,
        color: isValidHexColor(color) ? color : null,
      });
      onAdded(account);
      setLabel("");
    } catch (err) {
      const msg = errMsg(err);
      setFormError(msg);
      onError(msg);
    } finally {
      setAdding(false);
    }
  }

  return (
    <form className="inline-form" onSubmit={(e) => void handleAdd(e)}>
      <label>
        <span>New account label</span>
        <input
          value={label}
          onChange={(e) => setLabel(e.target.value)}
          placeholder="e.g. Work, Personal"
          maxLength={60}
          autoComplete="off"
        />
      </label>
      <label className="color-label">
        <span>Color</span>
        <input
          type="color"
          value={safeColor(color)}
          onChange={(e) => setColor(e.target.value)}
          aria-label="Account color"
        />
      </label>
      <button type="submit" disabled={adding}>
        {adding ? "Adding…" : "Add account"}
      </button>
      {formError && (
        <p className="form-error" role="alert">
          {formError}
        </p>
      )}
    </form>
  );
}

function adblockStateLabel(adblockEnabled: boolean, networkAdblock: boolean): string {
  if (!adblockEnabled) return "Ad blocking: off";
  if (networkAdblock) return "Ad blocking: on — network + cosmetic";
  return "Ad blocking: on — cosmetic only (network blocking is Windows-only)";
}

function AppSettingsForm({
  app,
  networkAdblock,
  onSaved,
  onError,
}: {
  app: WebApp;
  networkAdblock: boolean;
  onSaved: (app: WebApp) => void;
  onError: (msg: string) => void;
}) {
  const settings = app.settings ?? DEFAULT_SETTINGS;
  const [name, setName] = useState(app.name);
  const [url, setUrl] = useState(app.url);
  const [color, setColor] = useState(safeColor(app.color));
  const [popupPolicy, setPopupPolicy] = useState<"block" | "allow">(settings.popup_policy);
  const [allowlist, setAllowlist] = useState(settings.popup_allowlist.join("\n"));
  const [adblockEnabled, setAdblockEnabled] = useState(settings.adblock_enabled);
  const [suspendMinutes, setSuspendMinutes] = useState(String(settings.auto_suspend_minutes));
  const [saving, setSaving] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  async function handleSave(e: React.FormEvent) {
    e.preventDefault();
    setFormError(null);
    const cleanName = name.trim();
    if (!cleanName) {
      setFormError("Give the app a name.");
      return;
    }
    const cleanUrl = normalizeUrl(url);
    if (!cleanUrl) {
      setFormError("Enter a valid URL, e.g. https://example.com");
      return;
    }
    const minutes = Math.max(0, parseInt(suspendMinutes, 10) || 0);
    const newSettings: AppSettings = {
      popup_policy: popupPolicy,
      popup_allowlist: allowlist
        .split("\n")
        .map((s) => s.trim().toLowerCase())
        .filter(Boolean),
      adblock_enabled: adblockEnabled,
      auto_suspend_minutes: minutes,
    };
    setSaving(true);
    try {
      const updated = await invoke<WebApp>("update_app", {
        id: app.id,
        name: cleanName,
        url: cleanUrl,
        color: safeColor(color),
      });
      const savedSettings = await invoke<AppSettings>("update_app_settings", {
        id: app.id,
        settings: newSettings,
      });
      onSaved({ ...updated, settings: savedSettings });
    } catch (err) {
      const msg = errMsg(err);
      setFormError(msg);
      onError(msg);
    } finally {
      setSaving(false);
    }
  }

  return (
    <form className="settings-form" onSubmit={(e) => void handleSave(e)}>
      <h3>Settings</h3>
      <label>
        <span>Name</span>
        <input
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={80}
          autoComplete="off"
        />
      </label>
      <label>
        <span>URL</span>
        <input
          value={url}
          onChange={(e) => setUrl(e.target.value)}
          inputMode="url"
          autoComplete="off"
        />
      </label>
      <label className="color-label">
        <span>Color</span>
        <input
          type="color"
          value={safeColor(color)}
          onChange={(e) => setColor(e.target.value)}
          aria-label="App color"
        />
      </label>
      <label>
        <span>Popups</span>
        <select
          value={popupPolicy}
          onChange={(e) => setPopupPolicy(e.target.value === "allow" ? "allow" : "block")}
        >
          <option value="block">Block all popups</option>
          <option value="allow">Allow popups as contained windows</option>
        </select>
      </label>
      <label>
        <span>Popup allowlist</span>
        <textarea
          value={allowlist}
          onChange={(e) => setAllowlist(e.target.value)}
          rows={3}
          placeholder="accounts.google.com"
          spellCheck={false}
          autoComplete="off"
        />
        <span className="help">
          Sites allowed to open sign-in popups even when blocking, e.g. accounts.google.com.
          One hostname per line.
        </span>
      </label>
      <label className="check-row">
        <input
          type="checkbox"
          checked={adblockEnabled}
          onChange={(e) => setAdblockEnabled(e.target.checked)}
        />
        <span>{adblockStateLabel(adblockEnabled, networkAdblock)}</span>
      </label>
      <label>
        <span>Auto-suspend idle windows (minutes)</span>
        <input
          type="number"
          min={0}
          step={1}
          value={suspendMinutes}
          onChange={(e) => setSuspendMinutes(e.target.value)}
        />
        <span className="help">Idle account windows are suspended to save RAM. 0 = never suspend.</span>
      </label>
      <div className="form-actions">
        <button type="submit" disabled={saving}>
          {saving ? "Saving…" : "Save settings"}
        </button>
      </div>
      {formError && (
        <p className="form-error" role="alert">
          {formError}
        </p>
      )}
    </form>
  );
}

export default function App() {
  const [apps, setApps] = useState<WebApp[]>([]);
  const [platform, setPlatform] = useState<PlatformInfo | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [settingsOpen, setSettingsOpen] = useState<Set<string>>(new Set());

  // Launcher: hotkey-summoned search over apps, accounts, and programs.
  const [programs, setPrograms] = useState<NativeProgram[]>([]);
  const [launcherSettings, setLauncherSettings] = useState<LauncherSettings | null>(null);
  const [launcherPanelOpen, setLauncherPanelOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const searchRef = useRef<HTMLInputElement | null>(null);

  // Two views: the hotkey-summoned spotlight overlay ("launcher") and the
  // full management window ("library"). The hotkey always lands on the
  // launcher; the tray menu and the in-overlay button open the library.
  const [view, setView] = useState<"launcher" | "library">("launcher");
  // Bumped on every hotkey summon so the panel entrance animation replays
  // (the React tree stays mounted while the window just hides/shows).
  const [summonCount, setSummonCount] = useState(0);

  // Add-app form
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [color, setColor] = useState(DEFAULT_COLOR);
  const [adding, setAdding] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [list, info, progList, launchSettings] = await Promise.all([
        invoke<WebApp[]>("list_apps"),
        invoke<PlatformInfo>("platform_info"),
        invoke<NativeProgram[]>("list_programs"),
        invoke<LauncherSettings>("get_launcher_settings"),
      ]);
      setApps(list.map((a) => ({ ...a, accounts: a.accounts ?? [], settings: a.settings ?? DEFAULT_SETTINGS })));
      setPlatform(info);
      setPrograms(progList);
      setLauncherSettings(launchSettings);
      // Backfill logos for apps added before icon caching existed (or where
      // the fetch failed last time). Best-effort, in the background.
      for (const a of list) {
        if (!a.icon) void refreshAppIcon(a.id);
      }
    } catch (err) {
      setError(errMsg(err) === "Something went wrong." ? "Could not load your apps." : errMsg(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // The program scan runs in the background at startup; pick up its results
  // a few seconds later so search finds everything.
  useEffect(() => {
    const t = setTimeout(() => {
      invoke<NativeProgram[]>("list_programs")
        .then(setPrograms)
        .catch(() => {});
    }, 8000);
    return () => clearTimeout(t);
  }, []);

  // Notices ("already in your library") are transient — clear after a while
  // so a stale one can't confuse a later action.
  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => setNotice(null), 9000);
    return () => clearTimeout(t);
  }, [notice]);

  // Focus the search box every time the window is summoned.
  useEffect(() => {
    let unlisten: (() => void) | undefined;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (focused) {
          searchRef.current?.focus();
          searchRef.current?.select();
        }
      })
      .then((u) => {
        unlisten = u;
      })
      .catch(() => {});
    return () => unlisten?.();
  }, []);

  // The panel remounts on every summon (key bump replays the entrance
  // animation) — focus the fresh input after each remount, since the
  // window-focus event can race the remount.
  useEffect(() => {
    searchRef.current?.focus();
    searchRef.current?.select();
  }, [summonCount]);

  // View-switch events from the backend: tray "Show library" and the hotkey
  // summon (which always resets to the spotlight view).
  useEffect(() => {
    let offLibrary: (() => void) | undefined;
    let offLauncher: (() => void) | undefined;
    listen("appforge:show-library", () => setView("library"))
      .then((off) => {
        offLibrary = off;
      })
      .catch(() => {});
    listen("appforge:show-launcher", () => {
      setView("launcher");
      setSummonCount((c) => c + 1);
    })
      .then((off) => {
        offLauncher = off;
      })
      .catch(() => {});
    return () => {
      offLibrary?.();
      offLauncher?.();
    };
  }, []);

  // A preview that was added as an app: pick it up in the library and open
  // its first account, which carries the session the user signed in to.
  // If the site was already in the library (no duplicate was created),
  // reveal the existing entry instead.
  useEffect(() => {
    let off: (() => void) | undefined;
    listen<AddAppOutcome>("appforge:preview-added", (event) => {
      const created = {
        ...event.payload.app,
        accounts: event.payload.app.accounts ?? [],
        settings: event.payload.app.settings ?? DEFAULT_SETTINGS,
      };
      if (!event.payload.created) {
        revealApp(created, `"${created.name}" is already in your library — showing it instead of adding a duplicate.`);
        return;
      }
      setApps((prev) => [...prev, created]);
      const first = created.accounts[0];
      if (first) {
        setError(null);
        invoke("open_account", { appId: created.id, accountId: first.id }).catch(
          (err) => setError(`Could not open "${created.name}". ${errMsg(err)}`)
        );
      }
      // The new tile gets its logo in the background.
      void refreshAppIcon(created.id);
    })
      .then((unlisten) => {
        off = unlisten;
      })
      .catch(() => {});
    return () => off?.();
  }, []);

  // Window chrome per view: the launcher is a small frameless spotlight
  // overlay; the library is a full window. Best-effort — if the window
  // manager refuses, the window still works.
  useEffect(() => {
    const win = getCurrentWindow();
    // The main window is transparent-capable (tauri.conf.json). The
    // launcher overlay must leave the canvas unpainted so the desktop shows
    // through the frosted panel; the library view keeps the normal opaque
    // background from styles.css.
    document.documentElement.style.background =
      view === "launcher" ? "transparent" : "";
    void (async () => {
      try {
        if (view === "launcher") {
          await win.setSize(new LogicalSize(680, 480));
          await win.setDecorations(false);
        } else {
          await win.setSize(new LogicalSize(1020, 720));
          await win.setDecorations(true);
        }
        await win.center();
      } catch {
        /* non-fatal */
      }
    })();
  }, [view]);

  function toggleExpanded(id: string) {
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function toggleSettings(id: string) {
    setSettingsOpen((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }

  function replaceApp(updated: WebApp) {
    setApps((prev) => prev.map((a) => (a.id === updated.id ? { ...updated, accounts: updated.accounts ?? [], settings: updated.settings ?? DEFAULT_SETTINGS } : a)));
  }

  /**
   * Reveal an already-existing app instead of creating a duplicate: make
   * sure it's in the list, expand its card, and say what's happening.
   */
  function revealApp(app: WebApp, message: string) {
    const full = { ...app, accounts: app.accounts ?? [], settings: app.settings ?? DEFAULT_SETTINGS };
    setApps((prev) => (prev.some((a) => a.id === full.id) ? prev : [...prev, full]));
    setExpanded((prev) => {
      const next = new Set(prev);
      next.add(full.id);
      return next;
    });
    setError(null);
    setNotice(message);
  }

  async function handleAdd(e: React.FormEvent) {
    e.preventDefault();
    setFormError(null);
    const cleanName = name.trim();
    if (!cleanName) {
      setFormError("Give the app a name.");
      return;
    }
    const cleanUrl = normalizeUrl(url);
    if (!cleanUrl) {
      setFormError("Enter a valid URL, e.g. https://example.com");
      return;
    }
    setAdding(true);
    try {
      const outcome = await invoke<AddAppOutcome>("add_app", {
        name: cleanName,
        url: cleanUrl,
      });
      if (!outcome.created) {
        revealApp(outcome.app, `"${outcome.app.name}" is already in your library — showing it instead of adding a duplicate.`);
      } else {
        const created = outcome.app;
        setNotice(null);
        setApps((prev) => [
          ...prev,
          { ...created, accounts: created.accounts ?? [], settings: created.settings ?? DEFAULT_SETTINGS },
        ]);
      }
      setName("");
      setUrl("");
      setColor(DEFAULT_COLOR);
    } catch (err) {
      setFormError(errMsg(err));
    } finally {
      setAdding(false);
    }
  }

  async function handleRemoveApp(app: WebApp) {
    const n = app.accounts.length;
    const accountWord = n === 1 ? "its 1 account" : `all ${n} of its accounts`;
    if (
      !window.confirm(
        `Remove "${app.name}" and ${accountWord}? This deletes their local session data (cookies, logins, cache) on this PC. It does not delete anything on the sites' servers.`
      )
    ) {
      return;
    }
    setError(null);
    try {
      await invoke("remove_app", { id: app.id });
      setApps((prev) => prev.filter((a) => a.id !== app.id));
      setExpanded((prev) => {
        const next = new Set(prev);
        next.delete(app.id);
        return next;
      });
    } catch (err) {
      setError(errMsg(err));
    }
  }

  async function handleOpenAccount(app: WebApp, account: Account): Promise<boolean> {
    setError(null);
    try {
      await invoke("open_account", { appId: app.id, accountId: account.id });
      // Refresh last_opened display.
      const now = Math.floor(Date.now() / 1000);
      setApps((prev) =>
        prev.map((a) =>
          a.id === app.id
            ? {
                ...a,
                accounts: a.accounts.map((acc) =>
                  acc.id === account.id ? { ...acc, last_opened: now } : acc
                ),
              }
            : a
        )
      );
      return true;
    } catch (err) {
      setError(`Could not open "${account.label}". ${errMsg(err)}`);
      return false;
    }
  }

  async function handleSuspendAccount(app: WebApp, account: Account) {
    setError(null);
    try {
      await invoke("suspend_account", { appId: app.id, accountId: account.id });
    } catch (err) {
      setError(`Could not suspend "${account.label}". ${errMsg(err)}`);
    }
  }

  async function handleRemoveAccount(app: WebApp, account: Account) {
    if (
      !window.confirm(
        `Remove account "${account.label}"? This deletes its local session data (cookies, logins, cache) on this PC. It does not delete anything on the site's servers.`
      )
    ) {
      return;
    }
    setError(null);
    try {
      await invoke("remove_account", { appId: app.id, accountId: account.id });
      setApps((prev) =>
        prev.map((a) =>
          a.id === app.id
            ? { ...a, accounts: a.accounts.filter((acc) => acc.id !== account.id) }
            : a
        )
      );
    } catch (err) {
      setError(errMsg(err));
    }
  }

  async function handleRenameApp(app: WebApp, name: string) {
    setError(null);
    try {
      // Tauri exposes Rust snake_case params as camelCase to JS.
      const updated = await invoke<WebApp>("rename_app", { id: app.id, name });
      replaceApp(updated);
    } catch (err) {
      setError(errMsg(err));
    }
  }

  async function handleRenameAccount(app: WebApp, account: Account, label: string) {
    setError(null);
    try {
      const updated = await invoke<Account>("rename_account", {
        appId: app.id,
        accountId: account.id,
        label,
      });
      setApps((prev) =>
        prev.map((a) =>
          a.id === app.id
            ? {
                ...a,
                accounts: a.accounts.map((acc) =>
                  acc.id === account.id ? { ...acc, label: updated.label } : acc
                ),
              }
            : a
        )
      );
    } catch (err) {
      setError(errMsg(err));
    }
  }

  /**
   * Best-effort logo fetch: ask the backend to download the site's icon and
   * cache it locally, then paint it onto the app's tiles. Failures are
   * silent by design — the tiles keep their fallbacks.
   */
  const refreshAppIcon = useCallback(async (appId: string) => {
    try {
      const icon = await invoke<string | null>("fetch_favicon", { appId });
      if (icon) {
        setApps((prev) =>
          prev.map((a) => (a.id === appId ? { ...a, icon } : a))
        );
      }
    } catch {
      /* best-effort: keep the fallback logo */
    }
  }, []);

  // --- launcher search -------------------------------------------------

  // Empty query shows the whole phone-folder grid; typing filters it with
  // the fuzzy matcher.
  const items = useMemo(
    () => (query.trim() ? buildResults(query, apps, programs) : browseAll(apps, programs)),
    [query, apps, programs]
  );

  useEffect(() => {
    setActiveIndex(0);
  }, [query]);

  // Keep the highlight inside the list when the items change underneath it
  // (e.g. apps finishing loading while the overlay is open).
  useEffect(() => {
    setActiveIndex((i) => Math.min(i, Math.max(0, items.length - 1)));
  }, [items]);

  // Keep the highlighted tile visible while arrow-keying through the grid.
  useEffect(() => {
    document
      .querySelector(".icon-tile.is-active")
      ?.scrollIntoView({ block: "nearest" });
  }, [activeIndex]);

  function mostRecentAccount(app: WebApp): Account | undefined {
    return [...app.accounts].sort((a, b) => b.last_opened - a.last_opened)[0];
  }

  async function activateResult(r: SearchResult) {
    setError(null);
    try {
      if (r.kind === "program") {
        await invoke("launch_program", { id: r.program.id });
      } else if (r.kind === "account") {
        const ok = await handleOpenAccount(r.app, r.account);
        if (!ok) return;
      } else {
        // Web app row: open the most recently used account.
        const acct = mostRecentAccount(r.app) ?? r.app.accounts[0];
        if (!acct) return;
        const ok = await handleOpenAccount(r.app, acct);
        if (!ok) return;
      }
      // Spotlight behavior: a successful activation dismisses the overlay.
      setQuery("");
      await invoke("hide_library");
    } catch (err) {
      setError(errMsg(err));
    }
  }

  function onSearchKeyDown(e: React.KeyboardEvent) {
    const last = items.length - 1;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setActiveIndex((i) => Math.min(i + GRID_COLUMNS, last));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setActiveIndex((i) => Math.max(i - GRID_COLUMNS, 0));
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      setActiveIndex((i) => Math.min(i + 1, last));
    } else if (e.key === "ArrowLeft") {
      e.preventDefault();
      setActiveIndex((i) => Math.max(i - 1, 0));
    } else if (e.key === "Enter") {
      const r = items[activeIndex];
      if (r) void activateResult(r);
    } else if (e.key === "Escape") {
      if (query) {
        setQuery("");
      } else {
        void invoke("hide_library").catch((err) => setError(errMsg(err)));
      }
    }
  }

  function renderResultIcon(r: SearchResult): React.ReactNode {
    if (r.kind === "program") return <ProgramIcon program={r.program} />;
    return <AppIcon app={r.app} />;
  }

  const banners = (
    <>
      {error && (
        <div className="banner banner-error" role="alert">
          {error}
        </div>
      )}
      {notice && (
        <div className="banner banner-info" role="status">
          {notice}
        </div>
      )}
      {platform && !platform.network_adblock && (
        <div className="banner banner-info" role="status">
          Network-level ad blocking is Windows-only in this build. On this device you still
          get popup blocking and cosmetic ad hiding.
        </div>
      )}
      {platform && platform.network_adblock && !platform.filter_lists_loaded && (
        <div className="banner banner-info" role="status">
          Ad filter lists are still downloading — blocking starts automatically when ready.
        </div>
      )}
    </>
  );

  const addCreatedApp = (outcome: AddAppOutcome) => {
    if (!outcome.created) {
      revealApp(outcome.app, `"${outcome.app.name}" is already in your library — showing it instead of adding a duplicate.`);
      return;
    }
    const created = outcome.app;
    setNotice(null);
    setApps((prev) => [
      ...prev,
      { ...created, accounts: created.accounts ?? [], settings: created.settings ?? DEFAULT_SETTINGS },
    ]);
    // Fetch the site's logo in the background so the new tile gets its icon.
    void refreshAppIcon(created.id);
  };

  // Phone-folder overlay: search field on top, grid of app icons below.
  // Management lives one click away in the library view.
  if (view === "launcher") {
    return (
      <div className="launcher-shell">
        <div className="folder-panel" key={summonCount}>
          <SearchBar
            query={query}
            onQuery={setQuery}
            onKeyDown={onSearchKeyDown}
            inputRef={searchRef}
          />
          {banners}
          <div className="folder-grid-wrap">
            {loading ? (
              <p className="muted folder-hint">Loading…</p>
            ) : items.length === 0 && query.trim() ? (
              <p className="muted folder-hint">No matches.</p>
            ) : items.length === 0 ? (
              <p className="muted folder-hint">
                Nothing here yet — add your first web app from Manage apps below.
              </p>
            ) : (
              <IconGrid
                items={items}
                activeIndex={activeIndex}
                onHover={setActiveIndex}
                onActivate={(r) => void activateResult(r)}
                renderIcon={renderResultIcon}
              />
            )}
          </div>
          <footer className="launcher-foot">
            <button className="text-button" onClick={() => setView("library")}>
              Manage apps →
            </button>
            <span className="muted small">
              {apps.length} web apps · {programs.length} programs
            </span>
          </footer>
        </div>
      </div>
    );
  }

  return (
    <div className="shell">
      <button className="text-button library-back" onClick={() => setView("launcher")}>
        ← Launcher
      </button>
      <header className="header">
        <h1>AppForge</h1>
        <p className="subtitle">Your web apps, each with its own isolated accounts.</p>
      </header>

      {banners}

      <section className="panel">
        <h2>Add a web app</h2>
        <QuickAddForm onAdded={addCreatedApp} onError={setError} />
        <PreviewSignInForm onError={setError} />
        <details className="manual-add">
          <summary>Add manually instead</summary>
          <form className="add-form" onSubmit={(e) => void handleAdd(e)}>
            <label>
              <span>Name</span>
              <input
                value={name}
                onChange={(e) => setName(e.target.value)}
                placeholder="e.g. Gmail"
                maxLength={80}
                autoComplete="off"
              />
            </label>
            <label>
              <span>URL</span>
              <input
                value={url}
                onChange={(e) => setUrl(e.target.value)}
                placeholder="https://mail.google.com"
                inputMode="url"
                autoComplete="off"
              />
            </label>
            <label className="color-label">
              <span>Color</span>
              <input
                type="color"
                value={safeColor(color)}
                onChange={(e) => setColor(e.target.value)}
                aria-label="App color"
              />
            </label>
            <button type="submit" disabled={adding}>
              {adding ? "Adding…" : "Add app"}
            </button>
          </form>
          {formError && (
            <p className="form-error" role="alert">
              {formError}
            </p>
          )}
        </details>
      </section>

      <section className="panel">
        <h2>Library</h2>
        {loading ? (
          <p className="muted">Loading…</p>
        ) : apps.length === 0 ? (
          <p className="muted">No apps yet. Add your first web app above.</p>
        ) : (
          <ul className="app-grid">
            {apps.map((app) => {
              const isExpanded = expanded.has(app.id);
              const showSettings = settingsOpen.has(app.id);
              return (
                <li key={app.id} className={`app-card${isExpanded ? " is-expanded" : ""}`}>
                  <div className="card-head">
                    <AppIcon app={app} />
                    <div className="app-meta">
                      <InlineEdit
                        value={app.name}
                        onSave={(name) => void handleRenameApp(app, name)}
                        className="app-name"
                        maxLength={80}
                      />
                      <span className="app-url" title={app.url}>
                        {hostOf(app.url)}
                      </span>
                    </div>
                    <span
                      className="color-badge"
                      aria-label="App color"
                      title="App color"
                      style={{ backgroundColor: safeColor(app.color) }}
                    />
                    <div className="app-actions">
                      <button
                        className="text-button"
                        onClick={() => toggleExpanded(app.id)}
                        aria-expanded={isExpanded}
                      >
                        {isExpanded
                          ? "Hide"
                          : `Accounts (${app.accounts.length})`}
                      </button>
                      <button className="danger" onClick={() => void handleRemoveApp(app)}>
                        Remove
                      </button>
                    </div>
                  </div>

                  {isExpanded && (
                    <div className="card-body">
                      {app.accounts.length === 0 ? (
                        <p className="muted small">No accounts yet.</p>
                      ) : (
                        <ul className="account-list">
                          {app.accounts.map((account) => (
                            <AccountRow
                              key={account.id}
                              account={account}
                              onOpen={() => void handleOpenAccount(app, account)}
                              onSuspend={() => void handleSuspendAccount(app, account)}
                              onRemove={() => void handleRemoveAccount(app, account)}
                              onRename={(label) => void handleRenameAccount(app, account, label)}
                            />
                          ))}
                        </ul>
                      )}

                      <AddAccountForm
                        app={app}
                        onAdded={(account) =>
                          setApps((prev) =>
                            prev.map((a) =>
                              a.id === app.id ? { ...a, accounts: [...a.accounts, account] } : a
                            )
                          )
                        }
                        onError={setError}
                      />

                      <button
                        className="text-button settings-toggle"
                        onClick={() => toggleSettings(app.id)}
                        aria-expanded={showSettings}
                      >
                        {showSettings ? "Hide settings" : "Settings"}
                      </button>

                      {showSettings && (
                        <AppSettingsForm
                          app={app}
                          networkAdblock={platform?.network_adblock ?? false}
                          onSaved={replaceApp}
                          onError={setError}
                        />
                      )}
                    </div>
                  )}
                </li>
              );
            })}
          </ul>
        )}
      </section>

      <section className="panel">
        <div className="panel-head">
          <h2>Launcher</h2>
          <button
            className="text-button"
            onClick={() => setLauncherPanelOpen((v) => !v)}
            aria-expanded={launcherPanelOpen}
          >
            {launcherPanelOpen ? "Hide" : "Show"}
          </button>
        </div>
        {launcherPanelOpen &&
          (launcherSettings ? (
            <LauncherSettingsPanel
              settings={launcherSettings}
              onSaved={setLauncherSettings}
              programsCount={programs.length}
              onProgramsRefreshed={setPrograms}
              onError={setError}
            />
          ) : (
            <p className="muted">Loading…</p>
          ))}
      </section>

      <footer className="footer muted">
        Accounts are isolated browser sessions stored on this PC. Network ad blocking:{" "}
        {platform
          ? platform.network_adblock
            ? "available"
            : "Windows only"
          : "checking…"}
        .
      </footer>
    </div>
  );
}
