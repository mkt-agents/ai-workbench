import { useEffect, useMemo, useRef, useState } from "react";
import { useTranslation } from "react-i18next";
import { Plus, Trash2, Copy, Pencil, Check, XCircle, Sparkles, X, Download, Upload, Loader2, FileText, Search, Zap, ChevronDown, MoreHorizontal, Terminal, ClipboardCopy, CopyPlus } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { useGlobalStore } from "../core/store";
import type { Snippet, SnippetKind, SnippetTaskBind } from "../core/types";

const SNIPPET_EXPORT_VERSION = 2;
/** Mirrors package.json; bump with release triad when shipping. */
const APP_VERSION = "0.2.26";

const TASK_BIND_OPTIONS: Array<SnippetTaskBind | ""> = [
  "",
  "debug",
  "explain",
  "polish",
  "translate",
  "summarize",
];
import ModalTitleRow from "./ModalTitleRow";
import { useConfirm } from "./ConfirmModal";
import DropdownMenu from "./DropdownMenu";
import { PROMPT_SCENARIOS } from "../lib/promptOptimize";
import { fillParams, parseParamNames, splitTags } from "../lib/snippets";

const UNDO_MS = 8000;

const PARAM_SPLIT = /(\{\{\w+\}\})/g;
const CJK = /[\u3000-\u303f\u4e00-\u9fff\uff00-\uffef]/g;

/** Mostly-ASCII content (commands, config, code) reads better in mono; CJK prose in mono looks cramped. */
function isCodeLike(content: string) {
  const sample = content.slice(0, 400);
  if (!sample.trim()) return false;
  return (sample.match(CJK)?.length ?? 0) / sample.length < 0.05;
}

/** Preview text with `{{param}}` placeholders highlighted (same pattern as fillParams). */
function renderPreview(content: string) {
  const compact = content.trim().replace(/\n[ \t]*(?:\n[ \t]*)+/g, "\n");
  return compact.split(PARAM_SPLIT).map((part, i) =>
    i % 2 === 1 ? (
      <mark key={i} className="snippet-param-hl">
        {part}
      </mark>
    ) : (
      part
    )
  );
}

type SortMode = "useCount" | "updated" | "manual";
type KindFilter = "all" | "text" | "prompt";

const SORT_KEY = "snippets-sort";
const ORDER_KEY = "snippets-order";

function loadSortMode(): SortMode {
  const v = localStorage.getItem(SORT_KEY);
  return v === "updated" || v === "manual" ? v : "useCount";
}

