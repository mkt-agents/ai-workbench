import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import {
  Check,
  Copy,
  Download,
  Eye,
  EyeOff,
  ExternalLink,
  Globe,
  Loader2,
  Pencil,
  RefreshCw,
  Square,
  Trash2,
  XCircle,
} from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import type { CloudflaredNamedProfile } from "../core/types";

const RECENT_PORTS_KEY = "ai-workbench-cloudflared-recent-ports";
/** Pre-rename key: still read so recent ports survive the rename. */
const LEGACY_RECENT_PORTS_KEY = "wt-cloudflared-recent-ports";
const PROTOCOL_KEY = "ai-workbench-cloudflared-protocol";
const MAX_RECENT = 8;
const MAX_LOG_LINES = 400;

function loadProtocol(): string {
  try {
    const v = localStorage.getItem(PROTOCOL_KEY);
    return v === "http2" || v === "quic" ? v : "auto";
  } catch {
    return "auto";
  }
}

type CfStatus = {
  installed: boolean;
  version: string;
  path: string;
  message: string;
  customPath: boolean;
};

type TunnelStatus = {
  id: string;
  running: boolean;
  pid?: number | null;
  localUrl?: string | null;
  publicUrl?: string | null;
  mode?: string | null;
  profileId?: string | null;
};

type LogEntry = {
  id: string;
  tag: string;
  line: string;
  at: number;
};

function parseLogPayload(payload: unknown): LogEntry | null {
  const at = Date.now();
  if (typeof payload === "string") {
    const line = payload.trimEnd();
    if (!line) return null;
    const m = line.match(/^\[([^\]]+)\]\s*([\s\S]*)$/);
    if (m) {
      return { id: "", tag: m[1], line: m[2], at };
    }
    return { id: "", tag: "", line, at };
  }
  if (payload && typeof payload === "object") {
    const obj = payload as { id?: string; tag?: string; line?: string };
    const line = typeof obj.line === "string" ? obj.line.trimEnd() : "";
    if (!line && !obj.tag) return null;
    return {
      id: typeof obj.id === "string" ? obj.id : "",
      tag: typeof obj.tag === "string" ? obj.tag : "",
      line: line || String(obj.line ?? ""),
      at,
    };
  }
  return null;
}

function formatLogLine(entry: LogEntry): string {
  return entry.tag ? `[${entry.tag}] ${entry.line}` : entry.line;
}

