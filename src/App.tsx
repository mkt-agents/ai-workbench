import { useEffect, useState, useRef } from "react";
import { useTranslation } from "react-i18next";
import GitManager from "./components/GitManager";
import SettingsPage from "./components/Settings";
import TestManager from "./components/TestManager";
import { ConfirmDialogProvider } from "./components/ConfirmModal";
import {
  GitBranch,
  ChevronRight,
  ChevronDown,
  Settings,
  Puzzle,
  Network,
  Minus,
  Square,
  X,
  Cpu,
  Layers,
  Wand2,
  Cloud,
  FileText,
  PanelLeftClose,
  PanelLeftOpen,
  KeyRound,
  Terminal,
  FlaskConical as Flask,
} from "lucide-react";
import HostsManager from "./components/HostsManager";
import PluginBrowser from "./components/PluginBrowser";
import CursorManager from "./components/CursorManager";
import AIAssistant from "./components/AIAssistant";
import DeepSeekHarness from "./components/DeepSeekHarness";
import PromptOptimizer from "./components/PromptOptimizer";
import VersionSwitcher from "./components/VersionSwitcher";
import CloudflaredManager from "./components/CloudflaredManager";
import SnippetsManager from "./components/SnippetsManager";
import DevTools from "./components/DevTools";
import AppLogoMark from "./components/AppLogoMark";
import { bootApp } from "./core/boot";
import { useGlobalStore } from "./core/store";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { emit } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { registerQuickAskShortcut } from "./lib/quickAskShortcut";
import { applyDocumentTheme } from "./lib/theme";
import { useTauriEvent } from "./hooks/useTauriEvent";
import "./styles.css";

type Tab =
  | "git"
  | "hosts"
  | "settings"
  | "plugins"
  | "cursor-accounts"
  | "ai-chat"
  | "ai-models"
  | "ai-prompt"
  | "runtime"
  | "cloudflared"
  | "snippets"
  | "devtools"
  | "test-manager";

type NavLeaf = { id: Tab; labelKey: string; icon: React.ReactNode };
type NavGroup = {
  group: string;
  labelKey: string;
  defaultOpen?: boolean;
  children: NavLeaf[];
};
type NavNode = NavLeaf | NavGroup;

const NARROW_COLLAPSE_PX = 720;
const WIDE_EXPAND_PX = 900;

