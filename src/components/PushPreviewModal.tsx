import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { AlertTriangle, GitBranch, Loader2, RotateCw } from "lucide-react";
import { useGlobalStore } from "../core/store";
import { GIT_CONCURRENCY, mapPool } from "../core/asyncPool";
import { pushTargetNote } from "../core/gitPushScope";
import { parseInvokeError } from "../hooks/useInvokeError";
import type { GitPendingCommits, RepoBatchItem } from "../core/types";
import ModalTitleRow from "./ModalTitleRow";

/** Files listed under one commit before the rest collapse into a count. */
const FILE_CAP = 12;

export type PushPreviewRow = {
  path: string;
  label: string;
  summary: RepoBatchItem;
  /**
   * Commit-then-push: the commit does not exist yet when this dialog opens,
   * so the caller passes the files about to be committed in this repo — they
   * show up as a pseudo-commit row instead of an empty "nothing to push" line.
   */
  pendingFiles?: string[];
};

/** A row with no piece yet is simply still reading. */
type Piece = { state: "ok"; data: GitPendingCommits } | { state: "error"; message: string };

type Props = {
  rows: PushPreviewRow[];
  /** Commit-then-push: that commit does not exist yet, so it is not in these lists. */
  addedCommits?: number;
  onClose: () => void;
  onConfirm: () => void;
};

function byPath(a: string, b: string) {
  return a < b ? -1 : a > b ? 1 : 0;
}

