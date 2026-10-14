import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Trash2, AlertTriangle, Info, HelpCircle } from "lucide-react";

type ConfirmIcon = "danger" | "warning" | "info" | "question";

export type ConfirmChoice = "confirm" | "alt" | "cancel";

interface ConfirmOptions {
  title: string;
  message: string;
  warning?: string;
  confirmText?: string;
  /** Optional third action (e.g. “Use preset”). */
  altConfirmText?: string;
  cancelText?: string;
  icon?: ConfirmIcon;
}

interface ConfirmState extends ConfirmOptions {
  resolve: (value: ConfirmChoice) => void;
}

let globalSetConfirmState: React.Dispatch<React.SetStateAction<ConfirmState | null>> | null = null;

function openConfirm(options: ConfirmOptions): Promise<ConfirmChoice> {
  return new Promise((resolve) => {
    if (globalSetConfirmState) {
      globalSetConfirmState({ ...options, resolve });
    } else {
      resolve(window.confirm(options.message) ? "confirm" : "cancel");
    }
  });
}

/** Hook to access the global confirm dialog (yes / no). */
export function useConfirm() {
  const confirm = useCallback(async (options: ConfirmOptions): Promise<boolean> => {
    const choice = await openConfirm({ ...options, altConfirmText: undefined });
    return choice === "confirm";
  }, []);

  return confirm;
}

/** Three-way confirm: primary / alternate / cancel. */
export function useConfirmChoice() {
  const confirmChoice = useCallback((options: ConfirmOptions): Promise<ConfirmChoice> => {
    return openConfirm(options);
  }, []);

  return confirmChoice;
}

/** Global confirm dialog provider - place once at the app root. */
export function ConfirmDialogProvider({ children }: { children: React.ReactNode }) {
  const { t } = useTranslation("common");
  const [state, setState] = useState<ConfirmState | null>(null);
  const confirmRef = useRef<HTMLButtonElement>(null);
  globalSetConfirmState = setState;

  const settle = (value: ConfirmChoice) => {
    if (state) {
      state.resolve(value);
      setState(null);
    }
  };

  // Esc = 取消确认（确认框永远是最上层遮罩）
  useEffect(() => {
    if (!state) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        state.resolve("cancel");
        setState(null);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [state]);

  // 焦点落到主操作，Enter 即可确认；destructive 的图标（danger）用 btn-danger 承接。
  useEffect(() => {
    if (state) confirmRef.current?.focus();
  }, [state]);

  const iconMap: Record<ConfirmIcon, React.ReactNode> = {
    danger: <Trash2 size={17} />,
    warning: <AlertTriangle size={17} />,
    info: <Info size={17} />,
    question: <HelpCircle size={17} />,
  };

  if (!state) return <>{children}</>;

  return (
    <>
      {children}
      <div className="modal-overlay">
        <div className="modal confirm-modal" role="alertdialog" aria-modal="true">
          <div className="confirm-modal-header">
            <div className={`confirm-modal-icon confirm-modal-icon-${state.icon || "danger"}`}>
              {iconMap[state.icon || "danger"]}
            </div>
            <div className="confirm-modal-title">{state.title}</div>
          </div>
          <div className="confirm-modal-body">
            <p className="confirm-modal-message">{state.message}</p>
            {state.warning && <p className="confirm-modal-warning">{state.warning}</p>}
          </div>
          <div className="modal-actions">
            <button className="btn btn-secondary" onClick={() => settle("cancel")}>
              {state.cancelText || t("actions.cancel")}
            </button>
            {state.altConfirmText && (
              <button className="btn btn-secondary" onClick={() => settle("alt")}>
                {state.altConfirmText}
              </button>
            )}
            <button
              ref={confirmRef}
              className={`btn ${state.icon === "danger" || !state.icon ? "btn-danger" : "btn-primary"}`}
              onClick={() => settle("confirm")}
            >
              {state.confirmText || t("actions.confirm")}
            </button>
          </div>
        </div>
      </div>
    </>
  );
}
