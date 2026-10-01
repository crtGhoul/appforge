# AppForge (working title)

Turn any website into a desktop web app. Add a site by name + URL, and it
opens in its own dedicated window — no browser tabs, no address bar clutter.

**Status: v0 shell.** The library, persistence, and per-app windows work.
Multi-account isolation, popup blocking, and ad blocking are not in this
build yet (see "What's next").

## What exists

- **Library window** — lists your web apps with name, URL, and icon. Icons
  are fetched from the site's own `/favicon.ico`; when that fails a generic
  letter glyph is shown. Empty state is honest: "No apps yet."
- **Add / remove / open** — add an app with name + URL (URLs are validated
  and normalized; `example.com` becomes `https://example.com/`), remove with
  a confirmation, open an app in its own window.
- **Dedicated app windows** — each app opens in its own Tauri `WebviewWindow`
  pointed at the site URL: no URL bar, just the site content, with the native
  OS window frame and the app name as the title. Re-opening focuses the
  existing window instead of duplicating it.
- **Persistence** — the app list is stored as JSON (`apps.json`) in the Tauri
  app-data directory via Rust commands (`list_apps`, `add_app`,
  `remove_app`). Writes go through a temp file + rename so a crash can't
  leave a half-written file. A corrupt `apps.json` is backed up next to
  itself (`apps.json.corrupt-<timestamp>.bak`) and replaced with a fresh
  empty library.
- **No credential handling** — you sign in directly on each site's own page
  inside its app window. AppForge never sees, stores, or transmits passwords.

## Project layout

```
appforge/
  src/                 React + TypeScript frontend (Vite)
    App.tsx            Library UI: add form, app grid, open/remove
  src-tauri/
    src/main.rs        Tauri entry: commands list_apps / add_app / remove_app
    src/store.rs       JSON persistence, corrupt-file backup, URL validation
    tauri.conf.json    App config (main window + bundle settings)
    capabilities/      Frontend permissions (incl. creating app windows)
    icons/             Placeholder icons (not final branding)
```

## Run it

```sh
npm install
npm run tauri dev     # desktop app with hot-reload
```

Checks:

```sh
npx tsc --noEmit          # frontend typecheck
npm run build             # vite production build (tsc && vite build)
cd src-tauri && cargo check   # rust backend check
```

## What's next (not yet implemented)

1. **Isolated accounts** — multiple signed-in accounts per site, each in its
   own storage partition (per-webview data directories on Windows/WebView2),
   so Account A and Account B never share cookies.
2. **Popup blocking** — intercept new-window requests; block by default, with
   a per-site allow for legitimate popups (e.g. OAuth sign-in flows).
3. **Network-level ad blocking** — match requests against uBlock-style filter
   lists before they download, plus cosmetic rules for leftover placeholders.
4. **Auto-suspend** — idle app windows get their webviews discarded and
   restored on focus, keeping RAM near one-tab-per-open-app.

## Notes

- Tauri 2 uses the OS webview (WebView2 on Windows, WebKitGTK on Linux) —
  no bundled Chromium, which is the main structural RAM/disk saving versus
  Electron-based wrappers.
- A few sites (notably Google) sometimes refuse sign-ins inside embedded
  webviews; the fallback is opening that site in the system browser.