function PushPreviewModal({ rows, addedCommits = 0, onClose, onConfirm }: Props) {
  const { t } = useTranslation("git");
  const { t: tc } = useTranslation("common");
  const invokeGitPendingCommits = useGlobalStore((s) => s.invokeGitPendingCommits);

  // 进度只有一个来源：某仓有 piece 就是读完了。
  const [pieces, setPieces] = useState<Record<string, Piece>>({});
  /** `rows` is rebuilt by the parent on every render; only its paths drive a re-read. */
  const rowsRef = useRef(rows);
  rowsRef.current = rows;
  const pathsKey = useMemo(() => rows.map((r) => r.path).join("|"), [rows]);

  useEffect(() => {
    let live = true;
    setPieces({});
    void mapPool(rowsRef.current, GIT_CONCURRENCY, async (row) => {
      try {
        const data = await invokeGitPendingCommits(row.path);
        if (live) setPieces((prev) => ({ ...prev, [row.path]: { state: "ok", data } }));
      } catch (e) {
        // Tauri 侧的错误是字符串（可能带 [CODE] 前缀），直接 String(e) 会多出 "Error: "。
        if (live)
          setPieces((prev) => ({
            ...prev,
            [row.path]: { state: "error", message: parseInvokeError(e).message },
          }));
      }
    });
    return () => {
      live = false;
    };
  }, [pathsKey, invokeGitPendingCommits]);

  // Esc 由 ModalTitleRow 统一处理（它认最上层遮罩），这里不再挂第二个监听器。

  const notes = rows.map((row) => pushTargetNote(row.summary, t, { addedCommits }));
  const riskyCount = notes.filter((n) => n.risky).length;
  const settled = rows.filter((row) => pieces[row.path]);
  const failed = rows.filter((row) => pieces[row.path]?.state === "error");
  const commitTotal = rows.reduce((n, row) => {
    const p = pieces[row.path];
    // 提交即推送的流程里，即将创建的那条也算一条（下面会以 ⟳ 行列出）。
    const pending = (row.pendingFiles?.length ?? 0) > 0 ? 1 : 0;
    return n + pending + (p && p.state === "ok" ? p.data.total : 0);
  }, 0);
  const dataOf = (path: string) => {
    const p = pieces[path];
    return p && p.state === "ok" ? p.data : null;
  };

  const scopeTip = (d: GitPendingCommits) =>
    d.scope === "upstream"
      ? t("pushPreview.scopeUpstream", { name: d.basis })
      : d.scope === "remoteTracking"
        ? t("pushPreview.scopeTracking", { name: d.basis })
        : d.scope === "unpushed"
          ? t("pushPreview.scopeUnpushed")
          : t("pushPreview.scopeNone");

  return (
    <div className="modal-overlay">
      <div className="modal push-preview-modal">
        <ModalTitleRow
          title={t("pushPreview.title")}
          onClose={onClose}
          badge={
            <span className="card-title-badge">
              {t("pushPreview.selectedRepos", { count: rows.length })}
            </span>
          }
        />

        <div className="pp-body">
          <div className="pp-summary">
            <span>
              {settled.length < rows.length
                ? t("pushPreview.reading", { done: settled.length, total: rows.length })
                : t("pushPreview.totalCommits", { commits: commitTotal, repos: rows.length })}
            </span>
            {failed.length > 0 && (
              <span className="pp-failed">{t("pushPreview.readFailed", { count: failed.length })}</span>
            )}
          </div>
          {addedCommits > 0 && !rows.some((r) => (r.pendingFiles?.length ?? 0) > 0) && (
            <p className="pp-added">{t("pushPreview.addedNote", { count: addedCommits })}</p>
          )}
          {riskyCount > 0 && (
            <p className="pp-warning">
              <AlertTriangle size={13} />
              <span>{t("batch.pushWarning")}</span>
            </p>
          )}

          <div className="pp-list">
            {rows.map((row, i) => {
              const data = dataOf(row.path);
              const piece = pieces[row.path];
              const pending = [...(row.pendingFiles ?? [])].sort(byPath);
              const empty = data && data.commits.length === 0 && pending.length === 0;
              return (
                <section className={`pp-repo${notes[i].risky ? " is-risky" : ""}`} key={row.path}>
                  <div className="pp-repo-head">
                    <GitBranch size={13} className="pp-repo-icon" />
                    <span className="pp-repo-name" title={row.path}>
                      {row.label}
                    </span>
                    <span className="pp-repo-branch" title={row.summary.branch}>
                      {row.summary.branch}
                    </span>
                    <span
                      className="pp-repo-note"
                      title={data ? scopeTip(data) : undefined}
                    >
                      {notes[i].text}
                    </span>
                  </div>

                  {!piece && (
                    <div className="pp-line">
                      <Loader2 size={13} className="spin" />
                      <span>{t("pushPreview.readingRepo")}</span>
                    </div>
                  )}
                  {piece?.state === "error" && (
                    <div className="pp-line pp-line-error">{piece.message}</div>
                  )}
                  {empty && (
                    <div className="pp-line">
                      {data?.scope === "noCommits"
                        ? t("pushPreview.noCommitsYet")
                        : t("pushPreview.noneHere")}
                    </div>
                  )}
                  {(data?.commits.length ?? 0) > 0 || pending.length > 0 ? (
                    <ul className="pp-commits">
                      {data?.commits.map((c) => {
                        const files = [...c.files].sort(byPath);
                        const rest = files.length - FILE_CAP;
                        return (
                          <li className="pp-commit" key={c.hash}>
                            <div className="pp-commit-top">
                              <code className="pp-hash">{c.hash}</code>
                              <span className="pp-subject" title={c.subject}>
                                {c.subject || t("pushPreview.noSubject")}
                              </span>
                              <span className="pp-when">{c.when}</span>
                            </div>
                            {files.length > 0 && (
                              <ul className="pp-files">
                                {files.slice(0, FILE_CAP).map((f) => (
                                  <li key={f}>
                                    <code title={f}>{f}</code>
                                  </li>
                                ))}
                                {rest > 0 && (
                                  <li className="pp-more">{t("pushPreview.moreFiles", { count: rest })}</li>
                                )}
                              </ul>
                            )}
                          </li>
                        );
                      })}
                      {pending.length > 0 && (
                        <li className="pp-commit is-pending">
                          <div className="pp-commit-top">
                            <RotateCw size={11} className="pp-pending-icon" />
                            <span className="pp-subject">
                              {t("pushPreview.pendingCommit", { count: pending.length })}
                            </span>
                          </div>
                          <ul className="pp-files">
                            {pending.slice(0, FILE_CAP).map((f) => (
                              <li key={f}>
                                <code title={f}>{f}</code>
                              </li>
                            ))}
                            {pending.length > FILE_CAP && (
                              <li className="pp-more">
                                {t("pushPreview.moreFiles", { count: pending.length - FILE_CAP })}
                              </li>
                            )}
                          </ul>
                        </li>
                      )}
                      {data?.truncated && (
                        <li className="pp-line pp-trunc">
                          {t("pushPreview.truncated", { shown: data.commits.length, total: data.total })}
                        </li>
                      )}
                    </ul>
                  ) : null}
                </section>
              );
            })}
          </div>
        </div>

        <div className="modal-actions pp-actions">
          <button type="button" className="btn btn-secondary" onClick={onClose}>
            {tc("actions.cancel")}
          </button>
          <button
            type="button"
            className="btn btn-primary"
            onClick={onConfirm}
            disabled={rows.length === 0}
          >
            {t("batch.push")} · {rows.length}
          </button>
        </div>
      </div>
    </div>
  );
}

export default PushPreviewModal;

/**
 * Awaitable push preview, same shape as `useConfirm` but with a real body: the
 * caller keeps its `if (!(await ask(...))) return;` flow and the dialog mounts
 * where the hook is used, so two pages can hold one each.
 */
export function usePushPreview() {
  const [pending, setPending] = useState<{ rows: PushPreviewRow[]; addedCommits: number } | null>(
    null
  );
  const resolveRef = useRef<((ok: boolean) => void) | null>(null);

  const askPushPreview = (rows: PushPreviewRow[], addedCommits = 0) =>
    new Promise<boolean>((resolve) => {
      // 连着问两次：上一个等待者按取消结算，否则它的 Promise 永远悬着。
      resolveRef.current?.(false);
      resolveRef.current = resolve;
      setPending({ rows, addedCommits });
    });

  const settle = (ok: boolean) => {
    const resolve = resolveRef.current;
    resolveRef.current = null;
    setPending(null);
    resolve?.(ok);
  };

  const pushPreviewDialog = pending ? (
    <PushPreviewModal
      rows={pending.rows}
      addedCommits={pending.addedCommits}
      onClose={() => settle(false)}
      onConfirm={() => settle(true)}
    />
  ) : null;

  return { askPushPreview, pushPreviewDialog };
}
