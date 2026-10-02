import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { WebApp } from "./types";

interface LinkRule {
  domain: string;
  appId: string;
  accountId: string;
}

interface LinkConfig {
  optIn: boolean;
  rules: LinkRule[];
}

/**
 * Settings section for the link dispatcher. The coordinator places it in
 * the settings view; it manages its own state via the links commands.
 */
export default function LinkRules() {
  const [config, setConfig] = useState<LinkConfig | null>(null);
  const [apps, setApps] = useState<WebApp[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    invoke<LinkConfig>("get_link_config")
      .then(setConfig)
      .catch((e) => setError(`Could not load link settings: ${e}`));
    invoke<WebApp[]>("list_apps")
      .then(setApps)
      .catch(() => {});
  }, []);

  async function toggleOptIn(enabled: boolean) {
    setBusy(true);
    setError(null);
    try {
      const next = await invoke<LinkConfig>("set_link_opt_in", { enabled });
      setConfig(next);
    } catch (e) {
      setError(`Could not save: ${e}`);
    } finally {
      setBusy(false);
    }
  }

  async function removeRule(domain: string) {
    setBusy(true);
    setError(null);
    try {
      const rules = await invoke<LinkRule[]>("remove_link_rule", { domain });
      setConfig((c) => (c ? { ...c, rules } : c));
    } catch (e) {
      setError(`Could not remove the rule: ${e}`);
    } finally {
      setBusy(false);
    }
  }

  function describeRule(rule: LinkRule): string {
    const app = apps.find((a) => a.id === rule.appId);
    const account = app?.accounts.find((a) => a.id === rule.accountId);
    const appName = app ? app.name : "Removed app";
    const accountLabel = account ? account.label : "Removed account";
    return `${appName} / ${accountLabel}`;
  }

  if (config === null && error === null) {
    return <p style={{ fontSize: 14, color: "#5a5e66" }}>Loading link settings…</p>;
  }

  return (
    <section aria-label="Web links">
      <h3 style={{ margin: "0 0 8px", fontSize: 16 }}>Web links</h3>
      <label
        style={{
          display: "flex",
          alignItems: "flex-start",
          gap: 10,
          fontSize: 14,
          marginBottom: 6,
        }}
      >
        <input
          type="checkbox"
          checked={config?.optIn ?? false}
          disabled={busy || config === null}
          onChange={(e) => toggleOptIn(e.target.checked)}
          style={{ marginTop: 3 }}
        />
        <span>
          <strong>Handle web links</strong>
          <br />
          <span style={{ color: "#5a5e66", fontSize: 13 }}>
            When on, links you click can open in one of your accounts.
          </span>
        </span>
      </label>

      {error && (
        <p style={{ fontSize: 13, color: "#b42318", margin: "8px 0" }}>{error}</p>
      )}

      {config && config.rules.length > 0 && (
        <ul style={{ listStyle: "none", padding: 0, margin: "12px 0 0" }}>
          {config.rules.map((rule) => (
            <li
              key={rule.domain}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "space-between",
                gap: 12,
                padding: "8px 0",
                borderTop: "1px solid #e8eaed",
                fontSize: 14,
              }}
            >
              <span style={{ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
                <strong>{rule.domain}</strong>
                <span style={{ color: "#5a5e66" }}> → {describeRule(rule)}</span>
              </span>
              <button type="button" disabled={busy} onClick={() => removeRule(rule.domain)}>
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}
      {config && config.rules.length === 0 && (
        <p style={{ fontSize: 13, color: "#5a5e66", margin: "12px 0 0" }}>
          No link rules yet. When a link has no rule, you will be asked which
          account to open it in.
        </p>
      )}
    </section>
  );
}
