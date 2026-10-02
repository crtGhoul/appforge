import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";

/**
 * invoke("forget_login", { appId, accountId }) — wipes the stored session
 * for that account on this PC. camelCase per the AGENTS.md lesson: Rust
 * `app_id`/`account_id` become `appId`/`accountId` in JS.
 */
export async function forgetLogin(appId: string, accountId: string): Promise<void> {
  await invoke("forget_login", { appId, accountId });
}

/**
 * Confirm dialog before forgetting a login. The coordinator calls
 * `forgetLogin(appId, accountId)` inside `onConfirm`, then refreshes the
 * app/account list.
 */
export function ForgetLoginDialog({
  appName,
  accountLabel,
  onConfirm,
  onCancel,
}: {
  appName: string;
  accountLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", onKey, true);
    return () => document.removeEventListener("keydown", onKey, true);
  }, [onCancel]);

  return (
    <div
      className="modal-backdrop"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onCancel();
      }}
    >
      <div
        className="modal-card"
        role="alertdialog"
        aria-modal="true"
        aria-label="Forget this login"
      >
        <h2 className="modal-title">Forget this login?</h2>
        <p className="modal-sub">
          Forget &lsquo;{accountLabel}&rsquo; on {appName}? This signs you out
          of this account on this PC. Your other accounts are not affected.
        </p>
        <div className="modal-actions">
          <button type="button" className="text-button" onClick={onCancel}>
            Cancel
          </button>
          <button
            type="button"
            className="modal-danger"
            autoFocus
            onClick={onConfirm}
          >
            Forget this login
          </button>
        </div>
      </div>
    </div>
  );
}
