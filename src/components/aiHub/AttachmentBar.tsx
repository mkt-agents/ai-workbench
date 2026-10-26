import { useTranslation } from "react-i18next";
import { Scissors, X } from "lucide-react";
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
        <span
          key={a.id}
          className={`qa-attach-chip${a.truncated ? " is-cut" : ""}`}
          title={
            a.image
              ? `${a.name} · ${t("attach.image")}`
              : `${a.name} · ${a.content.length} chars${a.truncated ? ` · ${t("attach.truncated")}` : ""}`
          }
        >
          {a.image && (
            <img
              className="qa-attach-thumb"
              src={`data:${a.image.mediaType};base64,${a.image.data}`}
              alt=""
            />
          )}
          <span className="qa-attach-name">{a.name}</span>
          {a.truncated && <Scissors size={10} className="qa-attach-cut" />}
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
