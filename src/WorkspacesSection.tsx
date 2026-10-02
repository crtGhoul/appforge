import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { Account, WebApp } from "./types";

/**
 * Workspaces: named groups of apps/accounts ("Work", "Personal").
 *
 * Backend contract (src-tauri/src/workspaces.rs — do not extend):
 *   invoke("list_workspaces") -> WorkspaceList
 *   invoke("save_workspace", { workspace }) -> WorkspaceList
 *   invoke("delete_workspace", { workspaceId }) -> WorkspaceList
 *   invoke("set_active_workspace", { workspaceId }) -> WorkspaceList
 *   invoke("create_workspace_from_open", { name }) -> WorkspaceList
 *
 * IMPORTANT — invoke argument naming: Rust `workspace_id` becomes
 * `workspaceId` in JS (tsc cannot catch a mismatch; it fails at runtime
 * with "missing required key workspaceId"). Struct FIELDS stay snake_case
 * in JSON (serde default, no rename): `app_id`, `account_id`,
 * `active_workspace_id`.
 *
 * A workspace is filter-only: the launcher (App.tsx) shows just the active
 * workspace's member apps/accounts. `account_id: null` on a member means the
 * whole app — every account it has now or gains later.
 */

// ---------------------------------------------------------------------------
// Types (defined locally, exported — types.ts is owned by another worker).
// ---------------------------------------------------------------------------

export interface WorkspaceMember {
  app_id: string;
  /** null = the whole app; otherwise just this account. */
  account_id: string | null;
}

export interface Workspace {
  id: string;
  name: string;
  hotkey: string | null;
  members: WorkspaceMember[];
}

export interface WorkspaceList {
  workspaces: Workspace[];
  /** null = the "All" view. */
  active_workspace_id: string | null;
}

// ---------------------------------------------------------------------------
// Pure logic — mirrors the Rust predicates in workspaces.rs so the launcher
// filter and the section agree. Exported for App.tsx and for unit tests.
// ---------------------------------------------------------------------------

/** Does this member cover the given (app, account) tile? */
export function workspaceCovers(
  members: WorkspaceMember[],
  appId: string,
  accountId: string
): boolean {
  return members.some((m) => {
    if (!m.app_id || m.app_id !== appId) return false;
    return m.account_id === null || m.account_id === accountId;
  });
}

/**
 * Filter apps to a workspace. Whole-app members keep the app with all its
 * accounts; account members keep the app with only the listed accounts.
 * `null` workspace = All (no filtering).
 */
export function filterAppsByWorkspace(
  apps: WebApp[],
  ws: Workspace | null
): WebApp[] {
  if (!ws) return apps;
  const out: WebApp[] = [];
  for (const app of apps) {
    const whole = ws.members.some(
      (m) => m.app_id === app.id && m.account_id === null
    );
    if (whole) {
      out.push(app);
      continue;
    }
    const accounts = app.accounts.filter((a) =>
      workspaceCovers(ws.members, app.id, a.id)
    );
    if (accounts.length > 0) out.push({ ...app, accounts });
  }
  return out;
}

function errMsg(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "string") return err;
  try {
    return JSON.stringify(err);
  } catch {
    return String(err);
  }
}

// ---------------------------------------------------------------------------
// Member picker state: two sets, so ids never need parsing.
// ---------------------------------------------------------------------------

type PickerSelection = {
  wholeApps: Set<string>;
  accounts: Set<string>; // `${appId}:${accountId}`
};

function selectionFromMembers(members: WorkspaceMember[]): PickerSelection {
  const wholeApps = new Set<string>();
  const accounts = new Set<string>();
  for (const m of members) {
    if (m.account_id === null) wholeApps.add(m.app_id);
    else accounts.add(`${m.app_id}:${m.account_id}`);
  }
  return { wholeApps, accounts };
}

function membersFromSelection(sel: PickerSelection): WorkspaceMember[] {
  const members: WorkspaceMember[] = [];
  for (const appId of sel.wholeApps) {
    members.push({ app_id: appId, account_id: null });
  }
  for (const key of sel.accounts) {
    const sep = key.indexOf(":");
    const appId = key.slice(0, sep);
    if (sel.wholeApps.has(appId)) continue; // whole-app already covers it
    members.push({ app_id: appId, account_id: key.slice(sep + 1) });
  }
  return members;
}

function toggleIn<T>(set: Set<T>, value: T): Set<T> {
  const next = new Set(set);
  if (next.has(value)) next.delete(value);
  else next.add(value);
  return next;
}

