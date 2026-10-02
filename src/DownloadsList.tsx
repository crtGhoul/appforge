import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

/** Mirrors the Rust DownloadEntry (camelCased by serde). */
export type DownloadEntry = {
  id: string;
  filename: string;
  url: string;
  state: "active" | "complete" | "failed";
  receivedBytes: number;
  totalBytes: number | null;
  path: string;
};

function errMsg(err: unknown): string {
  return typeof err === "string" ? err : "Something went wrong.";
}

function fmtBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const mb = bytes / (1024 * 1024);
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

function statusLine(entry: DownloadEntry): string {
  if (entry.state === "complete") return "Complete";
  if (entry.state === "failed") return "Failed";
  const received = fmtBytes(entry.receivedBytes ?? 0);
  // Sizes are what the download engine reported; when the total is unknown
  // we never invent a percentage.
  if (entry.totalBytes != null && entry.totalBytes > 0) {
    return `Downloading… ${received} of ${fmtBytes(entry.totalBytes)}`;
  }
  return `Downloading… ${received} so far`;
}

/**
 * Download list. Shows files downloaded inside account windows, with live
 * progress, open / show-in-folder / remove actions, and the destination
 * folder picker.
 *
 * No filesystem browsing: this is a list, not a file manager.
 */
export function DownloadsList({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const [entries, setEntries] = useState<DownloadEntry[]>([]);
  const [downloadDir, setDownloadDir] = useState<string>("");
  const [dirInput, setDirInput] = useState<string>("");
  const [savingDir, setSavingDir] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const [list, dir] = await Promise.all([
        invoke<DownloadEntry[]>("list_downloads"),
        invoke<string>("get_download_dir"),
      ]);
      setEntries(list);
      setDownloadDir(dir);
      setDirInput(dir);
    } catch (err) {
      setError(errMsg(err));
    }
  }, []);

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  useEffect(() => {
    if (!open) return;
    let unlisten: (() => void) | null = null;
    void listen<DownloadEntry>("appmaka:download-progress", (event) => {
      const entry = event.payload;
      setEntries((prev) => {
        const idx = prev.findIndex((e) => e.id === entry.id);
        if (idx >= 0) {
          const next = prev.slice();
          next[idx] = entry;
          return next;
        }
        return [entry, ...prev];
      });
    }).then((fn) => {
      unlisten = fn;
    });
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey, true);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      unlisten?.();
    };
  }, [open, onClose]);

  if (!open) return null;

  async function removeEntry(id: string) {
    setError(null);
    try {
      await invoke("remove_download", { id });
      setEntries((prev) => prev.filter((e) => e.id !== id));
    } catch (err) {
      setError(errMsg(err));
    }
  }

  async function clearFinished() {
    setError(null);
    try {
      await invoke("clear_finished");
      setEntries((prev) => prev.filter((e) => e.state === "active"));
    } catch (err) {
      setError(errMsg(err));
    }
  }

  async function saveDir() {
    if (savingDir) return;
    setSavingDir(true);
    setError(null);
    try {
      await invoke("set_download_dir", { path: dirInput });
      const dir = await invoke<string>("get_download_dir");
      setDownloadDir(dir);
      setDirInput(dir);
    } catch (err) {
      setError(errMsg(err));
    } finally {
      setSavingDir(false);
    }
  }

  const hasFinished = entries.some((e) => e.state !== "active");

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="modal-card"
        role="dialog"
        aria-modal="true"
        aria-label="Downloads"
      >
        <h2 className="modal-title">Downloads</h2>
        <p className="modal-sub">
          Files downloaded inside your account windows land here. This is a
          list, not a file browser.
        </p>

        {error && (
          <p className="modal-error" role="alert">
            {error}
          </p>
        )}

        <div style={{ marginBottom: 12 }}>
          <div className="muted" style={{ marginBottom: 4 }}>
            Download folder
          </div>
          <div style={{ fontSize: 13, wordBreak: "break-all" }}>{downloadDir}</div>
          <div style={{ display: "flex", gap: 8, marginTop: 6 }}>
            <input
              type="text"
              value={dirInput}
              onChange={(e) => setDirInput(e.target.value)}
              aria-label="Download folder path"
              style={{ flex: 1 }}
            />
            <button
              type="button"
              onClick={() => void saveDir()}
              disabled={savingDir || dirInput.trim() === downloadDir}
            >
              {savingDir ? "Saving…" : "Save"}
            </button>
          </div>
        </div>

        {entries.length === 0 ? (
          <p className="muted">Nothing downloaded yet.</p>
        ) : (
          <ul style={{ listStyle: "none", padding: 0, margin: 0 }}>
            {entries.map((entry) => (
              <li
                key={entry.id}
                style={{
                  padding: "8px 0",
                  borderTop: "1px solid rgba(128,128,128,0.25)",
                }}
              >
                <div style={{ fontSize: 14, wordBreak: "break-all" }}>
                  {entry.filename}
                </div>
                <div className="muted" style={{ fontSize: 12, margin: "2px 0 6px" }}>
                  {statusLine(entry)}
                </div>
                <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
                  {entry.state === "complete" && (
                    <>
                      <button
                        type="button"
                        onClick={() =>
                          invoke("open_download", { id: entry.id }).catch((err) =>
                            setError(errMsg(err))
                          )
                        }
                      >
                        Open
                      </button>
                      <button
                        type="button"
                        onClick={() =>
                          invoke("show_in_folder", { id: entry.id }).catch((err) =>
                            setError(errMsg(err))
                          )
                        }
                      >
                        Show in folder
                      </button>
                    </>
                  )}
                  <button
                    type="button"
                    onClick={() => void removeEntry(entry.id)}
                  >
                    Remove
                  </button>
                </div>
              </li>
            ))}
          </ul>
        )}

        <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
          <button
            type="button"
            onClick={() => void clearFinished()}
            disabled={!hasFinished}
          >
            Clear finished
          </button>
          <button type="button" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
