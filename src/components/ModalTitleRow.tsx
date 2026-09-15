import { useEffect, useRef } from "react";
import { useTranslation } from "react-i18next";
import { X } from "lucide-react";

type Props = {
  /** 标题文本（必填；空字符串则只渲染 X） */
  title?: string;
  /** 关闭回调：X 与 Esc 共用 */
  onClose: () => void;
  /** busy 时禁用 X 与 Esc（与遮罩点击行为保持一致） */
  disabled?: boolean;
  /** 数量徽章等附加内容，渲染在标题文本右侧 */
  badge?: React.ReactNode;
};

/**
 * 弹框统一标题行：标题文本（+ 可选徽章）在左，X 关闭按钮在右，底部分隔线。
 * 同时挂载 Esc 关闭（随 disabled 禁用）。
 * 多层遮罩叠加时（如删除确认叠在管理弹窗上），只有最上层弹框响应 Esc。
 */
export default function ModalTitleRow({ title, onClose, disabled = false, badge }: Props) {
  const { t } = useTranslation("common");
  const rowRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (disabled) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // 只响应最上层遮罩：本组件所在 overlay 必须是 DOM 中最后一个
      const overlays = Array.from(document.querySelectorAll(".modal-overlay"));
      const mine = rowRef.current?.closest(".modal-overlay");
      if (!mine || overlays[overlays.length - 1] !== mine) return;
      e.stopPropagation();
      onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose, disabled]);

  return (
    <div className="modal-title-row" ref={rowRef}>
      <span className="modal-title-text">{title}</span>
      {badge}
      <button
        type="button"
        className="modal-close"
        onClick={onClose}
        disabled={disabled}
        aria-label={t("actions.close")}
        title={t("actions.close")}
      >
        <X size={16} />
      </button>
    </div>
  );
}