function loadOrder(): string[] {
  try {
    const v = JSON.parse(localStorage.getItem(ORDER_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function persist(key: string, value: string) {
  try {
    localStorage.setItem(key, value);
  } catch {
    /* private mode / quota — order just isn't remembered */
  }
}

/** 按当前排序方式排全部片段；手动序里没有的（新建 / 导入）排最前，按更新时间倒序。 */
function sortSnippets(list: Snippet[], mode: SortMode, order: string[]): Snippet[] {
  const byUpdated = (a: Snippet, b: Snippet) => b.updatedAt.localeCompare(a.updatedAt);
  if (mode === "useCount") return [...list].sort((a, b) => b.useCount - a.useCount || byUpdated(a, b));
  if (mode === "updated") return [...list].sort(byUpdated);
  const rank = new Map(order.map((id, i) => [id, i]));
  return [...list].sort((a, b) => {
    const ra = rank.get(a.id);
    const rb = rank.get(b.id);
    if (ra === undefined || rb === undefined) {
      if (ra === rb) return byUpdated(a, b);
      return ra === undefined ? -1 : 1;
    }
    return ra - rb;
  });
}

export default function SnippetsManager() {
  const { t } = useTranslation("snippets");
  const { t: tc } = useTranslation("common");
  // Prompt scenario labels live in the ai namespace (prompts.scenarios.*)
  const { t: ta } = useTranslation("ai");
  const confirm = useConfirm();
  const snippets = useGlobalStore((s) => s.snippets);
  const loadSnippets = useGlobalStore((s) => s.loadSnippets);
  const addSnippet = useGlobalStore((s) => s.addSnippet);
  const updateSnippet = useGlobalStore((s) => s.updateSnippet);
  const deleteSnippet = useGlobalStore((s) => s.deleteSnippet);
  const restoreSnippet = useGlobalStore((s) => s.restoreSnippet);
  const bumpSnippetUse = useGlobalStore((s) => s.bumpSnippetUse);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);
  const invokePickTextFile = useGlobalStore((s) => s.invokePickTextFile);

  const [query, setQuery] = useState("");
  const [tagFilter, setTagFilter] = useState<string>("");
  const [kindFilter, setKindFilter] = useState<KindFilter>("all");
  const [sortMode, setSortModeState] = useState<SortMode>(loadSortMode);
  const [order, setOrder] = useState<string[]>(loadOrder);
  const [dragId, setDragId] = useState<string | null>(null);
  const [overId, setOverId] = useState<string | null>(null);
  const [editing, setEditing] = useState<Partial<Snippet> | null>(null);
  const [useTarget, setUseTarget] = useState<Snippet | null>(null);
  const [paramValues, setParamValues] = useState<Record<string, string>>({});
  const [undoItem, setUndoItem] = useState<Snippet | null>(null);
  const [transferBusy, setTransferBusy] = useState<"export" | "import" | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  const undoTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const searchRef = useRef<HTMLInputElement>(null);

  const showMsg = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 4000);
  };

  useEffect(() => {
    void loadSnippets();
    return () => {
      if (undoTimer.current) clearTimeout(undoTimer.current);
    };
  }, [loadSnippets]);

  // Esc closes the topmost modal, matching the overlay-click affordance.
  useEffect(() => {
    if (!editing && !useTarget) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      if (useTarget) setUseTarget(null);
      else setEditing(null);
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [editing, useTarget]);

  // "/" focuses search from anywhere on the page (unless typing in a field).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || editing || useTarget) return;
      const el = document.activeElement;
      const tag = el?.tagName;
      if (tag === "INPUT" || tag === "TEXTAREA" || tag === "SELECT") return;
      e.preventDefault();
      searchRef.current?.focus();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [editing, useTarget]);

  const allTags = useMemo(() => {
    const set = new Set<string>();
    for (const s of snippets) {
      for (const tag of splitTags(s.tags)) set.add(tag);
    }
    return [...set].sort((a, b) => a.localeCompare(b));
  }, [snippets]);

  /** Search + tag match, before the kind filter — the kind chips count from this. */
  const matched = useMemo(() => {
    const q = query.trim().toLowerCase();
    return snippets.filter((s) => {
      if (tagFilter) {
        const tags = splitTags(s.tags).map((x) => x.toLowerCase());
        if (!tags.includes(tagFilter.toLowerCase())) return false;
      }
      if (!q) return true;
      return (
        s.name.toLowerCase().includes(q) ||
        s.tags.toLowerCase().includes(q) ||
        s.content.toLowerCase().includes(q)
      );
    });
  }, [snippets, query, tagFilter]);

  const kindCounts = useMemo(() => {
    let prompt = 0;
    for (const s of matched) if (s.kind === "prompt") prompt += 1;
    return { all: matched.length, text: matched.length - prompt, prompt };
  }, [matched]);

  const filtered = useMemo(
    () =>
      sortSnippets(
        matched.filter((s) => kindFilter === "all" || (s.kind ?? "text") === kindFilter),
        sortMode,
        order,
      ),
    [matched, kindFilter, sortMode, order],
  );

  const setSortMode = (mode: SortMode) => {
    setSortModeState(mode);
    persist(SORT_KEY, mode);
  };

  const endDrag = () => {
    setDragId(null);
    setOverId(null);
  };

  /** 拖放后切到手动排序。筛选中只重排可见的那几张：它们在全量顺序里占的位置不变，
   *  被筛掉的片段留在原位，取消筛选后不会被挤到末尾。 */
  const dropOn = (targetId: string) => {
    if (!dragId || dragId === targetId) return endDrag();
    const visible = filtered.map((s) => s.id);
    const from = visible.indexOf(dragId);
    const to = visible.indexOf(targetId);
    if (from === -1 || to === -1) return endDrag();
    visible.splice(to, 0, ...visible.splice(from, 1));

    const full = sortSnippets(snippets, sortMode, order).map((s) => s.id);
    const visibleSet = new Set(visible);
    let i = 0;
    const next = full.map((id) => (visibleSet.has(id) ? visible[i++] : id));
    setOrder(next);
    persist(ORDER_KEY, JSON.stringify(next));
    setSortMode("manual");
    endDrag();
  };

  const openCreate = () => {
    setEditing({ name: "", content: "", tags: "", params: "" });
  };

  const saveEdit = async () => {
    if (!editing?.name?.trim() || !editing.content?.trim()) return;
    // Duplicate names make the tag/search lists confusing; block them like the models page.
    const nameNorm = editing.name.trim().toLowerCase();
    if (snippets.some((s) => s.id !== editing.id && s.name.trim().toLowerCase() === nameNorm)) {
      showMsg("error", t("nameDuplicate"));
      return;
    }
    try {
      const kind: SnippetKind = editing.kind === "prompt" ? "prompt" : "text";
      const scenario = kind === "prompt" ? editing.scenario ?? "general" : undefined;
      // Params are derived from {{name}} in content — keep DB in sync, no hand-edit.
      const params = parseParamNames("", editing.content).join(", ");
      const taskBind =
        kind === "text" && editing.taskBind ? (editing.taskBind as SnippetTaskBind) : "";
      if (editing.id) {
        await updateSnippet(editing.id, {
          name: editing.name.trim(),
          content: editing.content,
          tags: editing.tags || "",
          params,
          kind,
          scenario,
          taskBind,
        });
      } else {
        await addSnippet({
          name: editing.name.trim(),
          content: editing.content,
          tags: editing.tags || "",
          params,
          kind,
          scenario,
          taskBind,
        });
      }
      setEditing(null);
    } catch (e) {
      showMsg("error", `${tc("status.error")}: ${e}`);
    }
  };

  const handleClone = async (s: Snippet) => {
    try {
      await addSnippet({
        name: `${s.name}${t("cloneSuffix")}`,
        content: s.content,
        tags: s.tags,
        params: s.params,
        kind: s.kind,
        scenario: s.scenario,
        taskBind: s.taskBind,
      });
      showMsg("success", t("cloned", { name: s.name }));
    } catch (e) {
      showMsg("error", `${tc("status.error")}: ${e}`);
    }
  };

  /** Backup / transfer. Snippets are plaintext; the JSON should still be treated carefully. */
  const handleExport = async () => {
    if (snippets.length === 0 || transferBusy) return;
    setTransferBusy("export");
    try {
      const payload = {
        app: "ai-workbench",
        kind: "snippets",
        version: SNIPPET_EXPORT_VERSION,
        appVersion: APP_VERSION,
        exportedAt: new Date().toISOString(),
        snippets: snippets.map((s) => ({
          name: s.name,
          content: s.content,
          tags: s.tags,
          params: s.params,
          kind: s.kind ?? "text",
          scenario: s.scenario,
          taskBind: s.taskBind || undefined,
        })),
      };
      const path = await invokeSaveTextFile(
        JSON.stringify(payload, null, 2),
        `snippets-${new Date().toISOString().slice(0, 10)}.json`,
        t("export")
      );
      showMsg("success", t("exportDone", { path }));
    } catch (error) {
      const message = String(error);
      // The native dialog reports a cancelled save as an error; that is not a failure.
      if (message.includes("已取消")) return;
      showMsg("error", t("exportFailed", { error: message }));
    } finally {
      setTransferBusy(null);
    }
  };

  const handleImport = async () => {
    if (transferBusy) return;
    setTransferBusy("import");
    try {
      const raw = await invokePickTextFile(t("import"));
      let parsed: unknown;
      try {
        parsed = JSON.parse(raw);
      } catch {
        showMsg("error", t("importInvalid"));
        return;
      }
      const list = Array.isArray(parsed)
        ? parsed
        : ((parsed as { snippets?: unknown }).snippets ?? []);
      if (!Array.isArray(list) || list.length === 0) {
        showMsg("error", t("importInvalid"));
        return;
      }
      const fingerprint = (s: { name?: string; content?: string }) =>
        `${s.name ?? ""}|${s.content ?? ""}`.trim();
      const existing = new Set(snippets.map(fingerprint));
      let added = 0;
      let skipped = 0;
      for (const item of list as Array<Record<string, unknown>>) {
        const entry = {
          name: String(item?.name ?? "").trim(),
          content: String(item?.content ?? ""),
        };
        if (!entry.name || !entry.content || existing.has(fingerprint(entry))) {
          skipped += 1;
          continue;
        }
        existing.add(fingerprint(entry));
        const bindRaw = String(item?.taskBind ?? item?.task_bind ?? "");
        const taskBind = (TASK_BIND_OPTIONS.includes(bindRaw as SnippetTaskBind)
          ? bindRaw
          : "") as SnippetTaskBind | "";
        await addSnippet({
          name: entry.name,
          content: entry.content,
          tags: String(item?.tags ?? ""),
          params: String(item?.params ?? ""),
          kind: item?.kind === "prompt" ? "prompt" : "text",
          scenario: item?.scenario ? String(item.scenario) : undefined,
          taskBind,
        });
        added += 1;
      }
      showMsg(
        added > 0 ? "success" : "error",
        t("importDone", { added, skipped })
      );
    } catch (error) {
      const message = String(error);
      if (message.includes("已取消")) return;
      showMsg("error", t("importFailed", { error: message }));
    } finally {
      setTransferBusy(null);
    }
  };

  const handleDelete = async (s: Snippet) => {
    const ok = await confirm({
      title: t("deleteTitle"),
      message: t("deleteConfirm", { name: s.name }),
      warning: t("deleteWarning"),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    await deleteSnippet(s.id);
    setUndoItem(s);
    if (undoTimer.current) clearTimeout(undoTimer.current);
    undoTimer.current = setTimeout(() => setUndoItem(null), UNDO_MS);
  };

  const handleUndo = async () => {
    if (!undoItem) return;
    await restoreSnippet(undoItem);
    setUndoItem(null);
    if (undoTimer.current) clearTimeout(undoTimer.current);
  };

  const resolvedText = (s: Snippet, values: Record<string, string>) =>
    Object.keys(values).length ? fillParams(s.content, values) : s.content;

  /** 无参数片段一键复制；有参数才弹填参窗（少两次点击） */
  const handleUse = async (s: Snippet) => {
    const names = parseParamNames("", s.content);
    if (names.length === 0) {
      try {
        await invokeCopyToClipboard(s.content);
        await bumpSnippetUse(s.id);
        showMsg("success", t("copiedToast"));
      } catch (e) {
        showMsg("error", `${tc("status.error")}: ${e}`);
      }
      return;
    }
    startUse(s);
  };

  const startUse = (s: Snippet) => {
    const names = parseParamNames("", s.content);
    if (names.length === 0) {
      setParamValues({});
      setUseTarget(s);
      return;
    }
    const init: Record<string, string> = {};
    names.forEach((n) => {
      init[n] = "";
    });
    setParamValues(init);
    setUseTarget(s);
  };

  const confirmCopy = async () => {
    if (!useTarget) return;
    const text = resolvedText(useTarget, paramValues);
    await invokeCopyToClipboard(text);
    await bumpSnippetUse(useTarget.id);
    setUseTarget(null);
  };

  const confirmInsertAiHub = async () => {
    if (!useTarget) return;
    const text = resolvedText(useTarget, paramValues);
    await bumpSnippetUse(useTarget.id);
    setUseTarget(null);
    try {
      await invoke("open_ai_hub_with_text", { text });
    } catch (e) {
      console.error(e);
    }
  };

  return (
    <div className="snippets-page">
      <div className="snippets-toolbar">
        <div className="snippets-search">
          <Search size={13} className="snippets-search-icon" />
          <input
            ref={searchRef}
            className="input-field"
            placeholder={t("search")}
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape" && query) {
                e.stopPropagation();
                setQuery("");
              }
            }}
            aria-label={t("search")}
          />
          {query && (
            <button
              type="button"
              className="snippets-search-clear"
              onClick={() => {
                setQuery("");
                searchRef.current?.focus();
              }}
              aria-label={t("clearSearch")}
              title={t("clearSearch")}
            >
              <X size={11} />
            </button>
          )}
        </div>
        <div className="snippets-toolbar-actions">
          <div className="snippets-select-wrap">
            <select
              className="input-field snippets-sort"
              value={sortMode}
              onChange={(e) => setSortMode(e.target.value as SortMode)}
              aria-label={t("sortLabel")}
            >
              <option value="useCount">{t("sortUseCount")}</option>
              <option value="updated">{t("sortUpdated")}</option>
              <option value="manual">{t("sortManual")}</option>
            </select>
            <ChevronDown size={12} />
          </div>
          <span className="snippets-count" title={t("countLabel", { shown: filtered.length, total: snippets.length })}>
            {filtered.length}/{snippets.length}
          </span>
          <DropdownMenu
            label={t("more")}
            triggerClassName="btn btn-secondary btn-icon"
            items={[
              {
                label: t("import"),
                icon: <Upload size={14} />,
                disabled: transferBusy !== null,
                onClick: () => void handleImport(),
              },
              {
                label: t("export"),
                icon: <Download size={14} />,
                disabled: transferBusy !== null || snippets.length === 0,
                onClick: () => void handleExport(),
              },
            ]}
          >
            {transferBusy ? <Loader2 size={14} className="spin" /> : <MoreHorizontal size={14} />}
          </DropdownMenu>
          <button type="button" className="btn btn-primary btn-small" onClick={openCreate} title={t("add")}>
            <Plus size={14} />
            <span className="snippets-add-label">{t("add")}</span>
          </button>
        </div>
      </div>

      <div className="snippets-filter-bar">
        <div className="snippets-kind-seg" role="tablist" aria-label={t("kindLabel")}>
          {(["all", "text", "prompt"] as KindFilter[]).map((k) => (
            <button
              key={k}
              type="button"
              role="tab"
              aria-selected={kindFilter === k}
              className={`snippets-kind-btn${kindFilter === k ? " is-active" : ""}`}
              onClick={() => setKindFilter(k)}
              title={k === "prompt" ? t("kindPromptHint") : undefined}
            >
              {k === "text" ? (
                <FileText size={12} className="snippets-kind-icon" aria-hidden />
              ) : k === "prompt" ? (
                <Sparkles size={12} className="snippets-kind-icon" aria-hidden />
              ) : null}
              <span className="snippets-kind-label">
                {k === "all" ? t("kindAll") : k === "text" ? t("kindText") : t("kindPrompt")}
              </span>
              <span className="snippets-kind-count">{kindCounts[k]}</span>
            </button>
          ))}
        </div>
        {allTags.length > 0 && (
          <div className="snippets-tags" role="group" aria-label={t("tags")}>
            {allTags.map((tag) => (
              <button
                key={tag}
                type="button"
                className={`snippets-tag-chip${tagFilter === tag ? " on" : ""}`}
                onClick={() => setTagFilter(tagFilter === tag ? "" : tag)}
                title={tagFilter === tag ? t("tagClear") : tag}
              >
                {tag}
              </button>
            ))}
          </div>
        )}
      </div>

      <div className="snippets-list">
        {filtered.map((s) => {
          const tags = splitTags(s.tags);
          const paramNames = parseParamNames("", s.content);
          const codeLike = isCodeLike(s.content);
          return (
            <div
              key={s.id}
              className={`snippet-card${dragId === s.id ? " is-dragging" : ""}${
                overId === s.id && dragId !== s.id ? " is-drop-target" : ""
              }`}
              tabIndex={0}
              title={t("cardHint")}
              draggable
              onDragStart={(e) => {
                e.dataTransfer.effectAllowed = "move";
                e.dataTransfer.setData("text/plain", s.name);
                setDragId(s.id);
              }}
              onDragOver={(e) => {
                if (!dragId) return;
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (overId !== s.id) setOverId(s.id);
              }}
              onDragLeave={(e) => {
                if (overId === s.id && !e.currentTarget.contains(e.relatedTarget as Node | null)) setOverId(null);
              }}
              onDrop={(e) => {
                e.preventDefault();
                dropOn(s.id);
              }}
              onDragEnd={endDrag}
              onDoubleClick={(e) => {
                if ((e.target as HTMLElement).closest("button")) return;
                void handleUse(s);
              }}
              onKeyDown={(e) => {
                if (e.target !== e.currentTarget) return;
                if (e.key === "Enter") void handleUse(s);
                else if (e.key === "F2") setEditing({ ...s });
              }}
            >
              <div className="snippet-card-head">
                <span className={`snippet-kind-icon is-${s.kind === "prompt" ? "prompt" : codeLike ? "code" : "text"}`}>
                  {s.kind === "prompt" ? <Sparkles size={14} /> : codeLike ? <Terminal size={14} /> : <FileText size={14} />}
                </span>
                <div className="snippet-head-text">
                  <div className="snippet-name-row">
                    <strong className="snippet-name" title={s.name}>{s.name}</strong>
                    {s.useCount > 0 && (
                      <span className="snippet-uses" title={t("used", { count: s.useCount })}>
                        {s.useCount}×
                      </span>
                    )}
                  </div>
                  <div className="snippet-subline">
                    <span
                      className={`snippet-kind-label${s.kind === "prompt" ? " is-prompt" : ""}`}
                      title={s.kind === "prompt" ? t("kindPromptHint") : undefined}
                    >
                      {s.kind === "prompt"
                        ? s.scenario
                          ? `${t("kindPrompt")} · ${ta(`prompts.scenarios.${s.scenario}`)}`
                          : t("kindPrompt")
                        : t("kindText")}
                    </span>
                    {tags.map((tag) => (
                      <button
                        key={tag}
                        type="button"
                        className={`snippet-tag-link${tagFilter === tag ? " on" : ""}`}
                        onClick={() => setTagFilter(tagFilter === tag ? "" : tag)}
                        title={tagFilter === tag ? t("tagAll") : tag}
                      >
                        #{tag}
                      </button>
                    ))}
                  </div>
                </div>
              </div>
              <pre className={`snippet-preview${codeLike ? " is-code" : ""}`}>
                <span className="snippet-preview-text">{renderPreview(s.content)}</span>
              </pre>
              <div className="snippet-actions">
                <div
                  className="snippet-params"
                  title={paramNames.length > 0 ? t("paramsHint") : undefined}
                >
                  {paramNames.map((p) => (
                    <code key={p} className="snippet-param">{p}</code>
                  ))}
                </div>
                <span className="snippet-actions-tools">
                  <button
                    type="button"
                    className="snippet-icon-btn"
                    onClick={() => setEditing({ ...s })}
                    title={t("edit")}
                    aria-label={t("edit")}
                  >
                    <Pencil size={13} />
                  </button>
                  <button
                    type="button"
                    className="snippet-icon-btn"
                    onClick={() => void handleClone(s)}
                    title={t("cloneHint")}
                    aria-label={t("clone")}
                  >
                    <CopyPlus size={13} />
                  </button>
                  <button
                    type="button"
                    className="snippet-icon-btn danger"
                    onClick={() => void handleDelete(s)}
                    title={t("delete")}
                    aria-label={t("delete")}
                  >
                    <Trash2 size={13} />
                  </button>
                </span>
                <button type="button" className="snippet-use-btn" onClick={() => void handleUse(s)} title={t("useHint")}>
                  {paramNames.length > 0 ? <Zap size={12} /> : <ClipboardCopy size={12} />}
                  {paramNames.length > 0 ? t("use") : t("copy")}
                </button>
              </div>
            </div>
          );
        })}
        {filtered.length === 0 && (
          <div className="snippets-empty">
            <span className="snippets-empty-icon">
              {snippets.length === 0 ? <FileText size={22} /> : <Search size={22} />}
            </span>
            <p>{snippets.length === 0 ? t("empty") : t("noMatch")}</p>
            {snippets.length === 0 ? (
              <>
                <span className="snippets-empty-desc">{t("emptyHint")}</span>
                <div className="snippets-empty-actions">
                  <button type="button" className="btn btn-primary" onClick={openCreate}>
                    <Plus size={14} /> {t("add")}
                  </button>
                </div>
              </>
            ) : (
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => {
                  setQuery("");
                  setTagFilter("");
                  setKindFilter("all");
                }}
              >
                {t("clearSearch")}
              </button>
            )}
          </div>
        )}
      </div>

      {editing && (
        <div className="modal-overlay">
          <div className="modal snippets-modal">
            <ModalTitleRow title={editing.id ? t("editTitle") : t("addTitle")} onClose={() => setEditing(null)} />
            <div className="snippets-modal-body">
              <div className="snippets-modal-row">
                <div className="input-group">
                  <label className="input-label">{t("name")}</label>
                  <input
                    className="input-field"
                    value={editing.name || ""}
                    onChange={(e) => setEditing({ ...editing, name: e.target.value })}
                    autoFocus
                  />
                </div>
                <div className="input-group">
                  <label className="input-label">{t("kindLabel")}</label>
                  <select
                    className="input-field"
                    value={editing.kind ?? "text"}
                    onChange={(e) => {
                      const kind = e.target.value as SnippetKind;
                      setEditing({
                        ...editing,
                        kind,
                        scenario: kind === "prompt" ? editing.scenario ?? "general" : undefined,
                        taskBind: kind === "prompt" ? "" : editing.taskBind,
                      });
                    }}
                  >
                    <option value="text">{t("kindText")}</option>
                    <option value="prompt">{t("kindPrompt")}</option>
                  </select>
                </div>
              </div>
              <div className="snippets-modal-row">
                {(editing.kind ?? "text") !== "prompt" ? (
                  <div className="input-group">
                    <label className="input-label">{t("taskBindLabel")}</label>
                    <select
                      className="input-field"
                      value={editing.taskBind || ""}
                      onChange={(e) =>
                        setEditing({
                          ...editing,
                          taskBind: e.target.value as SnippetTaskBind | "",
                        })
                      }
                    >
                      {TASK_BIND_OPTIONS.map((opt) => (
                        <option key={opt || "none"} value={opt}>
                          {opt ? t(`taskBind.${opt}`) : t("taskBind.none")}
                        </option>
                      ))}
                    </select>
                    <span className="setting-hint">{t("taskBindHint")}</span>
                  </div>
                ) : (
                  <div className="input-group">
                    <label className="input-label">{t("scenarioLabel")}</label>
                    <select
                      className="input-field"
                      value={editing.scenario ?? "general"}
                      onChange={(e) => setEditing({ ...editing, scenario: e.target.value })}
                    >
                      {PROMPT_SCENARIOS.map((sc) => (
                        <option key={sc} value={sc}>
                          {ta(`prompts.scenarios.${sc}`)}
                        </option>
                      ))}
                    </select>
                  </div>
                )}
                <div className="input-group">
                  <label className="input-label">{t("tags")}</label>
                  <input
                    className="input-field"
                    value={editing.tags || ""}
                    onChange={(e) => setEditing({ ...editing, tags: e.target.value })}
                  />
                </div>
              </div>
              <div className="input-group snippets-modal-content">
                <label className="input-label">{t("content")}</label>
                <textarea
                  className="input-field snippets-content-editor"
                  rows={10}
                  value={editing.content || ""}
                  onChange={(e) => setEditing({ ...editing, content: e.target.value })}
                  spellCheck={false}
                />
              </div>
              <div className="snippets-params-block">
                <div className="snippets-params-block-head">
                  <span className="input-label">{t("params")}</span>
                  <span className="snippets-params-hint">{t("paramsDerivedHint")}</span>
                </div>
                {(() => {
                  const derived = parseParamNames("", editing.content || "");
                  return derived.length > 0 ? (
                    <div className="snippets-params-derived" aria-live="polite">
                      {derived.map((p) => (
                        <code key={p} className="snippet-param">
                          {p}
                        </code>
                      ))}
                    </div>
                  ) : (
                    <p className="snippets-params-empty">{t("paramsNone")}</p>
                  );
                })()}
              </div>
            </div>
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={() => setEditing(null)}>
                {t("cancel")}
              </button>
              <button
                type="button"
                className="btn btn-primary"
                onClick={() => void saveEdit()}
                disabled={!editing.name?.trim() || !editing.content?.trim()}
              >
                {t("save")}
              </button>
            </div>
          </div>
        </div>
      )}

      {useTarget && (
        <div className="modal-overlay">
          <div className="modal snippets-modal">
            <ModalTitleRow
              title={Object.keys(paramValues).length ? t("fillParams") : t("useTitle")}
              onClose={() => setUseTarget(null)}
            />
            {Object.keys(paramValues).length === 0 ? (
              <p className="setting-hint" style={{ marginBottom: 12 }}>
                {t("useHint")}
              </p>
            ) : (
              Object.keys(paramValues).map((key) => (
                <div className="input-group" key={key}>
                  <label className="input-label">{key}</label>
                  <input
                    className="input-field"
                    value={paramValues[key]}
                    onChange={(e) => setParamValues({ ...paramValues, [key]: e.target.value })}
                  />
                </div>
              ))
            )}
            {/* Live preview of the resolved text before copying / inserting. */}
            <div className="input-group">
              <label className="input-label">{t("preview")}</label>
              <pre className="snippet-preview snippets-use-preview">
                {resolvedText(useTarget, paramValues) || useTarget.content}
              </pre>
            </div>
            <div className="modal-actions">
              <button type="button" className="btn btn-secondary" onClick={() => setUseTarget(null)}>
                {t("cancel")}
              </button>
              <button type="button" className="btn btn-secondary" onClick={() => void confirmCopy()}>
                <Copy size={12} /> {t("copyUse")}
              </button>
              <button type="button" className="btn btn-primary" onClick={() => void confirmInsertAiHub()}>
                <Sparkles size={12} /> {t("insertAiHub")}
              </button>
            </div>
          </div>
        </div>
      )}

      {undoItem && (
        <div className="toast toast-warning" role="status">
          <span className="toast-icon">
            <XCircle size={16} />
          </span>
          <span className="toast-text">{t("deletedToast", { name: undoItem.name })}</span>
          <button type="button" className="btn btn-secondary btn-small toast-action" onClick={() => void handleUndo()}>
            <Check size={12} /> {t("undoDelete")}
          </button>
        </div>
      )}

      {message && (
        <div className={`toast toast-${message.type}`} role="status">
          <span className="toast-icon">
            {message.type === "success" ? <Check size={16} /> : <X size={16} />}
          </span>
          {message.text}
        </div>
      )}
    </div>
  );
}
