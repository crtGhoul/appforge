/**
 * Tracks whether the main webview is hidden (minimized to the tray).
 *
 * The main webview intentionally stays resident for instant summon — this
 * only avoids timer-driven network activity while the window is hidden
 * (e.g. the updater's 24h auto-check skips its tick). Do NOT attempt to
 * suspend the webview itself.
 */
let appIsHidden = false;

document.addEventListener("visibilitychange", () => {
  appIsHidden = document.hidden;
});

/** True when the window was last reported hidden by the visibility API. */
export function isAppHidden(): boolean {
  return appIsHidden;
}