function normalizePublicHost(hostname: string): string {
  return hostname.replace(/^https?:\/\//i, "").replace(/\/$/, "").toLowerCase();
}

function quickIdForLocalUrl(local: string): string {
  return `quick:${buildLocalUrl(local) || local.trim()}`;
}

function loadRecentPorts(): number[] {
  try {
    const raw =
      localStorage.getItem(RECENT_PORTS_KEY) ??
      localStorage.getItem(LEGACY_RECENT_PORTS_KEY);
    if (!raw) return [3000, 5173, 8080];
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return [3000, 5173, 8080];
    return parsed
      .map((n) => Number(n))
      .filter((n) => Number.isInteger(n) && n > 0 && n < 65536)
      .slice(0, MAX_RECENT);
  } catch {
    return [3000, 5173, 8080];
  }
}

function saveRecentPort(port: number) {
  const next = [port, ...loadRecentPorts().filter((p) => p !== port)].slice(0, MAX_RECENT);
  localStorage.setItem(RECENT_PORTS_KEY, JSON.stringify(next));
  return next;
}

function buildLocalUrl(input: string): string {
  const trimmed = input.trim();
  if (!trimmed) return "";
  if (/^https?:\/\//i.test(trimmed)) return trimmed;
  if (/^\d+$/.test(trimmed)) return `http://localhost:${trimmed}`;
  return `http://${trimmed}`;
}

function extractPort(url: string): number | null {
  try {
    const u = new URL(buildLocalUrl(url) || "http://localhost");
    if (u.port) return Number(u.port);
    if (u.protocol === "https:") return 443;
    return 80;
  } catch {
    return null;
  }
}

function maskToken(token?: string): string {
  if (!token || token.length <= 8) return "••••••••";
  return `${token.slice(0, 4)}••••${token.slice(-4)}`;
}

/** Format cloudflared --version build time: `...T11:16 UTC` → local `YYYY-MM-DD HH:mm`. */
function formatCloudflaredVersion(raw: string): string {
  return raw.replace(
    /built\s+(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})(?::\d{2})?\s*UTC/gi,
    (_match, date: string, time: string) => {
      const d = new Date(`${date}T${time}:00Z`);
      if (Number.isNaN(d.getTime())) {
        return `built ${date} ${time}`;
      }
      const pad = (n: number) => String(n).padStart(2, "0");
      const local = `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(
        d.getHours()
      )}:${pad(d.getMinutes())}`;
      return `built ${local}`;
    }
  );
}

function CloudflaredManager() {
  const { t } = useTranslation("cloudflared");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();

  const profiles = useGlobalStore((s) => s.cloudflaredProfiles);
  const loadCloudflaredProfiles = useGlobalStore((s) => s.loadCloudflaredProfiles);
  const addCloudflaredProfile = useGlobalStore((s) => s.addCloudflaredProfile);
  const updateCloudflaredProfile = useGlobalStore((s) => s.updateCloudflaredProfile);
  const deleteCloudflaredProfile = useGlobalStore((s) => s.deleteCloudflaredProfile);

  const invokeCloudflaredStatus = useGlobalStore((s) => s.invokeCloudflaredStatus);
  const invokeCloudflaredInstall = useGlobalStore((s) => s.invokeCloudflaredInstall);
  const invokeCloudflaredOpenDownload = useGlobalStore((s) => s.invokeCloudflaredOpenDownload);
  const invokeCloudflaredPickBinary = useGlobalStore((s) => s.invokeCloudflaredPickBinary);
  const invokeCloudflaredClearBinaryPath = useGlobalStore(
    (s) => s.invokeCloudflaredClearBinaryPath
  );
  const invokeCloudflaredPickConfig = useGlobalStore((s) => s.invokeCloudflaredPickConfig);
  const invokeCloudflaredStartQuickTunnel = useGlobalStore(
    (s) => s.invokeCloudflaredStartQuickTunnel
  );
  const invokeCloudflaredStartNamedTunnel = useGlobalStore(
    (s) => s.invokeCloudflaredStartNamedTunnel
  );
  const invokeCloudflaredStopTunnel = useGlobalStore((s) => s.invokeCloudflaredStopTunnel);
  const invokeCloudflaredStopAllTunnels = useGlobalStore((s) => s.invokeCloudflaredStopAllTunnels);
  const invokeCloudflaredTunnelStatus = useGlobalStore((s) => s.invokeCloudflaredTunnelStatus);
  const invokeCloudflaredSetupNewDomain = useGlobalStore((s) => s.invokeCloudflaredSetupNewDomain);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);

  const [status, setStatus] = useState<CfStatus | null>(null);
  const [tunnels, setTunnels] = useState<TunnelStatus[]>([]);
  const [localUrl, setLocalUrl] = useState("http://localhost:3000");
  const [recentPorts, setRecentPorts] = useState<number[]>(() => loadRecentPorts());
  /** Edge protocol for both quick and named tunnels; "auto" = cloudflared default. */
  const [protocol, setProtocol] = useState<string>(() => loadProtocol());
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [logFilterId, setLogFilterId] = useState<string>("all");
  const [busy, setBusy] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(
    null
  );
  const [showModal, setShowModal] = useState(false);
  const [editing, setEditing] = useState<CloudflaredNamedProfile | null>(null);
  const [form, setForm] = useState({
    name: "",
    hostname: "",
    localUrl: "http://localhost:3000",
    authMode: "config" as "token" | "config",
    token: "",
    configPath: "",
  });
  const [showFormToken, setShowFormToken] = useState(false);
  const [revealedTokenIds, setRevealedTokenIds] = useState<Set<string>>(() => new Set());
  const [logQuery, setLogQuery] = useState("");
  const [autoScroll, setAutoScroll] = useState(true);
  const logEndRef = useRef<HTMLDivElement>(null);

  const runningTunnels = tunnels.filter((t) => t.running);
  const quickTunnels = runningTunnels.filter((t) => t.mode === "quick");
  const namedTunnels = runningTunnels.filter((t) => t.mode === "named");
  const currentQuickId = quickIdForLocalUrl(localUrl);
  const activeQuick = quickTunnels.find(
    (t) => t.id === currentQuickId || t.localUrl === buildLocalUrl(localUrl)
  );

  const findNamedTunnel = useCallback(
    (profile: CloudflaredNamedProfile) => {
      const host = normalizePublicHost(profile.hostname);
      return namedTunnels.find((t) => {
        if (t.profileId === profile.id || t.id === profile.id) return true;
        const pub = t.publicUrl ? normalizePublicHost(t.publicUrl) : "";
        return pub === host || pub === `https://${host}`.replace(/^https?:\/\//i, "");
      });
    },
    [namedTunnels]
  );

  const logSourceTabs = (() => {
    const byId = new Map<string, { id: string; tag: string; label: string }>();
    for (const tn of runningTunnels) {
      const tag =
        tn.mode === "quick"
          ? `quick:${extractPort(tn.localUrl || "") ?? "url"}`
          : `named:${normalizePublicHost(tn.publicUrl || tn.localUrl || tn.id)}`;
      const label =
        tn.mode === "quick"
          ? tn.localUrl || tag
          : tn.publicUrl?.replace(/^https?:\/\//i, "") || tag;
      byId.set(tn.id, { id: tn.id, tag, label });
    }
    for (const entry of logs) {
      if (!entry.id || byId.has(entry.id)) continue;
      byId.set(entry.id, {
        id: entry.id,
        tag: entry.tag || entry.id,
        label: entry.tag || entry.id,
      });
    }
    return Array.from(byId.values());
  })();
  const logSourceTabIds = logSourceTabs.map((s) => s.id).join("\0");

  const filteredTag =
    logFilterId === "all"
      ? null
      : logSourceTabs.find((s) => s.id === logFilterId)?.tag ?? null;
  const tunnelFilteredLogs =
    logFilterId === "all"
      ? logs
      : logs.filter(
          (e) =>
            e.id === logFilterId ||
            (!e.id && !!filteredTag && e.tag === filteredTag)
        );
  // Free-text filter on top of the per-tunnel filter.
  const logQueryNorm = logQuery.trim().toLowerCase();
  const visibleLogs = logQueryNorm
    ? tunnelFilteredLogs.filter((e) =>
        formatLogLine(e).toLowerCase().includes(logQueryNorm)
      )
    : tunnelFilteredLogs;

  useEffect(() => {
    if (logFilterId === "all") return;
    if (!logSourceTabIds.split("\0").includes(logFilterId)) {
      setLogFilterId("all");
    }
  }, [logFilterId, logSourceTabIds]);

  useEffect(() => {
    if (!autoScroll) return;
    logEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [visibleLogs, autoScroll]);

  // Esc closes the binding modal, matching the overlay-click affordance.
  useEffect(() => {
    if (!showModal) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setShowModal(false);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [showModal]);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 5000);
  }, []);

  const handleProtocolChange = (value: string) => {
    setProtocol(value);
    try {
      localStorage.setItem(PROTOCOL_KEY, value);
    } catch {
      /* private mode: the choice just doesn't persist */
    }
  };

  const refreshStatus = useCallback(async () => {
    try {
      const s = await invokeCloudflaredStatus();
      setStatus(s);
      const list = await invokeCloudflaredTunnelStatus();
      setTunnels(Array.isArray(list) ? list : []);
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    }
  }, [invokeCloudflaredStatus, invokeCloudflaredTunnelStatus, showMsg, t]);

  useEffect(() => {
    refreshStatus();
    loadCloudflaredProfiles().catch(() => {});
  }, [refreshStatus, loadCloudflaredProfiles]);

  /** A spawn succeeding does not mean the tunnel survived: bad ingress or a stale
      token kills cloudflared seconds later. Re-check once so the success toast
      becomes the error the user actually needs. */
  const recheckTunnelAlive = useCallback(
    (id: string) => {
      setTimeout(() => {
        void (async () => {
          try {
            const list = await invokeCloudflaredTunnelStatus();
            if (!list.some((t) => t.id === id)) {
              showMsg("error", t("startDiedHint"));
              await refreshStatus();
            }
          } catch {
            /* ignore — the poller covers the rest */
          }
        })();
      }, 6000);
    },
    [invokeCloudflaredTunnelStatus, refreshStatus, showMsg, t]
  );

  // cloudflared can die on its own (invalid token, edge unreachable, killed
  // elsewhere). Poll while anything is running so a dead process leaves the UI
  // promptly — a stale "running" card is exactly what makes a hostname answer
  // 1033 Argo Tunnel error while the user thinks the tunnel is up.
  useEffect(() => {
    if (runningTunnels.length === 0) return;
    const timer = setInterval(() => {
      invokeCloudflaredTunnelStatus()
        .then((list) => setTunnels(Array.isArray(list) ? list : []))
        .catch(() => {});
    }, 5000);
    return () => clearInterval(timer);
  }, [runningTunnels.length, invokeCloudflaredTunnelStatus]);

  useEffect(() => {
    let unLog: (() => void) | undefined;
    let unUrl: (() => void) | undefined;
    let cancelled = false;

    (async () => {
      unLog = await listen<unknown>("cloudflared-log", (event) => {
        if (cancelled) return;
        const entry = parseLogPayload(event.payload);
        if (!entry) return;
        setLogs((prev) => [...prev, entry].slice(-MAX_LOG_LINES));
      });
      unUrl = await listen<unknown>("cloudflared-url", (event) => {
        if (cancelled) return;
        const payload = event.payload;
        let id: string | undefined;
        let url: string | undefined;
        if (typeof payload === "string") {
          url = payload.trim();
        } else if (payload && typeof payload === "object") {
          const obj = payload as { id?: string; url?: string };
          id = obj.id;
          url = typeof obj.url === "string" ? obj.url.trim() : undefined;
        }
        if (!url) return;
        setTunnels((prev) => {
          if (id) {
            const found = prev.some((t) => t.id === id);
            if (found) {
              return prev.map((t) =>
                t.id === id ? { ...t, running: true, publicUrl: url } : t
              );
            }
          }
          return prev;
        });
        void refreshStatus();
      });
    })();

    return () => {
      cancelled = true;
      unLog?.();
      unUrl?.();
    };
  }, [refreshStatus]);

  const handleInstall = async () => {
    setInstalling(true);
    try {
      const msg = await invokeCloudflaredInstall();
      showMsg("success", msg || t("installOk"));
      await refreshStatus();
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    } finally {
      setInstalling(false);
    }
  };

  const handlePickBinary = async () => {
    setBusy(true);
    try {
      const path = await invokeCloudflaredPickBinary();
      if (!path) return;
      showMsg("success", t("pickBinaryOk"));
      await refreshStatus();
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleClearBinary = async () => {
    setBusy(true);
    try {
      const msg = await invokeCloudflaredClearBinaryPath();
      showMsg("success", msg || t("clearBinaryOk"));
      await refreshStatus();
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleStartQuick = async () => {
    const target = buildLocalUrl(localUrl);
    if (!target) {
      showMsg("error", t("needLocalUrl"));
      return;
    }
    try {
      const u = new URL(target);
      if (!u.hostname) throw new Error("empty host");
    } catch {
      showMsg("error", t("invalidLocalUrl"));
      return;
    }
    if (!status?.installed) {
      showMsg("error", t("notInstalled"));
      return;
    }
    if (activeQuick) {
      showMsg("error", t("alreadyRunning"));
      return;
    }
    setBusy(true);
    try {
      const ts = await invokeCloudflaredStartQuickTunnel(
        target,
        protocol === "auto" ? undefined : protocol
      );
      setTunnels((prev) => {
        const rest = prev.filter((t) => t.id !== ts.id);
        return [...rest, ts];
      });
      setLocalUrl(target);
      const port = extractPort(target);
      if (port && port !== 80 && port !== 443) {
        setRecentPorts(saveRecentPort(port));
      }
      showMsg("success", t("startOk"));
      recheckTunnelAlive(ts.id);
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
      await refreshStatus();
    } finally {
      setBusy(false);
    }
  };

  const handleStartNamed = async (profile: CloudflaredNamedProfile) => {
    if (!status?.installed) {
      showMsg("error", t("notInstalled"));
      return;
    }
    if (findNamedTunnel(profile)) {
      showMsg("error", t("alreadyRunning"));
      return;
    }
    const mode = profile.authMode || (profile.configPath ? "config" : "token");
    if (mode === "config" && !profile.configPath?.trim()) {
      showMsg("error", t("namedConfigRequired"));
      return;
    }
    if (mode === "token" && !profile.token?.trim()) {
      showMsg("error", t("namedTokenRequired"));
      return;
    }
    setBusy(true);
    try {
      const ts = await invokeCloudflaredStartNamedTunnel({
        profileId: profile.id,
        hostname: profile.hostname,
        localUrl: profile.localUrl,
        ...(mode === "config"
          ? { configPath: profile.configPath }
          : { token: profile.token }),
        ...(protocol === "auto" ? {} : { protocol }),
      });
      setTunnels((prev) => {
        const rest = prev.filter((t) => t.id !== ts.id);
        return [...rest, ts];
      });
      showMsg("success", t("namedStartOk"));
      recheckTunnelAlive(ts.id);
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
      await refreshStatus();
    } finally {
      setBusy(false);
    }
  };

  const handleSetupNewDomain = async (profile: CloudflaredNamedProfile) => {
    if (!status?.installed) {
      showMsg("error", t("notInstalled"));
      return;
    }
    const mode = profile.authMode || (profile.configPath ? "config" : "token");
    if (mode === "config" && !profile.configPath?.trim()) {
      showMsg("error", t("namedConfigRequired"));
      return;
    }
    if (!profile.hostname?.trim()) {
      showMsg("error", t("namedHostnameRequired"));
      return;
    }
    const local = buildLocalUrl(profile.localUrl) || profile.localUrl.trim();
    if (!local) {
      showMsg("error", t("needLocalUrl"));
      return;
    }
    setBusy(true);
    try {
      const msg = await invokeCloudflaredSetupNewDomain({
        configPath: profile.configPath || "",
        hostname: profile.hostname,
        localUrl: local,
      });
      showMsg("success", msg || "配置成功");
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    } finally {
      setBusy(false);
    }
  };

  const handleStop = async (id: string) => {
    setBusy(true);
    try {
      const msg = await invokeCloudflaredStopTunnel(id);
      setTunnels((prev) => prev.filter((t) => t.id !== id));
      showMsg("success", msg || t("stopOk"));
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    } finally {
      setBusy(false);
      await refreshStatus();
    }
  };

  const handleStopAll = async () => {
    if (runningTunnels.length === 0) return;
    setBusy(true);
    try {
      const msg = await invokeCloudflaredStopAllTunnels();
      setTunnels([]);
      showMsg("success", msg || t("stopOk"));
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    } finally {
      setBusy(false);
      await refreshStatus();
    }
  };

  const handleCopy = async (text?: string | null) => {
    const url = text;
    if (!url) return;
    try {
      await invokeCopyToClipboard(url);
      showMsg("success", t("copied"));
    } catch (e) {
      showMsg("error", `${t("copyFailed")}: ${e}`);
    }
  };

  const handleOpenUrl = async (url?: string | null) => {
    const target = url?.trim();
    if (!target) return;
    try {
      await invoke("open_in_browser", { url: target });
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    }
  };

  const handleCopyLogs = async () => {
    if (visibleLogs.length === 0) return;
    try {
      await invokeCopyToClipboard(visibleLogs.map(formatLogLine).join("\n"));
      showMsg("success", t("copied"));
    } catch (e) {
      showMsg("error", `${t("copyFailed")}: ${e}`);
    }
  };

  const handleExportLogs = async () => {
    if (visibleLogs.length === 0) return;
    try {
      const content = visibleLogs
        .map((e) => `${new Date(e.at).toISOString()} ${formatLogLine(e)}`)
        .join("\n");
      const path = await invokeSaveTextFile(
        content,
        `cloudflared-logs-${new Date().toISOString().slice(0, 10)}.log`,
        t("exportLogs")
      );
      showMsg("success", t("exportLogsOk", { path }));
    } catch (e) {
      const msg = String(e);
      // The native dialog reports a cancelled save as an error; that is not a failure.
      if (msg.includes("已取消")) return;
      showMsg("error", t("exportLogsFailed", { error: msg }));
    }
  };

  const openAdd = () => {
    setEditing(null);
    setShowFormToken(false);
    setForm({
      name: "",
      hostname: "",
      localUrl: "http://localhost:3000",
      authMode: "config",
      token: "",
      configPath: "",
    });
    setShowModal(true);
  };

  const openEdit = (p: CloudflaredNamedProfile) => {
    setEditing(p);
    setShowFormToken(false);
    setForm({
      name: p.name,
      hostname: p.hostname,
      localUrl: p.localUrl,
      authMode: p.authMode || (p.configPath ? "config" : "token"),
      token: p.token || "",
      configPath: p.configPath || "",
    });
    setShowModal(true);
  };

  const handlePickConfig = async () => {
    try {
      const path = await invokeCloudflaredPickConfig();
      if (!path) return;
      setForm((f) => ({ ...f, configPath: path, authMode: "config" }));
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    }
  };

  const handleSaveProfile = async () => {
    const name = form.name.trim() || form.hostname.trim();
    const hostname = form.hostname.trim();
    const token = form.token.trim();
    const configPath = form.configPath.trim();
    const local = buildLocalUrl(form.localUrl) || form.localUrl.trim();
    if (!hostname) {
      showMsg("error", t("namedHostnameRequired"));
      return;
    }
    // One tunnel per hostname: reject duplicates early (edit excludes itself).
    const hostNorm = normalizePublicHost(hostname);
    if (
      profiles.some(
        (p) => p.id !== editing?.id && normalizePublicHost(p.hostname) === hostNorm
      )
    ) {
      showMsg("error", t("namedHostnameDuplicate"));
      return;
    }
    if (form.authMode === "config") {
      if (!configPath) {
        showMsg("error", t("namedConfigRequired"));
        return;
      }
    } else if (!token) {
      showMsg("error", t("namedTokenRequired"));
      return;
    }
    const payload = {
      name,
      hostname,
      localUrl: local,
      authMode: form.authMode,
      token: form.authMode === "token" ? token : undefined,
      configPath: form.authMode === "config" ? configPath : undefined,
    };
    try {
      if (editing) {
        await updateCloudflaredProfile(editing.id, payload);
        showMsg("success", t("namedUpdated"));
      } else {
        await addCloudflaredProfile(payload);
        showMsg("success", t("namedSaved"));
      }
      setShowModal(false);
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    }
  };

  const handleDeleteProfile = async (p: CloudflaredNamedProfile) => {
    const running = findNamedTunnel(p);
    const ok = await confirm({
      title: t("namedDeleteTitle"),
      message: t("namedConfirmDelete", { name: p.name || p.hostname }),
      warning: running ? t("namedDeleteRunningWarning") : undefined,
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      // Deleting a running binding must not orphan the cloudflared process — it
      // would keep serving the hostname with no config left to manage it.
      if (running) {
        await invokeCloudflaredStopTunnel(running.id);
        setTunnels((prev) => prev.filter((t) => t.id !== running.id));
      }
      await deleteCloudflaredProfile(p.id);
      showMsg("success", t("namedDeleted"));
    } catch (e) {
      showMsg("error", t("error", { error: String(e) }));
    }
  };

  const toggleTokenReveal = (id: string) => {
    setRevealedTokenIds((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };

  return (
    <div className="cloudflared-page">
      <div className="cloudflared-main">
        <div className="card cloudflared-status-card">
          <div className="cloudflared-status-row">
            <div className="cloudflared-status-info">
              {!status ? (
                <span className="runtime-muted">
                  <Loader2 size={14} className="spin" /> …
                </span>
              ) : status.installed ? (
                <>
                  <span className="status-label">{t("installed")}</span>
                  <span className="cloudflared-status-meta" title={status.path || undefined}>
                    {t("version")}: {formatCloudflaredVersion(status.version || "—")}
                    {status.customPath ? ` · ${t("customPath")}` : ""}
                    {status.path ? ` · ${status.path}` : ""}
                  </span>
                </>
              ) : (
                <>
                  <span className="status-label">{t("notInstalled")}</span>
                  <span className="cloudflared-status-meta">{t("installHint")}</span>
                </>
              )}
            </div>
            <div className="btn-group cloudflared-status-actions">
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={async () => {
                  await refreshStatus();
                  showMsg("success", t("statusRefreshOk"));
                }}
                disabled={busy || installing}
              >
                <RefreshCw size={12} />
                {t("refresh")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={handlePickBinary}
                disabled={installing || busy}
              >
                {t("pickBinary")}
              </button>
              {status?.customPath && (
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={handleClearBinary}
                  disabled={installing || busy}
                >
                  {t("clearBinary")}
                </button>
              )}
              {status && !status.installed && (
                <button
                  type="button"
                  className="btn btn-primary btn-small"
                  onClick={handleInstall}
                  disabled={installing || busy}
                >
                  {installing ? (
                    <>
                      <Loader2 size={12} className="spin" /> {t("installing")}
                    </>
                  ) : (
                    t("installWinget")
                  )}
                </button>
              )}
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() =>
                  invokeCloudflaredOpenDownload().catch((e) =>
                    showMsg("error", t("error", { error: String(e) }))
                  )
                }
                disabled={installing}
              >
                <Download size={12} />
                {t("openDownload")}
              </button>
            </div>
          </div>
        </div>

        {runningTunnels.length > 0 && (
          <div className="card cloudflared-running-card">
            <div className="card-title cursor-card-title">
              <span>
                {t("runningTitle")}{" "}
                <span className="runtime-muted">({runningTunnels.length})</span>
              </span>
              <button
                type="button"
                className="btn btn-danger btn-small"
                onClick={handleStopAll}
                disabled={busy}
              >
                {t("stopAll")}
              </button>
            </div>
            <ul className="cloudflared-running-list">
              {runningTunnels.map((tn) => {
                const modeLabel =
                  tn.mode === "quick" ? t("authModeQuick") : t("authModeNamed");
                const primary = tn.publicUrl || tn.localUrl || tn.id;
                const secondary =
                  tn.mode === "quick"
                    ? tn.publicUrl
                      ? tn.localUrl
                      : t("waitingUrl")
                    : tn.localUrl;
                return (
                  <li key={tn.id} className="cloudflared-running-item">
                    <div className="cloudflared-running-info">
                      <div className="cloudflared-running-title">
                        <span className="cloudflared-mode-badge">{modeLabel}</span>
                        <span className="cloudflared-running-url" title={primary}>
                          {primary}
                        </span>
                        {tn.pid ? (
                          <span className="runtime-muted">pid {tn.pid}</span>
                        ) : null}
                      </div>
                      {secondary && secondary !== primary ? (
                        <div className="runtime-muted cloudflared-running-sub">
                          {secondary}
                        </div>
                      ) : null}
                    </div>
                    <div className="account-actions">
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        onClick={() => void handleOpenUrl(tn.publicUrl)}
                        disabled={!tn.publicUrl}
                        title={t("openInBrowser")}
                      >
                        <ExternalLink size={12} />
                      </button>
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        onClick={() => handleCopy(tn.publicUrl)}
                        disabled={!tn.publicUrl}
                      >
                        <Copy size={12} />
                        {t("copyUrl")}
                      </button>
                      <button
                        type="button"
                        className="btn btn-danger btn-small"
                        onClick={() => handleStop(tn.id)}
                        disabled={busy}
                      >
                        {t("stop")}
                      </button>
                    </div>
                  </li>
                );
              })}
            </ul>
          </div>
        )}

        <div className="cloudflared-tunnel-grid">
          <div className="card cloudflared-named-card">
            <div className="card-title cursor-card-title">
              <span>{t("namedTitle")}</span>
              <button
                type="button"
                className="btn btn-primary btn-small"
                onClick={openAdd}
                disabled={busy}
              >
                + {t("namedAdd")}
              </button>
            </div>
            <p className="runtime-muted cloudflared-hint">{t("namedHint")}</p>
            {profiles.length === 0 ? (
              <p className="runtime-muted" style={{ fontSize: 13 }}>
                {t("namedEmpty")}
              </p>
            ) : (
              <ul className="account-list cloudflared-binding-list">
                {profiles.map((p) => {
                  const publicUrl = `https://${p.hostname
                    .replace(/^https?:\/\//i, "")
                    .replace(/\/$/, "")}`;
                  const authMode = p.authMode || (p.configPath ? "config" : "token");
                  const active = findNamedTunnel(p);
                  const isActive = Boolean(active);
                  return (
                    <li
                      key={p.id}
                      className={`account-item cloudflared-binding-item ${isActive ? "active" : ""}`}
                    >
                      <div className="cloudflared-binding-body">
                        <div className="cloudflared-binding-top">
                          <span className="cloudflared-binding-name">
                            {p.name || p.hostname}
                          </span>
                          <span
                            className="account-note"
                            title={authMode === "config" ? p.configPath : undefined}
                          >
                            {authMode === "config" ? t("authModeConfig") : t("authModeToken")}
                          </span>
                          {isActive ? (
                            <span className="cloudflared-active-mark">
                              {t("running")}
                              {active?.pid ? ` · pid ${active.pid}` : ""}
                            </span>
                          ) : null}
                        </div>
                        <div className="cloudflared-binding-host">{publicUrl}</div>
                        <div className="cloudflared-binding-local">
                          → {p.localUrl || "—"}
                        </div>
                        {authMode === "config" ? (
                          <div className="cloudflared-binding-path" title={p.configPath}>
                            {p.configPath || "—"}
                          </div>
                        ) : (
                          <div className="cursor-password-row cloudflared-binding-token">
                            <span className="account-email cursor-password-value">
                              {revealedTokenIds.has(p.id) ? p.token : maskToken(p.token)}
                            </span>
                            <button
                              type="button"
                              className="btn btn-secondary btn-small"
                              onClick={() => toggleTokenReveal(p.id)}
                              title={
                                revealedTokenIds.has(p.id) ? t("hideToken") : t("showToken")
                              }
                            >
                              {revealedTokenIds.has(p.id) ? (
                                <EyeOff size={12} />
                              ) : (
                                <Eye size={12} />
                              )}
                            </button>
                            <button
                              type="button"
                              className="btn btn-secondary btn-small"
                              onClick={() => handleCopy(p.token)}
                              title={t("copyToken")}
                              disabled={!p.token}
                            >
                              <Copy size={12} />
                            </button>
                          </div>
                        )}
                      </div>
                      <div className="account-actions cloudflared-binding-actions">
                        {isActive && active ? (
                          <button
                            type="button"
                            className="btn btn-danger btn-small"
                            onClick={() => handleStop(active.id)}
                            disabled={busy}
                          >
                            {t("stop")}
                          </button>
                        ) : (
                          <button
                            type="button"
                            className="btn btn-primary btn-small"
                            onClick={() => handleStartNamed(p)}
                            disabled={busy || installing || !status?.installed}
                          >
                            {t("namedStart")}
                          </button>
                        )}
                        <button
                          type="button"
                          className="btn btn-secondary btn-small"
                          onClick={() => handleSetupNewDomain(p)}
                          disabled={busy || installing || !status?.installed}
                          title={t("oneClickSetup")}
                        >
                          {t("oneClickSetup")}
                        </button>
                        <button
                          type="button"
                          className="btn btn-secondary btn-small"
                          onClick={() => void handleOpenUrl(publicUrl)}
                          title={t("openInBrowser")}
                        >
                          <ExternalLink size={12} />
                        </button>
                        <button
                          type="button"
                          className="btn btn-secondary btn-small"
                          onClick={() => handleCopy(publicUrl)}
                          title={t("copyUrl")}
                        >
                          <Copy size={12} />
                        </button>
                        <button
                          type="button"
                          className="btn btn-secondary btn-small"
                          onClick={() => openEdit(p)}
                          disabled={busy}
                          title={tc("actions.edit")}
                        >
                          <Pencil size={12} />
                        </button>
                        <button
                          type="button"
                          className="btn btn-danger btn-small"
                          onClick={() => handleDeleteProfile(p)}
                          disabled={busy}
                          title={tc("actions.delete")}
                        >
                          <Trash2 size={12} />
                        </button>
                      </div>
                    </li>
                  );
                })}
              </ul>
            )}
          </div>

          <div className="card cloudflared-quick-card">
            <div className="card-title">{t("tunnelTitle")}</div>
            <p className="runtime-muted cloudflared-hint">{t("tunnelHint")}</p>
            <div className="input-group">
              <label className="input-label">{t("localUrl")}</label>
              <input
                className="input-field"
                value={localUrl}
                onChange={(e) => setLocalUrl(e.target.value)}
                placeholder={t("localUrlPlaceholder")}
                disabled={busy}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("protocol")}</label>
              <select
                className="input-field"
                value={protocol}
                onChange={(e) => handleProtocolChange(e.target.value)}
                disabled={busy}
                title={t("protocolHint")}
                aria-label={t("protocol")}
              >
                <option value="auto">{t("protocolAuto")}</option>
                <option value="http2">{t("protocolHttp2")}</option>
                <option value="quic">{t("protocolQuic")}</option>
              </select>
              <p className="runtime-muted cloudflared-hint" style={{ marginTop: 4 }}>
                {t("protocolHint")}
              </p>
            </div>
            {recentPorts.length > 0 && (
              <div className="input-group">
                <label className="input-label">{t("recentPorts")}</label>
                <div className="btn-group cloudflared-port-chips">
                  {recentPorts.map((port) => (
                    <button
                      key={port}
                      type="button"
                      className="btn btn-secondary btn-small"
                      disabled={busy}
                      onClick={() => setLocalUrl(`http://localhost:${port}`)}
                    >
                      :{port}
                    </button>
                  ))}
                </div>
              </div>
            )}
            <div className="btn-group qs-apply-row">
              {!activeQuick ? (
                <button
                  type="button"
                  className="btn btn-primary"
                  onClick={handleStartQuick}
                  disabled={busy || installing || !status?.installed}
                >
                  {busy ? (
                    <>
                      <Loader2 size={14} className="spin" /> {t("starting")}
                    </>
                  ) : (
                    <>
                      <Globe size={14} /> {t("start")}
                    </>
                  )}
                </button>
              ) : (
                <button
                  type="button"
                  className="btn btn-danger"
                  onClick={() => handleStop(activeQuick.id)}
                  disabled={busy}
                >
                  {busy ? (
                    <>
                      <Loader2 size={14} className="spin" /> {t("stopping")}
                    </>
                  ) : (
                    <>
                      <Square size={14} /> {t("stop")}
                    </>
                  )}
                </button>
              )}
              <span className="runtime-muted" style={{ alignSelf: "center", fontSize: 12 }}>
                {activeQuick ? t("running") : t("stopped")}
                {activeQuick?.pid ? ` · pid ${activeQuick.pid}` : ""}
              </span>
            </div>

            <div className="input-group" style={{ marginTop: 14 }}>
              <label className="input-label">{t("publicUrl")}</label>
              <div className="qs-path-row">
                <input
                  className="input-field"
                  readOnly
                  value={activeQuick ? activeQuick.publicUrl || t("waitingUrl") : ""}
                />
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => void handleOpenUrl(activeQuick?.publicUrl)}
                  disabled={!activeQuick?.publicUrl}
                  title={t("openInBrowser")}
                >
                  <ExternalLink size={12} />
                </button>
                <button
                  type="button"
                  className="btn btn-secondary"
                  onClick={() => handleCopy(activeQuick?.publicUrl)}
                  disabled={!activeQuick?.publicUrl}
                >
                  <Copy size={12} />
                  {t("copyUrl")}
                </button>
              </div>
            </div>
          </div>
        </div>
      </div>

      <div className="cloudflared-side">
        <div className="card cloudflared-log-card">
          <div className="card-title cursor-card-title">
            <span>{t("logsTitle")}</span>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => {
                setLogs([]);
                setLogFilterId("all");
                setLogQuery("");
              }}
            >
              {t("clearLogs")}
            </button>
          </div>
          <div className="cloudflared-log-tools">
            <input
              className="input-field"
              type="search"
              value={logQuery}
              onChange={(e) => setLogQuery(e.target.value)}
              placeholder={t("logsSearch")}
              aria-label={t("logsSearch")}
            />
            <button
              type="button"
              className={`btn btn-secondary btn-small${autoScroll ? " is-active" : ""}`}
              onClick={() => setAutoScroll((v) => !v)}
              title={t("autoScroll")}
              aria-pressed={autoScroll}
            >
              {t("autoScroll")}
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => void handleCopyLogs()}
              disabled={visibleLogs.length === 0}
              title={t("copyLogs")}
            >
              <Copy size={12} />
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-small"
              onClick={() => void handleExportLogs()}
              disabled={visibleLogs.length === 0}
              title={t("exportLogs")}
            >
              <Download size={12} />
            </button>
          </div>
          {(logSourceTabs.length > 0 || logs.length > 0) && (
            <div className="scope-tabs cloudflared-log-tabs">
              <button
                type="button"
                className={`scope-tab ${logFilterId === "all" ? "active" : ""}`}
                onClick={() => setLogFilterId("all")}
              >
                {t("logsAll")}
              </button>
              {logSourceTabs.map((src) => (
                <button
                  key={src.id}
                  type="button"
                  className={`scope-tab ${logFilterId === src.id ? "active" : ""}`}
                  onClick={() => setLogFilterId(src.id)}
                  title={src.id}
                >
                  {src.label}
                </button>
              ))}
            </div>
          )}
          <pre
            className={`cloudflared-log ${visibleLogs.length === 0 ? "is-empty" : ""}`}
          >
            {visibleLogs.length === 0
              ? logQueryNorm
                ? t("logsNoMatch")
                : t("noLogs")
              : visibleLogs.map(formatLogLine).join("\n")}
            <div ref={logEndRef} />
          </pre>
        </div>
      </div>

      {showModal && (
        <div className="modal-overlay" onClick={() => setShowModal(false)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <ModalTitleRow title={editing ? t("namedEdit") : t("namedAdd")} onClose={() => setShowModal(false)} />
            <div className="input-group">
              <label className="input-label">{t("namedName")}</label>
              <input
                className="input-field"
                value={form.name}
                onChange={(e) => setForm({ ...form, name: e.target.value })}
                placeholder={t("namedNamePlaceholder")}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("namedHostname")}</label>
              <input
                className="input-field"
                value={form.hostname}
                onChange={(e) => setForm({ ...form, hostname: e.target.value })}
                placeholder={t("namedHostnamePlaceholder")}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("localUrl")}</label>
              <input
                className="input-field"
                value={form.localUrl}
                onChange={(e) => setForm({ ...form, localUrl: e.target.value })}
                placeholder={t("localUrlPlaceholder")}
              />
            </div>
            <div className="input-group">
              <label className="input-label">{t("authMode")}</label>
              <div className="scope-tabs" style={{ marginTop: 4 }}>
                <button
                  type="button"
                  className={`scope-tab ${form.authMode === "config" ? "active" : ""}`}
                  onClick={() => setForm({ ...form, authMode: "config" })}
                >
                  {t("authModeConfig")}
                </button>
                <button
                  type="button"
                  className={`scope-tab ${form.authMode === "token" ? "active" : ""}`}
                  onClick={() => setForm({ ...form, authMode: "token" })}
                >
                  {t("authModeToken")}
                </button>
              </div>
            </div>
            {form.authMode === "config" ? (
              <div className="input-group">
                <label className="input-label">{t("configPath")}</label>
                <div className="qs-path-row">
                  <input
                    className="input-field"
                    value={form.configPath}
                    onChange={(e) => setForm({ ...form, configPath: e.target.value })}
                    placeholder={t("configPathPlaceholder")}
                  />
                  <button type="button" className="btn btn-secondary" onClick={handlePickConfig}>
                    {t("pickConfig")}
                  </button>
                </div>
                <p className="runtime-muted" style={{ marginTop: 4, fontSize: 12 }}>
                  {t("configPathNote")}
                </p>
              </div>
            ) : (
              <div className="input-group">
                <label className="input-label">{t("namedToken")}</label>
                <div className="cursor-password-input-wrap">
                  <input
                    className="input-field"
                    type={showFormToken ? "text" : "password"}
                    value={form.token}
                    onChange={(e) => setForm({ ...form, token: e.target.value })}
                    placeholder={t("namedTokenPlaceholder")}
                    autoComplete="off"
                  />
                  <button
                    type="button"
                    className="btn btn-secondary btn-small cursor-password-toggle"
                    onClick={() => setShowFormToken((v) => !v)}
                  >
                    {showFormToken ? <EyeOff size={14} /> : <Eye size={14} />}
                  </button>
                </div>
                <p className="runtime-muted" style={{ marginTop: 4, fontSize: 12 }}>
                  {t("namedTokenNote")}
                </p>
              </div>
            )}
            <div className="modal-actions">
              <button
                type="button"
                className="btn btn-secondary"
                onClick={() => setShowModal(false)}
              >
                {tc("actions.cancel")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={handleSaveProfile}
                disabled={
                  !form.hostname.trim() ||
                  (form.authMode === "config"
                    ? !form.configPath.trim()
                    : !form.token.trim())
                }
              >
                {editing ? tc("actions.save") : tc("actions.add")}
              </button>
            </div>
          </div>
        </div>
      )}

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <XCircle size={16} />}
          </span>
          {message.text}
        </div>
      )}
    </div>
  );
}

export default CloudflaredManager;
