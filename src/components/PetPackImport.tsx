import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { invoke } from "@tauri-apps/api/core";
import { ImagePlus, Loader2, Wand2 } from "lucide-react";
import ModalTitleRow from "./ModalTitleRow";
import { PET_GESTURES, PET_MOODS, type PetGesture, type PetMood } from "../lib/petSkins";
import {
  buildFrames,
  guessSlot,
  loadCells,
  type BuiltFrame,
  type CellAssignment,
  type PickedFile,
  type SourceCell,
} from "../lib/petPack";

const SLOTS: (PetMood | PetGesture)[] = [...PET_MOODS, ...PET_GESTURES];

interface Props {
  onClose: () => void;
  /** 保存成功后由设置页切皮肤并广播给桌宠窗 */
  onApplied: (frameCount: number) => void;
}

/** 一格缩略图：等比塞进小方格，透明交给 CSS 的棋盘底显示。 */
function CellThumb({ cell }: { cell: SourceCell }) {
  const ref = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const c = ref.current;
    if (!c) return;
    const size = 84;
    const s = Math.min(size / cell.w, size / cell.h);
    c.width = Math.max(1, Math.round(cell.w * s));
    c.height = Math.max(1, Math.round(cell.h * s));
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(cell.bitmap, cell.x, cell.y, cell.w, cell.h, 0, 0, c.width, c.height);
  }, [cell]);
  return <canvas ref={ref} className="pet-import-thumb" />;
}

/**
 * 桌宠素材入库向导：挑图 →（可选）按 n×n 切格 → 每格指定情绪 → 抠背景 + 归一化 → 存进应用数据目录。
 * 处理全在本机画布里，图片不出这台电脑。
 */
