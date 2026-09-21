import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { FolderGit, Loader2 } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { pathKey } from "../core/pathUtils";
import ModalTitleRow from "./ModalTitleRow";

type Props = {
  /** 扫描根目录 */
  rootPath: string;
  /** 首次扫描结果（调用方已扫好） */
  initialRepos: { path: string; name: string }[];
  onClose: () => void;
  /** 添加完成（返回实际新增数） */
  onAdded: (addedCount: number) => void;
};

/**
 * 多项目文件夹扫描结果弹窗：勾选要收藏的仓库，批量添加。
 * 默认全选未收藏项；已收藏的显示徽章并禁用；支持「再扫一层」。
 */
function ScanReposModal({ rootPath, initialRepos, onClose, onAdded }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");

  const recentProjects = useGlobalStore((s) => s.recentProjects);
  const addRecentProjects = useGlobalStore((s) => s.addRecentProjects);
  const invokeGitScanRepos = useGlobalStore((s) => s.invokeGitScanRepos);

  const [repos, setRepos] = useState(initialRepos);
  const [depth, setDepth] = useState(1);
  const [scanning, setScanning] = useState(false);
  const [adding, setAdding] = useState(false);
  // Only what is not collected yet starts checked — pre-checking everything made a
  // rescan look like "全选" and re-added repos whose stored path differs in spelling.
  const [selected, setSelected] = useState<Set<string>>(() => {
    const existing = new Set(useGlobalStore.getState().recentProjects.map((p) => pathKey(p.path)));
    return new Set(initialRepos.filter((r) => !existing.has(pathKey(r.path))).map((r) => r.path));
  });
  const [error, setError] = useState("");

  const existingPaths = useMemo(
    () => new Set(recentProjects.map((p) => pathKey(p.path))),
    [recentProjects]
  );
  const isExisting = (path: string) => existingPaths.has(pathKey(path));

  const selectable = repos.filter((r) => !isExisting(r.path));
  const selectedCount = repos.filter((r) => selected.has(r.path) && !isExisting(r.path)).length;

  const toggle = (path: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(path)) {
        next.delete(path);
      } else {
        next.add(path);
      }
      return next;
    });
  };

  const selectAll = () => setSelected(new Set(selectable.map((r) => r.path)));
  const selectNone = () => setSelected(new Set());

  /** 再扫一层：depth+1 重扫并合并（保留已勾选状态） */
  const scanDeeper = async () => {
    if (scanning) return;
    setScanning(true);
    setError("");
    try {
      const nextDepth = depth + 1;
      const found = await invokeGitScanRepos(rootPath, nextDepth);
      const known = new Set(repos.map((r) => r.path));
      const merged = [...repos, ...found.filter((r) => !known.has(r.path))];
      setRepos(merged);
      setSelected((prev) => {
        const next = new Set(prev);
        for (const r of found) {
          if (!isExisting(r.path)) next.add(r.path);
        }
        return next;
      });
      setDepth(nextDepth);
    } catch (e) {
      setError(String(e));
    } finally {
      setScanning(false);
    }
  };

  const handleAdd = async () => {
    if (adding || selectedCount === 0) return;
    setAdding(true);
    setError("");
    try {
      const toAdd = repos
        .filter((r) => selected.has(r.path) && !isExisting(r.path))
        .map((r) => ({ path: r.path, name: r.name }));
      const added = await addRecentProjects(toAdd);
      onAdded(added);
    } catch (e) {
      setError(String(e));
      setAdding(false);
    }
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal scan-repos-modal" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow
          title={t("scan.title", { count: repos.length })}
          onClose={onClose}
        />

        <div className="scan-repos-toolbar">
          <button type="button" className="btn btn-secondary btn-small" onClick={selectAll} disabled={selectable.length === 0}>
            {t("scan.selectAll")}
          </button>
          <button type="button" className="btn btn-secondary btn-small" onClick={selectNone} disabled={selectedCount === 0}>
            {t("scan.selectNone")}
          </button>
          <span className="runtime-muted scan-repos-hint">
            {existingPaths.size > 0
              ? t("scan.favoritesHint", { count: existingPaths.size })
              : ""}
          </span>
          <button
            type="button"
            className="btn btn-secondary btn-small"
            onClick={scanDeeper}
            disabled={scanning || adding}
            title={t("scan.deeperTitle")}
          >
            {scanning ? <Loader2 size={12} className="spin" /> : null}
            {t("scan.deeper")}
          </button>
        </div>

        <ul className="scan-repos-list">
          {repos.map((r) => {
            const favorited = isExisting(r.path);
            const checked = selected.has(r.path);
            return (
              <li
                key={pathKey(r.path)}
                className={`scan-repos-item ${favorited ? "is-existing" : ""}`}
                onClick={() => !favorited && toggle(r.path)}
              >
                <input
                  type="checkbox"
                  checked={checked}
                  disabled={favorited}
                  onChange={() => !favorited && toggle(r.path)}
                  onClick={(e) => e.stopPropagation()}
                />
                <FolderGit size={14} className="scan-repos-icon" />
                <span className="scan-repos-name" title={r.name}>
                  {r.name}
                </span>
                <span className="scan-repos-path" title={r.path}>
                  {r.path}
                </span>
                {favorited && (
                  <span className="runtime-badge active">{t("scan.alreadyFavorited")}</span>
                )}
              </li>
            );
          })}
        </ul>

        {error && <div className="repos-modal-error">{error}</div>}

        <div className="modal-actions">
          <button className="btn btn-secondary" onClick={onClose} disabled={adding}>
            {tc("actions.cancel")}
          </button>
          <button
            className="btn btn-primary"
            onClick={handleAdd}
            disabled={adding || selectedCount === 0}
          >
            {adding ? <Loader2 size={14} className="spin" /> : null}
            {t("scan.addSelected", { count: selectedCount })}
          </button>
        </div>
      </div>
    </div>
  );
}

export default ScanReposModal;
