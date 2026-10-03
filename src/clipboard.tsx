import { useEffect, useMemo, useRef, useState } from "react";
import ReactDOM from "react-dom/client";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";

/**
 * Clipboard history popup (v0.9.3, text + images + multi-select).
 *
 * Summoned by the global hotkey; the backend builds this window on a
 * dedicated thread. Search filters the history, Enter/click copies the
 * entry back to the OS clipboard and closes the popup (it does NOT
 * synthesize Ctrl+V — the user pastes normally). The "Select" footer
 * button enters multi-select mode: check off several text entries and
 * "Copy selected" (or Ctrl+Enter) merges them into one payload, newest
 * first. Esc or focus loss closes.
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
  kind: "text" | "image";
  preview: string;
  chars: number;
  truncated: boolean;
  createdAtMs: number;
  imagePath: string | null;
  width: number | null;
  height: number | null;
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
  // v0.9.3 multi-select: check off several text entries, copy them as one
  // payload. Images stay out (the OS clipboard holds one image).
  const [selectMode, setSelectMode] = useState(false);
  const [checked, setChecked] = useState<ReadonlySet<string>>(new Set());
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
    setSelectMode(false);
    setChecked(new Set());
    void invoke("hide_clipboard_popup").catch(() => {});
  };

  const toggleCheck = (id: string) => {
    setChecked((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  const exitSelect = () => {
    setSelectMode(false);
    setChecked(new Set());
  };

  useEffect(() => {
    shownAt.current = Date.now();
    setQuery("");
    setSelected(0);
    setSelectMode(false);
    setChecked(new Set());
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
        setSelectMode(false);
        setChecked(new Set());
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
    // Drop image entries whose PNG is gone from disk (deleted outside the
    // app) — a blank row would be worse than no row.
    const alive = entries.filter(
      (e) => e.kind !== "image" || e.imagePath
    );
    const q = query.trim().toLowerCase();
    if (!q) return alive;
    // Images have no searchable text; they match a bare "image" query and
    // are hidden by any other query.
    return alive.filter((e) =>
      e.kind === "image" ? "image".includes(q) : e.preview.toLowerCase().includes(q)
    );
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

  // Multi-select: the checked ids, in display order (newest first — the
  // backend preserves it when joining). Images can never be checked, so
  // this is text-only by construction.
  async function copySelected() {
    const ids = filtered
      .filter((e) => e.kind === "text" && checked.has(e.id))
      .map((e) => e.id);
    if (ids.length === 0) return;
    try {
      await invoke("copy_clipboard_entries", { entryIds: ids });
    } catch {
      // Same honest-close rule as single copy.
    }
    hide();
  }

  function onKeyDown(e: React.KeyboardEvent) {
    if (e.key === "Escape") {
      e.preventDefault();
      // First Esc leaves select mode; the next one closes.
      if (selectMode) exitSelect();
      else hide();
    } else if (e.key === "Enter" && e.ctrlKey) {
      // Ctrl+Enter copies the checked entries from the search box.
      e.preventDefault();
      if (selectMode) void copySelected();
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
              Copy some text or an image and it will show up here.
            </>
          ) : (
            <>No matches for "{query}".</>
          )}
        </div>
      ) : (
        <ul className="clip-list" role="listbox" aria-label="Clipboard history">
          {filtered.map((entry, i) => {
            // In select mode the row toggles its checkbox instead of
            // copying; image rows have no checkbox and stay inert.
            return (
              <li key={entry.id} className="clip-row">
                {selectMode && entry.kind === "text" && (
                  <input
                    type="checkbox"
                    className="clip-check"
                    checked={checked.has(entry.id)}
                    onChange={() => toggleCheck(entry.id)}
                    tabIndex={-1}
                    aria-hidden={true}
                  />
                )}
                <button
                  type="button"
                  role="option"
                  aria-selected={i === selected}
                  aria-checked={selectMode && entry.kind === "text" ? checked.has(entry.id) : undefined}
                  className={
                    "clip-item" +
                    (i === selected ? " selected" : "") +
                    (selectMode && entry.kind === "image" ? " dimmed" : "")
                  }
                  onClick={() => {
                    if (selectMode) {
                      if (entry.kind === "text") toggleCheck(entry.id);
                    } else {
                      void choose(entry);
                    }
                  }}
                  onMouseEnter={() => setSelected(i)}
                >
                {entry.kind === "image" && entry.imagePath ? (
                  <>
                    <img
                      className="thumb"
                      src={convertFileSrc(entry.imagePath)}
                      alt={`Copied image${entry.width && entry.height ? `, ${entry.width} by ${entry.height}` : ""}`}
                    />
                    <span className="meta">
                      Image
                      {entry.width && entry.height
                        ? ` · ${entry.width}×${entry.height}`
                        : ""}{" "}
                      · {timeAgo(entry.createdAtMs)}
                    </span>
                  </>
                ) : (
                  <>
                    <span className="text">{entry.preview}</span>
                    <span className="meta">
                      {timeAgo(entry.createdAtMs)} ·{" "}
                      {entry.chars === 1 ? "1 char" : `${entry.chars} chars`}
                      {entry.truncated ? " · preview" : ""}
                    </span>
                  </>
                )}
                </button>
              </li>
            );
          })}
        </ul>
      )}
      <div className="clip-foot">
        {selectMode ? (
          <>
            <button
              type="button"
              onClick={() => void copySelected()}
              disabled={checked.size === 0}
            >
              Copy selected{checked.size > 0 ? ` (${checked.size})` : ""}
            </button>
            <button type="button" onClick={exitSelect}>
              Cancel
            </button>
            <span className="muted">text entries only</span>
          </>
        ) : (
          <>
            <span>
              <b>Enter</b> copies
            </span>
            <span>
              <b>Esc</b> closes
            </span>
            {filtered.some((e) => e.kind === "text") && (
              <button type="button" className="clip-select-btn" onClick={() => setSelectMode(true)}>
                Select
              </button>
            )}
          </>
        )}
      </div>
    </>
  );
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <ClipboardPopup />
);