export default function PetPackImport({ onClose, onApplied }: Props) {
  const { t } = useTranslation("settings");
  const { t: tc } = useTranslation("common");
  const [files, setFiles] = useState<PickedFile[]>([]);
  const [cells, setCells] = useState<SourceCell[]>([]);
  const [slots, setSlots] = useState<CellAssignment[]>([]);
  const [rows, setRows] = useState(1);
  const [cols, setCols] = useState(1);
  const [keyBackground, setKeyBackground] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<BuiltFrame[] | null>(null);

  const slotLabel = useCallback(
    (slot: CellAssignment) =>
      slot ? t(`aiHub.petMood${slot[0].toUpperCase()}${slot.slice(1)}`) : t("aiHub.petImportNone"),
    [t],
  );

  /** 切格后重排分配：文件名能认情绪就认，全认不出时按七态顺序往下排。 */
  const reseat = useCallback((next: SourceCell[]) => {
    setCells(next);
    setPreview(null);
    const guessed = next.map((c) => guessSlot(c.key));
    const byOrder = next.map((_, i) => (i < SLOTS.length ? SLOTS[i] : null));
    setSlots(guessed.some(Boolean) ? guessed.map((g, i) => g ?? byOrder[i]) : byOrder);
  }, []);

  const pick = async () => {
    setError(null);
    try {
      const paths = await invoke<string[]>("pick_pet_images");
      if (!paths.length) return;
      const picked = await invoke<PickedFile[]>("read_pet_images", { paths });
      setFiles(picked);
      reseat(await loadCells(picked, rows, cols));
    } catch (e) {
      setError(String(e));
    }
  };

  const resize = async (nextRows: number, nextCols: number) => {
    setRows(nextRows);
    setCols(nextCols);
    if (!files.length) return;
    setError(null);
    try {
      reseat(await loadCells(files, nextRows, nextCols));
    } catch (e) {
      setError(String(e));
    }
  };

  const runPreview = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setPreview(await buildFrames(cells, slots, { keyBackground }));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, [cells, slots, keyBackground]);

  const apply = async () => {
    if (!slots.includes("idle")) {
      setError(t("aiHub.petImportNeedIdle"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const frames = preview ?? (await buildFrames(cells, slots, { keyBackground }));
      onApplied(await invoke<number>("save_pet_pack", { frames }));
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const assigned = useMemo(() => slots.filter(Boolean).length, [slots]);

  return (
    <div className="modal-overlay">
      <div className="modal modal-lg pet-import-modal">
        <ModalTitleRow title={t("aiHub.petImport")} onClose={onClose} disabled={busy} />
        <div className="modal-body pet-import-body">
          <p className="setting-hint">{t("aiHub.petImportHint")}</p>
          <p className="setting-hint pet-import-license">{t("aiHub.petLicenseHint")}</p>

          <div className="pet-import-tools">
            <button className="btn btn-primary" onClick={() => void pick()} disabled={busy}>
              <ImagePlus size={14} />
              {files.length
                ? t("aiHub.petImportPicked", { n: files.length })
                : t("aiHub.petImportPick")}
            </button>
            <label className="pet-import-key">
              <span>{t("aiHub.petImportKey")}</span>
              <span className="toggle">
                <input
                  type="checkbox"
                  checked={keyBackground}
                  onChange={(e) => {
                    setKeyBackground(e.target.checked);
                    // 预览是在当前选项下算出来的，选项一变就得重算，不能拿旧的去保存
                    setPreview(null);
                  }}
                />
                <span className="toggle-slider" />
              </span>
            </label>
          </div>

          <div className="pet-import-grid" role="group" aria-label={t("aiHub.petImportGrid")}>
            <span className="pet-import-grid-label">{t("aiHub.petImportGrid")}</span>
            <input
              className="input-field pet-import-num"
              type="number"
              min={1}
              max={6}
              value={rows}
              onChange={(e) => void resize(Math.min(6, Math.max(1, Number(e.target.value) || 1)), cols)}
            />
            <span className="pet-import-grid-unit">{t("aiHub.petImportRows")}</span>
            <span className="pet-import-grid-x">×</span>
            <input
              className="input-field pet-import-num"
              type="number"
              min={1}
              max={6}
              value={cols}
              onChange={(e) => void resize(rows, Math.min(6, Math.max(1, Number(e.target.value) || 1)))}
            />
            <span className="pet-import-grid-unit">{t("aiHub.petImportCols")}</span>
          </div>

          {cells.length > 0 && (
            <div className="pet-import-cells">
              {cells.map((cell, i) => (
                <div className="pet-import-cell" key={cell.key}>
                  <div className="pet-import-cell-thumb">
                    <CellThumb cell={cell} />
                  </div>
                  <select
                    className="input-field"
                    value={slots[i] ?? ""}
                    onChange={(e) => {
                      setSlots(
                        slots.map((s, j) => (j === i ? ((e.target.value || null) as CellAssignment) : s)),
                      );
                      // 预览是按旧的分配算出来的：改了就得作废，否则「保存」会存下那张旧图
                      setPreview(null);
                    }}
                  >
                    <option value="">{t("aiHub.petImportNone")}</option>
                    {SLOTS.map((slot) => (
                      <option key={slot} value={slot}>
                        {slotLabel(slot)}
                      </option>
                    ))}
                  </select>
                </div>
              ))}
            </div>
          )}

          {preview && preview.length > 0 && (
            <>
              <h3 className="settings-group">{t("aiHub.petImportPreview")}</h3>
              <div className="pet-import-preview">
                {preview.map((frame) => (
                  <img key={frame.name} src={frame.dataUrl} alt={frame.name} className="pet-import-preview-img" />
                ))}
              </div>
            </>
          )}

          {error && <p className="pet-import-error">{error}</p>}
        </div>
        <div className="modal-actions modal-actions--split">
          <div className="modal-actions-secondary">
            <button className="btn" onClick={() => void runPreview()} disabled={busy || assigned === 0}>
              <Wand2 size={14} />
              {t("aiHub.petImportPreview")}
            </button>
          </div>
          <div className="modal-actions-primary">
            <button className="btn" onClick={onClose} disabled={busy}>
              {tc("actions.cancel")}
            </button>
            <button className="btn btn-primary" onClick={() => void apply()} disabled={busy || assigned === 0}>
              {busy ? <Loader2 size={14} className="spin" /> : <ImagePlus size={14} />}
              {busy ? t("aiHub.petImportBusy") : t("aiHub.petImportRun")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
