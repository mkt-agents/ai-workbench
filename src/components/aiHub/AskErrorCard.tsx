import { useTranslation } from "react-i18next";
import { AlertCircle, MessageCircle, RefreshCw, X } from "lucide-react";
import type { AskMode } from "./route";

type Props = {
  error: string;
  /** Mode of the failed request (null = no retry offered). */
  failedMode: AskMode | null;
  busy: boolean;
  onRetry: (asChat: boolean) => void;
  onDismiss: () => void;
};

export default function AskErrorCard({ error, failedMode, busy, onRetry, onDismiss }: Props) {
  const { t } = useTranslation("aihub");
  return (
    <div className="qa-error-card" role="alert">
      <AlertCircle size={14} className="qa-error-icon" />
      <span className="qa-error-text" title={error}>{error}</span>
      {failedMode && !busy && (
        <span className="qa-error-actions">
          <button type="button" className="qa-mini-btn" onClick={() => onRetry(false)}>
            <RefreshCw size={12} /> {t("retry")}
          </button>
          {failedMode === "project" && (
            <button type="button" className="qa-mini-btn" onClick={() => onRetry(true)}>
              <MessageCircle size={12} /> {t("retryAsChat")}
            </button>
          )}
        </span>
      )}
      <button type="button" className="qa-error-close" onClick={onDismiss} title={t("dismiss")}>
        <X size={12} />
      </button>
    </div>
  );
}
