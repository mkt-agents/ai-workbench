import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { DownloadCloud, X } from "lucide-react";
import { checkForUpdatesMultiSource, isVersionSkipped } from "../lib/version";

type Props = {
  /** 点「查看更新」：由 App 打开升级弹窗（日志预览 + 应用内升级）。 */
  onOpen: (target: { version: string; htmlUrl: string }) => void;
};

/**
 * Startup update check with a non-blocking bottom-right banner.
 * Silent unless a genuinely newer, non-skipped release is found —
 * every source failing (Gitee / GitHub / jsDelivr) never nags.
 */
export default function UpdateBanner({ onOpen }: Props) {
  const { t } = useTranslation("update");
  const [info, setInfo] = useState<{ remote: string; htmlUrl: string } | null>(null);
  const [dismissed, setDismissed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    const timer = setTimeout(async () => {
      let local = "0.0.0";
      try {
        local = await getVersion();
      } catch {
        /* plain-browser dev: keep placeholder */
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

  if (!info || dismissed) return null;

  return (
    <div className="update-banner" role="status">
      <span className="update-banner-icon">
        <DownloadCloud size={16} />
      </span>
      <span className="update-banner-text">{t("available", { remote: info.remote })}</span>
      <button
        type="button"
        className="btn btn-primary btn-small"
        onClick={() => onOpen({ version: info.remote, htmlUrl: info.htmlUrl })}
      >
        {t("view")}
      </button>
      <button
        type="button"
        className="update-banner-close"
        onClick={() => setDismissed(true)}
        aria-label={t("close")}
      >
        <X size={14} />
      </button>
    </div>
  );
}
