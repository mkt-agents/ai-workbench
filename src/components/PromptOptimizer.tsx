import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { computeFloatingMenuStyle, type FloatingMenuStyle } from "../lib/floatingMenu";
import { useTranslation } from "react-i18next";
import {
  ArrowLeftRight,
  ArrowRightLeft,
  Bot,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Download,
  Eraser,
  History,
  LayoutTemplate,
  ListTree,
  Loader2,
  Pencil,
  Plus,
  RefreshCw,
  Sparkles,
  Star,
  Trash2,
  Upload,
  X,
} from "lucide-react";
import { useGlobalStore } from "../core/store";
import type { AIModelConfig } from "../core/types";
import {
  DEFAULT_PROMPT_GOALS,
  PROMPT_GOALS,
  PROMPT_SCENARIOS,
  PROMPT_TEMPLATES,
  buildExplainChangesMessages,
  buildPromptOptimizeMessages,
  cleanOptimizedPrompt,
  deleteCustomTemplate,
  exportPromptHistory,
  importPromptHistory,
  loadCustomTemplates,
  loadPromptHistory,
  pushPromptHistory,
  savePromptHistory,
  toggleHistoryPinned,
  upsertCustomTemplate,
  type CustomPromptTemplate,
  type PromptGoal,
  type PromptHistoryItem,
  type PromptScenario,
  type PromptTemplate,
} from "../lib/promptOptimize";

type Props = {
  onGoModels?: () => void;
};

type EditableTemplate = {
  id?: string;
  title: string;
  body: string;
};

function formatInvokeError(err: unknown): string {
  const text = String(err ?? "").trim();
  return text || "failed";
}

function isMacPlatform(): boolean {
  if (typeof navigator === "undefined") return false;
  return /Mac|iPhone|iPad|iPod/i.test(navigator.platform || navigator.userAgent);
}

