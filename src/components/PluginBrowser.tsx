import { useState, useEffect, useCallback, useMemo, useRef } from "react";
import { useTranslation } from "react-i18next";
import { openBrowser, closeBrowser, onBrowserClosed, browserMapKey, getOpenBrowserKeys } from "../lib/browser";
import { useGlobalStore } from "../core/store";
import { useConfirm } from "./ConfirmModal";
import type { WebPlugin, UserScript } from "../core/types";
import {
  X, Plus, Trash2, Edit2, Check, Globe, ExternalLink, Search, Link2,
  BookmarkPlus, GripVertical, LayoutGrid, List, FolderOpen, Inbox,
  Keyboard, Download, Upload, ChevronDown, ChevronRight, ArrowUpDown, Code,
  Copy, FileText, Zap, ClipboardCopy,
} from "lucide-react";

const VIEW_MODE_KEY = "ai-workbench.webPlugins.viewMode";

type ViewMode = "list" | "card";
type SortMode = "manual" | "recent";
type FormMode = "add" | "edit";

function normalizeHttpUrl(raw: string): URL | null {
  const trimmed = raw.trim();
  if (!trimmed) return null;
  const withProtocol = /^[a-zA-Z][a-zA-Z0-9+.-]*:/.test(trimmed)
    ? trimmed
    : `https://${trimmed}`;
  try {
    const u = new URL(withProtocol);
    if (u.protocol !== "http:" && u.protocol !== "https:") return null;
    if (!u.hostname) return null;
    return u;
  } catch {
    return null;
  }
}

function urlKey(url: URL): string {
  return browserMapKey(url.href);
}

