import { invoke } from "@tauri-apps/api/core";

/**
 * Floating back/forward pill for account windows.
 *
 * Account windows are bare webviews with no browser chrome, so this tiny
 * first-party window is the visible navigation (Alt+Left / Alt+Right are
 * handled in-page by an init script). It is created `focusable(false)` on
 * the backend, so clicks land without stealing keyboard focus from the
 * page. It only ever talks to our own backend — site windows never get IPC.
 */
export function NavToolbar({
  appId,
  accountId,
}: {
  appId: string;
  accountId: string;
}) {
  function nav(direction: "back" | "forward") {
    // Fire-and-forget: a failed nav (window just closed) is not worth
    // surfacing anywhere.
    invoke("account_nav", { appId, accountId, direction }).catch(() => {});
  }

  return (
    <div className="nav-toolbar">
      <button
        type="button"
        className="nav-btn"
        onClick={() => nav("back")}
        aria-label="Go back"
        title="Back (Alt+Left)"
      >
        &#8592;
      </button>
      <button
        type="button"
        className="nav-btn"
        onClick={() => nav("forward")}
        aria-label="Go forward"
        title="Forward (Alt+Right)"
      >
        &#8594;
      </button>
    </div>
  );
}
