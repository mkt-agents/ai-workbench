import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import {
  AlertTriangle,
  ArrowRight,
  Download,
  ExternalLink,
  FileArchive,
  Loader2,
  RefreshCw,
  Sparkles,
  XCircle,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import ModalTitleRow from "./ModalTitleRow";
import MarkdownView from "./MarkdownView";
import { useConfirm } from "./ConfirmModal";
import useTauriEvent from "../hooks/useTauriEvent";
import { parseInvokeError } from "../hooks/useInvokeError";
import { skipVersion } from "../lib/version";
import type { ReleaseInfo, UpdateProgress } from "../core/types";

type Props = {
  /** 要升级到的版本（三源探测拿到的 tag，可能带 v 前缀）。 */
  version: string;
  /** 发布页地址，探测兜底源给的；后端没返回 htmlUrl 时用它。 */
  fallbackHtmlUrl?: string;
  onClose: () => void;
};

type Phase = "loading" | "ready" | "downloading" | "installing" | "restarting" | "failed";

function formatSize(bytes: number): string {
  const kb = bytes / 1024;
  if (kb < 1024) return `${kb.toFixed(0)} KB`;
  return `${(kb / 1024).toFixed(1)} MB`;
}

/**
 * 升级弹窗：先给人看这个版本改了什么（release 正文），再决定下不下载。
 *
 * 下载/安装全在 Rust 侧（`update_commands.rs`）：这里是 loading → ready →
 * downloading → installing → restarting 的可见状态机，每条失败路径都必须留一个
 * **可点的出口**（重试 / 改用浏览器下载），不是只甩一段红字。
 *
 * 进度文案在这里拼：后端只发 `{stage, percent, downloaded, total}` 四个数，
 * 中文字串留在后端会让 en-US 界面漏出中文。
 */
function UpdateModal({ version, fallbackHtmlUrl, onClose }: Props) {
  const { t } = useTranslation("update");
  const confirm = useConfirm();

  const invokeUpdateReleaseInfo = useGlobalStore((s) => s.invokeUpdateReleaseInfo);
  const invokeUpdateDownload = useGlobalStore((s) => s.invokeUpdateDownload);
  const invokeUpdateInstall = useGlobalStore((s) => s.invokeUpdateInstall);
  const invokeCancelRequest = useGlobalStore((s) => s.invokeCancelRequest);

  const [phase, setPhase] = useState<Phase>("loading");
  const [info, setInfo] = useState<ReleaseInfo | null>(null);
  const [progress, setProgress] = useState<UpdateProgress | null>(null);
  const [failure, setFailure] = useState<{ code: string | null; message: string } | null>(null);
  const [currentVersion, setCurrentVersion] = useState<string | null>(null);
  const requestIdRef = useRef<string | null>(null);

  const targetVersion = info?.version || version;
  const htmlUrl = info?.htmlUrl || fallbackHtmlUrl || "";
  // NSIS setup.exe 优先；只有 release 没带 setup 资产时才回落到 MSI（安装侧自动换 msiexec）。
  const installerKind: "exe" | "msi" | null = info?.setupUrl ? "exe" : info?.msiUrl ? "msi" : null;
  const assetUrl = installerKind === "exe" ? info?.setupUrl ?? null : info?.msiUrl ?? null;
  const assetSize = installerKind === "exe" ? info?.setupSize ?? 0 : 0;
  const installerLabel = installerKind === "msi" ? t("installerMsi") : t("installerExe");
  const busy = phase === "downloading" || phase === "installing" || phase === "restarting";

  useEffect(() => {
    // 纯浏览器 dev 没有 Tauri IPC，取不到就不显示「从 v… 升级」。
    getVersion().then(setCurrentVersion).catch(() => {});
  }, []);

  const loadInfo = useCallback(async () => {
    setPhase("loading");
    setFailure(null);
    try {
      const data = await invokeUpdateReleaseInfo(version);
      setInfo(data);
      setPhase("ready");
    } catch (e) {
      // 取不到日志不等于不能升级：仍给浏览器出口，同时保留重试。
      const { code, message } = parseInvokeError(e);
      setInfo(null);
      setFailure({ code, message });
      setPhase(code === "UPDATE_CANCELLED" ? "ready" : "failed");
    }
  }, [invokeUpdateReleaseInfo, version]);

  useEffect(() => {
    void loadInfo();
  }, [loadInfo]);

  useTauriEvent<UpdateProgress>("app:update_progress", (event) => {
    setProgress(event.payload);
  }, []);

  const finishFailure = useCallback((e: unknown) => {
    const { code, message } = parseInvokeError(e);
    if (code === "UPDATE_CANCELLED") {
      setPhase("ready");
      setProgress(null);
      return;
    }
    setFailure({ code, message });
    setPhase("failed");
  }, []);

  const startUpgrade = async () => {
    if (!assetUrl || busy) return;
    const ok = await confirm({
      title: t("upgradeConfirmTitle"),
      message: t("upgradeConfirmMessage", {
        version: targetVersion,
        installer: installerLabel,
      }),
      warning: t("upgradeConfirmWarning"),
      icon: "warning",
      confirmText: t("upgrade"),
    });
    if (!ok) return;
    const requestId = `update-${crypto.randomUUID()}`;
    requestIdRef.current = requestId;
    setFailure(null);
    setProgress({ stage: "download", percent: 0, downloaded: 0, total: assetSize });
    setPhase("downloading");
    try {
      const file = await invokeUpdateDownload(requestId, assetUrl);
      setPhase("installing");
      await invokeUpdateInstall(file.path);
      // 成功即退出本应用：留在 restarting 态，让用户看到发生了什么。
      setPhase("restarting");
    } catch (e) {
      finishFailure(e);
    } finally {
      requestIdRef.current = null;
    }
  };

  const cancelDownload = () => {
    const id = requestIdRef.current;
    if (!id) return;
    void invokeCancelRequest(id).catch(() => {
      /* 取消失败也要给人退路：关闭按钮与取消本身都仍可用 */
    });
  };

  const skip = () => {
    skipVersion(targetVersion);
    onClose();
  };

  const openInBrowser = () => {
    if (!htmlUrl) return;
    // window.open 在 Tauri webview 里会被静默吞掉，走我们这个校验 http(s) 的命令。
    invoke("open_in_browser", { url: htmlUrl }).catch(() => window.open(htmlUrl, "_blank"));
  };

  const downloading = phase === "downloading";
  // 有稳定错误码就用码对应的文案（已按源区分限流/截断/拦截）；没有码时不把人看不懂的
  // 异常原文当主文案，主文案给通用引导，原文降级成一行次要细节。
  const translated = failure?.code ? t(`err.${failure.code}`) : "";
  const hasTranslation = !!translated && translated !== `err.${failure?.code}`;
  const errorText = hasTranslation ? translated : t("errorGeneric");
  const percent = phase === "restarting" ? 100 : progress?.percent ?? 0;
  const stageText =
    phase === "installing" ? t("verifying") : phase === "restarting" ? t("restarting") : t("downloading");
  const bytesText =
    progress && progress.downloaded > 0
      ? progress.total > 0
        ? t("progressBytes", {
            done: formatSize(progress.downloaded),
            total: formatSize(progress.total),
          })
        : t("progressBytesOpen", { done: formatSize(progress.downloaded) })
      : "";

  return (
    <div className="modal-overlay" onClick={busy ? undefined : onClose}>
      <div className="modal update-modal" role="dialog" aria-modal="true" onClick={(e) => e.stopPropagation()}>
        <ModalTitleRow
          title={t("title", { version: targetVersion })}
          onClose={onClose}
          disabled={busy}
        />

        {info && (
          <div className="update-modal-meta">
            <span className="update-modal-meta-item">
              <ArrowRight size={12} />
              {currentVersion && currentVersion !== targetVersion
                ? t("metaFrom", { from: currentVersion, to: targetVersion })
                : t("metaTo", { to: targetVersion })}
            </span>
            {installerKind && (
              <span className="update-modal-meta-item">
                <FileArchive size={12} />
                {installerLabel}
                {assetSize > 0 ? ` · ${t("installerSize", { size: (assetSize / 1024 / 1024).toFixed(1) })}` : ""}
              </span>
            )}
          </div>
        )}

        {phase === "loading" && (
          <div className="update-modal-empty">
            <Loader2 size={18} className="spin" />
            <span>{t("loadingNotes")}</span>
          </div>
        )}

        {info?.notes ? (
          <div className="update-modal-notes">
            <div className="update-modal-notes-head">
              <Sparkles size={12} />
              <span>{t("changelogTitle")}</span>
            </div>
            <MarkdownView text={info.notes} />
          </div>
        ) : phase !== "loading" && phase !== "failed" ? (
          <div className="update-modal-empty">
            <AlertTriangle size={16} />
            <span>
              {info?.degraded ? t("notesRateLimited") : failure ? t("notesFailed") : t("notesEmpty")}
            </span>
          </div>
        ) : null}

        {info && !installerKind && phase === "ready" && (
          <div className="update-modal-hint">{t("assetMissing")}</div>
        )}

        {phase === "failed" && failure && (
          <div className="update-modal-error" role="alert">
            <XCircle size={14} />
            <div className="update-modal-error-text">
              <span className="update-modal-error-title">{t("errorTitle")}</span>
              <span>{errorText}</span>
              {failure.message && <span className="update-modal-error-detail">{failure.message}</span>}
            </div>
          </div>
        )}

        {(downloading || phase === "installing" || phase === "restarting") && (
          <div className="update-modal-progress">
            <div className="update-modal-progress-head">
              <span>{stageText}</span>
              <span className="update-modal-progress-pct">{percent}%</span>
            </div>
            <div className="dsh-progress-bar">
              <div className="dsh-progress-fill" style={{ width: `${percent}%` }} />
            </div>
            {bytesText && <div className="update-modal-progress-sub">{bytesText}</div>}
          </div>
        )}

        <div className="modal-actions modal-actions--split">
          <div className="modal-actions-secondary">
            {downloading ? (
              <button type="button" className="btn btn-secondary btn-small" onClick={cancelDownload}>
                {t("cancelDownload")}
              </button>
            ) : (
              <>
                {phase !== "failed" && (
                  <button type="button" className="btn btn-secondary btn-small" onClick={skip} disabled={busy}>
                    {t("skipVersion")}
                  </button>
                )}
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={openInBrowser}
                  disabled={!htmlUrl || busy}
                >
                  <ExternalLink size={13} />
                  {t("openInBrowser")}
                </button>
              </>
            )}
          </div>
          <div className="modal-actions-primary">
            {phase === "failed" ? (
              <button type="button" className="btn btn-primary btn-small" onClick={() => void loadInfo()}>
                <RefreshCw size={13} />
                {t("retry")}
              </button>
            ) : (
              <button
                type="button"
                className="btn btn-primary btn-small"
                onClick={() => void startUpgrade()}
                disabled={busy || !assetUrl}
              >
                {downloading ? <Loader2 size={13} className="spin" /> : <Download size={13} />}
                {downloading ? t("downloading") : t("upgrade")}
              </button>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

export default UpdateModal;
