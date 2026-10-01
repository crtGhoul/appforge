import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import "./App.css";

interface WebApp {
  id: string;
  name: string;
  url: string;
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

function AppIcon({ app }: { app: WebApp }) {
  const [failed, setFailed] = useState(false);
  const src = faviconUrl(app.url);
  if (!src || failed) {
    return (
      <span className="app-icon-fallback" aria-hidden="true">
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

export default function App() {
  const [apps, setApps] = useState<WebApp[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [adding, setAdding] = useState(false);
  const [formError, setFormError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await invoke<WebApp[]>("list_apps");
      setApps(list);
    } catch (err) {
      setError(typeof err === "string" ? err : "Could not load your apps.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

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
      setApps((prev) => [...prev, created]);
      setName("");
      setUrl("");
    } catch (err) {
      setFormError(typeof err === "string" ? err : "Could not add the app.");
    } finally {
      setAdding(false);
    }
  }

  async function handleRemove(id: string) {
    const target = apps.find((a) => a.id === id);
    if (
      !window.confirm(`Remove "${target?.name ?? "this app"}" from your library?`)
    ) {
      return;
    }
    setError(null);
    try {
      await invoke("remove_app", { id });
      setApps((prev) => prev.filter((a) => a.id !== id));
    } catch (err) {
      setError(typeof err === "string" ? err : "Could not remove the app.");
    }
  }

  async function handleOpen(app: WebApp) {
    setError(null);
    try {
      const label = `app-${app.id}`;
      const existing = await WebviewWindow.getByLabel(label);
      if (existing) {
        await existing.setFocus();
        return;
      }
      const win = new WebviewWindow(label, {
        url: app.url,
        title: app.name,
        width: 1200,
        height: 800,
        center: true,
      });
      void win.once("tauri://error", () => {
        setError(`Could not open "${app.name}".`);
      });
    } catch {
      setError(`Could not open "${app.name}".`);
    }
  }

  return (
    <div className="shell">
      <header className="header">
        <h1>AppForge</h1>
        <p className="subtitle">Your web apps, in one place.</p>
      </header>

      {error && (
        <div className="banner banner-error" role="alert">
          {error}
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
            {apps.map((app) => (
              <li key={app.id} className="app-card">
                <AppIcon app={app} />
                <div className="app-meta">
                  <span className="app-name">{app.name}</span>
                  <span className="app-url" title={app.url}>
                    {hostOf(app.url)}
                  </span>
                </div>
                <div className="app-actions">
                  <button onClick={() => void handleOpen(app)}>Open</button>
                  <button
                    className="danger"
                    onClick={() => void handleRemove(app.id)}
                  >
                    Remove
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}
      </section>

      <footer className="footer muted">
        v0 shell — multiple accounts, popup blocking, and ad blocking are not in
        this build yet.
      </footer>
    </div>
  );
}