function PromptOptimizer({ onGoModels }: Props) {
  const { t, i18n } = useTranslation("ai");
  const aiModels = useGlobalStore((s) => s.aiModels);
  const invokeGenerateText = useGlobalStore((s) => s.invokeGenerateText);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);

  const [scenario, setScenario] = useState<PromptScenario>("general");
  const [goals, setGoals] = useState<PromptGoal[]>(DEFAULT_PROMPT_GOALS);
  const [goalsOpen, setGoalsOpen] = useState(true);
  const [input, setInput] = useState("");
  const [output, setOutput] = useState("");
  const [busy, setBusy] = useState(false);
  const [explaining, setExplaining] = useState(false);
  const [explainText, setExplainText] = useState("");
  const [history, setHistory] = useState<PromptHistoryItem[]>([]);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [historyQuery, setHistoryQuery] = useState("");
  const [historyScenario, setHistoryScenario] = useState<PromptScenario | "all">("all");
  const [selectedModelId, setSelectedModelId] = useState<string>("");
  const [modelOpen, setModelOpen] = useState(false);
  const [modelQuery, setModelQuery] = useState("");
  const [modelMenuStyle, setModelMenuStyle] = useState<FloatingMenuStyle | null>(null);
  const [templateOpen, setTemplateOpen] = useState(false);
  const [templateQuery, setTemplateQuery] = useState("");
  const [templateMenuStyle, setTemplateMenuStyle] = useState<FloatingMenuStyle | null>(null);
  const [customTemplates, setCustomTemplates] = useState<CustomPromptTemplate[]>([]);
  const [templateForm, setTemplateForm] = useState<EditableTemplate | null>(null);
  const [copied, setCopied] = useState(false);
  const [toast, setToast] = useState<{ type: "success" | "error"; text: string } | null>(
    null
  );

  const inputRef = useRef<HTMLTextAreaElement>(null);
  const outputRef = useRef<HTMLTextAreaElement>(null);
  const modelMenuRef = useRef<HTMLDivElement>(null);
  const modelTriggerRef = useRef<HTMLButtonElement>(null);
  const modelDropdownRef = useRef<HTMLDivElement>(null);
  const modelSearchRef = useRef<HTMLInputElement>(null);
  const templateMenuRef = useRef<HTMLDivElement>(null);
  const templateTriggerRef = useRef<HTMLButtonElement>(null);
  const templateDropdownRef = useRef<HTMLDivElement>(null);
  const templateSearchRef = useRef<HTMLInputElement>(null);
  const historyFileRef = useRef<HTMLInputElement>(null);
  const pendingFocus = useRef<"input" | "output" | null>(null);
  const copiedTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    setHistory(loadPromptHistory());
    setCustomTemplates(loadCustomTemplates());
  }, []);

  useEffect(() => {
    const target = pendingFocus.current;
    if (!target) return;
    if (target === "output" && !output) return;
    pendingFocus.current = null;
    const el = target === "output" ? outputRef.current : inputRef.current;
    if (!el) return;
    el.focus();
    const len = el.value.length;
    el.setSelectionRange(len, len);
  }, [input, output]);

  useEffect(() => {
    if (aiModels.length === 0) {
      setSelectedModelId("");
      return;
    }
    setSelectedModelId((prev) => {
      if (prev && aiModels.some((m) => m.id === prev)) return prev;
      const preferred = aiModels.find((m) => m.isDefault) || aiModels[0];
      return preferred.id;
    });
  }, [aiModels]);

  useEffect(() => {
    if (!modelOpen) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (modelMenuRef.current?.contains(t) || modelDropdownRef.current?.contains(t)) return;
      setModelOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [modelOpen]);

  useEffect(() => {
    if (!modelOpen) {
      setModelQuery("");
      setModelMenuStyle(null);
      return;
    }
    const id = window.setTimeout(() => modelSearchRef.current?.focus(), 0);
    return () => window.clearTimeout(id);
  }, [modelOpen]);

  useLayoutEffect(() => {
    if (!modelOpen) return;
    const update = () => {
      const el = modelTriggerRef.current;
      if (!el) return;
      setModelMenuStyle(
        computeFloatingMenuStyle(el, {
          align: "right",
          minWidth: 220,
          preferMaxHeight: 280,
        })
      );
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [modelOpen]);

  useEffect(() => {
    if (!templateOpen) return;
    const onDoc = (e: MouseEvent) => {
      const t = e.target as Node;
      if (templateMenuRef.current?.contains(t) || templateDropdownRef.current?.contains(t)) return;
      setTemplateOpen(false);
      setTemplateForm(null);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [templateOpen]);

  useEffect(() => {
    if (!templateOpen) {
      setTemplateQuery("");
      setTemplateForm(null);
      setTemplateMenuStyle(null);
      return;
    }
    const id = window.setTimeout(() => templateSearchRef.current?.focus(), 0);
    return () => window.clearTimeout(id);
  }, [templateOpen]);

  useLayoutEffect(() => {
    if (!templateOpen) return;
    const update = () => {
      const el = templateTriggerRef.current;
      if (!el) return;
      setTemplateMenuStyle(
        computeFloatingMenuStyle(el, {
          align: "right",
          minWidth: 280,
          preferMaxHeight: 420,
        })
      );
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [templateOpen, templateForm]);

  useEffect(
    () => () => {
      if (copiedTimer.current) clearTimeout(copiedTimer.current);
    },
    []
  );

  const selectedModel = useMemo(
    () => aiModels.find((m) => m.id === selectedModelId) || null,
    [aiModels, selectedModelId]
  );

  const filteredModels = useMemo(() => {
    const q = modelQuery.trim().toLowerCase();
    if (!q) return aiModels;
    return aiModels.filter(
      (m) =>
        m.name.toLowerCase().includes(q) ||
        m.model.toLowerCase().includes(q)
    );
  }, [aiModels, modelQuery]);

  const shortcutLabel = useMemo(
    () => (isMacPlatform() ? t("prompts.shortcutMac") : t("prompts.shortcut")),
    [t]
  );

  const scenarioCustoms = useMemo(
    () => customTemplates.filter((c) => c.scenario === scenario),
    [customTemplates, scenario]
  );

  const filteredBuiltin = useMemo(() => {
    const q = templateQuery.trim().toLowerCase();
    const list = PROMPT_TEMPLATES[scenario] || [];
    if (!q) return list;
    return list.filter((tpl) => {
      const title = t(tpl.titleKey).toLowerCase();
      return title.includes(q) || tpl.body.toLowerCase().includes(q);
    });
  }, [scenario, t, templateQuery]);

  const filteredCustom = useMemo(() => {
    const q = templateQuery.trim().toLowerCase();
    if (!q) return scenarioCustoms;
    return scenarioCustoms.filter(
      (tpl) =>
        tpl.title.toLowerCase().includes(q) || tpl.body.toLowerCase().includes(q)
    );
  }, [scenarioCustoms, templateQuery]);

  const filteredHistory = useMemo(() => {
    const q = historyQuery.trim().toLowerCase();
    return history.filter((item) => {
      if (historyScenario !== "all" && item.scenario !== historyScenario) return false;
      if (!q) return true;
      return (
        item.input.toLowerCase().includes(q) ||
        item.output.toLowerCase().includes(q) ||
        t(`prompts.scenarios.${item.scenario}`).toLowerCase().includes(q)
      );
    });
  }, [history, historyQuery, historyScenario, t]);

  const pinnedHistory = useMemo(
    () => filteredHistory.filter((h) => h.pinned),
    [filteredHistory]
  );
  const recentHistory = useMemo(
    () => filteredHistory.filter((h) => !h.pinned),
    [filteredHistory]
  );

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setToast({ type, text });
    setTimeout(() => setToast(null), type === "error" ? 8000 : 3500);
  }, []);

  const toggleGoal = (goal: PromptGoal) => {
    setGoals((prev) => {
      if (prev.includes(goal)) {
        if (prev.length <= 1) {
          showMsg("error", t("prompts.needOneGoal"));
          return prev;
        }
        return prev.filter((g) => g !== goal);
      }
      return [...prev, goal];
    });
  };

  const polishDisabledReason = useMemo(() => {
    if (busy) return t("prompts.disabledBusy");
    if (!selectedModel) return t("prompts.needModel");
    if (!input.trim()) return t("prompts.needInput");
    return "";
  }, [busy, input, selectedModel, t]);

  const runPolishWithDraft = useCallback(
    async (draft: string, options?: { adoptDraft?: boolean }) => {
      if (busy) return;
      const text = draft.trim();
      if (!text) {
        showMsg("error", t("prompts.needInput"));
        return;
      }
      if (!selectedModel) {
        showMsg("error", t("prompts.needModel"));
        return;
      }

      if (options?.adoptDraft) {
        setInput(text);
      }

      setBusy(true);
      setExplainText("");
      try {
        const { system, user } = buildPromptOptimizeMessages(scenario, goals, text);
        const config: AIModelConfig = {
          ...selectedModel,
          maxTokens: Math.max(selectedModel.maxTokens || 0, 1200),
        };
        const raw = await invokeGenerateText({ config, system, user });
        const cleaned = cleanOptimizedPrompt(raw);
        if (!cleaned) {
          showMsg("error", t("prompts.failed"));
          return;
        }
        setOutput(cleaned);
        setHistory((prev) =>
          pushPromptHistory(prev, {
            scenario,
            goals: [...goals],
            input: text,
            output: cleaned,
          })
        );
        showMsg("success", t("prompts.ok"));
        pendingFocus.current = "output";
      } catch (e) {
        showMsg("error", `${t("prompts.failed")}: ${formatInvokeError(e)}`);
      } finally {
        setBusy(false);
      }
    },
    [busy, goals, invokeGenerateText, scenario, selectedModel, showMsg, t]
  );

  const runPolish = useCallback(async () => {
    await runPolishWithDraft(input);
  }, [input, runPolishWithDraft]);

  const runRepolish = useCallback(async () => {
    const draft = output.trim();
    if (!draft) return;
    await runPolishWithDraft(draft, { adoptDraft: true });
  }, [output, runPolishWithDraft]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
        e.preventDefault();
        void runPolish();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [runPolish]);

  const handleCopy = async () => {
    const text = output.trim();
    if (!text) return;
    try {
      await invokeCopyToClipboard(text);
      setCopied(true);
      if (copiedTimer.current) clearTimeout(copiedTimer.current);
      copiedTimer.current = setTimeout(() => setCopied(false), 1500);
      showMsg("success", t("prompts.copied"));
    } catch (e) {
      showMsg("error", formatInvokeError(e));
    }
  };

  const handleAdopt = () => {
    if (!output.trim()) return;
    setInput(output);
    pendingFocus.current = "input";
    showMsg("success", t("prompts.adopted"));
  };

  const handleSwap = () => {
    if (!input && !output) return;
    const left = input;
    const right = output;
    setInput(right);
    setOutput(left);
    setExplainText("");
    showMsg("success", t("prompts.swapped"));
  };

  const applyBuiltinTemplate = (tpl: PromptTemplate) => {
    setInput(tpl.body);
    setTemplateOpen(false);
    setTemplateForm(null);
    pendingFocus.current = "input";
  };

  const applyCustomTemplate = (tpl: CustomPromptTemplate) => {
    setInput(tpl.body);
    setTemplateOpen(false);
    setTemplateForm(null);
    pendingFocus.current = "input";
  };

  const handleClear = () => {
    setInput("");
    setOutput("");
    setExplainText("");
    setCopied(false);
    pendingFocus.current = "input";
  };

  const openTemplates = () => {
    setTemplateOpen(true);
  };

  const saveTemplateForm = () => {
    if (!templateForm) return;
    try {
      setCustomTemplates((prev) =>
        upsertCustomTemplate(prev, {
          id: templateForm.id,
          scenario,
          title: templateForm.title,
          body: templateForm.body,
        })
      );
      setTemplateForm(null);
      showMsg("success", t("prompts.customSaved"));
    } catch {
      showMsg("error", t("prompts.customNeedTitle"));
    }
  };

  const removeCustom = (id: string) => {
    if (!window.confirm(t("prompts.customDeleteConfirm"))) return;
    setCustomTemplates((prev) => deleteCustomTemplate(prev, id));
    if (templateForm?.id === id) setTemplateForm(null);
    showMsg("success", t("prompts.customDeleted"));
  };

  const runExplain = async () => {
    if (explaining || busy) return;
    const original = input.trim();
    const polished = output.trim();
    if (!original || !polished) {
      showMsg("error", t("prompts.explainNeedBoth"));
      return;
    }
    if (!selectedModel) {
      showMsg("error", t("prompts.needModel"));
      return;
    }
    setExplaining(true);
    try {
      const { system, user } = buildExplainChangesMessages(original, polished);
      const config: AIModelConfig = {
        ...selectedModel,
        maxTokens: Math.max(selectedModel.maxTokens || 0, 600),
      };
      const raw = await invokeGenerateText({ config, system, user });
      const cleaned = cleanOptimizedPrompt(raw);
      if (!cleaned) {
        showMsg("error", t("prompts.explainFailed"));
        return;
      }
      setExplainText(cleaned);
    } catch (e) {
      showMsg("error", `${t("prompts.explainFailed")}: ${formatInvokeError(e)}`);
    } finally {
      setExplaining(false);
    }
  };

  const restoreHistory = (item: PromptHistoryItem) => {
    setScenario(item.scenario);
    setGoals(item.goals?.length ? item.goals : DEFAULT_PROMPT_GOALS);
    setInput(item.input);
    setOutput(item.output);
    setExplainText("");
  };

  const deleteHistory = (id: string) => {
    setHistory((prev) => {
      const next = prev.filter((h) => h.id !== id);
      savePromptHistory(next);
      return next;
    });
  };

  const pinHistory = (id: string) => {
    setHistory((prev) => toggleHistoryPinned(prev, id));
  };

  const clearHistory = () => {
    setHistory([]);
    savePromptHistory([]);
  };

  const handleExportHistory = async () => {
    if (history.length === 0) return;
    const json = exportPromptHistory(history);
    try {
      const blob = new Blob([json], { type: "application/json;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const a = document.createElement("a");
      a.href = url;
      a.download = `prompt-history-${new Date().toISOString().slice(0, 10)}.json`;
      a.click();
      URL.revokeObjectURL(url);
      await invokeCopyToClipboard(json);
      showMsg("success", t("prompts.historyExportOk"));
    } catch (e) {
      showMsg("error", `${t("prompts.historyExportFailed")}: ${formatInvokeError(e)}`);
    }
  };

  const handleImportHistoryFile = async (file: File | null) => {
    if (!file) return;
    try {
      const text = await file.text();
      setHistory((prev) => importPromptHistory(prev, text));
      showMsg("success", t("prompts.historyImportOk"));
    } catch (e) {
      const code = e instanceof Error ? e.message : "";
      if (code === "invalid_json" || code === "invalid_shape" || code === "empty") {
        showMsg("error", t("prompts.historyImportFailed"));
      } else {
        showMsg("error", `${t("prompts.historyImportFailed")}: ${formatInvokeError(e)}`);
      }
    } finally {
      if (historyFileRef.current) historyFileRef.current.value = "";
    }
  };

  const formatTime = (at: number) => {
    try {
      return new Date(at).toLocaleString(i18n.language === "zh-CN" ? "zh-CN" : "en-US", {
        month: "numeric",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
      });
    } catch {
      return "";
    }
  };

  const outputDeltaLabel = useMemo(() => {
    if (!output) return "";
    const outLen = output.length;
    const inLen = input.length;
    if (!input.trim()) return String(outLen);
    const delta = outLen - inLen;
    const sign = delta > 0 ? `+${delta}` : String(delta);
    return t("prompts.charCompare", { count: outLen, delta: sign });
  }, [input, output, t]);

  const showGuide = !input.trim() && !output.trim() && !busy;
  const canPolish = !busy && !!selectedModel && !!input.trim();
  const canRepolish = !busy && !!selectedModel && !!output.trim();
  const noTemplateMatch =
    filteredBuiltin.length === 0 && filteredCustom.length === 0 && !templateForm;

  const renderHistoryItems = (items: PromptHistoryItem[]) => (
    <ul className="prompt-history-list">
      {items.map((item) => {
        const preview = (item.output || item.input || "").trim();
        return (
          <li key={item.id} className="prompt-history-item">
            <button
              type="button"
              className={`btn btn-secondary btn-small prompt-history-pin ${
                item.pinned ? "is-pinned" : ""
              }`}
              aria-label={t("prompts.pinHistory")}
              title={t("prompts.pinHistory")}
              onClick={() => pinHistory(item.id)}
            >
              <Star size={12} fill={item.pinned ? "currentColor" : "none"} />
            </button>
            <button
              type="button"
              className="prompt-history-main"
              onClick={() => restoreHistory(item)}
            >
              <span className="prompt-history-meta">
                <span className="prompt-history-scenario">
                  {t(`prompts.scenarios.${item.scenario}`)}
                </span>
                <span className="prompt-history-time">{formatTime(item.at)}</span>
              </span>
              <span className="prompt-history-preview">
                {preview.slice(0, 80)}
                {preview.length > 80 ? "…" : ""}
              </span>
            </button>
            <button
              type="button"
              className="btn btn-secondary btn-small prompt-history-delete"
              aria-label={t("prompts.deleteHistory")}
              onClick={() => deleteHistory(item.id)}
            >
              <Trash2 size={12} />
            </button>
          </li>
        );
      })}
    </ul>
  );

  return (
    <div className="prompt-page">
      {toast && (
        <div className={`toast toast-${toast.type}`} role="status">
          <span className="toast-icon" />
          <span className="toast-text">{toast.text}</span>
        </div>
      )}

      <div className="prompt-top">
        <div className="prompt-top-bar">
          <div className="prompt-scenarios" role="tablist" aria-label={t("prompts.scenario")}>
            {PROMPT_SCENARIOS.map((s) => (
              <button
                key={s}
                type="button"
                role="tab"
                aria-selected={scenario === s}
                className={`prompt-scenario ${scenario === s ? "active" : ""}`}
                onClick={() => setScenario(s)}
              >
                {t(`prompts.scenarios.${s}`)}
              </button>
            ))}
          </div>
          <button
            type="button"
            className={`prompt-goals-toggle ${goalsOpen ? "is-open" : ""}`}
            aria-expanded={goalsOpen}
            aria-label={goalsOpen ? t("prompts.goalsCollapse") : t("prompts.goalsExpand")}
            onClick={() => setGoalsOpen((v) => !v)}
          >
            <span>{t("prompts.goalsToggle", { count: goals.length })}</span>
            {goalsOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
          </button>
        </div>
        {goalsOpen && (
          <div className="prompt-goals" aria-label={t("prompts.goals")}>
            {PROMPT_GOALS.map((g) => {
              const on = goals.includes(g);
              return (
                <button
                  key={g}
                  type="button"
                  className={`prompt-goal-chip ${on ? "active" : ""}`}
                  aria-pressed={on}
                  onClick={() => toggleGoal(g)}
                >
                  {t(`prompts.goal.${g}`)}
                </button>
              );
            })}
          </div>
        )}
      </div>

      <div className="prompt-panes">
        <div className="prompt-pane card">
          <div className="card-title prompt-pane-title">
            <span className="prompt-pane-heading">
              {t("prompts.input")}
              <span className="prompt-char-count">{input.length}</span>
            </span>
            <div className="prompt-pane-actions prompt-draft-actions">
              {aiModels.length > 0 ? (
                <div className="prompt-model-wrap" ref={modelMenuRef}>
                  <button
                    ref={modelTriggerRef}
                    type="button"
                    className="prompt-model-trigger"
                    disabled={busy || explaining}
                    aria-expanded={modelOpen}
                    aria-label={t("prompts.model")}
                    title={
                      selectedModel
                        ? `${selectedModel.name} — ${selectedModel.model}`
                        : t("prompts.model")
                    }
                    onClick={() => {
                      setTemplateOpen(false);
                      setModelOpen((v) => !v);
                    }}
                  >
                    <Bot size={12} className="prompt-model-icon" aria-hidden />
                    <span className="prompt-model-trigger-label">
                      {selectedModel?.name || t("prompts.model")}
                    </span>
                    <ChevronDown size={12} />
                  </button>
                  {modelOpen &&
                    modelMenuStyle &&
                    createPortal(
                      <div
                        ref={modelDropdownRef}
                        className="prompt-model-menu is-floating"
                        role="listbox"
                        style={{
                          top: modelMenuStyle.top,
                          bottom: modelMenuStyle.bottom,
                          left: modelMenuStyle.left,
                          width: modelMenuStyle.width,
                          maxHeight: modelMenuStyle.maxHeight,
                        }}
                      >
                        <input
                          ref={modelSearchRef}
                          className="prompt-model-search"
                          type="search"
                          value={modelQuery}
                          onChange={(e) => setModelQuery(e.target.value)}
                          placeholder={t("prompts.modelSearch")}
                          aria-label={t("prompts.modelSearch")}
                        />
                        <div className="prompt-model-menu-list">
                          {filteredModels.length === 0 ? (
                            <div className="prompt-model-empty">{t("prompts.modelNoMatch")}</div>
                          ) : (
                            filteredModels.map((m) => (
                              <button
                                key={m.id}
                                type="button"
                                role="option"
                                aria-selected={m.id === selectedModelId}
                                className={`prompt-model-item ${
                                  m.id === selectedModelId ? "is-active" : ""
                                }`}
                                title={`${m.name} — ${m.model}`}
                                onClick={() => {
                                  setSelectedModelId(m.id);
                                  setModelOpen(false);
                                  setModelQuery("");
                                }}
                              >
                                <span className="prompt-model-item-name">{m.name}</span>
                                <span className="prompt-model-item-id">{m.model}</span>
                              </button>
                            ))
                          )}
                        </div>
                      </div>,
                      document.body
                    )}
                </div>
              ) : (
                <button type="button" className="btn btn-secondary btn-small" onClick={onGoModels}>
                  <Bot size={12} />
                  {t("prompts.goModels")}
                </button>
              )}
              <div className="prompt-template-wrap" ref={templateMenuRef}>
                <button
                  ref={templateTriggerRef}
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => {
                    setModelOpen(false);
                    setTemplateOpen((v) => !v);
                  }}
                  aria-expanded={templateOpen}
                  aria-label={t("prompts.templatesBtn")}
                  title={t("prompts.templatesBtn")}
                >
                  <LayoutTemplate size={12} />
                  {t("prompts.templatesBtn")}
                  <ChevronDown size={12} />
                </button>
                {templateOpen &&
                  templateMenuStyle &&
                  createPortal(
                  <div
                    ref={templateDropdownRef}
                    className="prompt-template-menu is-floating"
                    role="menu"
                    style={{
                      top: templateMenuStyle.top,
                      bottom: templateMenuStyle.bottom,
                      left: templateMenuStyle.left,
                      width: templateMenuStyle.width,
                      maxHeight: templateMenuStyle.maxHeight,
                    }}
                  >
                    <div className="prompt-template-menu-label">
                      {t(`prompts.scenarios.${scenario}`)}
                    </div>
                    <input
                      ref={templateSearchRef}
                      className="prompt-template-search"
                      type="search"
                      value={templateQuery}
                      onChange={(e) => setTemplateQuery(e.target.value)}
                      placeholder={t("prompts.templateSearch")}
                      aria-label={t("prompts.templateSearch")}
                    />
                    <div className="prompt-template-scroll">
                      {noTemplateMatch ? (
                        <div className="prompt-template-empty">{t("prompts.templateNoMatch")}</div>
                      ) : (
                        <>
                          {filteredBuiltin.length > 0 && (
                            <>
                              <div className="prompt-template-section">
                                {t("prompts.templateBuiltin")}
                              </div>
                              {filteredBuiltin.map((tpl) => (
                                <button
                                  key={tpl.id}
                                  type="button"
                                  className="prompt-template-item"
                                  role="menuitem"
                                  onClick={() => applyBuiltinTemplate(tpl)}
                                >
                                  <span className="prompt-template-item-title">
                                    {t(tpl.titleKey)}
                                  </span>
                                  <span className="prompt-template-item-body">
                                    {tpl.body.slice(0, 48)}
                                    {tpl.body.length > 48 ? "…" : ""}
                                  </span>
                                </button>
                              ))}
                            </>
                          )}
                          <div className="prompt-template-section">
                            {t("prompts.templateCustom")}
                          </div>
                          {filteredCustom.length === 0 && !templateForm && (
                            <div className="prompt-template-empty">
                              {t("prompts.templateCustomEmpty")}
                            </div>
                          )}
                          {filteredCustom.map((tpl) => (
                            <div key={tpl.id} className="prompt-template-custom-row">
                              <button
                                type="button"
                                className="prompt-template-item"
                                role="menuitem"
                                onClick={() => applyCustomTemplate(tpl)}
                              >
                                <span className="prompt-template-item-title">{tpl.title}</span>
                                <span className="prompt-template-item-body">
                                  {tpl.body.slice(0, 48)}
                                  {tpl.body.length > 48 ? "…" : ""}
                                </span>
                              </button>
                              <div className="prompt-template-custom-actions">
                                <button
                                  type="button"
                                  className="btn btn-secondary btn-small"
                                  title={t("prompts.customEdit")}
                                  aria-label={t("prompts.customEdit")}
                                  onClick={() =>
                                    setTemplateForm({
                                      id: tpl.id,
                                      title: tpl.title,
                                      body: tpl.body,
                                    })
                                  }
                                >
                                  <Pencil size={12} />
                                </button>
                                <button
                                  type="button"
                                  className="btn btn-secondary btn-small"
                                  title={t("prompts.customDelete")}
                                  aria-label={t("prompts.customDelete")}
                                  onClick={() => removeCustom(tpl.id)}
                                >
                                  <Trash2 size={12} />
                                </button>
                              </div>
                            </div>
                          ))}
                        </>
                      )}
                      {templateForm ? (
                        <div className="prompt-template-form">
                          <input
                            className="prompt-template-form-title"
                            value={templateForm.title}
                            onChange={(e) =>
                              setTemplateForm((prev) =>
                                prev ? { ...prev, title: e.target.value } : prev
                              )
                            }
                            placeholder={t("prompts.customTitlePlaceholder")}
                            aria-label={t("prompts.customTitlePlaceholder")}
                          />
                          <textarea
                            className="prompt-template-form-body"
                            value={templateForm.body}
                            onChange={(e) =>
                              setTemplateForm((prev) =>
                                prev ? { ...prev, body: e.target.value } : prev
                              )
                            }
                            placeholder={t("prompts.customBodyPlaceholder")}
                            rows={5}
                          />
                          <div className="prompt-template-form-actions">
                            <button
                              type="button"
                              className="btn btn-secondary btn-small"
                              onClick={() => setTemplateForm(null)}
                            >
                              {t("prompts.customCancel")}
                            </button>
                            <button
                              type="button"
                              className="btn btn-primary btn-small"
                              onClick={saveTemplateForm}
                            >
                              {t("prompts.customSave")}
                            </button>
                          </div>
                        </div>
                      ) : (
                        <button
                          type="button"
                          className="btn btn-secondary btn-small prompt-template-new"
                          onClick={() =>
                            setTemplateForm({
                              title: "",
                              body: input.trim() || "",
                            })
                          }
                        >
                          <Plus size={12} />
                          {t("prompts.customNew")}
                        </button>
                      )}
                    </div>
                  </div>,
                  document.body
                  )}
              </div>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={handleClear}
                disabled={!input && !output}
                aria-label={t("prompts.clear")}
                title={t("prompts.clear")}
              >
                <Eraser size={12} />
                {t("prompts.clear")}
              </button>
              <button
                type="button"
                className="btn btn-primary btn-small"
                disabled={!canPolish}
                onClick={() => void runPolish()}
                title={polishDisabledReason || shortcutLabel}
              >
                {busy ? <Loader2 size={12} className="spin" /> : <Sparkles size={12} />}
                {busy ? t("prompts.polishing") : t("prompts.polish")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={!canRepolish}
                onClick={() => void runRepolish()}
                title={t("prompts.repolishHint")}
              >
                <RefreshCw size={12} />
                {t("prompts.repolish")}
              </button>
            </div>
          </div>
          <textarea
            ref={inputRef}
            className="prompt-textarea"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            placeholder={t("prompts.inputPlaceholder")}
            spellCheck={false}
          />
        </div>
        <div className="prompt-pane card">
          <div className="card-title prompt-pane-title">
            <span>{t("prompts.output")}</span>
            <div className="prompt-pane-actions">
              {outputDeltaLabel && (
                <span className="prompt-char-count">{outputDeltaLabel}</span>
              )}
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={!input && !output}
                onClick={handleSwap}
                title={t("prompts.swap")}
              >
                <ArrowLeftRight size={12} />
                {t("prompts.swap")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={!output.trim()}
                onClick={handleAdopt}
                title={t("prompts.adopt")}
              >
                <ArrowRightLeft size={12} />
                {t("prompts.adopt")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={!output.trim() || !input.trim() || explaining || busy}
                onClick={() => void runExplain()}
                title={t("prompts.explain")}
              >
                {explaining ? <Loader2 size={12} className="spin" /> : <ListTree size={12} />}
                {explaining ? t("prompts.explaining") : t("prompts.explain")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={!output.trim()}
                onClick={() => void handleCopy()}
              >
                {copied ? <Check size={12} /> : <Copy size={12} />}
                {copied ? t("prompts.copied") : t("prompts.copy")}
              </button>
            </div>
          </div>
          {output ? (
            <textarea
              ref={outputRef}
              className="prompt-textarea"
              value={output}
              onChange={(e) => {
                setOutput(e.target.value);
                setExplainText("");
              }}
              spellCheck={false}
            />
          ) : showGuide ? (
            <div className="prompt-empty prompt-guide">
              <p className="prompt-guide-title">{t("prompts.guideTitle")}</p>
              <p className="prompt-guide-desc">{t(`prompts.scenarioHint.${scenario}`)}</p>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={openTemplates}
              >
                <LayoutTemplate size={12} />
                {t("prompts.openTemplates")}
              </button>
              <p className="prompt-guide-shortcut">{t("prompts.guideExampleHint")}</p>
              <p className="prompt-guide-shortcut">
                {t("prompts.guideShortcut", { shortcut: shortcutLabel })}
              </p>
            </div>
          ) : busy ? (
            <div className="prompt-empty prompt-loading">
              <Loader2 size={22} className="spin" />
              <span>{t("prompts.loadingResult")}</span>
            </div>
          ) : (
            <div className="prompt-empty">{t("prompts.outputEmpty")}</div>
          )}
          {explainText && (
            <div className="prompt-explain">
              <div className="prompt-explain-head">
                <span>{t("prompts.explainTitle")}</span>
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={() => setExplainText("")}
                  aria-label={t("prompts.explainClose")}
                >
                  <X size={12} />
                </button>
              </div>
              <pre className="prompt-explain-body">{explainText}</pre>
            </div>
          )}
        </div>
      </div>

      <div className={`prompt-history card ${historyOpen ? "is-open" : ""}`}>
        <div className="card-title prompt-pane-title">
          <button
            type="button"
            className="prompt-history-toggle"
            onClick={() => setHistoryOpen((v) => !v)}
            aria-expanded={historyOpen}
          >
            {historyOpen ? <ChevronDown size={14} /> : <ChevronRight size={14} />}
            <History size={14} />
            {t("prompts.history")}
            {history.length > 0 && (
              <span className="prompt-char-count">{history.length}</span>
            )}
          </button>
          {historyOpen && (
            <div className="prompt-history-toolbar">
              <button
                type="button"
                className="btn btn-secondary btn-small"
                disabled={history.length === 0}
                onClick={() => void handleExportHistory()}
                title={t("prompts.historyExport")}
              >
                <Download size={12} />
                {t("prompts.historyExport")}
              </button>
              <button
                type="button"
                className="btn btn-secondary btn-small"
                onClick={() => historyFileRef.current?.click()}
                title={t("prompts.historyImport")}
              >
                <Upload size={12} />
                {t("prompts.historyImport")}
              </button>
              <input
                ref={historyFileRef}
                type="file"
                accept="application/json,.json"
                hidden
                onChange={(e) => void handleImportHistoryFile(e.target.files?.[0] ?? null)}
              />
              {history.length > 0 && (
                <button
                  type="button"
                  className="btn btn-secondary btn-small"
                  onClick={clearHistory}
                >
                  {t("prompts.clearHistory")}
                </button>
              )}
            </div>
          )}
        </div>
        {historyOpen &&
          (history.length === 0 ? (
            <div className="prompt-history-empty">{t("prompts.historyEmpty")}</div>
          ) : (
            <div className="prompt-history-body">
              <div className="prompt-history-filters">
                <input
                  className="prompt-history-search"
                  type="search"
                  value={historyQuery}
                  onChange={(e) => setHistoryQuery(e.target.value)}
                  placeholder={t("prompts.historySearch")}
                  aria-label={t("prompts.historySearch")}
                />
                <div className="prompt-history-scenario-filters" role="group">
                  <button
                    type="button"
                    className={`prompt-goal-chip ${
                      historyScenario === "all" ? "active" : ""
                    }`}
                    onClick={() => setHistoryScenario("all")}
                  >
                    {t("prompts.historyFilterAll")}
                  </button>
                  {PROMPT_SCENARIOS.map((s) => (
                    <button
                      key={s}
                      type="button"
                      className={`prompt-goal-chip ${
                        historyScenario === s ? "active" : ""
                      }`}
                      onClick={() => setHistoryScenario(s)}
                    >
                      {t(`prompts.scenarios.${s}`)}
                    </button>
                  ))}
                </div>
              </div>
              {filteredHistory.length === 0 ? (
                <div className="prompt-history-empty">{t("prompts.historyNoMatch")}</div>
              ) : (
                <>
                  {pinnedHistory.length > 0 && (
                    <div className="prompt-history-group">
                      <div className="prompt-history-group-title">
                        {t("prompts.historyPinned")}
                      </div>
                      {renderHistoryItems(pinnedHistory)}
                    </div>
                  )}
                  {recentHistory.length > 0 && (
                    <div className="prompt-history-group">
                      <div className="prompt-history-group-title">
                        {t("prompts.historyRecent")}
                      </div>
                      {renderHistoryItems(recentHistory)}
                    </div>
                  )}
                </>
              )}
            </div>
          ))}
      </div>
    </div>
  );
}

export default PromptOptimizer;
