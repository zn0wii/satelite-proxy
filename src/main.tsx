import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { logFrontendEvent } from "./api";
import { installFastTitleTooltips } from "./fastTooltip";

// Native title tooltips fire after an OS-controlled (~0.5s+) delay; swap
// every [title] hover for a short-delay styled bubble app-wide.
installFastTitleTooltips();

// Disable the webview's default right-click context menu (Windows & macOS)
document.addEventListener(
  "contextmenu",
  (e) => {
    e.preventDefault();
  },
  { capture: true },
);

// Last-resort crash trail: anything that escapes React's ErrorBoundary
// (async callbacks, event handlers outside the tree) is reported to the
// app log instead of vanishing with a white window.
window.addEventListener("error", (e) => {
  logFrontendEvent(
    `uncaught: ${e.message} @ ${e.filename ?? "?"}:${e.lineno ?? "?"}`,
  );
});
window.addEventListener("unhandledrejection", (e) => {
  logFrontendEvent(`unhandled rejection: ${String(e.reason)}`);
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
