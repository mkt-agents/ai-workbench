import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import AiHubApp from "./components/AiHubApp";
import AiHubBubble from "./components/AiHubBubble";
import { ConfirmDialogProvider } from "./components/ConfirmModal";
import ErrorBoundary from "./components/ErrorBoundary";
import "./i18n/config";
import "./styles/index.css";

const hash = window.location.hash;
const isAiHubBubble = hash.startsWith("#/ai-hub-bubble");
const isAiHub = hash.startsWith("#/ai-hub") && !isAiHubBubble;
/** Splash paints in index.html before this module; keep it long enough for the entrance. */
const BOOT_SPLASH_MIN_MS = 780;
const bootSplashShownAt = performance.now();

if (isAiHubBubble) {
  document.documentElement.classList.add("qa-bubble-mode");
  document.body?.classList.add("qa-bubble-mode");
}

function dismissBootSplash() {
  const el = document.getElementById("boot-splash");
  if (!el || el.classList.contains("is-gone")) return;

  // Secondary windows never show the staged splash; drop it without a reveal.
  if (isAiHubBubble || isAiHub) {
    el.classList.add("is-gone");
    el.remove();
    return;
  }

  const reduced =
    typeof window.matchMedia === "function" &&
    window.matchMedia("(prefers-reduced-motion: reduce)").matches;
  const wait = Math.max(0, BOOT_SPLASH_MIN_MS - (performance.now() - bootSplashShownAt));

  window.setTimeout(() => {
    if (reduced) {
      el.classList.add("is-gone");
      el.remove();
      return;
    }
    el.classList.add("is-leaving");
    const finish = () => {
      el.classList.add("is-gone");
      el.remove();
    };
    el.addEventListener("transitionend", finish, { once: true });
    window.setTimeout(finish, 480);
  }, wait);
}

function Root() {
  if (isAiHubBubble) return <AiHubBubble />;
  if (isAiHub)
    return (
      <ConfirmDialogProvider>
        <AiHubApp />
      </ConfirmDialogProvider>
    );
  return <App />;
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <ErrorBoundary>
      <Root />
    </ErrorBoundary>
  </React.StrictMode>
);

// Wait for the first paint of the app chrome, then hand off from the splash.
requestAnimationFrame(() => {
  requestAnimationFrame(dismissBootSplash);
});