function parseTags(tags: string): string[] {
  return tags
    .split(/[,，]/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function compareRecent(a: WebPlugin, b: WebPlugin): number {
  const ta = a.lastOpenedAt ? Date.parse(a.lastOpenedAt) : 0;
  const tb = b.lastOpenedAt ? Date.parse(b.lastOpenedAt) : 0;
  if (tb !== ta) return tb - ta;
  return a.order - b.order;
}

function loadViewMode(): ViewMode {
  try {
    const v = localStorage.getItem(VIEW_MODE_KEY);
    return v === "card" ? "card" : "list";
  } catch {
    return "list";
  }
}

/** Test if a URL matches a userscript match pattern */
function testMatchPattern(pattern: string, url: string): boolean {
  if (pattern === "<all_urls>") return true;
  const trimmed = pattern.trim();
  if (!trimmed) return false;
  let regex = "";
  for (const ch of trimmed) {
    if (ch === "*") regex += ".*";
    else if (ch === "?") regex += ".";
    else if ("+.^${}()|[]\\".includes(ch)) regex += "\\" + ch;
    else regex += ch;
  }
  try {
    return new RegExp("^" + regex + "$").test(url);
  } catch {
    return false;
  }
}

/** Preset script templates — users can start from these */
interface ScriptPreset {
  id: string;
  nameKey: string;
  matchPatterns: string[];
  code: string;
}

const SCRIPT_PRESETS: ScriptPreset[] = [
  {
    id: "highlight-url",
    nameKey: "usPresetHighlight",
    matchPatterns: ["<all_urls>"],
    code: `(function() {
  'use strict';
  // Highlight the current URL in the page title
  const url = location.href;
  document.title = "📍 " + url;

  // Show a small floating badge with the URL
  const badge = document.createElement('div');
  badge.textContent = new URL(url).hostname;
  Object.assign(badge.style, {
    position: 'fixed', top: '8px', left: '8px', zIndex: '999999',
    background: 'rgba(88,166,255,0.9)', color: '#fff',
    padding: '4px 10px', borderRadius: '6px', fontSize: '12px',
    fontFamily: 'system-ui, sans-serif', pointerEvents: 'none',
  });
  document.documentElement.appendChild(badge);
})();`,
  },
  {
    id: "dark-mode",
    nameKey: "usPresetDarkMode",
    matchPatterns: ["<all_urls>"],
    code: `(function() {
  'use strict';
  // Force dark mode via CSS filter
  const style = document.createElement('style');
  style.textContent = \`
    html {
      filter: invert(1) hue-rotate(180deg) !important;
    }
    img, video, svg, [style*="background-image"] {
      filter: invert(1) hue-rotate(180deg) !important;
    }
  \`;
  document.head.appendChild(style);
})();`,
  },
  {
    id: "remove-ads",
    nameKey: "usPresetRemoveAds",
    matchPatterns: ["<all_urls>"],
    code: `(function() {
  'use strict';
  // Remove common ad elements
  const adSelectors = [
    '[id*="google_ads"]', '[class*="google-ad"]',
    '[id*="ad-"]', '[class*="ad-"]',
    'iframe[src*="doubleclick"]', 'iframe[src*="ads"]',
    '.adsbygoogle', '[data-ad-slot]',
  ];
  setInterval(() => {
    adSelectors.forEach(sel => {
      document.querySelectorAll(sel).forEach(el => el.remove());
    });
  }, 1000);
})();`,
  },
  {
    id: "word-count",
    nameKey: "usPresetWordCount",
    matchPatterns: ["<all_urls>"],
    code: `(function() {
  'use strict';
  // Count words in the page body and show a badge
  function countWords() {
    const text = document.body?.innerText || '';
    const words = text.trim().split(/\\s+/).filter(w => w.length > 0).length;
    const chars = text.length;
    return { words, chars };
  }
  const { words, chars } = countWords();
  const badge = document.createElement('div');
  badge.textContent = words + " words | " + chars + " chars";
  Object.assign(badge.style, {
    position: 'fixed', bottom: '8px', right: '8px', zIndex: '999999',
    background: 'rgba(16,185,129,0.9)', color: '#fff',
    padding: '6px 12px', borderRadius: '6px', fontSize: '12px',
    fontFamily: 'system-ui, sans-serif',
  });
  document.documentElement.appendChild(badge);
})();`,
  },
];

function formatInvokeError(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}

function PluginBrowser() {
  const { t } = useTranslation("plugins");
  const { t: tc } = useTranslation("common");
  const webPlugins = useGlobalStore((s) => s.webPlugins);
  const loadWebPlugins = useGlobalStore((s) => s.loadWebPlugins);
  const addWebPlugin = useGlobalStore((s) => s.addWebPlugin);
  const updateWebPlugin = useGlobalStore((s) => s.updateWebPlugin);
  const deleteWebPlugin = useGlobalStore((s) => s.deleteWebPlugin);
  const reorderWebPlugins = useGlobalStore((s) => s.reorderWebPlugins);
  const recordPluginOpen = useGlobalStore((s) => s.recordPluginOpen);
  const addPresetPlugins = useGlobalStore((s) => s.addPresetPlugins);
  const confirm = useConfirm();
  const userScripts = useGlobalStore((s) => s.userScripts);
  const loadUserScripts = useGlobalStore((s) => s.loadUserScripts);
  const addUserScript = useGlobalStore((s) => s.addUserScript);
  const updateUserScript = useGlobalStore((s) => s.updateUserScript);
  const deleteUserScript = useGlobalStore((s) => s.deleteUserScript);
  const toggleUserScript = useGlobalStore((s) => s.toggleUserScript);

  const [address, setAddress] = useState("");
  const [activeKeys, setActiveKeys] = useState<Set<string>>(new Set());
  const [loading, setLoading] = useState(false);
  const [filter, setFilter] = useState("");
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const [viewMode, setViewMode] = useState<ViewMode>(loadViewMode);
  const [sortMode, setSortMode] = useState<SortMode>("manual");
  const [selectedGroup, setSelectedGroup] = useState<string>("all");
  const [expandedGroups, setExpandedGroups] = useState<Set<string>>(new Set());
  const dragIdRef = useRef<string | null>(null);
  const dragOverIdRef = useRef<string | null>(null);
  const didDragRef = useRef(false);
  const reorderedRef = useRef(false);

  const [formOpen, setFormOpen] = useState(false);
  const [formMode, setFormMode] = useState<FormMode>("add");
  const [formName, setFormName] = useState("");
  const [formUrl, setFormUrl] = useState("");
  const [formGroup, setFormGroup] = useState("");
  const [formTags, setFormTags] = useState("");
  const [formHotkey, setFormHotkey] = useState("");
  const [editingId, setEditingId] = useState<string | null>(null);

  const [activeTab, setActiveTab] = useState<"plugins" | "userscripts">("plugins");
  const [usFormOpen, setUsFormOpen] = useState(false);
  const [usFormName, setUsFormName] = useState("");
  const [usDesc, setUsDesc] = useState("");
  const [usMatch, setUsMatch] = useState<string[]>(["<all_urls>"]);
  const [usCode, setUsCode] = useState("");
  const [usEditingId, setUsEditingId] = useState<string | null>(null);
  const [usSearch, setUsSearch] = useState("");
  const [usShowPresets, setUsShowPresets] = useState(false);
  const [usExpandedCode, setUsExpandedCode] = useState<Set<string>>(new Set());
  const [usTestUrl, setUsTestUrl] = useState("");
  const usCodeRef = useRef<HTMLTextAreaElement>(null);

  const filterInputRef = useRef<HTMLInputElement>(null);
  const canDrag = sortMode === "manual";

  const filteredUserScripts = useMemo(() => {
    const q = usSearch.trim().toLowerCase();
    if (!q) return userScripts;
    return userScripts.filter((s) =>
      s.name.toLowerCase().includes(q) ||
      s.description.toLowerCase().includes(q) ||
      s.matchPatterns.some((p) => p.toLowerCase().includes(q))
    );
  }, [userScripts, usSearch]);

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 3000);
  }, []);

  const changeViewMode = useCallback((mode: ViewMode) => {
    setViewMode(mode);
    try {
      localStorage.setItem(VIEW_MODE_KEY, mode);
    } catch {
      /* ignore */
    }
  }, []);

  useEffect(() => {
    loadWebPlugins();
    loadUserScripts();
  }, [loadWebPlugins, loadUserScripts]);

  useEffect(() => {
    setActiveKeys(new Set(getOpenBrowserKeys()));
  }, []);

  useEffect(() => {
    return onBrowserClosed((key) => {
      setActiveKeys((prev) => {
        const next = new Set(prev);
        next.delete(key);
        return next;
      });
    });
  }, []);

  const addressParsed = useMemo(() => normalizeHttpUrl(address), [address]);

  const openPlugin = useCallback(
    async (pluginUrl: string, pluginId?: string, opts?: { quiet?: boolean }) => {
      const parsed = normalizeHttpUrl(pluginUrl);
      if (!parsed) {
        showMsg("error", t("invalidUrlOpen"));
        return;
      }
      setLoading(true);
      try {
        const key = urlKey(parsed);
        await openBrowser(parsed.href, 900, 700, key);
        setActiveKeys((prev) => new Set(prev).add(key));
        setAddress(parsed.href);
        if (pluginId) void recordPluginOpen(pluginId);
        if (!opts?.quiet) showMsg("success", t("openedInPopup"));
      } catch (e) {
        showMsg("error", t("openFailed", { error: formatInvokeError(e) }));
      } finally {
        setLoading(false);
      }
    },
    [showMsg, t, recordPluginOpen]
  );

  useEffect(() => {
    const registered: Array<{ unregister: () => Promise<void> }> = [];
    let cancelled = false;
    const registerAll = async () => {
      const { register, unregister, isRegistered } = await import("@tauri-apps/plugin-global-shortcut");
      const failed: string[] = [];
      for (const p of webPlugins) {
        if (!p.hotkey) continue;
        try {
          if (await isRegistered(p.hotkey)) {
            await unregister(p.hotkey);
          }
          await register(p.hotkey, async (event) => {
            if (event.state !== "Pressed") return;
            const parsed = normalizeHttpUrl(p.url);
            if (parsed) {
              await openPlugin(parsed.href, p.id, { quiet: true });
            }
          });
          registered.push({
            unregister: async () => {
              try {
                await unregister(p.hotkey);
              } catch {
                /* ignore */
              }
            },
          });
        } catch {
          failed.push(`${p.name} (${p.hotkey})`);
        }
      }
      if (!cancelled && failed.length > 0) {
        showMsg("error", t("hotkeyRegisterFailed", { list: failed.slice(0, 3).join(", ") }));
      }
    };
    void registerAll();
    return () => {
      cancelled = true;
      for (const r of registered) void r.unregister();
    };
  }, [webPlugins, openPlugin, showMsg, t]);

  useEffect(() => {
    if (!formOpen) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setFormOpen(false);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [formOpen]);

  // Ctrl/Cmd+F focuses the site filter (not the address bar)
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "f") {
        e.preventDefault();
        filterInputRef.current?.focus();
        filterInputRef.current?.select();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  const addressAlreadySaved = useMemo(() => {
    if (!addressParsed) return false;
    const key = urlKey(addressParsed);
    return webPlugins.some((p) => {
      const parsed = normalizeHttpUrl(p.url);
      return parsed ? urlKey(parsed) === key : false;
    });
  }, [addressParsed, webPlugins]);

  const groups = useMemo(() => {
    const groupSet = new Set<string>();
    webPlugins.forEach((p) => {
      if (p.group) groupSet.add(p.group);
    });
    return Array.from(groupSet).sort();
  }, [webPlugins]);

  // Default-expand newly seen groups (preserves user collapses)
  useEffect(() => {
    if (groups.length === 0) return;
    setExpandedGroups((prev) => {
      let changed = false;
      const next = new Set(prev);
      for (const g of groups) {
        if (!next.has(g)) {
          next.add(g);
          changed = true;
        }
      }
      return changed ? next : prev;
    });
  }, [groups]);

  const filteredPlugins = useMemo(() => {
    const q = filter.trim().toLowerCase();
    let list = webPlugins;
    if (selectedGroup !== "all") {
      list = list.filter((p) => p.group === selectedGroup);
    }
    if (q) {
      list = list.filter((p) => {
        const parsed = normalizeHttpUrl(p.url);
        const host = parsed?.hostname.toLowerCase() ?? "";
        return (
          p.name.toLowerCase().includes(q) ||
          host.includes(q) ||
          p.url.toLowerCase().includes(q) ||
          p.tags.toLowerCase().includes(q)
        );
      });
    }
    if (sortMode === "recent") {
      return [...list].sort(compareRecent);
    }
    return [...list].sort((a, b) => a.order - b.order);
  }, [webPlugins, filter, selectedGroup, sortMode]);

  const groupedPlugins = useMemo(() => {
    const map = new Map<string, WebPlugin[]>();
    for (const p of filteredPlugins) {
      const g = p.group || "";
      if (!map.has(g)) map.set(g, []);
      map.get(g)!.push(p);
    }
    return map;
  }, [filteredPlugins]);

  const openFormAdd = (prefill?: { name?: string; url?: string; group?: string }) => {
    setFormMode("add");
    setEditingId(null);
    setFormName(prefill?.name ?? "");
    setFormUrl(prefill?.url ?? "");
    setFormGroup(prefill?.group ?? "");
    setFormTags("");
    setFormHotkey("");
    setFormOpen(true);
  };

  const openFormEdit = (p: WebPlugin) => {
    setFormMode("edit");
    setEditingId(p.id);
    setFormName(p.name);
    setFormUrl(p.url);
    setFormGroup(p.group);
    setFormTags(p.tags);
    setFormHotkey(p.hotkey);
    setFormOpen(true);
  };

  const closeForm = () => {
    setFormOpen(false);
    setEditingId(null);
    setFormName("");
    setFormUrl("");
    setFormGroup("");
    setFormTags("");
    setFormHotkey("");
  };

  const savePlugin = async (andOpen: boolean) => {
    if (!formName.trim() || !formUrl.trim()) {
      showMsg("error", t("nameUrlRequired"));
      return;
    }
    const parsed = normalizeHttpUrl(formUrl);
    if (!parsed) {
      showMsg("error", t("invalidUrl"));
      return;
    }

    if (formMode === "edit" && editingId) {
      await updateWebPlugin(editingId, {
        name: formName.trim(),
        url: parsed.href,
        group: formGroup.trim(),
        tags: formTags.trim(),
        hotkey: formHotkey.trim(),
      });
      const id = editingId;
      closeForm();
      showMsg("success", t("updated"));
      if (andOpen) await openPlugin(parsed.href, id);
      return;
    }

    const newId = Date.now().toString();
    await addWebPlugin({
      id: newId,
      name: formName.trim(),
      url: parsed.href,
      group: formGroup.trim(),
      tags: formTags.trim(),
      hotkey: formHotkey.trim(),
      order: webPlugins.length,
      lastOpenedAt: "",
      openCount: 0,
      isPreset: false,
    });
    closeForm();
    if (andOpen) {
      await openPlugin(parsed.href, newId);
    } else {
      showMsg("success", t("added"));
    }
  };

  const handleDelete = async (p: WebPlugin) => {
    const ok = await confirm({
      title: t("deleteTitle"),
      message: t("deleteMessage", { name: p.name }),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (ok) {
      await deleteWebPlugin(p.id);
      showMsg("success", t("deleted"));
    }
  };

  const handleSaveAddress = () => {
    if (!addressParsed) {
      showMsg("error", t("invalidUrlOpen"));
      return;
    }
    openFormAdd({
      name: addressParsed.hostname.replace(/^www\./, ""),
      url: addressParsed.href,
    });
  };

  const handleAddressKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && address.trim()) openPlugin(address);
  };

  const handleFormKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter") {
      e.preventDefault();
      savePlugin(formMode === "add");
    }
  };

  const handleUsFormKeyDown = (e: React.KeyboardEvent) => {
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      // Trigger save
      document.querySelector<HTMLButtonElement>("[data-us-save]")?.click();
    }
  };

  const copyToClipboard = useCallback(async (text: string, msg: string) => {
    try {
      await navigator.clipboard.writeText(text);
      showMsg("success", msg);
    } catch {
      showMsg("error", "Failed to copy");
    }
  }, [showMsg]);

  const clearDragClasses = () => {
    document
      .querySelectorAll(".plugin-item-drag-over, .is-dragging")
      .forEach((el) => {
        el.classList.remove("plugin-item-drag-over");
        el.classList.remove("is-dragging");
      });
  };

  const handleDragStart = (e: React.DragEvent, id: string) => {
    if (!canDrag) {
      e.preventDefault();
      return;
    }
    didDragRef.current = false;
    reorderedRef.current = false;
    dragIdRef.current = id;
    dragOverIdRef.current = null;
    clearDragClasses();
    if (e.currentTarget instanceof HTMLElement) {
      e.currentTarget.classList.add("is-dragging");
    }
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData("text/plain", id);
    if (e.dataTransfer.setDragImage && e.currentTarget instanceof HTMLElement) {
      e.dataTransfer.setDragImage(e.currentTarget, 16, 16);
    }
  };

  const handleDragOver = (e: React.DragEvent, id: string) => {
    if (!canDrag || !dragIdRef.current) return;
    e.preventDefault();
    e.stopPropagation();
    e.dataTransfer.dropEffect = "move";
    if (dragOverIdRef.current === id) return;
    didDragRef.current = true;
    dragOverIdRef.current = id;
    // DOM class only — avoid React re-render mid-drag (breaks WebView2 DnD)
    document
      .querySelectorAll(".plugin-item-drag-over")
      .forEach((el) => el.classList.remove("plugin-item-drag-over"));
    if (e.currentTarget instanceof HTMLElement) {
      e.currentTarget.classList.add("plugin-item-drag-over");
    }
  };

  const applyReorder = (fromId: string, toId: string) => {
    if (fromId === toId) return;
    const allIds = filteredPlugins.map((p) => p.id);
    const fromIdx = allIds.indexOf(fromId);
    const toIdx = allIds.indexOf(toId);
    if (fromIdx === -1 || toIdx === -1) return;
    // Cross-group drop: adopt the target's group so the card visibly moves
    // into that group (an order-only reorder would snap it back to its old
    // group). `withTable` serialises same-table writes, so the group update
    // lands before the reorder reads state.
    const fromPlugin = webPlugins.find((p) => p.id === fromId);
    const toPlugin = webPlugins.find((p) => p.id === toId);
    if (fromPlugin && toPlugin && fromPlugin.group !== toPlugin.group) {
      void updateWebPlugin(fromId, { group: toPlugin.group });
    }
    const newIds = [...allIds];
    newIds.splice(fromIdx, 1);
    newIds.splice(toIdx, 0, fromId);
    const fullIds = webPlugins.map((p) => p.id);
    const filteredSet = new Set(newIds);
    const result: string[] = [];
    let fi = 0;
    for (const id of fullIds) {
      if (filteredSet.has(id)) {
        result.push(newIds[fi++]);
      } else {
        result.push(id);
      }
    }
    void reorderWebPlugins(result);
  };

  const finishDrag = (fromId: string | null, toId: string | null) => {
    if (!reorderedRef.current && fromId && toId && fromId !== toId) {
      reorderedRef.current = true;
      didDragRef.current = true;
      applyReorder(fromId, toId);
    }
    dragIdRef.current = null;
    dragOverIdRef.current = null;
    clearDragClasses();
  };

  const handleDrop = (e: React.DragEvent, id: string) => {
    if (!canDrag) return;
    e.preventDefault();
    e.stopPropagation();
    const fromId = dragIdRef.current || e.dataTransfer.getData("text/plain") || null;
    finishDrag(fromId, id);
  };

  const handleDragEnd = () => {
    finishDrag(dragIdRef.current, dragOverIdRef.current);
  };

  const handleItemClick = (p: WebPlugin, parsed: URL | null) => {
    if (didDragRef.current) {
      didDragRef.current = false;
      return;
    }
    if (!parsed) {
      showMsg("error", t("invalidPluginUrl", { name: p.name }));
      return;
    }
    void openPlugin(parsed.href, p.id);
  };

  const toggleGroup = (g: string) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(g)) next.delete(g);
      else next.add(g);
      return next;
    });
  };

  const toggleGroupFilter = (g: string) => {
    setSelectedGroup((prev) => (prev === g ? "all" : g));
  };

  const handleExport = async () => {
    const data = {
      version: 1,
      sites: webPlugins.map((p) => ({
        name: p.name,
        url: p.url,
        group: p.group,
        tags: p.tags,
        hotkey: p.hotkey,
      })),
    };
    try {
      const path = await useGlobalStore.getState().invokeSaveTextFile(
        JSON.stringify(data, null, 2),
        "web-tools-backup.json",
        t("export")
      );
      showMsg("success", t("exportedTo", { path }));
    } catch (e) {
      const msg = String(e);
      if (!msg.includes("取消")) showMsg("error", msg);
    }
  };

  const handleImport = async () => {
    try {
      const text = await useGlobalStore.getState().invokePickTextFile(t("import"));
      const data = JSON.parse(text);
      const sites = data.sites || data;
      if (!Array.isArray(sites)) {
        showMsg("error", t("importInvalid"));
        return;
      }
      let imported = 0;
      const existingUrls = new Set(
        webPlugins
          .map((p) => normalizeHttpUrl(p.url))
          .filter((u): u is URL => !!u)
          .map((u) => u.href)
      );
      for (const s of sites) {
        if (!s.url || !s.name) continue;
        const parsed = normalizeHttpUrl(s.url);
        if (!parsed) continue;
        if (existingUrls.has(parsed.href)) continue;
        existingUrls.add(parsed.href);
        await addWebPlugin({
          id: `import-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
          name: s.name,
          url: parsed.href,
          group: s.group || "",
          tags: s.tags || "",
          hotkey: s.hotkey || "",
          order: webPlugins.length + imported,
          lastOpenedAt: "",
          openCount: 0,
          isPreset: false,
        });
        imported++;
      }
      showMsg("success", t("imported", { count: imported }));
    } catch (e) {
      const msg = String(e);
      if (msg.includes("取消")) return;
      if (msg.includes("JSON") || e instanceof SyntaxError) {
        showMsg("error", t("importInvalid"));
      } else {
        showMsg("error", t("importFailed"));
      }
    }
  };

  const renderTags = (tags: string) => {
    const list = parseTags(tags);
    if (list.length === 0) return null;
    return (
      <div className="plugin-tag-list">
        {list.map((tag) => (
          <button
            key={tag}
            type="button"
            className="plugin-tag-chip"
            title={tag}
            onClick={(e) => {
              e.stopPropagation();
              setFilter(tag);
              filterInputRef.current?.focus();
            }}
          >
            {tag}
          </button>
        ))}
      </div>
    );
  };

  const renderPluginItem = (p: WebPlugin) => {
    const parsed = normalizeHttpUrl(p.url);
    const host = parsed?.hostname || "—";
    const key = parsed ? urlKey(parsed) : p.id;
    const isActive = activeKeys.has(key);
    return (
      <div
        key={p.id}
        className={`plugin-item ${!parsed ? "plugin-item-invalid" : ""} ${
          isActive ? "plugin-item-active" : ""
        } ${canDrag ? "is-draggable" : ""}`}
        draggable={canDrag}
        onDragStart={(e) => handleDragStart(e, p.id)}
        onDragOver={(e) => handleDragOver(e, p.id)}
        onDrop={(e) => handleDrop(e, p.id)}
        onDragEnd={handleDragEnd}
        onClick={() => handleItemClick(p, parsed)}
      >
        {canDrag && (
          <div
            className="plugin-item-drag"
            title={t("sortManual")}
            onClick={(e) => e.stopPropagation()}
          >
            <GripVertical size={12} />
          </div>
        )}
        <div className="plugin-item-icon">
          {parsed ? (
            <img
              src={`https://www.google.com/s2/favicons?domain=${host}&sz=32`}
              alt=""
              draggable={false}
              onError={(e) => {
                (e.target as HTMLImageElement).style.display = "none";
                (e.target as HTMLImageElement).nextElementSibling?.classList.remove("hidden");
              }}
            />
          ) : null}
          <Globe
            size={14}
            className={`plugin-item-icon-fallback ${parsed ? "hidden" : ""}`}
          />
        </div>
        <div className="plugin-item-content">
          <div className="plugin-item-name">
            {p.name}
            {isActive && <span className="plugin-item-badge">{t("active")}</span>}
            {p.hotkey && (
              <span className="plugin-item-hotkey" title={t("hotkeyLabel")}>
                <Keyboard size={10} /> {p.hotkey}
              </span>
            )}
          </div>
          <div className="plugin-item-meta">
            <span className="plugin-item-url">{host}</span>
            {p.openCount > 0 && (
              <span className="plugin-item-count" title={t("openCountLabel")}>
                {p.openCount}
              </span>
            )}
            {renderTags(p.tags)}
          </div>
        </div>
        <div
          className="plugin-item-actions"
          onClick={(e) => e.stopPropagation()}
        >
          <button
            className="plugin-item-btn"
            onClick={() => copyToClipboard(parsed?.href || p.url, t("urlCopied"))}
            title={t("copyUrl")}
            type="button"
          >
            <ClipboardCopy size={11} />
          </button>
          <button
            className="plugin-item-btn"
            onClick={() => openFormAdd({ name: p.name + " (copy)", url: p.url, group: p.group })}
            title={t("duplicate")}
            type="button"
          >
            <Copy size={11} />
          </button>
          <button
            className="plugin-item-btn"
            onClick={() => openFormEdit(p)}
            title={t("edit")}
            type="button"
          >
            <Edit2 size={11} />
          </button>
          <button
            className="plugin-item-btn plugin-item-btn-danger"
            onClick={() => handleDelete(p)}
            title={tc("actions.delete")}
            type="button"
          >
            <Trash2 size={11} />
          </button>
        </div>
      </div>
    );
  };

  const renderCardItem = (p: WebPlugin) => {
    const parsed = normalizeHttpUrl(p.url);
    const host = parsed?.hostname || "—";
    const key = parsed ? urlKey(parsed) : p.id;
    const isActive = activeKeys.has(key);
    return (
      <div
        key={p.id}
        className={`plugin-card ${isActive ? "plugin-card-active" : ""} ${
          canDrag ? "is-draggable" : ""
        }`}
        draggable={canDrag}
        onDragStart={(e) => handleDragStart(e, p.id)}
        onDragOver={(e) => handleDragOver(e, p.id)}
        onDrop={(e) => handleDrop(e, p.id)}
        onDragEnd={handleDragEnd}
        onClick={() => handleItemClick(p, parsed)}
      >
        <div className="plugin-card-header">
          <div className="plugin-card-icon">
            {parsed ? (
              <img
                src={`https://www.google.com/s2/favicons?domain=${host}&sz=32`}
                alt=""
                draggable={false}
                onError={(e) => {
                  (e.target as HTMLImageElement).style.display = "none";
                  (e.target as HTMLImageElement).nextElementSibling?.classList.remove("hidden");
                }}
              />
            ) : null}
            <Globe
              size={20}
              className={`plugin-item-icon-fallback ${parsed ? "hidden" : ""}`}
            />
          </div>
          <div className="plugin-card-actions">
            <button
              className="plugin-item-btn"
              onClick={(e) => { e.stopPropagation(); copyToClipboard(parsed?.href || p.url, t("urlCopied")); }}
              title={t("copyUrl")}
              type="button"
            >
              <ClipboardCopy size={11} />
            </button>
            <button
              className="plugin-item-btn"
              onClick={(e) => { e.stopPropagation(); openFormEdit(p); }}
              title={t("edit")}
              type="button"
            >
              <Edit2 size={11} />
            </button>
            <button
              className="plugin-item-btn plugin-item-btn-danger"
              onClick={(e) => { e.stopPropagation(); handleDelete(p); }}
              title={tc("actions.delete")}
              type="button"
            >
              <Trash2 size={11} />
            </button>
          </div>
        </div>
        <div className="plugin-card-body">
          <div className="plugin-card-name">{p.name}</div>
          <div className="plugin-card-url">{host}</div>
          {renderTags(p.tags)}
        </div>
        <div className="plugin-card-footer">
          {p.hotkey && (
            <span className="plugin-card-hotkey">
              <Keyboard size={10} /> {p.hotkey}
            </span>
          )}
          {p.openCount > 0 && (
            <span className="plugin-card-count">{p.openCount}x</span>
          )}
          {isActive && <span className="plugin-item-badge">{t("active")}</span>}
        </div>
      </div>
    );
  };

  const renderGroupHeader = (group: string, items: WebPlugin[]) => {
    const isUngrouped = group === "";
    const isExpanded = isUngrouped || expandedGroups.has(group) || selectedGroup !== "all";
    return (
      <div
        key={isUngrouped ? "group-ungrouped" : `group-${group}`}
        className="plugin-group"
      >
        {isUngrouped ? (
          // Static header: ungrouped sites always show, never collapsed
          <div className="plugin-group-header plugin-group-header-static">
            <Inbox size={14} />
            <span>{t("ungrouped")}</span>
            <span className="plugin-group-count">{items.length}</span>
          </div>
        ) : (
          <button
            className="plugin-group-header"
            onClick={() => toggleGroup(group)}
            type="button"
          >
            {isExpanded ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            <FolderOpen size={14} />
            <span>{group}</span>
            <span className="plugin-group-count">{items.length}</span>
          </button>
        )}
        {isExpanded && (
          <div className="plugin-group-items">
            {viewMode === "list"
              ? items.map(renderPluginItem)
              : <div className="plugin-card-grid">{items.map(renderCardItem)}</div>}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="plugin-browser">
      <div className="plugin-tabs">
        <button
          type="button"
          className={`plugin-tab ${activeTab === "plugins" ? "active" : ""}`}
          onClick={() => setActiveTab("plugins")}
        >
          <Globe size={14} />
          <span>{t("tabPlugins")}</span>
        </button>
        <button
          type="button"
          className={`plugin-tab ${activeTab === "userscripts" ? "active" : ""}`}
          onClick={() => setActiveTab("userscripts")}
        >
          <Code size={14} />
          <span>{t("tabUserscripts")}</span>
          <span className="plugin-tab-count">{userScripts.length}</span>
        </button>
      </div>

      {activeTab === "plugins" && (
      <div className="plugin-shell">
        <div className="plugin-toolbar">
          <div className="plugin-toolbar-label">{t("openUrlLabel")}</div>
          <div className="plugin-toolbar-url">
            <div className="plugin-url-input-wrapper plugin-url-grow plugin-address-input">
              <Link2 size={13} className="plugin-url-icon" />
              <input
                className="plugin-url-input"
                value={address}
                onChange={(e) => setAddress(e.target.value)}
                onKeyDown={handleAddressKeyDown}
                placeholder={t("urlPlaceholder")}
                aria-label={t("openUrlLabel")}
              />
            </div>
            <div className="plugin-toolbar-actions">
              <button
                className="btn btn-primary btn-small"
                onClick={() => address.trim() && openPlugin(address)}
                disabled={loading || !address.trim()}
              >
                <ExternalLink size={12} /> {t("open")}
              </button>
              {addressParsed && !addressAlreadySaved ? (
                <button
                  className="btn btn-secondary btn-small"
                  onClick={handleSaveAddress}
                  disabled={loading}
                  title={t("saveAsPlugin")}
                >
                  <BookmarkPlus size={12} /> {t("saveAsPlugin")}
                </button>
              ) : null}
              {activeKeys.size > 0 ? (
                <button
                  className="btn btn-secondary btn-small"
                  onClick={async () => { await closeBrowser(); setActiveKeys(new Set()); }}
                  disabled={loading}
                  title={t("closeAllWindows")}
                >
                  <X size={12} /> {t("closeAllWindows")}
                </button>
              ) : null}
            </div>
          </div>
        </div>

        <div className="plugin-main">
          <div className="plugin-header">
            <div className="plugin-header-title">
              <Globe size={16} />
              <h2>{t("myPlugins")}</h2>
              <span className="plugin-count">{webPlugins.length}</span>
            </div>
            <div className="plugin-header-actions">
              <div className="plugin-view-toggle">
                <button
                  className={`plugin-view-btn ${viewMode === "list" ? "active" : ""}`}
                  onClick={() => changeViewMode("list")}
                  title={t("listView")}
                  type="button"
                >
                  <List size={14} />
                </button>
                <button
                  className={`plugin-view-btn ${viewMode === "card" ? "active" : ""}`}
                  onClick={() => changeViewMode("card")}
                  title={t("cardView")}
                  type="button"
                >
                  <LayoutGrid size={14} />
                </button>
              </div>
              <button
                className="btn btn-secondary btn-small"
                onClick={handleImport}
                title={t("import")}
              >
                <Upload size={12} />
              </button>
              <button
                className="btn btn-secondary btn-small"
                onClick={handleExport}
                title={t("export")}
              >
                <Download size={12} />
              </button>
              {webPlugins.length === 0 && (
                <button
                  className="btn btn-secondary btn-small"
                  onClick={addPresetPlugins}
                  title={t("addPresets")}
                >
                  {t("addPresets")}
                </button>
              )}
              <button
                className="btn btn-primary btn-small"
                onClick={() => openFormAdd()}
                type="button"
              >
                <Plus size={12} /> {t("add")}
              </button>
            </div>
          </div>

          {webPlugins.length > 0 && (
            <div className="plugin-filter-row">
              <div className="plugin-url-input-wrapper plugin-filter-input">
                <Search size={13} className="plugin-url-icon" />
                <input
                  ref={filterInputRef}
                  className="plugin-url-input"
                  value={filter}
                  onChange={(e) => setFilter(e.target.value)}
                  placeholder={t("searchPlaceholder")}
                  aria-label={t("searchPlaceholder")}
                />
              </div>
              <div className="plugin-sort-toggle" role="group" aria-label={t("sortManual")}>
                <button
                  type="button"
                  className={`plugin-sort-btn ${sortMode === "manual" ? "active" : ""}`}
                  onClick={() => setSortMode("manual")}
                  title={t("sortManualHint")}
                >
                  <ArrowUpDown size={12} />
                  <span>{t("sortManual")}</span>
                </button>
                <button
                  type="button"
                  className={`plugin-sort-btn ${sortMode === "recent" ? "active" : ""}`}
                  onClick={() => setSortMode("recent")}
                  title={t("sortRecentHint")}
                >
                  <span>{t("sortRecent")}</span>
                </button>
              </div>
              {sortMode === "recent" && (
                <span className="plugin-sort-hint runtime-muted">{t("sortRecentHint")}</span>
              )}
              <div className="plugin-group-filters">
                <button
                  className={`plugin-group-filter-btn ${selectedGroup === "all" ? "active" : ""}`}
                  onClick={() => setSelectedGroup("all")}
                  type="button"
                >
                  {t("allGroups")}
                </button>
                {groups.map((g) => (
                  <button
                    key={g}
                    className={`plugin-group-filter-btn ${selectedGroup === g ? "active" : ""}`}
                    onClick={() => toggleGroupFilter(g)}
                    type="button"
                  >
                    {g}
                  </button>
                ))}
              </div>
            </div>
          )}

          <div className="plugin-list">
            {webPlugins.length === 0 ? (
              <div className="plugin-empty">
                <div className="plugin-empty-icon">
                  <Globe size={32} strokeWidth={1.5} />
                </div>
                <p className="plugin-empty-title">{t("noPlugins")}</p>
                <p className="plugin-hint-text">{t("hint")}</p>
                <div className="plugin-empty-actions">
                  <button
                    className="btn btn-primary btn-small"
                    onClick={() => openFormAdd()}
                    type="button"
                  >
                    <Plus size={12} /> {t("emptyCta")}
                  </button>
                  <button
                    className="btn btn-secondary btn-small"
                    onClick={addPresetPlugins}
                    type="button"
                  >
                    {t("addPresets")}
                  </button>
                </div>
              </div>
            ) : filteredPlugins.length === 0 ? (
              <div className="plugin-empty plugin-empty-compact">
                <p className="plugin-empty-title">{t("noFilterResults")}</p>
              </div>
            ) : selectedGroup !== "all" ? (
              viewMode === "list" ? (
                filteredPlugins.map(renderPluginItem)
              ) : (
                <div className="plugin-card-grid">
                  {filteredPlugins.map(renderCardItem)}
                </div>
              )
            ) : (
              <>
                {Array.from(groupedPlugins.entries())
                  .filter(([g]) => g !== "")
                  .map(([group, items]) => renderGroupHeader(group, items))}
                {/* Ungrouped sites render as their own section, always last,
                    so they never look like stray cards of the group above */}
                {groupedPlugins.has("") &&
                  renderGroupHeader("", groupedPlugins.get("")!)}
              </>
            )}
          </div>
        </div>
      </div>
      )}

      {activeTab === "userscripts" && (
      <div className="plugin-shell">
        <div className="plugin-main">
          <div className="plugin-header">
            <div className="plugin-header-title">
              <Code size={16} />
              <h2>{t("tabUserscripts")}</h2>
              <span className="plugin-count">{userScripts.length}</span>
            </div>
            <div className="plugin-header-actions">
              <button
                className="btn btn-secondary btn-small"
                onClick={async () => {
                  const data = {
                    version: 2,
                    userscripts: userScripts.map((s) => ({
                      name: s.name,
                      description: s.description,
                      matchPatterns: s.matchPatterns,
                      code: s.code,
                      enabled: s.enabled,
                    })),
                  };
                  try {
                    const path = await useGlobalStore.getState().invokeSaveTextFile(
                      JSON.stringify(data, null, 2),
                      "userscripts-backup.json",
                      t("usExport")
                    );
                    showMsg("success", t("exportedTo", { path }));
                  } catch (e) {
                    const msg = String(e);
                    if (!msg.includes("取消")) showMsg("error", msg);
                  }
                }}
                title={t("usExport")}
                disabled={userScripts.length === 0}
              >
                <Download size={12} /> {t("export")}
              </button>
              <button
                className="btn btn-secondary btn-small"
                onClick={async () => {
                  try {
                    const text = await useGlobalStore.getState().invokePickTextFile(t("usImport"));
                    const data = JSON.parse(text);
                    const scripts = data.userscripts || data.scripts || data;
                    if (!Array.isArray(scripts)) {
                      showMsg("error", t("importInvalid"));
                      return;
                    }
                    let imported = 0;
                    for (const s of scripts) {
                      if (!s.name || !s.code) continue;
                      // 兼容新旧格式
                      let patterns: string[];
                      if (Array.isArray(s.matchPatterns) && s.matchPatterns.length > 0) {
                        patterns = s.matchPatterns;
                      } else if (s.matchPattern) {
                        patterns = [s.matchPattern];
                      } else {
                        patterns = ["<all_urls>"];
                      }
                      await addUserScript({
                        id: `import-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
                        name: s.name,
                        description: s.description || "",
                        matchPatterns: patterns,
                        code: s.code,
                        enabled: s.enabled !== false,
                      });
                      imported++;
                    }
                    showMsg("success", t("imported", { count: imported }));
                  } catch (e) {
                    const msg = String(e);
                    if (msg.includes("取消")) return;
                    showMsg("error", t("importFailed"));
                  }
                }}
                title={t("usImport")}
              >
                <Upload size={12} /> {t("import")}
              </button>
              <button
                className="btn btn-secondary btn-small"
                onClick={() => setUsShowPresets(!usShowPresets)}
                title={t("usPresets")}
              >
                <FileText size={12} /> {t("usPresets")}
              </button>
              <button
                className="btn btn-primary btn-small"
                onClick={() => {
                  setUsEditingId(null);
                  setUsFormName("");
                  setUsDesc("");
                  setUsMatch(["<all_urls>"]);
                  setUsCode("");
                  setUsFormOpen(true);
                }}
                type="button"
              >
                <Plus size={12} /> {t("add")}
              </button>
            </div>
          </div>

          {usShowPresets && (
            <div className="us-presets-bar">
              <div className="us-presets-header">
                <Zap size={14} />
                <span>{t("usPresets")}</span>
                <button
                  className="us-presets-close"
                  onClick={() => setUsShowPresets(false)}
                  aria-label={tc("actions.close")}
                >
                  <X size={14} />
                </button>
              </div>
              <div className="us-presets-grid">
                {SCRIPT_PRESETS.map((preset) => (
                  <button
                    key={preset.id}
                    className="us-preset-card"
                    onClick={() => {
                      setUsEditingId(null);
                      setUsFormName(t(preset.nameKey));
                      setUsDesc("");
                      setUsMatch(preset.matchPatterns);
                      setUsCode(preset.code);
                      setUsFormOpen(true);
                      setUsShowPresets(false);
                    }}
                    type="button"
                  >
                    <Code size={16} />
                    <span className="us-preset-name">{t(preset.nameKey)}</span>
                    <span className="us-preset-match">{preset.matchPatterns.join(", ")}</span>
                  </button>
                ))}
              </div>
            </div>
          )}

          {userScripts.length > 0 && (
            <div className="plugin-filter-row">
              <div className="plugin-url-input-wrapper plugin-filter-input">
                <Search size={13} className="plugin-url-icon" />
                <input
                  className="plugin-url-input"
                  value={usSearch}
                  onChange={(e) => setUsSearch(e.target.value)}
                  placeholder={t("usSearchPlaceholder")}
                  aria-label={t("usSearchPlaceholder")}
                />
              </div>
            </div>
          )}

          <div className="plugin-list">
            {userScripts.length === 0 ? (
              <div className="plugin-empty">
                <div className="plugin-empty-icon">
                  <Code size={32} strokeWidth={1.5} />
                </div>
                <p className="plugin-empty-title">{t("usNoScripts")}</p>
                <p className="plugin-hint-text">{t("usHint")}</p>
                <div className="plugin-empty-actions">
                  <button
                    className="btn btn-primary btn-small"
                    onClick={() => setUsFormOpen(true)}
                    type="button"
                  >
                    <Plus size={12} /> {t("usAddFirst")}
                  </button>
                  <button
                    className="btn btn-secondary btn-small"
                    onClick={() => setUsShowPresets(true)}
                    type="button"
                  >
                    <FileText size={12} /> {t("usPresets")}
                  </button>
                </div>
              </div>
            ) : filteredUserScripts.length === 0 ? (
              <div className="plugin-empty plugin-empty-compact">
                <p className="plugin-empty-title">{t("noFilterResults")}</p>
              </div>
            ) : (
              <div className="userscript-list">
                {filteredUserScripts.map((s) => (
                  <UserscriptCard
                    key={s.id}
                    script={s}
                    t={t}
                    tc={tc}
                    onToggle={() => toggleUserScript(s.id)}
                    onEdit={() => {
                      setUsEditingId(s.id);
                      setUsFormName(s.name);
                      setUsDesc(s.description);
                      setUsMatch(s.matchPatterns.length > 0 ? s.matchPatterns : ["<all_urls>"]);
                      setUsCode(s.code);
                      setUsFormOpen(true);
                    }}
                    onDelete={async () => {
                      const ok = await confirm({
                        title: t("deleteTitle"),
                        message: t("deleteMessage", { name: s.name }),
                        confirmText: tc("actions.delete"),
                        icon: "danger",
                      });
                      if (ok) deleteUserScript(s.id);
                    }}
                    onDuplicate={async () => {
                      await addUserScript({
                        id: Date.now().toString(),
                        name: s.name + " (copy)",
                        description: s.description,
                        matchPatterns: [...s.matchPatterns],
                        code: s.code,
                        enabled: false,
                      });
                      showMsg("success", t("usAdded"));
                    }}
                    expanded={usExpandedCode.has(s.id)}
                    onToggleExpand={() => {
                      setUsExpandedCode((prev) => {
                        const next = new Set(prev);
                        if (next.has(s.id)) next.delete(s.id);
                        else next.add(s.id);
                        return next;
                      });
                    }}
                  />
                ))}
              </div>
            )}
          </div>
        </div>
      </div>
      )}

      {formOpen && (
        <div className="modal-overlay" onClick={closeForm}>
          <div className="modal plugin-edit-modal" onClick={(e) => e.stopPropagation()}>
            <div className="plugin-edit-modal-header">
              {formMode === "edit" ? <Edit2 size={16} /> : <Plus size={16} />}
              {formMode === "edit" ? t("editPlugin") : t("addPlugin")}
              <button
                type="button"
                className="modal-close"
                onClick={closeForm}
                aria-label={tc("actions.close")}
                title={tc("actions.close")}
              >
                <X size={16} />
              </button>
            </div>
            <div className="plugin-edit-modal-body">
              <div className="input-group">
                <label className="input-label">{t("name")}</label>
                <input
                  className="input-field"
                  value={formName}
                  onChange={(e) => setFormName(e.target.value)}
                  onKeyDown={handleFormKeyDown}
                  placeholder={t("name")}
                  autoFocus
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("url")}</label>
                <input
                  className="input-field"
                  value={formUrl}
                  onChange={(e) => setFormUrl(e.target.value)}
                  onKeyDown={handleFormKeyDown}
                  placeholder="example.com"
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("group")}</label>
                <input
                  className="input-field"
                  value={formGroup}
                  onChange={(e) => setFormGroup(e.target.value)}
                  placeholder={t("groupPlaceholder")}
                  list="group-options"
                />
                <datalist id="group-options">
                  {groups.map((g) => (
                    <option key={g} value={g} />
                  ))}
                </datalist>
              </div>
              <div className="input-group">
                <label className="input-label">{t("tags")}</label>
                <input
                  className="input-field"
                  value={formTags}
                  onChange={(e) => setFormTags(e.target.value)}
                  placeholder={t("tagsPlaceholder")}
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("hotkey")}</label>
                <input
                  className="input-field"
                  value={formHotkey}
                  onChange={(e) => setFormHotkey(e.target.value)}
                  placeholder={t("hotkeyPlaceholder")}
                />
              </div>
            </div>
            <div className="modal-actions">
              <button className="btn btn-secondary" onClick={closeForm} type="button">
                {tc("actions.cancel")}
              </button>
              {formMode === "add" ? (
                <>
                  <button
                    className="btn btn-secondary"
                    onClick={() => savePlugin(false)}
                    type="button"
                  >
                    {t("addOnly")}
                  </button>
                  <button
                    className="btn btn-primary"
                    onClick={() => savePlugin(true)}
                    type="button"
                    disabled={loading}
                  >
                    <ExternalLink size={14} /> {t("addAndOpen")}
                  </button>
                </>
              ) : (
                <button
                  className="btn btn-primary"
                  onClick={() => savePlugin(false)}
                  type="button"
                >
                  <Check size={14} /> {t("save")}
                </button>
              )}
            </div>
          </div>
        </div>
      )}

      {usFormOpen && (
        <div className="modal-overlay" onClick={() => setUsFormOpen(false)}>
          <div className="modal plugin-edit-modal us-edit-modal" onClick={(e) => e.stopPropagation()} onKeyDown={handleUsFormKeyDown}>
            <div className="plugin-edit-modal-header">
              {usEditingId ? <Edit2 size={16} /> : <Plus size={16} />}
              {usEditingId ? t("usEditScript") : t("usAddScript")}
              <button
                type="button"
                className="modal-close"
                onClick={() => setUsFormOpen(false)}
                aria-label={tc("actions.close")}
                title={tc("actions.close")}
              >
                <X size={16} />
              </button>
            </div>
            <div className="plugin-edit-modal-body">
              <div className="input-group">
                <label className="input-label">{t("name")}</label>
                <input
                  className="input-field"
                  value={usFormName}
                  onChange={(e) => setUsFormName(e.target.value)}
                  placeholder={t("name")}
                  autoFocus
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("usDescription")}</label>
                <input
                  className="input-field"
                  value={usDesc}
                  onChange={(e) => setUsDesc(e.target.value)}
                  placeholder={t("usDescriptionPlaceholder")}
                />
              </div>
              <div className="input-group">
                <label className="input-label">{t("usMatchPattern")}</label>
                <div className="us-match-patterns">
                  {usMatch.map((pattern, idx) => {
                    const isAllSites = pattern === "<all_urls>";
                    return (
                      <div key={idx} className="us-match-row">
                        {isAllSites && usMatch.length === 1 ? (
                          <div className="us-match-all-sites">
                            <Globe size={14} />
                            <span>{t("usAllSites")}</span>
                            <button
                              type="button"
                              className="us-match-clear"
                              onClick={() => setUsMatch([""])}
                              title={t("usAddPattern")}
                            >
                              <Edit2 size={12} />
                            </button>
                          </div>
                        ) : (
                          <>
                            <input
                              className="input-field us-match-input"
                              value={pattern}
                              onChange={(e) => {
                                const next = [...usMatch];
                                next[idx] = e.target.value;
                                setUsMatch(next);
                              }}
                              placeholder="*://example.com/*"
                            />
                            <button
                              type="button"
                              className="us-match-remove"
                              onClick={() => setUsMatch(usMatch.filter((_, i) => i !== idx))}
                              title={tc("actions.delete")}
                            >
                              <X size={14} />
                            </button>
                          </>
                        )}
                      </div>
                    );
                  })}
                  {!(usMatch.length === 1 && usMatch[0] === "<all_urls>") && (
                    <button
                      type="button"
                      className="us-match-add"
                      onClick={() => setUsMatch([...usMatch, ""])}
                    >
                      <Plus size={14} /> {t("usAddPattern")}
                    </button>
                  )}
                </div>
                <span className="input-hint">{t("usMatchHint")}</span>
                {/* Test pattern area */}
                <div className="us-test-row">
                  <div className="us-test-input-wrapper">
                    <Search size={12} className="us-test-icon" />
                    <input
                      className="input-field us-test-input"
                      value={usTestUrl}
                      onChange={(e) => setUsTestUrl(e.target.value)}
                      placeholder={t("usTestUrlPlaceholder")}
                    />
                  </div>
                  {usTestUrl.trim() && (
                    <span className={`us-test-result ${usMatch.some((p) => testMatchPattern(p, usTestUrl.trim())) ? "is-match" : "is-no-match"}`}>
                      {usMatch.some((p) => testMatchPattern(p, usTestUrl.trim()))
                        ? t("usPatternMatches")
                        : t("usPatternNoMatch")}
                    </span>
                  )}
                </div>
              </div>
              <div className="input-group">
                <label className="input-label">
                  {t("usCode")}
                  <span className="input-label-extra">
                    {usCode.split("\n").length} {t("usLineCountSplit")}
                  </span>
                </label>
                <textarea
                  ref={usCodeRef}
                  className="input-field us-code-editor"
                  value={usCode}
                  onChange={(e) => setUsCode(e.target.value)}
                  placeholder={t("usCodePlaceholder")}
                  rows={12}
                  spellCheck={false}
                />
              </div>
            </div>
            <div className="modal-actions">
              <button className="btn btn-secondary" onClick={() => { setUsFormOpen(false); setUsTestUrl(""); }} type="button">
                {tc("actions.cancel")}
              </button>
              <button
                data-us-save
                className="btn btn-primary"
                type="button"
                disabled={!usFormName.trim() || !usCode.trim()}
                onClick={async () => {
                  if (!usFormName.trim() || !usCode.trim()) return;
                  // 过滤空模式，如果没有有效模式则默认 <all_urls>
                  const patterns = usMatch.map((p) => p.trim()).filter(Boolean);
                  const finalPatterns = patterns.length > 0 ? patterns : ["<all_urls>"];
                  try {
                    if (usEditingId) {
                      await updateUserScript(usEditingId, {
                        name: usFormName.trim(),
                        description: usDesc.trim(),
                        matchPatterns: finalPatterns,
                        code: usCode,
                      });
                      showMsg("success", t("usSaved"));
                    } else {
                      await addUserScript({
                        id: Date.now().toString(),
                        name: usFormName.trim(),
                        description: usDesc.trim(),
                        matchPatterns: finalPatterns,
                        code: usCode,
                        enabled: true,
                      });
                      showMsg("success", t("usAdded"));
                    }
                    setUsFormOpen(false);
                    setUsEditingId(null);
                    setUsFormName("");
                    setUsDesc("");
                    setUsMatch(["<all_urls>"]);
                    setUsCode("");
                    setUsTestUrl("");
                  } catch (e) {
                    showMsg("error", String(e));
                  }
                }}
              >
                <Check size={14} /> {t("save")}
              </button>
            </div>
          </div>
        </div>
      )}

      {message && <div className={`toast toast-${message.type}`}>{message.text}</div>}
    </div>
  );
}

export default PluginBrowser;

/** Individual userscript card with expand/collapse code preview */
function UserscriptCard({
  script,
  t,
  tc,
  onToggle,
  onEdit,
  onDelete,
  onDuplicate,
  expanded,
  onToggleExpand,
}: {
  script: UserScript;
  t: (key: string) => string;
  tc: (key: string) => string;
  onToggle: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onDuplicate: () => void;
  expanded: boolean;
  onToggleExpand: () => void;
}) {
  const lineCount = script.code.split("\n").length;
  const charCount = script.code.length;
  const lineLabel = t("usLineCount").replace("{{count}}", String(lineCount));
  const charLabel = t("usCharCount").replace("{{count}}", String(charCount));
  const timeAgo = useMemo(() => {
    const ts = script.updatedAt || script.createdAt;
    if (!ts) return "";
    const diff = Date.now() - Date.parse(ts);
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return t("justNow");
    if (mins < 60) return `${mins}m`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h`;
    const days = Math.floor(hours / 24);
    return `${days}d`;
  }, [script.updatedAt, script.createdAt, t]);

  return (
    <div className={`userscript-item ${script.enabled ? "" : "userscript-disabled"}`}>
      <div className="userscript-header">
        <div className="userscript-info">
          <div className="userscript-name-row">
            <span className="userscript-name">{script.name}</span>
            <span className={`userscript-status ${script.enabled ? "is-enabled" : "is-disabled"}`}>
              {script.enabled ? t("usEnabled") : t("usDisabled")}
            </span>
          </div>
          {script.description && (
            <div className="userscript-desc">{script.description}</div>
          )}
          <div className="userscript-meta-row">
            <div className="userscript-match">
              <Link2 size={11} />
              {script.matchPatterns.length === 1 && script.matchPatterns[0] === "<all_urls>" ? (
                <span>{t("usMatchesAll")}</span>
              ) : script.matchPatterns.length <= 2 ? (
                script.matchPatterns.map((p, i) => (
                  <span key={i} className="us-pattern-tag">{p}</span>
                ))
              ) : (
                <>
                  {script.matchPatterns.slice(0, 2).map((p, i) => (
                    <span key={i} className="us-pattern-tag">{p}</span>
                  ))}
                  <span className="us-pattern-more">+{script.matchPatterns.length - 2}</span>
                </>
              )}
            </div>
            <span className="userscript-stats">
              {lineLabel} · {timeAgo}
            </span>
          </div>
        </div>
        <div className="userscript-controls">
          <label className="userscript-toggle" title={script.enabled ? t("usEnabled") : t("usDisabled")}>
            <input
              type="checkbox"
              checked={script.enabled}
              onChange={onToggle}
            />
            <span className="userscript-toggle-slider" />
          </label>
          <button
            className="plugin-item-btn"
            onClick={onEdit}
            title={t("edit")}
            type="button"
          >
            <Edit2 size={11} />
          </button>
          <button
            className="plugin-item-btn"
            onClick={onDuplicate}
            title={t("usDuplicate")}
            type="button"
          >
            <Copy size={11} />
          </button>
          <button
            className="plugin-item-btn plugin-item-btn-danger"
            onClick={onDelete}
            title={tc("actions.delete")}
            type="button"
          >
            <Trash2 size={11} />
          </button>
        </div>
      </div>
      <div
        className={`userscript-code-section ${expanded ? "is-expanded" : ""}`}
      >
        <button
          className="userscript-code-toggle"
          onClick={onToggleExpand}
          type="button"
        >
          {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          <Code size={12} />
          <span>{t("usViewCode")}</span>
          <span className="userscript-code-size">{charLabel}</span>
        </button>
        {expanded && (
          <pre className="userscript-code">{script.code}</pre>
        )}
      </div>
    </div>
  );
}
