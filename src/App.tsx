import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

/**
 * Backend contract (implemented by the Rust side — do not extend).
 * All commands are invoked by their exact names with snake_case args.
 */
interface AppSettings {
  popup_policy: "block" | "allow";
  popup_allowlist: string[];
  adblock_enabled: boolean;
  auto_suspend_minutes: number;
}

interface Account {
  id: string;
  app_id: string;
  label: string;
  color: string;
  session_dir: string;
  last_opened: number;
  created_at: number;
}

interface WebApp {
  id: string;
  name: string;
  url: string;
  icon: string | null;
  color: string;
  settings: AppSettings;
  accounts: Account[];
  created_at: number;
}

interface PlatformInfo {
  os: string;
  network_adblock: boolean;
  filter_lists_loaded: boolean;
  filter_lists_updated_at: number | null;
}

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
  const src = !failed ? app.icon ?? faviconUrl(app.url) : null;
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
}: {
  account: Account;
  onOpen: () => void;
  onSuspend: () => void;
  onRemove: () => void;
}) {
  return (
    <li className="account-row">
      <span
        className="dot"
        aria-hidden="true"
        style={{ backgroundColor: safeColor(account.color) }}
      />
      <div className="account-meta">
        <span className="account-label">{account.label}</span>
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
        app_id: app.id,
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
  const [expanded, setExpanded] = useState<Set<string>>(new Set());
  const [settingsOpen, setSettingsOpen] = useState<Set<string>>(new Set());

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
      const [list, info] = await Promise.all([
        invoke<WebApp[]>("list_apps"),
        invoke<PlatformInfo>("platform_info"),
      ]);
      setApps(list.map((a) => ({ ...a, accounts: a.accounts ?? [], settings: a.settings ?? DEFAULT_SETTINGS })));
      setPlatform(info);
    } catch (err) {
      setError(errMsg(err) === "Something went wrong." ? "Could not load your apps." : errMsg(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

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
      const created = await invoke<WebApp>("add_app", {
        name: cleanName,
        url: cleanUrl,
      });
      setApps((prev) => [
        ...prev,
        { ...created, accounts: created.accounts ?? [], settings: created.settings ?? DEFAULT_SETTINGS },
      ]);
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

  async function handleOpenAccount(app: WebApp, account: Account) {
    setError(null);
    try {
      await invoke("open_account", { app_id: app.id, account_id: account.id });
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
    } catch (err) {
      setError(`Could not open "${account.label}". ${errMsg(err)}`);
    }
  }

  async function handleSuspendAccount(app: WebApp, account: Account) {
    setError(null);
    try {
      await invoke("suspend_account", { app_id: app.id, account_id: account.id });
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
      await invoke("remove_account", { app_id: app.id, account_id: account.id });
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

  return (
    <div className="shell">
      <header className="header">
        <h1>AppForge</h1>
        <p className="subtitle">Your web apps, each with its own isolated accounts.</p>
      </header>

      {error && (
        <div className="banner banner-error" role="alert">
          {error}
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

      <section className="panel">
        <h2>Add a web app</h2>
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
                      <span className="app-name">{app.name}</span>
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
