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

if (isAiHubBubble) {
  document.documentElement.classList.add("qa-bubble-mode");
  document.body?.classList.add("qa-bubble-mode");
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
