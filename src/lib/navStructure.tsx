/**
 * 主窗侧栏导航的唯一事实（AGENTS.md：侧栏顺序来源）。
 *
 * - `Tab` 是页签 id 联合类型；`NAV_NODES` 是默认分组与默认顺序。
 * - 用户自定义（改名 / 排序 / 换组）见 `navCustomize.ts`，只覆盖显示层，
 *   不改这里的结构。
 * - 页面文案走 `navigation.json`（labelKey）。
 */
import type { ReactNode } from "react";
import {
  AppWindow,
  Bot,
  Cloud,
  Cpu,
  Gauge,
  GitBranch,
  KeyRound,
  Layers,
  Network,
  Puzzle,
  Sparkles,
  Rocket,
  StickyNote,
  Terminal,
} from "lucide-react";

export type Tab =
  | "git"
  | "hosts"
  | "settings"
  | "plugins"
  | "cursor-accounts"
  | "account-quota"
  | "ai-chat"
  | "ai-models"
  | "runtime"
  | "cloudflared"
  /** 智能互联 → IM 机器人（企业微信 / 后续微信等通道） */
  | "im-bots"
  /** 智能互联 → 天玑助手（页内 Tab：交互与权限 / 联网扩展） */
  | "tianji-assistant"
  /** @deprecated 已并入模型管理「提示词」Tab；hash / navigate-tab 仍可认，App 会重定向。 */
  | "snippets"
  | "drafts"
  | "devtools"
  | "app-launcher"
  | "desktop-apps";

export interface NavLeaf {
  id: Tab;
  labelKey: string;
  icon: ReactNode;
}

export interface NavGroup {
  group: string;
  labelKey: string;
  defaultOpen?: boolean;
  children: NavLeaf[];
}

export type NavNode = NavLeaf | NavGroup;

export const NAV_NODES: NavNode[] = [
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
      { id: "desktop-apps", labelKey: "desktopApps", icon: <AppWindow size={16} /> },
      { id: "drafts", labelKey: "drafts", icon: <StickyNote size={16} /> },
      { id: "ai-models", labelKey: "modelConfig", icon: <Cpu size={16} /> },
      { id: "app-launcher", labelKey: "quickApps", icon: <Rocket size={16} /> },
    ],
  },
  {
    group: "account",
    labelKey: "accountManagement",
    defaultOpen: true,
    children: [
      { id: "cursor-accounts", labelKey: "cursorAccounts", icon: <KeyRound size={16} /> },
      { id: "account-quota", labelKey: "accountQuota", icon: <Gauge size={16} /> },
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
      { id: "plugins", labelKey: "plugins", icon: <Puzzle size={16} /> },
      { id: "cloudflared", labelKey: "cloudflared", icon: <Cloud size={16} /> },
      { id: "hosts", labelKey: "hosts", icon: <Network size={16} /> },
    ],
  },
  {
    group: "smart",
    labelKey: "smartConnect",
    defaultOpen: true,
    children: [
      { id: "tianji-assistant", labelKey: "tianjiAssistant", icon: <Sparkles size={16} /> },
      { id: "im-bots", labelKey: "imBots", icon: <Bot size={16} /> },
    ],
  },
  {
    group: "system",
    labelKey: "systemUtils",
    defaultOpen: true,
    children: [{ id: "devtools", labelKey: "devtools", icon: <Terminal size={16} /> }],
  },
];
