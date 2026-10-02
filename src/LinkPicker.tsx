import { useMemo, useState } from "react";

export interface LinkPickerAccount {
  appId: string;
  appName: string;
  accountId: string;
  accountLabel: string;
}

interface LinkPickerProps {
  url: string;
  accounts: LinkPickerAccount[];
  onPick: (appId: string, accountId: string, remember: boolean) => void;
  onClose: () => void;
}

/**
 * Small modal shown when a clicked web link has no saved rule.
 * The parent renders it on `appmaka:link-no-rule` and, when `remember`
 * is true, calls `invoke("add_link_rule", { domain, appId, accountId })`.
 */
export default function LinkPicker({ url, accounts, onPick, onClose }: LinkPickerProps) {
  const [selected, setSelected] = useState<string | null>(
    accounts.length > 0 ? `${accounts[0].appId}/${accounts[0].accountId}` : null
  );
  const [remember, setRemember] = useState(true);

  const domain = useMemo(() => {
    try {
      return new URL(url).hostname;
    } catch {
      return "this site";
    }
  }, [url]);

  // Group accounts under their app, keeping the order the parent gave us.
  const groups = useMemo(() => {
    const out: { appId: string; appName: string; items: LinkPickerAccount[] }[] = [];
    for (const a of accounts) {
      let g = out.find((x) => x.appId === a.appId);
      if (!g) {
        g = { appId: a.appId, appName: a.appName, items: [] };
        out.push(g);
      }
      g.items.push(a);
    }
    return out;
  }, [accounts]);

  function handleOpen() {
    if (!selected) return;
    const account = accounts.find((a) => `${a.appId}/${a.accountId}` === selected);
    if (!account) return;
    onPick(account.appId, account.accountId, remember);
  }

  return (
    <div className="dialog-overlay" onClick={onClose}>
      <div
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-label="Open link in account"
        onClick={(e) => e.stopPropagation()}
      >
        <h3>Open this link in:</h3>
        <p style={{ fontSize: 13, color: "#5a5e66", margin: "0 0 12px", wordBreak: "break-all" }}>
          {url}
        </p>
        {groups.length === 0 ? (
          <p style={{ fontSize: 14 }}>No accounts yet. Add an app and sign in first.</p>
        ) : (
          <div style={{ display: "flex", flexDirection: "column", gap: 10, marginBottom: 14 }}>
            {groups.map((g) => (
              <div key={g.appId}>
                <div style={{ fontSize: 12, fontWeight: 700, color: "#6b6f76", marginBottom: 4 }}>
                  {g.appName}
                </div>
                {g.items.map((a) => {
                  const key = `${a.appId}/${a.accountId}`;
                  const active = selected === key;
                  return (
                    <button
                      key={key}
                      type="button"
                      onClick={() => setSelected(key)}
                      aria-pressed={active}
                      style={{
                        display: "block",
                        width: "100%",
                        textAlign: "left",
                        marginBottom: 4,
                        borderColor: active ? "#4f46e5" : undefined,
                        background: active ? "#eef0ff" : undefined,
                      }}
                    >
                      {a.accountLabel}
                    </button>
                  );
                })}
              </div>
            ))}
          </div>
        )}
        <label
          style={{
            display: "flex",
            alignItems: "center",
            gap: 8,
            fontSize: 14,
            marginBottom: 16,
          }}
        >
          <input
            type="checkbox"
            checked={remember}
            onChange={(e) => setRemember(e.target.checked)}
          />
          Remember for links from {domain}
        </label>
        <div style={{ display: "flex", justifyContent: "flex-end", gap: 8 }}>
          <button type="button" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            disabled={!selected}
            onClick={handleOpen}
            style={{ background: "#4f46e5", borderColor: "#4f46e5", color: "#fff" }}
          >
            Open
          </button>
        </div>
      </div>
    </div>
  );
}
