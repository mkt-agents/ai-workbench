import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import QuickAskApp from "./components/QuickAskApp";
import QuickAskBubble from "./components/QuickAskBubble";
import { ConfirmDialogProvider } from "./components/ConfirmModal";
import ErrorBoundary from "./components/ErrorBoundary";
import "./i18n/config";
import "./styles.css";

const hash = window.location.hash;
const isQuickAskBubble = hash.startsWith("#/quick-ask-bubble");
const isQuickAsk = hash.startsWith("#/quick-ask") && !isQuickAskBubble;

if (isQuickAskBubble) {
  document.documentElement.classList.add("qa-bubble-mode");
  document.body?.classList.add("qa-bubble-mode");
}

function Root() {
  if (isQuickAskBubble) return <QuickAskBubble />;
  if (isQuickAsk)
    return (
      <ConfirmDialogProvider>
        <QuickAskApp />
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