// ---------------------------------------------------------------------------
// The section.
// ---------------------------------------------------------------------------

export default function WorkspacesSection({
  apps,
  list,
  onList,
  onError,
}: {
  apps: WebApp[];
  /** Workspace state, owned by App.tsx so the launcher filter can use it. */
  list: WorkspaceList | null;
  onList: (list: WorkspaceList) => void;
  onError: (msg: string) => void;
}) {
  const [formError, setFormError] = useState<string | null>(null);
  const [showForm, setShowForm] = useState(false);
  const [editing, setEditing] = useState<Workspace | null>(null);
  const [name, setName] = useState("");
  const [hotkey, setHotkey] = useState("");
  const [selection, setSelection] = useState<PickerSelection>({
    wholeApps: new Set(),
    accounts: new Set(),
  });
  const [saving, setSaving] = useState(false);
  const [openName, setOpenName] = useState("");
  const [grouping, setGrouping] = useState(false);

  // NOTE: this section never fetches on its own. App.tsx owns the
  // WorkspaceList state: it loads `list_workspaces` once and refreshes on
  // the backend's `appmaka:workspace-changed` event (fired by
  // set_active_workspace, including hotkey presses while this section is
  // unmounted in the launcher view). Every mutation below returns the fresh
  // WorkspaceList from the backend, so onList keeps App.tsx in sync with no
  // extra round-trip.

  function openCreate() {
    setEditing(null);
    setName("");
    setHotkey("");
    setSelection({ wholeApps: new Set(), accounts: new Set() });
    setFormError(null);
    setShowForm(true);
  }

  function openEdit(ws: Workspace) {
    setEditing(ws);
    setName(ws.name);
    setHotkey(ws.hotkey ?? "");
    setSelection(selectionFromMembers(ws.members ?? []));
    setFormError(null);
    setShowForm(true);
  }

  async function handleSave(e: React.FormEvent) {
    e.preventDefault();
    const members = membersFromSelection(selection);
    if (members.length === 0) {
      setFormError("Pick at least one app or account first.");
      return;
    }
    setSaving(true);
    setFormError(null);
    try {
      const workspace: Workspace = {
        id: editing?.id ?? "",
        name: name.trim(),
        hotkey: hotkey.trim() === "" ? null : hotkey.trim(),
        members,
      };
      const updated = await invoke<WorkspaceList>("save_workspace", {
        workspace,
      });
      onList(updated);
      setShowForm(false);
      setEditing(null);
    } catch (err) {
      setFormError(errMsg(err));
    } finally {
      setSaving(false);
    }
  }

  async function handleDelete(ws: Workspace) {
    if (
      !window.confirm(
        `Delete the "${ws.name}" workspace? Your apps and accounts stay untouched — only the grouping goes away.`
      )
    ) {
      return;
    }
    try {
      onList(await invoke<WorkspaceList>("delete_workspace", { workspaceId: ws.id }));
    } catch (err) {
      onError(`Could not delete workspace: ${errMsg(err)}`);
    }
  }

  async function handleActivate(workspaceId: string | null) {
    try {
      onList(
        await invoke<WorkspaceList>("set_active_workspace", { workspaceId })
      );
    } catch (err) {
      onError(`Could not switch workspace: ${errMsg(err)}`);
    }
  }

  async function handleGroupOpen(e: React.FormEvent) {
    e.preventDefault();
    if (!openName.trim()) {
      onError("Give the new workspace a name first.");
      return;
    }
    setGrouping(true);
    try {
      onList(
        await invoke<WorkspaceList>("create_workspace_from_open", {
          name: openName.trim(),
        })
      );
      setOpenName("");
    } catch (err) {
      onError(errMsg(err));
    } finally {
      setGrouping(false);
    }
  }

  const workspaces = list?.workspaces ?? [];
  const activeId = list?.active_workspace_id ?? null;

  if (!list) {
    return (
      <div className="workspaces">
        <p className="muted">Loading workspaces…</p>
      </div>
    );
  }

  return (
    <div className="workspaces">
      <p className="muted small">
        Group apps into workspaces like Work and Personal. The launcher then
        shows only the active workspace — switch with a click or a hotkey.
        Deleting a workspace never touches your apps.
      </p>

      <ul className="workspace-list">
        <li
          className={`workspace-row${activeId === null ? " is-active" : ""}`}
        >
          <div className="workspace-meta">
            <span className="workspace-name">All apps</span>
            <span className="muted small">Everything, unfiltered</span>
          </div>
          {activeId === null ? (
            <span className="muted small">In use</span>
          ) : (
            <button
              className="text-button"
              onClick={() => void handleActivate(null)}
            >
              Use
            </button>
          )}
        </li>
        {workspaces.map((ws) => (
          <li
            key={ws.id}
            className={`workspace-row${activeId === ws.id ? " is-active" : ""}`}
          >
            <div className="workspace-meta">
              <span className="workspace-name">{ws.name}</span>
              <span className="muted small">
                {ws.members?.length ?? 0}{" "}
                {(ws.members?.length ?? 0) === 1 ? "item" : "items"}
                {ws.hotkey ? ` · ${ws.hotkey}` : ""}
              </span>
            </div>
            <div className="workspace-actions">
              {activeId === ws.id ? (
                <span className="muted small">In use</span>
              ) : (
                <button
                  className="text-button"
                  onClick={() => void handleActivate(ws.id)}
                >
                  Use
                </button>
              )}
              <button className="text-button" onClick={() => openEdit(ws)}>
                Edit
              </button>
              <button className="danger" onClick={() => void handleDelete(ws)}>
                Delete
              </button>
            </div>
          </li>
        ))}
      </ul>

      <div className="workspace-create-row">
        {!showForm && (
          <button className="text-button" onClick={openCreate}>
            + New workspace
          </button>
        )}
        <form className="inline-form" onSubmit={(e) => void handleGroupOpen(e)}>
          <input
            value={openName}
            onChange={(e) => setOpenName(e.target.value)}
            placeholder="Name for what's open"
            maxLength={60}
            autoComplete="off"
            aria-label="Name for a workspace built from open windows"
          />
          <button type="submit" disabled={grouping}>
            {grouping ? "Grouping…" : "Group what's open"}
          </button>
        </form>
      </div>
      <p className="muted small">
        “Group what's open” makes a workspace out of the account windows you
        have open right now.
      </p>

      {showForm && (
        <form className="workspace-form" onSubmit={(e) => void handleSave(e)}>
          <h3>{editing ? `Edit ${editing.name}` : "New workspace"}</h3>
          <label>
            <span>Name</span>
            <input
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="e.g. Work"
              maxLength={60}
              autoComplete="off"
            />
          </label>
          <label>
            <span>Hotkey (optional)</span>
            <input
              value={hotkey}
              onChange={(e) => setHotkey(e.target.value)}
              placeholder="e.g. Ctrl+Alt+W"
              autoComplete="off"
            />
          </label>
          <p className="muted small">
            Pressing the hotkey switches the launcher to this workspace.
          </p>

          <fieldset className="member-picker">
            <legend>Which apps belong here?</legend>
            {apps.length === 0 ? (
              <p className="muted small">No apps yet — add one above first.</p>
            ) : (
              <ul>
                {apps.map((app) => (
                  <li key={app.id}>
                    <label className="member-app">
                      <input
                        type="checkbox"
                        checked={selection.wholeApps.has(app.id)}
                        onChange={() =>
                          setSelection((s) => ({
                            ...s,
                            wholeApps: toggleIn(s.wholeApps, app.id),
                          }))
                        }
                      />
                      <span>
                        {app.name}
                        <span className="muted small"> — whole app</span>
                      </span>
                    </label>
                    {app.accounts.length > 0 &&
                      !selection.wholeApps.has(app.id) && (
                        <ul className="member-accounts">
                          {app.accounts.map((a: Account) => {
                            const key = `${app.id}:${a.id}`;
                            return (
                              <li key={a.id}>
                                <label>
                                  <input
                                    type="checkbox"
                                    checked={selection.accounts.has(key)}
                                    onChange={() =>
                                      setSelection((s) => ({
                                        ...s,
                                        accounts: toggleIn(s.accounts, key),
                                      }))
                                    }
                                  />
                                  <span>{a.label}</span>
                                </label>
                              </li>
                            );
                          })}
                        </ul>
                      )}
                  </li>
                ))}
              </ul>
            )}
          </fieldset>

          {formError && (
            <p className="form-error" role="alert">
              {formError}
            </p>
          )}
          <div className="form-actions">
            <button type="submit" disabled={saving}>
              {saving ? "Saving…" : editing ? "Save changes" : "Create workspace"}
            </button>
            <button
              type="button"
              className="text-button"
              onClick={() => {
                setShowForm(false);
                setEditing(null);
              }}
            >
              Cancel
            </button>
          </div>
        </form>
      )}
    </div>
  );
}
