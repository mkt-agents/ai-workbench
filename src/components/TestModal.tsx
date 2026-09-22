import type { ReactNode } from "react";
import ModalTitleRow from "./ModalTitleRow";

type Props = {
  title: string;
  onClose: () => void;
  /**
   * Hard lock for short, must-finish form submits (saving a row). Long AI calls
   * deliberately do NOT set this: orphaning a request is better than trapping
   * the user in a dialog while a model hangs.
   */
  lockClose?: boolean;
  wide?: boolean;
  children: ReactNode;
  footer?: ReactNode;
};

/**
 * Dialog shell for the test-management page: the shared overlay and title row from
 * the design system, plus the page's own width and scrolling rules (`tm-modal`).
 */
export default function TestModal({ title, onClose, lockClose = false, wide = false, children, footer }: Props) {
  return (
    <div
      className="modal-overlay"
      onMouseDown={(e) => {
        if (!lockClose && e.target === e.currentTarget) onClose();
      }}
    >
      <div
        className={`tm-modal${wide ? " tm-modal-wide" : ""}`}
        role="dialog"
        aria-modal="true"
        aria-label={title}
      >
        <ModalTitleRow title={title} onClose={onClose} disabled={lockClose} />
        <div className="tm-modal-body">{children}</div>
        {footer ? <div className="tm-modal-footer">{footer}</div> : null}
      </div>
    </div>
  );
}