function App() {
  const { t } = useTranslation("navigation");
  const { t: tc } = useTranslation("common");
  const { i18n } = useTranslation();
  const settings = useGlobalStore((s) => s.settings);
  const setSettings = useGlobalStore((s) => s.setSettings);

  const [activeTab, setActiveTab] = useState<Tab>("ai-chat");
  const [gitMounted, setGitMounted] = useState(false);
  const [dshMounted, setDshMounted] = useState(true);
  const collapsed = settings.sidebarCollapsed;
  const autoCollapsedByWidth = useRef(false);
  const userOverrideCollapse = useRef(false);

  const setCollapsed = (value: boolean | ((prev: boolean) => boolean)) => {
    const next = typeof value === "function" ? value(settings.sidebarCollapsed) : value;
    userOverrideCollapse.current = true;
    autoCollapsedByWidth.current = false;
    setSettings({ sidebarCollapsed: next });
  };
  const [openGroups, setOpenGroups] = useState<Set<string>>(
    () => new Set<string>(["ai", "account", "version", "network", "system"])
  );
  const [isReady, setIsReady] = useState(false);
  const [trayToast, setTrayToast] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    // Immediately render the UI; load data in background. This avoids blocking the
    // message pump on first launch (WebView2 env creation + SQLite setup), which was
    // preventing the OS from marking the window as "Not Responding".
    setIsReady(true);

    bootApp()
      .then(async () => {
        if (cancelled) return;
        const s = useGlobalStore.getState().settings;
        const result = await registerQuickAskShortcut(s.quickAskShortcut);
        if (!result.ok) {
          console.warn("quick-ask shortcut failed:", result.error);
        }
        try {
          // Drop legacy top-right positions that cover the titlebar (pre-fix HiDPI bug).
          const pos = s.quickAskBubblePos;
          const unsafeTopRight =
            pos &&
            Number.isFinite(pos.x) &&
            Number.isFinite(pos.y) &&
            pos.y < 120;
          if (unsafeTopRight) {
            setSettings({ quickAskBubblePos: undefined });
          }
          await invoke("set_quick_ask_bubble_visible", {
            visible: s.quickAskBubbleEnabled !== false,
            x: unsafeTopRight ? null : (pos?.x ?? null),
            y: unsafeTopRight ? null : (pos?.y ?? null),
          });
        } catch (e) {
          console.warn("quick-ask bubble sync failed:", e);
        }
      })
      .catch(console.error);
    applyDocumentTheme(settings.theme);
    if (i18n.language !== settings.language) {
      i18n.changeLanguage(settings.language);
    }
    return () => {
      cancelled = true;
    };
  }, []);

  useTauriEvent<string>("navigate-tab", (event) => {
    const tab = event.payload as Tab;
    if (tab) setActiveTab(tab);
  }, [setActiveTab]);

  useTauriEvent("tray-minimized", () => {
    const s = useGlobalStore.getState().settings;
    if (!s.trayHintShown) {
      setTrayToast(tc("trayMinimizedHint"));
      setSettings({ trayHintShown: true });
      setTimeout(() => setTrayToast(null), 4000);
    }
  }, [setSettings, tc]);

  useTauriEvent<boolean>("quick-ask-bubble-enabled", (event) => {
    setSettings({ quickAskBubbleEnabled: Boolean(event.payload) });
  }, [setSettings]);

  const toggleGroup = (group: string) => {
    setOpenGroups((prev) => {
      const next = new Set(prev);
      if (next.has(group)) next.delete(group);
      else next.add(group);
      return next;
    });
  };

  const NAV_NODES: NavNode[] = [
    {
      group: "ai",
      labelKey: "aiWorkbench",
      defaultOpen: true,
      children: [
        {
          id: "ai-chat",
          labelKey: "harness",
          icon: (
            <img
              src="/deepseek-logo.svg"
              alt=""
              width={16}
              height={16}
              className="nav-brand-icon"
              draggable={false}
            />
          ),
        },
        { id: "ai-models", labelKey: "modelConfig", icon: <Cpu size={16} /> },
        { id: "ai-prompt", labelKey: "aiPrompts", icon: <Wand2 size={16} /> },
        { id: "snippets", labelKey: "snippets", icon: <FileText size={16} /> },
      ],
    },
    {
      group: "account",
      labelKey: "accountManagement",
      defaultOpen: true,
      children: [
        { id: "cursor-accounts", labelKey: "cursorAccounts", icon: <KeyRound size={16} /> },
      ],
    },
    {
      group: "version",
      labelKey: "versionManagement",
      defaultOpen: true,
      children: [
        { id: "git", labelKey: "gitManagement", icon: <GitBranch size={16} /> },
        { id: "runtime", labelKey: "runtimeVersion", icon: <Layers size={16} /> },
      ],
    },
    {
      group: "network",
      labelKey: "networkManagement",
      defaultOpen: true,
      children: [
        { id: "hosts", labelKey: "hosts", icon: <Network size={16} /> },
        { id: "cloudflared", labelKey: "cloudflared", icon: <Cloud size={16} /> },
        { id: "plugins", labelKey: "plugins", icon: <Puzzle size={16} /> },
      ],
    },
    {
      group: "system",
      labelKey: "systemUtils",
      defaultOpen: true,
      children: [
        { id: "test-manager", labelKey: "testManagement", icon: <Flask size={16} /> },
        { id: "devtools", labelKey: "devtools", icon: <Terminal size={16} /> },
      ],
    },
  ];

  const NAV_MAP: Record<string, string> = {
    settings: t("settings"),
  };
  for (const node of NAV_NODES) {
    if ("group" in node) {
      for (const child of node.children) NAV_MAP[child.id] = t(child.labelKey);
    } else {
      NAV_MAP[node.id] = t(node.labelKey);
    }
  }

  const renderLeafButton = (leaf: NavLeaf, className = "") => (
    <button
      key={leaf.id}
      onClick={() => setActiveTab(leaf.id)}
      className={`sidebar-nav-item ${className} ${activeTab === leaf.id ? "active" : ""}`.trim()}
      title={t(leaf.labelKey)}
    >
      <span className="nav-icon">{leaf.icon}</span>
      {!collapsed && <span className="nav-label">{t(leaf.labelKey)}</span>}
    </button>
  );

  useEffect(() => {
    let timer: number | undefined;
    const onResize = () => {
      document.body.classList.add("resizing");
      if (timer) clearTimeout(timer);
      timer = window.setTimeout(
        () => document.body.classList.remove("resizing"),
        300
      );

      const w = window.innerWidth;
      const { sidebarCollapsed } = useGlobalStore.getState().settings;

      if (w < NARROW_COLLAPSE_PX && !sidebarCollapsed && !userOverrideCollapse.current) {
        autoCollapsedByWidth.current = true;
        useGlobalStore.getState().setSettings({ sidebarCollapsed: true });
      } else if (
        w >= WIDE_EXPAND_PX &&
        autoCollapsedByWidth.current &&
        sidebarCollapsed &&
        !userOverrideCollapse.current
      ) {
        autoCollapsedByWidth.current = false;
        useGlobalStore.getState().setSettings({ sidebarCollapsed: false });
      }

      // Crossing back to wide clears manual override so auto-collapse can re-apply later
      if (w >= WIDE_EXPAND_PX) {
        userOverrideCollapse.current = false;
      }
    };

    onResize();
    window.addEventListener("resize", onResize);
    return () => {
      window.removeEventListener("resize", onResize);
      if (timer) clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    if (activeTab === "git") setGitMounted(true);
    if (activeTab === "ai-chat") setDshMounted(true);
  }, [activeTab]);

  const handleMinimize = async () => {
    await getCurrentWindow().minimize();
  };

  const handleMaximize = async () => {
    const win = getCurrentWindow();
    if (await win.isMaximized()) {
      await win.unmaximize();
    } else {
      await win.maximize();
    }
  };

  const handleClose = async () => {
    // Prefer hide over close(): tray CloseRequested also hides, but a direct hide
    // is more reliable if an always-on-top overlay previously stole clicks.
    const win = getCurrentWindow();
    await win.hide();
    try {
      await emit("tray-minimized");
    } catch {
      /* ignore */
    }
  };

  if (!isReady) {
    return null;
  }

  return (
    <ConfirmDialogProvider>
      <div className="app-layout">
        <aside className={`sidebar ${collapsed ? "collapsed" : ""}`}>
        <div className="sidebar-logo">
          <AppLogoMark
            size={28}
            className="sidebar-logo-icon"
          />
          {!collapsed && <span className="sidebar-logo-text">{tc("app.name")}</span>}
        </div>

        <nav className="sidebar-nav">
          {NAV_NODES.map((node) => {
            if ("group" in node) {
              if (collapsed) {
                return (
                  <div key={node.group} className="sidebar-group sidebar-group-collapsed">
                    {node.children.map((child) => renderLeafButton(child))}
                  </div>
                );
              }
              const isOpen = openGroups.has(node.group);
              const hasActive = node.children.some((c) => c.id === activeTab);
              const label = t(node.labelKey);
              return (
                <div key={node.group} className="sidebar-group">
                  <button
                    className={`sidebar-group-header ${hasActive ? "has-active" : ""}`}
                    onClick={() => toggleGroup(node.group)}
                    type="button"
                  >
                    <span className="nav-label">{label}</span>
                    <span className="sidebar-group-chevron">
                      {isOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
                    </span>
                  </button>
                  {isOpen &&
                    node.children.map((child) =>
                      renderLeafButton(child, "sidebar-group-item")
                    )}
                </div>
              );
            }
            return renderLeafButton(node);
          })}
        </nav>

        <div className="sidebar-nav-footer">
          <button
            onClick={() => setActiveTab("settings")}
            className={`sidebar-nav-item ${activeTab === "settings" ? "active" : ""}`}
            title={t("settings")}
            type="button"
          >
            <span className="nav-icon">
              <Settings size={16} />
            </span>
            {!collapsed && <span className="nav-label">{t("settings")}</span>}
          </button>
          <button
            onClick={() => setCollapsed(!collapsed)}
            className="sidebar-collapse-btn"
            type="button"
            title={collapsed ? t("expandSidebar") : t("collapseSidebar")}
            aria-label={collapsed ? t("expandSidebar") : t("collapseSidebar")}
          >
            {collapsed ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}
          </button>
        </div>
      </aside>

      <div className="right-area">
        <div className="titlebar" data-tauri-drag-region>
          <div className="titlebar-drag" />
          {/* Dev-instance badge (Vite compile-time flag): only rendered when
              launched via the dev server (npm run tauri dev); dead code in
              packaged builds — never ships. */}
          {import.meta.env.DEV && (
            <span className="dev-badge" data-tauri-drag-region={false}>
              DEV
            </span>
          )}
          <div className="titlebar-controls" data-tauri-drag-region={false}>
            <button
              className="titlebar-btn"
              data-tauri-drag-region={false}
              onClick={handleMinimize}
            >
              <Minus size={12} />
            </button>
            <button
              className="titlebar-btn"
              data-tauri-drag-region={false}
              onClick={handleMaximize}
            >
              <Square size={10} />
            </button>
            <button
              className="titlebar-btn titlebar-close"
              data-tauri-drag-region={false}
              onClick={handleClose}
            >
              <X size={12} />
            </button>
          </div>
        </div>

        <main className="main-content">
          <div className={`main-header ${["ai-chat", "ai-models"].includes(activeTab) ? "hidden" : ""}`}>
            <h1>{NAV_MAP[activeTab]}</h1>
          </div>
          <div className="main-body">
            <div className="page-in">
              {gitMounted && (
                <div
                  className={activeTab === "git" ? "page-panel is-active" : "page-panel"}
                  aria-hidden={activeTab !== "git"}
                >
                  <GitManager active={activeTab === "git"} />
                </div>
              )}
              {activeTab === "hosts" && <HostsManager />}
              {activeTab === "runtime" && <VersionSwitcher />}
              {activeTab === "cursor-accounts" && <CursorManager />}
              {activeTab === "ai-models" && <AIAssistant />}
              {dshMounted && (
                <div
                  className={activeTab === "ai-chat" ? "page-panel is-active" : "page-panel"}
                  aria-hidden={activeTab !== "ai-chat"}
                >
                  <DeepSeekHarness />
                </div>
              )}
              {activeTab === "ai-prompt" && (
                <PromptOptimizer onGoModels={() => setActiveTab("ai-models")} />
              )}
              {activeTab === "snippets" && <SnippetsManager />}
              {activeTab === "plugins" && <PluginBrowser />}
              {activeTab === "cloudflared" && <CloudflaredManager />}
              {activeTab === "devtools" && <DevTools />}
              {activeTab === "test-manager" && <TestManager />}
              {activeTab === "settings" && <SettingsPage />}
              {trayToast && (
                <div className="toast toast-success" role="status">
                  <span className="toast-text">{trayToast}</span>
                </div>
              )}
            </div>
          </div>
        </main>
      </div>
      </div>
    </ConfirmDialogProvider>
  );
}

export default App;
