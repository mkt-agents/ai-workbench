import { useTranslation } from "react-i18next";
import ModalTitleRow from "../ModalTitleRow";

type Props = {
  paramValues: Record<string, string>;
  setParamValues: (v: Record<string, string>) => void;
  onClose: () => void;
  onConfirm: () => void;
};

/** Modal to fill {{param}} placeholders of a picked snippet. */
export default function ParamModal(props: Props) {
  const { t } = useTranslation("quickask");
  const { paramValues, setParamValues, onClose, onConfirm } = props;
  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal snippets-modal" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow title={t("fillParams")} onClose={onClose} />
        {Object.keys(paramValues).map((key) => (
          <div className="input-group" key={key}>
            <label className="input-label">{key}</label>
            <input
              className="input-field"
              value={paramValues[key]}
              onChange={(e) => setParamValues({ ...paramValues, [key]: e.target.value })}
              onKeyDown={(e) => {
                if (e.key === "Enter") {
                  e.preventDefault();
                  onConfirm();
                }
              }}
              autoFocus={Object.keys(paramValues)[0] === key}
            />
          </div>
        ))}
        <div className="modal-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            {t("cancel")}
          </button>
          <button type="button" className="btn btn-primary" onClick={onConfirm}>
            {t("insertSnippet")}
          </button>
        </div>
      </div>
    </div>
  );
}
