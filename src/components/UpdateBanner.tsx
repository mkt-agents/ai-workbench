import { useEffect, useState } from "react";
import { useTranslation } from "react-i18next";
import { getVersion } from "@tauri-apps/api/app";
import { invoke } from "@tauri-apps/api/core";
import { DownloadCloud, X } from "lucide-react";
import { checkForUpdatesMultiSource } from "../lib/version";

const SKIP_KEY = "update-skip-version";

/**
 * Startup update check with a non-blocking bottom-right banner.
 * Silent unless a genuinely newer, non-skipped release is found —
 * unreachable sources (both Gitee and GitHub) never nag.
 */
export default function UpdateBanner() {
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
      try {
        if (localStorage.getItem(SKIP_KEY) === result.remote) return;
      } catch {
        /* storage unavailable: still show */
      }
      setInfo({ remote: result.remote, htmlUrl: result.htmlUrl });
    }, 1500);
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, []);

  if (!info || dismissed) return null;

  const open = () => {
    invoke("open_in_browser", { url: info.htmlUrl }).catch(() => window.open(info.htmlUrl, "_blank"));
  };
  const skip = () => {
    try {
      localStorage.setItem(SKIP_KEY, info.remote);
    } catch {
      /* ignore */
    }
    setDismissed(true);
  };

  return (
    <div className="update-banner" role="status">
      <span className="update-banner-icon">
        <DownloadCloud size={16} />
      </span>
      <span className="update-banner-text">{t("available", { remote: info.remote })}</span>
      <button type="button" className="btn btn-primary btn-small" onClick={open}>
        {t("view")}
      </button>
      <button type="button" className="btn btn-secondary btn-small" onClick={skip}>
        {t("skipVersion")}
      </button>
      <button type="button" className="update-banner-close" onClick={() => setDismissed(true)} aria-label={t("close")}>
        <X size={14} />
      </button>
    </div>
  );
}
