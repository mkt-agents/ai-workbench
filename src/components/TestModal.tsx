import type { ReactNode } from "react";
import ModalTitleRow from "./ModalTitleRow";

type Props = {
  title: string;
  onClose: () => void;
  /** Busy dialogs refuse Esc / backdrop / X so a running request cannot be orphaned. */
  busy?: boolean;
  wide?: boolean;
  children: ReactNode;
  footer?: ReactNode;
};

/**
 * Dialog shell for the test-management page: the shared overlay and title row from
 * the design system, plus the page's own width and scrolling rules (`tm-modal`).
 */
export default function TestModal({ title, onClose, busy = false, wide = false, children, footer }: Props) {
  return (
    <div
      className="modal-overlay"
      onMouseDown={(e) => {
        if (!busy && e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className={`tm-modal${wide ? " tm-modal-wide" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
      >
        <ModalTitleRow title={title} onClose={onClose} disabled={busy} />
        <div className="tm-modal-body">{children}</div>
        {footer ? <div className="tm-modal-footer">{footer}</div> : null}
      </div>
    </div>
  );
}
