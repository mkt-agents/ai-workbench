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

/** Marker appended by agent.rs `friendly_*_error`; keep both sides in sync. */
const RAW_MARK = "｜原文：";

export default function AskErrorCard({ error, failedMode, busy, onRetry, onDismiss }: Props) {
  const { t } = useTranslation("aihub");
  const cut = error.indexOf(RAW_MARK);
  const hint = cut < 0 ? error : error.slice(0, cut);
  const raw = cut < 0 ? "" : error.slice(cut + RAW_MARK.length);
  return (
    <div className="qa-error-card" role="alert">
      <div className="qa-error-main">
        <AlertCircle size={14} className="qa-error-icon" />
        <span className="qa-error-text" title={hint}>{hint}</span>
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
      {raw && (
        <details className="qa-error-raw">
          <summary>{t("errorRaw")}</summary>
          <pre>{raw}</pre>
        </details>
      )}
    </div>
  );
}
