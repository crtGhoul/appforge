import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { HotkeyCapture } from "./HotkeyCapture";
import type { ClipboardSettings } from "./types";

/**
 * Clipboard history settings (v0.9.0, text only).
 *
 * Backend contract (src-tauri/src/clipboard.rs): `get_clipboard_settings`,
 * `set_clipboard_hotkey` (goes through the shared hotkey registry, so
 * conflicts come back as plain-language errors), `set_clipboard_cap`,
 * `clear_clipboard`, `clipboard_hotkey_status`.
 */

function errMsg(err: unknown): string {
  return typeof err === "string" ? err : "Something went wrong.";
}

interface HotkeyStatus {
  hotkey: string;
  registered: boolean;
  error: string | null;
}

export function ClipboardSection() {
  const [settings, setSettings] = useState<ClipboardSettings | null>(null);
  const [hotkey, setHotkey] = useState("");
  const [status, setStatus] = useState<HotkeyStatus | null>(null);
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const [capInput, setCapInput] = useState("100");
  const [capError, setCapError] = useState<string | null>(null);
  const [capSaved, setCapSaved] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [cleared, setCleared] = useState(false);
  const [loadError, setLoadError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      const [s, st] = await Promise.all([
        invoke<ClipboardSettings>("get_clipboard_settings"),
        invoke<HotkeyStatus>("clipboard_hotkey_status"),
      ]);
      setSettings(s);
      setHotkey(s.hotkey);
      setCapInput(String(s.cap));
      setStatus(st);
      setLoadError(null);
    } catch (err) {
      setLoadError(errMsg(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function saveHotkey(next: string) {
    setHotkey(next);
    setHotkeyError(null);
    try {
      // Empty clears the binding (backend removes it).
      const saved = await invoke<string>("set_clipboard_hotkey", {
        hotkey: next.trim(),
      });
      setHotkey(saved);
      const st = await invoke<HotkeyStatus>("clipboard_hotkey_status");
      setStatus(st);
    } catch (err) {
      setHotkeyError(errMsg(err));
    }
  }

  async function saveCap() {
    setCapError(null);
    setCapSaved(false);
    const n = Number(capInput);
    if (!Number.isInteger(n) || n < 10 || n > 1000) {
      setCapError("Enter a whole number between 10 and 1000.");
      return;
    }
    try {
      const cap = await invoke<number>("set_clipboard_cap", { cap: n });
      setCapInput(String(cap));
      setCapSaved(true);
    } catch (err) {
      setCapError(errMsg(err));
    }
  }

  async function clearHistory() {
    setClearing(true);
    try {
      await invoke("clear_clipboard");
      setCleared(true);
    } catch (err) {
      setHotkeyError(errMsg(err));
    } finally {
      setClearing(false);
    }
  }

  if (loadError) {
    return <p className="modal-error">{loadError}</p>;
  }
  if (!settings) {
    return <p className="muted small">Loading clipboard settings…</p>;
  }

  return (
    <div>
      <label style={{ display: "block", fontSize: 14, marginBottom: 8 }}>
        <span className="field-label">Popup hotkey</span>
        <div style={{ marginTop: 4 }}>
          <HotkeyCapture
            value={hotkey}
            onChange={(v) => void saveHotkey(v)}
            ariaLabel="Clipboard history hotkey"
            placeholder="Click to set…"
            allowClear
          />
        </div>
      </label>
      {hotkeyError && (
        <p className="modal-error" role="alert">
          {hotkeyError}
        </p>
      )}
      {status && !status.registered && !hotkeyError && (
        <p className="muted small" role="status">
          {status.error
            ? `Couldn't register "${status.hotkey}": ${status.error}`
            : "No popup hotkey set."}
        </p>
      )}
      <p className="muted small" style={{ margin: "0 0 12px" }}>
        Press it anywhere to search everything you've copied. Click an entry
        (or press Enter) to copy it back, then paste as usual.
      </p>

      <label style={{ display: "block", fontSize: 14, marginBottom: 8 }}>
        <span className="field-label">Keep this many entries</span>
        <div style={{ display: "flex", gap: 8, marginTop: 4 }}>
          <input
            type="number"
            min={10}
            max={1000}
            value={capInput}
            onChange={(e) => {
              setCapInput(e.target.value);
              setCapSaved(false);
            }}
            style={{ width: 100 }}
            aria-label="Clipboard history size"
          />
          <button onClick={() => void saveCap()}>Apply</button>
          {capSaved && (
            <span className="muted small" role="status">
              Saved.
            </span>
          )}
        </div>
      </label>
      {capError && (
        <p className="modal-error" role="alert">
          {capError}
        </p>
      )}

      <div style={{ marginTop: 12 }}>
        <button onClick={() => void clearHistory()} disabled={clearing}>
          {clearing ? "Clearing…" : "Clear history"}
        </button>
        {cleared && (
          <span className="muted small" role="status" style={{ marginLeft: 8 }}>
            History cleared.
          </span>
        )}
      </div>

      <p className="muted small" style={{ marginTop: 12 }}>
        Clipboard history stays on this PC — it's never sent anywhere. Text
        only in this version.
      </p>
    </div>
  );
}
