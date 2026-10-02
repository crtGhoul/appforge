import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { WebApp } from "./types";

/**
 * Per-command hotkeys: bind a global hotkey straight to an app or an
 * app→account (e.g. Ctrl+Alt+G opens work Gmail). Lives in the launcher
 * settings area.
 *
 * Each row is one saved `CmdHotkey` (backend: cmdhotkeys.json). The hotkey
 * text is validated live against the shared hotkey registry — a conflict
 * (same key as another binding, or the summon hotkey) comes back as a
 * plain-language error shown under the row, v0.6.1 style. Bindings whose
 * registration failed (e.g. taken by another app at startup) surface in
 * the warning banner, read via `get_binding_status` per binding.
 */

export interface CmdHotkey {
  id: string;
  app_id: string;
  account_id: string | null;
  hotkey: string;
}

export interface BindingStatus {
  hotkey: string;
  registered: boolean;
  error: string | null;
}

function errMsg(err: unknown): string {
  return typeof err === "string" ? err : "Something went wrong.";
}

export function CmdHotkeysSection({
  apps,
  onError,
}: {
  apps: WebApp[];
  onError: (msg: string) => void;
}) {
  const [rows, setRows] = useState<CmdHotkey[]>([]);
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [statuses, setStatuses] = useState<Record<string, BindingStatus>>({});
  const [rowErrors, setRowErrors] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  // Add form.
  const [newAppId, setNewAppId] = useState("");
  const [newAccountId, setNewAccountId] = useState("");
  const [newHotkey, setNewHotkey] = useState("");
  const [adding, setAdding] = useState(false);
  const [addError, setAddError] = useState<string | null>(null);

  function describeRow(row: CmdHotkey): string {
    const app = apps.find((a) => a.id === row.app_id);
    const appName = app ? app.name : row.app_id;
    if (!row.account_id) return appName;
    const account = app?.accounts.find((c) => c.id === row.account_id);
    return `${appName} — ${account ? account.label : row.account_id}`;
  }

  async function refreshStatuses(list: CmdHotkey[]) {
    const next: Record<string, BindingStatus> = {};
    await Promise.all(
      list.map(async (row) => {
        try {
          const s = await invoke<BindingStatus | null>("get_binding_status", {
            bindingId: row.id,
          });
          if (s) next[row.id] = s;
        } catch {
          /* backend without the command; skip */
        }
      })
    );
    setStatuses(next);
  }

  useEffect(() => {
    let alive = true;
    void invoke<CmdHotkey[]>("list_cmdhotkeys")
      .then((list) => {
        if (!alive) return;
        setRows(list);
        const d: Record<string, string> = {};
        for (const r of list) d[r.id] = r.hotkey;
        setDrafts(d);
        void refreshStatuses(list);
      })
      .catch(() => {
        /* older backend without the commands; section stays empty */
      });
    return () => {
      alive = false;
    };
  }, []);

  async function handleSave(row: CmdHotkey) {
    const draft = (drafts[row.id] ?? "").trim();
    setBusy((b) => ({ ...b, [row.id]: true }));
    setRowErrors((e) => ({ ...e, [row.id]: "" }));
    try {
      const saved = await invoke<CmdHotkey>("save_cmdhotkey", {
        appId: row.app_id,
        accountId: row.account_id,
        hotkey: draft,
      });
      if (draft === "") {
        // Empty hotkey = removed.
        setRows((rs) => rs.filter((r) => r.id !== row.id));
      } else {
        setRows((rs) => rs.map((r) => (r.id === row.id ? saved : r)));
        setDrafts((d) => ({ ...d, [row.id]: saved.hotkey }));
      }
      await refreshStatuses(
        draft === "" ? rows.filter((r) => r.id !== row.id) : rows
      );
    } catch (err) {
      const msg = errMsg(err);
      setRowErrors((e) => ({ ...e, [row.id]: msg }));
      onError(msg);
    } finally {
      setBusy((b) => ({ ...b, [row.id]: false }));
    }
  }

  async function handleDelete(row: CmdHotkey) {
    setBusy((b) => ({ ...b, [row.id]: true }));
    try {
      await invoke("delete_cmdhotkey", { id: row.id });
      const next = rows.filter((r) => r.id !== row.id);
      setRows(next);
      setStatuses((s) => {
        const copy = { ...s };
        delete copy[row.id];
        return copy;
      });
    } catch (err) {
      const msg = errMsg(err);
      setRowErrors((e) => ({ ...e, [row.id]: msg }));
      onError(msg);
    } finally {
      setBusy((b) => ({ ...b, [row.id]: false }));
    }
  }

  async function handleAdd(e: React.FormEvent) {
    e.preventDefault();
    setAddError(null);
    if (!newAppId) {
      setAddError("Pick an app first.");
      return;
    }
    if (!newHotkey.trim()) {
      setAddError("Type a hotkey, e.g. Ctrl+Alt+G.");
      return;
    }
    setAdding(true);
    try {
      const saved = await invoke<CmdHotkey>("save_cmdhotkey", {
        appId: newAppId,
        accountId: newAccountId || null,
        hotkey: newHotkey.trim(),
      });
      const next = [...rows.filter((r) => r.id !== saved.id), saved];
      setRows(next);
      setDrafts((d) => ({ ...d, [saved.id]: saved.hotkey }));
      setNewHotkey("");
      await refreshStatuses(next);
    } catch (err) {
      const msg = errMsg(err);
      setAddError(msg);
      onError(msg);
    } finally {
      setAdding(false);
    }
  }

  const broken = rows.filter(
    (r) => statuses[r.id] && !statuses[r.id].registered
  );
  const selectedApp = apps.find((a) => a.id === newAppId);

  return (
    <div className="cmd-hotkeys">
      <h3>App hotkeys</h3>
      <p className="muted small">
        Jump straight to an app or one of its accounts from anywhere, without
        opening the launcher first.
      </p>
      {broken.length > 0 && (
        <div className="banner banner-error" role="alert">
          <strong>
            {broken.length === 1
              ? "One hotkey couldn't be registered"
              : `${broken.length} hotkeys couldn't be registered`}{" "}
            — another app may be using them. The rows below say which.
          </strong>
        </div>
      )}
      {rows.length === 0 && (
        <p className="muted small">No app hotkeys yet. Add one below.</p>
      )}
      <ul className="cmd-hotkey-rows">
        {rows.map((row) => {
          const status = statuses[row.id];
          const rowError = rowErrors[row.id];
          return (
            <li key={row.id} className="cmd-hotkey-row">
              <span className="cmd-hotkey-target">{describeRow(row)}</span>
              <input
                type="text"
                value={drafts[row.id] ?? ""}
                onChange={(e) =>
                  setDrafts((d) => ({ ...d, [row.id]: e.target.value }))
                }
                placeholder="Ctrl+Alt+G"
                aria-label={`Hotkey for ${describeRow(row)}`}
                spellCheck={false}
              />
              <button
                type="button"
                onClick={() => void handleSave(row)}
                disabled={!!busy[row.id]}
              >
                Save
              </button>
              <button
                type="button"
                className="danger"
                onClick={() => void handleDelete(row)}
                disabled={!!busy[row.id]}
              >
                Remove
              </button>
              {rowError && (
                <div className="form-error" role="alert">
                  {rowError}
                </div>
              )}
              {status && !status.registered && (
                <div className="muted small" role="alert">
                  Not registered{status.error ? `: ${status.error}` : "."} Pick
                  a different hotkey and save again.
                </div>
              )}
            </li>
          );
        })}
      </ul>
      <form className="inline-form" onSubmit={(e) => void handleAdd(e)}>
        <label>
          <span>App</span>
          <select
            value={newAppId}
            onChange={(e) => {
              setNewAppId(e.target.value);
              setNewAccountId("");
            }}
          >
            <option value="">Choose…</option>
            {apps.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Account</span>
          <select
            value={newAccountId}
            onChange={(e) => setNewAccountId(e.target.value)}
            disabled={!selectedApp}
          >
            <option value="">Whole app</option>
            {(selectedApp?.accounts ?? []).map((c) => (
              <option key={c.id} value={c.id}>
                {c.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>Hotkey</span>
          <input
            type="text"
            value={newHotkey}
            onChange={(e) => setNewHotkey(e.target.value)}
            placeholder="Ctrl+Alt+G"
            spellCheck={false}
          />
        </label>
        <button type="submit" disabled={adding}>
          Add hotkey
        </button>
      </form>
      {addError && (
        <div className="form-error" role="alert">
          {addError}
        </div>
      )}
    </div>
  );
}
