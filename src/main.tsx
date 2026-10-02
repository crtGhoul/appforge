import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { NavToolbar } from "./NavToolbar";
import "./styles.css";
import "./visibility";

// The floating back/forward toolbar for account windows loads this same
// bundle with `?toolbar=1&appId=…&accountId=…` and renders only the pill.
// It is a first-party window (never a site window), so using the Tauri API
// here is fine.
const params = new URLSearchParams(window.location.search);
const isToolbar = params.get("toolbar") === "1";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    {isToolbar ? (
      <NavToolbar
        appId={params.get("appId") ?? ""}
        accountId={params.get("accountId") ?? ""}
      />
    ) : (
      <App />
    )}
  </React.StrictMode>,
);
