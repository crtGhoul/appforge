import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { MemorySnapshot, OpenAccountWindow } from "./types";

function errMsg(err: unknown): string {
  return typeof err === "string" ? err : "Something went wrong.";
}

/** Kibibytes to a rounded "about X MB" label. */
function aboutMb(kb: number): string {
  return `about ${Math.round(Math.max(0, kb) / 1024)} MB`;
}

/**
 * Memory dashboard. Shows roughly how much RAM AppMaka is using, split
 * per open account window, with a "close them all" button.
 *
 * All numbers are labeled approximate — they come from OS RSS snapshots
 * and can't be split exactly per window, so each account's share is an
 * even estimate of (total - main process) across the open windows.
 */
export function RamDashboard({
  open,
  onClose,
}: {
  open: boolean;
  onClose: () => void;
}) {
  const [windows, setWindows] = useState<OpenAccountWindow[]>([]);
  const [snapshot, setSnapshot] = useState<MemorySnapshot | null>(null);
  const [loading, setLoading] = useState(false);
  const [closing, setClosing] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const [wins, snap] = await Promise.all([
        invoke<OpenAccountWindow[]>("list_open_account_windows"),
        invoke<MemorySnapshot>("memory_snapshot"),
      ]);
      setWindows(wins);
      setSnapshot(snap);
    } catch (err) {
      setError(errMsg(err));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    if (open) void refresh();
  }, [open, refresh]);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [open, onClose]);

  if (!open) return null;

  async function closeAll() {
    if (closing) return;
    setClosing(true);
    setError(null);
    try {
      await invoke<number>("close_all_account_windows");
      await refresh();
    } catch (err) {
      setError(errMsg(err));
    } finally {
      setClosing(false);
    }
  }

  // v0.9.9: "Don't close this window" toggle. Optimistic update; a failure
  // rolls the checkbox back via refresh().
  async function togglePin(w: OpenAccountWindow) {
    const next = !w.pinned;
    setWindows((prev) =>
      prev.map((x) => (x.label === w.label ? { ...x, pinned: next } : x))
    );
    try {
      await invoke("set_window_pinned", { label: w.label, pinned: next });
    } catch (err) {
      setError(errMsg(err));
      await refresh();
    }
  }

  // Even estimate per window; guarded against a zero count and backends
  // whose main-process number exceeds the total.
  const webviewTotalKb = Math.max(
    0,
    (snapshot?.totalRssKb ?? 0) - (snapshot?.mainRssKb ?? 0)
  );
  const perWindowKb = windows.length > 0 ? webviewTotalKb / windows.length : 0;

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className="modal-card ram-card"
        role="dialog"
        aria-modal="true"
        aria-label="Memory usage"
      >
        <h2 className="modal-title">Memory</h2>
        <p className="modal-sub">
          Roughly how much memory AppMaka is using right now.
        </p>

        {loading && !snapshot && (
          <p className="muted">Reading memory numbers…</p>
        )}

        {error && (
          <p className="modal-error" role="alert">
            {error}
          </p>
        )}

        {snapshot && !snapshot.supported && (
          <p className="muted">Memory numbers aren't available on this PC.</p>
        )}

        {snapshot && snapshot.supported && (
          <p className="ram-total">
            AppMaka is using {aboutMb(snapshot.totalRssKb)} in total.
            <span className="muted"> Numbers are approximate.</span>
          </p>
        )}

        {snapshot?.supported && windows.length > 0 && (
          <ul className="ram-list">
            {windows.map((w) => (
              <li key={w.label} className="ram-row ram-row-wrap">
                <span className="ram-row-main">
                  <span className="ram-row-title">{w.appName}</span>
                  <span className="ram-row-sub">{w.accountLabel}</span>
                </span>
                {w.focused && (
                  <span className="ram-focused" title="This window is in focus">
                    in focus
                  </span>
                )}
                <span className="ram-row-mem muted" title="Estimated share of memory">
                  {aboutMb(perWindowKb)} (estimated)
                </span>
                <label
                  className="ram-pin-toggle"
                  title="Asks before this window can be closed."
                >
                  <input
                    type="checkbox"
                    checked={w.pinned}
                    onChange={() => void togglePin(w)}
                  />
                  <span>Don't close this window</span>
                </label>
              </li>
            ))}
          </ul>
        )}

        {snapshot?.supported && !loading && windows.length === 0 && (
          <p className="muted">No account windows are open.</p>
        )}

        {windows.length > 0 && (
          <>
            <button
              type="button"
              className="ram-close-all"
              disabled={closing}
              onClick={() => void closeAll()}
            >
              {closing ? "Closing…" : "Close all account windows"}
            </button>
            <p className="muted small ram-note">
              Your sessions are kept, so reopening restores your logins.
            </p>
          </>
        )}

        <div className="modal-actions">
          <button type="button" className="text-button" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
