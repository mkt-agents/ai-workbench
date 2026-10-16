import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { CheckCircle2, DownloadCloud, X } from "lucide-react";
import {
  checkForUpdatesMultiSource,
  clearSkippedVersion,
  isVersionSkipped,
  takeInstalledUpdate,
} from "../lib/version";

type Props = {
  /** 点「查看更新」：由 App 打开升级弹窗（日志预览 + 应用内升级）。 */
  onOpen: (target: { version: string; htmlUrl: string }) => void;
  /** 设置页手动检查发现的新版：与启动检查共用同一条浮框。 */
  external?: { version: string; htmlUrl: string } | null;
  onClearExternal?: () => void;
};

/**
 * Startup update check with a non-blocking bottom-right banner.
 * Silent unless a genuinely newer, non-skipped release is found —
 * every source failing (GitHub / jsDelivr) never nags.
 * 设置页「检查更新」的结果也走这条浮框（external）。
 *
 * 升级闭环的最后一环：上一轮升级重启后，凭 localStorage 的 pending 版本锚点
 * 挂出「已升级到 vX」确认横幅（10s 自动消失）；没装上则静默清锚点，
 * 让下面常规的「发现新版本」横幅说明现状。
 */
export default function UpdateBanner({ onOpen, external, onClearExternal }: Props) {
  const { t } = useTranslation("update");
  const [info, setInfo] = useState<{ remote: string; htmlUrl: string } | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const [installed, setInstalled] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(async () => {
      let local = "0.0.0";
      try {
        local = await getVersion();
      } catch {
        /* plain-browser dev: keep placeholder */
      }
      const done = takeInstalledUpdate(local);
      if (done) {
        // 已装上：顺带清掉「跳过此版本」，别让它压住之后同一版本的提示
        clearSkippedVersion();
        if (!cancelled) setInstalled(done);
        return;
      }
      const result = await checkForUpdatesMultiSource(local);
      if (cancelled || result.status !== "updateAvailable") return;
      if (isVersionSkipped(result.remote)) return;
      setInfo({ remote: result.remote, htmlUrl: result.htmlUrl });
    }, 1500);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    if (!installed) return;
    const timer = setTimeout(() => setInstalled(null), 10_000);
    return () => clearTimeout(timer);
  }, [installed]);

  // 手动「检查更新」是用户主动操作：盖过已关闭的启动横幅，重新弹出。
  useEffect(() => {
    if (external) setDismissed(false);
  }, [external]);

  if (installed) {
    return (
      <div className="update-banner update-banner--success" role="status">
        <span className="update-banner-icon">
          <CheckCircle2 size={16} />
        </span>
        <span className="update-banner-text">{t("installed", { version: installed })}</span>
        <button
          type="button"
          className="update-banner-close"
          onClick={() => setInstalled(null)}
          aria-label={t("close")}
        >
          <X size={14} />
        </button>
      </div>
    );
  }

  // 手动检查优先；否则用启动检查结果（字段统一成 version）。
  const active = external
    ?? (info ? { version: info.remote, htmlUrl: info.htmlUrl } : null);

  if (!active || dismissed) return null;

  return (
    <div className="update-banner" role="status">
      <span className="update-banner-icon">
        <DownloadCloud size={16} />
      </span>
      <span className="update-banner-text">{t("available", { remote: active.version })}</span>
      <button
        type="button"
        className="btn btn-primary btn-small"
        onClick={() => onOpen({ version: active.version, htmlUrl: active.htmlUrl })}
      >
        {t("view")}
      </button>
      <button
        type="button"
        className="update-banner-close"
        onClick={() => {
          setDismissed(true);
          if (external) onClearExternal?.();
        }}
        aria-label={t("close")}
      >
        <X size={14} />
      </button>
    </div>
  );
}
