import { useEffect, useMemo, useRef, useState } from "react";
import ReactDOM from "react-dom/client";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * Clipboard history popup (v0.9.0, text only).
 *
 * Summoned by the global hotkey; the backend builds this window on a
 * dedicated thread. Search filters the history, Enter/click copies the
 * entry back to the OS clipboard and closes the popup (v1 does NOT
 * synthesize Ctrl+V — the user pastes normally). Esc or focus loss closes.
 *
 * The list refreshes on mount and then polls every second while open —
 * a push event from the backend proved unreliable for secondary windows,
 * and a 1s poll of a tiny local list is cheap.
 *
 * Bundled separately from the main app (vite.config.ts "clipboard" input,
 * loaded by the backend as clipboard.html). The window.__TAURI__ global is
 * never injected (withGlobalTauri stays false); only the bundled
 * @tauri-apps/api imports are used.
 */

interface ClipboardListEntry {
  id: string;
  preview: string;
  chars: number;
  truncated: boolean;
  createdAtMs: number;
}

function timeAgo(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 10) return "just now";
  if (s < 60) return `${s}s ago`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function ClipboardPopup() {
  const [entries, setEntries] = useState<ClipboardListEntry[]>([]);
  const [query, setQuery] = useState("");
  const [selected, setSelected] = useState(0);
  const [error, setError] = useState<string | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const shownAt = useRef(Date.now());

  const refresh = async () => {
    try {
      const list = await invoke<ClipboardListEntry[]>("list_clipboard");
      setEntries(list);
      setError(null);
    } catch (err) {
      setError(typeof err === "string" ? err : "Something went wrong.");
    }
  };

  // Hide the popup and reset the search box so the next summon starts fresh.
  // Hiding goes through a backend command: the Rust-side hide is the path
  // proven to work for this window.
  const hide = () => {
    setQuery("");
    void invoke("hide_clipboard_popup").catch(() => {});
  };

  useEffect(() => {
    shownAt.current = Date.now();
    setQuery("");
    setSelected(0);
    void refresh();
    // Poll while open: a push event from the backend proved unreliable for
    // secondary windows, and one tiny invoke per second is cheap.
    const timer = window.setInterval(() => {
      void refresh();
    }, 1000);
    // Hide when focus moves elsewhere (with a grace period so the
    // show->focus race can't instantly dismiss the popup). On focus,
    // reset the search box and refresh — copies made while the popup
    // was hidden show up immediately.
    const focusPromise = getCurrentWindow().onFocusChanged(({ payload }) => {
      if (payload) {
        shownAt.current = Date.now();
        setQuery("");
        setSelected(0);
        inputRef.current?.focus();
        inputRef.current?.select();
        void refresh();
      } else if (Date.now() - shownAt.current > 500) {
        hide();
      }
    });
    // Focus the search box whenever the popup is summoned.
    inputRef.current?.focus();
    return () => {
      window.clearInterval(timer);
      void focusPromise.then((u) => u());
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    if (!q) return entries;
    return entries.filter((e) => e.preview.toLowerCase().includes(q));
  }, [entries, query]);

  useEffect(() => {
    setSelected(0);
  }, [query]);

  async function choose(entry: ClipboardListEntry | undefined) {
    if (!entry) return;
    try {
      await invoke("copy_clipboard_entry", { entryId: entry.id });
    } catch {
      // Even if the copy failed, closing is the honest outcome — the
      // error would otherwise strand the user on a dead popup.
    }
    hide();
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      hide();
    } else if (e.key === "ArrowDown") {
      e.preventDefault();
      setSelected((s) => Math.min(s + 1, filtered.length - 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSelected((s) => Math.max(s - 1, 0));
    } else if (e.key === "Enter") {
      e.preventDefault();
      void choose(filtered[selected]);
    }
  }

  return (
    <>
      <div className="clip-search">
        <input
          ref={inputRef}
          type="text"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onKeyDown}
          placeholder="Search clipboard history…"
          aria-label="Search clipboard history"
          autoComplete="off"
          spellCheck={false}
        />
      </div>
      {error ? (
        <div className="clip-error" role="alert">
          {error}
        </div>
      ) : filtered.length === 0 ? (
        <div className="clip-empty">
          {entries.length === 0 ? (
            <>
              Nothing copied yet.
              <br />
              Copy some text and it will show up here.
            </>
          ) : (
            <>No matches for "{query}".</>
          )}
        </div>
      ) : (
        <ul className="clip-list" role="listbox" aria-label="Clipboard history">
          {filtered.map((entry, i) => (
            <li key={entry.id}>
              <button
                type="button"
                role="option"
                aria-selected={i === selected}
                className={
                  "clip-item" + (i === selected ? " selected" : "")
                }
                onClick={() => void choose(entry)}
                onMouseEnter={() => setSelected(i)}
              >
                <span className="text">{entry.preview}</span>
                <span className="meta">
                  {timeAgo(entry.createdAtMs)} ·{" "}
                  {entry.chars === 1 ? "1 char" : `${entry.chars} chars`}
                  {entry.truncated ? " · preview" : ""}
                </span>
              </button>
            </li>
          ))}
        </ul>
      )}
      <div className="clip-foot">
        <span>
          <b>Enter</b> copies
        </span>
        <span>
          <b>Esc</b> closes
        </span>
      </div>
    </>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <ClipboardPopup />
);
