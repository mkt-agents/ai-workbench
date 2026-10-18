import { useTranslation } from "react-i18next";
import { X } from "lucide-react";
import type { AskAttachment } from "../../lib/askAttachments";

/** Light attachments riding along with the next question; × removes one. */
export default function AttachmentBar({
  attachments,
  onRemove,
}: {
  attachments: AskAttachment[];
  onRemove: (id: string) => void;
}) {
  const { t } = useTranslation("aihub");
  if (attachments.length === 0) return null;
  return (
    <div className="qa-attach-bar">
      {attachments.map((a) => (
        <span key={a.id} className="qa-attach-chip" title={`${a.name} · ${a.content.length} chars`}>
          {a.name}
          <button
            type="button"
            className="qa-attach-chip-x"
            onClick={() => onRemove(a.id)}
            aria-label={t("attach.remove")}
          >
            <X size={11} />
          </button>
        </span>
      ))}
    </div>
  );
}
