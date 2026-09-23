import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { Loader2, Trash2, Plus, Edit2, Check, Eye, EyeOff, X, ArrowRightCircle, Zap, RefreshCw, ChevronDown, ChevronsDownUp, ChevronsUpDown, Copy, CopyPlus, Search, Download, Upload, Layers, AlertCircle } from "lucide-react";
import AppLogoMark from "./AppLogoMark";
import { useGlobalStore } from "../core/store";
import { MODEL_TEST_CONCURRENCY, mapPool } from "../core/asyncPool";
import { useConfirm } from "./ConfirmModal";
import ModalTitleRow from "./ModalTitleRow";
import { computeFloatingMenuStyle, type FloatingMenuStyle } from "../lib/floatingMenu";
import type { AIModelConfig, AuthType } from "../core/types";
import {
  AI_PROVIDERS,
  getDefaultUrl,
  getProviderMeta,
  providerIconLabel,
  providerSupportsModelList,
  type ModelPreset,
} from "../lib/aiProviders";

type ProviderType = AIModelConfig["provider"];

function finiteOr(value: unknown, fallback: number): number {
  if (value === null || value === undefined || value === "") return fallback;
  const n = typeof value === "number" ? value : Number(value);
  return Number.isFinite(n) ? n : fallback;
}

/** Temperature presets, named by what they do rather than by the number. */
const TEMPERATURE_PRESETS = [
  { value: 0.2, key: "tempPresetPrecise" },
  { value: 0.7, key: "tempPresetBalanced" },
  { value: 1.2, key: "tempPresetCreative" },
] as const;

/** Every power of two from 1K to 1M — one slider stop each. */
const MAX_TOKEN_STEPS = Array.from({ length: 11 }, (_, i) => 1024 * 2 ** i);

function formatTokenCount(value: number): string {
  if (value >= 1024 * 1024 && value % (1024 * 1024) === 0) return `${value / 1048576}M`;
  if (value >= 1024 && value % 1024 === 0) return `${value / 1024}K`;
  return String(value);
}

/** "4096 · 4K" for the round values, plain "5000" for anything else. */
function tokenDisplay(value: number): string {
  const short = formatTokenCount(value);
  return short === String(value) ? short : `${value} · ${short}`;
}

/** Label + live value on one line, control, then a one-line explanation. */
function FieldShell(props: {
  label: string;
  display: string;
  hint: string;
  /** The raw API parameter name, kept reachable on hover only. */
  apiName?: string;
  children: ReactNode;
}) {
  return (
    <div className="input-group ai-preset-field">
      <label className="input-label" title={props.apiName}>
        <span>{props.label}</span>
        <span className="ai-preset-value">{props.display}</span>
      </label>
      <div className="ai-preset-control">{props.children}</div>
      <div className="ai-preset-hint">{props.hint}</div>
    </div>
  );
}

/**
 * Temperature as words: a slider forces the user to already know what 0.7 vs
 * 1.1 means. Values outside the presets still get an input so nothing is lost.
 */
function TemperatureField(props: { value: number; onPick: (value: number) => void }) {
  const { value, onPick } = props;
  const { t } = useTranslation("ai");
  const isPreset = TEMPERATURE_PRESETS.some((preset) => preset.value === value);
  return (
    <FieldShell
      label={t("models.temperature")}
      display={value.toFixed(1)}
      hint={t("models.temperatureHint")}
      apiName={t("models.temperatureApi")}
    >
      <div className="ai-seg ai-seg-fill">
        {TEMPERATURE_PRESETS.map((preset) => (
          <button
            key={preset.value}
            type="button"
            className={`ai-seg-btn ${value === preset.value ? "is-active" : ""}`}
            onClick={() => onPick(preset.value)}
          >
            {t(`models.${preset.key}`)}
          </button>
        ))}
      </div>
      {!isPreset && (
        <div className="ai-preset-custom">
          <span className="ai-preset-custom-label">{t("models.customValue")}</span>
          <input
            className="input-field ai-seg-input"
            type="number"
            min={0}
            max={2}
            step={0.1}
            value={value}
            aria-label={t("models.temperature")}
            onChange={(e) => onPick(finiteOr(parseFloat(e.target.value), value))}
          />
        </div>
      )}
    </FieldShell>
  );
}

/**
 * Max tokens on a log scale: the useful range spans 1K→1M, so a linear slider
 * is unusable and typing exact values is noise. The handle snaps to powers of
 * two, which is what every provider documents.
 */
function TokenField(props: { value: number; onPick: (value: number) => void }) {
  const { value, onPick } = props;
  const { t } = useTranslation("ai");
  const nearestStep = Math.min(
    MAX_TOKEN_STEPS.length - 1,
    Math.max(0, Math.round(Math.log2(Math.max(1, value) / 1024)))
  );
  return (
    <FieldShell
      label={t("models.maxTokens")}
      display={tokenDisplay(value)}
      hint={t("models.maxTokensHint")}
      apiName={t("models.maxTokensApi")}
    >
      <input
        className="ai-token-slider"
        type="range"
        min={0}
        max={MAX_TOKEN_STEPS.length - 1}
        step={1}
        value={nearestStep}
        style={
          {
            "--ai-slider-progress": `${(nearestStep / (MAX_TOKEN_STEPS.length - 1)) * 100}%`,
          } as CSSProperties
        }
        aria-label={t("models.maxTokens")}
        onChange={(e) => onPick(MAX_TOKEN_STEPS[finiteOr(parseInt(e.target.value, 10), nearestStep)])}
      />
      <div className="ai-token-scale">
        {[
          MAX_TOKEN_STEPS[0],
          MAX_TOKEN_STEPS[(MAX_TOKEN_STEPS.length - 1) / 2],
          MAX_TOKEN_STEPS[MAX_TOKEN_STEPS.length - 1],
        ].map((step) => (
          <span key={step}>{formatTokenCount(step)}</span>
        ))}
      </div>
    </FieldShell>
  );
}

const defaultProvider = AI_PROVIDERS[0];

const emptyForm = (): Omit<AIModelConfig, "id" | "createdAt" | "updatedAt"> => ({
  name: "",
  provider: defaultProvider.value,
  apiKey: "",
  authType: "api",
  baseUrl: defaultProvider.defaultUrl,
  model: defaultProvider.defaultModel,
  temperature: 0.7,
  maxTokens: 4096,
  isDefault: false,
});

function truncateUrl(url: string, max = 42): string {
  const t = url.trim();
  if (t.length <= max) return t;
  return `${t.slice(0, max - 1)}…`;
}

/** List-view prefs (grouping, folds, sort, filter) survive page remounts. */
const MODELS_VIEW_PREFS_KEY = "workbench-models-view";

function loadModelsViewPrefs(): {
  groupByProvider?: boolean;
  collapsedGroups?: string[];
  sortBy?: string;
  statusFilter?: string;
} {
  try {
    const raw = window.localStorage.getItem(MODELS_VIEW_PREFS_KEY);
    return raw ? JSON.parse(raw) : {};
  } catch {
    return {};
  }
}

type TestState = "untested" | "passed" | "failed";

function testStateOf(config: AIModelConfig): TestState {
  if (!config.lastTest) return "untested";
  return config.lastTest.ok ? "passed" : "failed";
}

function testTimeOf(config: AIModelConfig): number {
  const at = config.lastTest?.at;
  const time = at ? new Date(at).getTime() : NaN;
  return Number.isFinite(time) ? time : 0;
}

function providerLabelOf(config: AIModelConfig): string {
  return getProviderMeta(config.provider)?.label || config.provider;
}

/**
 * "Default first" is the useful ordering when nothing was picked, but it hides
 * both the failures and the alphabetical position, so the list is sortable.
 */
function sortConfigs(list: AIModelConfig[], sortBy: string): AIModelConfig[] {
  const byName = (a: AIModelConfig, b: AIModelConfig) =>
    a.name.localeCompare(b.name, "zh-Hans-CN");
  const sorted = [...list];
  switch (sortBy) {
    case "name":
      return sorted.sort(byName);
    case "provider":
      return sorted.sort(
        (a, b) => providerLabelOf(a).localeCompare(providerLabelOf(b), "zh-Hans-CN") || byName(a, b)
      );
    case "recentTest":
      return sorted.sort((a, b) => testTimeOf(b) - testTimeOf(a) || byName(a, b));
    default:
      return sorted.sort(
        (a, b) => Number(b.isDefault) - Number(a.isDefault) || byName(a, b)
      );
  }
}

function isValidHttpUrl(url: string): boolean {
  try {
    const u = new URL(url);
    return u.protocol === "http:" || u.protocol === "https:";
  } catch {
    return false;
  }
}

/** Provider error bodies can be a whole HTML page — keep the toast readable. */
function brief(text: string, max = 120): string {
  const oneLine = text.replace(/\s+/g, " ").trim();
  return oneLine.length <= max ? oneLine : `${oneLine.slice(0, max)}…`;
}

/** Coarse "how long ago" stamp for a persisted test verdict. */
function formatSince(iso: string, t: TFunction<"ai">): string {
  if (!iso) return "";
  const then = new Date(iso).getTime();
  if (!Number.isFinite(then)) return "";
  const minutes = Math.floor((Date.now() - then) / 60_000);
  if (minutes < 1) return t("models.testJustNow");
  if (minutes < 60) return t("models.testMinutesAgo", { count: minutes });
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return t("models.testHoursAgo", { count: hours });
  const days = Math.floor(hours / 24);
  if (days < 30) return t("models.testDaysAgo", { count: days });
  return new Date(then).toLocaleDateString();
}



function AIAssistant() {
  const { t } = useTranslation("ai");
  const { t: tc } = useTranslation("common");
  const confirm = useConfirm();
  const aiModels = useGlobalStore((s) => s.aiModels);
  const addAIModel = useGlobalStore((s) => s.addAIModel);
  const updateAIModel = useGlobalStore((s) => s.updateAIModel);
  const deleteAIModel = useGlobalStore((s) => s.deleteAIModel);
  const setDefaultAIModel = useGlobalStore((s) => s.setDefaultAIModel);
  const recordAIModelTests = useGlobalStore((s) => s.recordAIModelTests);
  const invokeSyncModelToDsh = useGlobalStore((s) => s.invokeSyncModelToDsh);
  const invokeTestModelConnection = useGlobalStore((s) => s.invokeTestModelConnection);
  const invokeListProviderModels = useGlobalStore((s) => s.invokeListProviderModels);
  const invokeCopyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard);
  const invokeSaveTextFile = useGlobalStore((s) => s.invokeSaveTextFile);
  const invokePickTextFile = useGlobalStore((s) => s.invokePickTextFile);

  const [showModal, setShowModal] = useState(false);
  const [editingId, setEditingId] = useState<string | null>(null);
  const [formData, setFormData] = useState(emptyForm);
  const [fetchedModels, setFetchedModels] = useState<ModelPreset[] | null>(null);
  const [fetchingModels, setFetchingModels] = useState(false);
  const [testStatus, setTestStatus] = useState<{
    testing: boolean;
    result: { success: boolean; message: string } | null;
  }>({ testing: false, result: null });
  const [showApiKey, setShowApiKey] = useState(false);
  const [syncingId, setSyncingId] = useState<string | null>(null);
  const [testingId, setTestingId] = useState<string | null>(null);
  const [message, setMessage] = useState<{ type: "success" | "error"; text: string } | null>(null);
  // Model picker: a searchable list is the only way to use a 10+ model endpoint, and the
  // copy buttons save retyping an id elsewhere.
  const [pickerOpen, setPickerOpen] = useState(false);
  const [pickerQuery, setPickerQuery] = useState("");
  const [pickerMenuStyle, setPickerMenuStyle] = useState<FloatingMenuStyle | null>(null);
  /** Key of the value copied most recently, e.g. `abc:model`, `abc:url`, `picker:xyz`. */
  const [copiedField, setCopiedField] = useState<string | null>(null);
  const [listQuery, setListQuery] = useState("");
  const [groupByProvider, setGroupByProvider] = useState(
    () => loadModelsViewPrefs().groupByProvider ?? true
  );
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(
    () => new Set(loadModelsViewPrefs().collapsedGroups ?? [])
  );
  const [sortBy, setSortBy] = useState<string>(
    () => loadModelsViewPrefs().sortBy ?? "default"
  );
  const [statusFilter, setStatusFilter] = useState<string>(
    () => loadModelsViewPrefs().statusFilter ?? "all"
  );
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [bulkBusy, setBulkBusy] = useState<"test" | "sync" | null>(null);
  const [bulkProgress, setBulkProgress] = useState<{ done: number; total: number } | null>(null);
  const [transferBusy, setTransferBusy] = useState<"export" | "import" | null>(null);
  const pickerRootRef = useRef<HTMLDivElement | null>(null);
  const pickerTriggerRef = useRef<HTMLButtonElement | null>(null);
  const pickerMenuRef = useRef<HTMLDivElement | null>(null);
  const pickerSearchRef = useRef<HTMLInputElement | null>(null);

  const showMsg = (type: "success" | "error", text: string) => {
    setMessage({ type, text });
    setTimeout(() => setMessage(null), 4000);
  };

  const closeModelPicker = () => {
    setPickerOpen(false);
    setPickerQuery("");
    setPickerMenuStyle(null);
  };

  const commitModel = (id: string) => {
    const value = id.trim();
    if (!value) return;
    setFormData((cur) => ({ ...cur, model: value }));
    closeModelPicker();
  };

  const copyValue = async (key: string, value: string) => {
    try {
      await invokeCopyToClipboard(value);
      setCopiedField(key);
      window.setTimeout(() => setCopiedField((cur) => (cur === key ? null : cur)), 1500);
    } catch (error) {
      showMsg("error", t("models.copyFailed", { error: String(error) }));
    }
  };

  // The menu is portalled to <body>, so an outside click has to be checked against the
  // trigger wrapper and the menu itself.
  useEffect(() => {
    if (!pickerOpen) return;
    const onDoc = (e: MouseEvent) => {
      const target = e.target as Node;
      if (pickerRootRef.current?.contains(target) || pickerMenuRef.current?.contains(target)) {
        return;
      }
      closeModelPicker();
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [pickerOpen]);

  useEffect(() => {
    if (!pickerOpen) return;
    const id = window.setTimeout(() => pickerSearchRef.current?.focus(), 0);
    return () => window.clearTimeout(id);
  }, [pickerOpen]);

  // Persist list-view prefs; cheap and only fires on real toggles.
  useEffect(() => {
    try {
      window.localStorage.setItem(
        MODELS_VIEW_PREFS_KEY,
        JSON.stringify({
          groupByProvider,
          collapsedGroups: [...collapsedGroups],
          sortBy,
          statusFilter,
        })
      );
    } catch {
      /* private-mode / quota: prefs just don't persist */
    }
  }, [groupByProvider, collapsedGroups, sortBy, statusFilter]);

  // Esc closes the edit modal, matching the overlay-click affordance.
  useEffect(() => {
    if (!showModal) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeModal();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [showModal]);

  // Keep the floating menu glued to its trigger while the modal body scrolls.
  useLayoutEffect(() => {
    if (!pickerOpen) return;
    const update = () => {
      const el = pickerTriggerRef.current;
      if (!el) return;
      setPickerMenuStyle(
        computeFloatingMenuStyle(el, { align: "left", minWidth: 260, preferMaxHeight: 300 })
      );
    };
    update();
    window.addEventListener("resize", update);
    window.addEventListener("scroll", update, true);
    return () => {
      window.removeEventListener("resize", update);
      window.removeEventListener("scroll", update, true);
    };
  }, [pickerOpen]);

  const isOllama = formData.provider === "ollama";
  const isMimo = formData.provider === "mimo";
  const supportsList = providerSupportsModelList(formData.provider);
  const listedModels = fetchedModels ?? [];
  const showModelSelect = listedModels.length > 0;
  // Without a fetched list there is nothing to choose from, so the plain input stays.
  const showCustomModelInput = !showModelSelect;
  const modelQueryNorm = pickerQuery.trim().toLowerCase();
  const filteredModels = useMemo(
    () =>
      modelQueryNorm
        ? listedModels.filter(
            (m) =>
              m.id.toLowerCase().includes(modelQueryNorm) ||
              (m.label ?? "").toLowerCase().includes(modelQueryNorm)
          )
        : listedModels,
    [listedModels, modelQueryNorm]
  );
  const customCandidate = pickerQuery.trim();
  const showCustomRow =
    customCandidate.length > 0 && !listedModels.some((m) => m.id === customCandidate);

  // Saved-config filter: status first (so "only failures" survives a search),
  // then name / model id / base URL / provider label, then the chosen order.
  const listQueryNorm = listQuery.trim().toLowerCase();
  const filteredConfigs = useMemo(() => {
    const byStatus =
      statusFilter === "all"
        ? aiModels
        : aiModels.filter((config) => testStateOf(config) === statusFilter);
    const matches = listQueryNorm
      ? byStatus.filter((config) =>
          `${config.name} ${config.model} ${config.baseUrl} ${providerLabelOf(config)}`
            .toLowerCase()
            .includes(listQueryNorm)
        )
      : [...byStatus];
    return sortConfigs(matches, sortBy);
  }, [aiModels, listQueryNorm, sortBy, statusFilter]);

  const testStateCounts = useMemo(() => {
    const counts = { untested: 0, passed: 0, failed: 0 };
    for (const config of aiModels) counts[testStateOf(config)] += 1;
    return counts;
  }, [aiModels]);

  /** Every feature that generates text falls back to this one; none is a real gap. */
  const defaultConfig = useMemo(
    () => aiModels.find((config) => config.isDefault) ?? null,
    [aiModels]
  );

  // Derived from the live list, so deleting a config cannot leave a stale selection behind.
  const selectedConfigs = useMemo(
    () => aiModels.filter((config) => selectedIds.includes(config.id)),
    [aiModels, selectedIds]
  );
  const allFilteredSelected =
    filteredConfigs.length > 0 &&
    filteredConfigs.every((config) => selectedIds.includes(config.id));

  // Group the (filtered) configs by provider for the foldable view. Order: the group
  // holding the default first, then alphabetically by provider label. Per-group health
  // counts let a folded header still flag what needs attention inside it.
  const groupedConfigs = useMemo(() => {
    if (!groupByProvider) return [];
    const byProvider = new Map<
      string,
      {
        provider: string;
        label: string;
        configs: AIModelConfig[];
        failed: number;
        untested: number;
      }
    >();
    for (const config of filteredConfigs) {
      const label = providerLabelOf(config);
      let entry = byProvider.get(config.provider);
      if (!entry) {
        entry = { provider: config.provider, label, configs: [], failed: 0, untested: 0 };
        byProvider.set(config.provider, entry);
      }
      entry.configs.push(config);
      const state = testStateOf(config);
      if (state === "failed") entry.failed += 1;
      else if (state === "untested") entry.untested += 1;
    }
    return Array.from(byProvider.values()).sort((a, b) => {
      const aDefault = a.configs.some((c) => c.isDefault);
      const bDefault = b.configs.some((c) => c.isDefault);
      if (aDefault !== bDefault) return aDefault ? -1 : 1;
      return a.label.localeCompare(b.label);
    });
  }, [filteredConfigs, groupByProvider]);

  /** Every visible group folded — drives the single expand-all / collapse-all toggle. */
  const allGroupsCollapsed =
    groupedConfigs.length > 0 &&
    groupedConfigs.every((group) => collapsedGroups.has(group.provider));

  const toggleAllGroups = () => {
    setCollapsedGroups(
      allGroupsCollapsed ? new Set() : new Set(groupedConfigs.map((group) => group.provider))
    );
  };

  const toggleGroup = (provider: string) => {
    setCollapsedGroups((cur) => {
      const next = new Set(cur);
      if (next.has(provider)) next.delete(provider);
      else next.add(provider);
      return next;
    });
  };

  const keyRequired = !isOllama;
  const canRefreshModels =
    supportsList &&
    Boolean(formData.baseUrl.trim()) &&
    isValidHttpUrl(formData.baseUrl.trim()) &&
    (isOllama || Boolean(formData.apiKey.trim())) &&
    !fetchingModels;
  const refreshDisabledReason = !supportsList
    ? t("models.refreshNoListSupport")
    : !formData.baseUrl.trim() || (!isOllama && !formData.apiKey.trim())
      ? t("models.refreshNeedCredentials")
      : undefined;
  const canTest =
    Boolean(formData.baseUrl.trim() && formData.model.trim()) &&
    (!keyRequired || Boolean(formData.apiKey.trim()));
  const canSubmit =
    Boolean(formData.name.trim() && formData.baseUrl.trim() && formData.model.trim()) &&
    (!keyRequired || Boolean(formData.apiKey.trim()));

  const resetModalState = () => {
    const form = emptyForm();
    setFormData(form);
    closeModelPicker();
    setFetchedModels(null);
    setFetchingModels(false);
    setEditingId(null);
    setShowApiKey(false);
    setTestStatus({ testing: false, result: null });
  };

  const closeModal = () => {
    resetModalState();
    setShowModal(false);
  };

  const openCreateModal = () => {
    resetModalState();
    setShowModal(true);
  };

  const handleProviderChange = (provider: string) => {
    const next = getProviderMeta(provider);
    const prev = getProviderMeta(formData.provider);
    setFetchedModels(null);
    closeModelPicker();
    setFormData((cur) => {
      const prevDefaultUrl = prev ? getDefaultUrl(prev.value, cur.authType) : "";
      const urlMatchesPrevDefault = !prev || !cur.baseUrl || cur.baseUrl === prevDefaultUrl;
      const modelMatchesPrevDefault = !prev || !cur.model || cur.model === prev.defaultModel;
      const isCreate = !editingId;
      const authType: AuthType = provider === "mimo" ? cur.authType : "api";
      const newModel =
        isCreate || modelMatchesPrevDefault ? next?.defaultModel ?? "" : cur.model;
      return {
        ...cur,
        provider: provider as ProviderType,
        authType,
        baseUrl:
          isCreate || urlMatchesPrevDefault
            ? getDefaultUrl(provider, authType)
            : cur.baseUrl,
        model: newModel,
      };
    });
  };

  const handleAuthTypeChange = (authType: AuthType) => {
    const prevUrl = getDefaultUrl("mimo", formData.authType);
    const urlMatches = !formData.baseUrl || formData.baseUrl === prevUrl;
    setFetchedModels(null);
    closeModelPicker();
    setFormData((cur) => ({
      ...cur,
      authType,
      baseUrl: urlMatches ? getDefaultUrl("mimo", authType) : cur.baseUrl,
    }));
  };

  const handleCustomModelChange = (model: string) => {
    setFormData((cur) => ({ ...cur, model }));
  };

  const handleRefreshModels = async () => {
    if (!supportsList) {
      showMsg("error", t("models.refreshNoListSupport"));
      return;
    }
    if (!canRefreshModels && !fetchingModels) {
      showMsg("error", t("models.refreshNeedCredentials"));
      return;
    }
    if (!isValidHttpUrl(formData.baseUrl.trim())) {
      showMsg("error", t("models.validation.urlInvalid"));
      return;
    }
    setFetchingModels(true);
    try {
      const result = await invokeListProviderModels({
        provider: formData.provider,
        apiKey: formData.apiKey.trim(),
        baseUrl: formData.baseUrl.trim().replace(/\/+$/, ""),
      });
      if (!result.success) {
        showMsg("error", t("models.refreshFailed", { error: result.message || "unknown" }));
        return;
      }
      const fetched = result.models.map((m) => ({ id: m.id, label: m.label || m.id }));
      setFetchedModels(fetched);
      showMsg("success", t("models.refreshSuccess", { count: fetched.length }));
    } catch (error) {
      showMsg("error", t("models.refreshFailed", { error: String(error) }));
    } finally {
      setFetchingModels(false);
    }
  };

  const handleEdit = (config: AIModelConfig) => {
    setEditingId(config.id);
    setFormData({
      name: config.name,
      provider: config.provider,
      apiKey: config.apiKey,
      authType: config.authType || "api",
      baseUrl: config.baseUrl,
      model: config.model,
      temperature: config.temperature,
      maxTokens: config.maxTokens,
      isDefault: config.isDefault,
    });
    setFetchedModels(null);
    closeModelPicker();
    setShowApiKey(false);
    setTestStatus({ testing: false, result: null });
    setShowModal(true);
  };

  const handleDelete = async (id: string) => {
    const ok = await confirm({
      title: t("models.deleteTitle"),
      message: t("models.confirmDelete", { name: aiModels.find((m) => m.id === id)?.name || "" }),
      confirmText: tc("actions.delete"),
      icon: "danger",
    });
    if (!ok) return;
    try {
      await deleteAIModel(id);
      showMsg("success", t("models.deleted"));
    } catch (e) {
      showMsg("error", `${tc("status.error")}: ${e}`);
    }
  };

  const handleSetDefault = async (id: string) => {
    try {
      await setDefaultAIModel(id);
      showMsg("success", t("models.setDefault"));
    } catch (e) {
      showMsg("error", t("models.validation.setDefaultFailed", { error: e }));
    }
  };

  const handleSubmit = async () => {
    if (!formData.name.trim()) {
      showMsg("error", t("models.validation.nameRequired"));
      return;
    }
    const nameNorm = formData.name.trim().toLowerCase();
    if (aiModels.some((m) => m.id !== editingId && m.name.trim().toLowerCase() === nameNorm)) {
      showMsg("error", t("models.validation.nameDuplicate"));
      return;
    }
    if (formData.maxTokens < 1) {
      showMsg("error", t("models.validation.maxTokensInvalid"));
      return;
    }
    if (keyRequired && !formData.apiKey.trim()) {
      showMsg("error", t("models.validation.keyRequired"));
      return;
    }
    if (!formData.baseUrl.trim()) {
      showMsg("error", t("models.validation.urlRequired"));
      return;
    }
    if (!isValidHttpUrl(formData.baseUrl.trim())) {
      showMsg("error", t("models.validation.urlInvalid"));
      return;
    }
    if (!formData.model.trim()) {
      showMsg("error", t("models.validation.modelRequired"));
      return;
    }

    const payload = {
      ...formData,
      name: formData.name.trim(),
      apiKey: formData.apiKey.trim(),
      baseUrl: formData.baseUrl.trim().replace(/\/+$/, ""),
      model: formData.model.trim(),
    };

    try {
      if (editingId) {
        await updateAIModel(editingId, payload);
        showMsg("success", t("models.updated"));
      } else {
        await addAIModel(payload);
        showMsg("success", t("models.saved"));
      }
      closeModal();
    } catch (e) {
      showMsg("error", t("models.validation.saveFailed", { error: e }));
    }
  };

  const runConnectionTest = async (config: AIModelConfig) => {
    return invokeTestModelConnection({
      ...config,
      baseUrl: config.baseUrl.trim().replace(/\/+$/, ""),
    });
  };

  /** Persisted on the config, so the verdict is still there after a restart. */
  const persistTestResult = async (
    id: string,
    result: { success: boolean; message: string }
  ) => {
    try {
      await recordAIModelTests([
        { id, ok: result.success, message: result.message, at: new Date().toISOString() },
      ]);
    } catch (error) {
      showMsg("error", t("models.testSaveFailed", { error: String(error) }));
    }
  };

  /** Backup / transfer. The file carries plaintext API keys, so the confirm says so. */
  const handleExportConfigs = async () => {
    if (aiModels.length === 0) return;
    const ok = await confirm({
      title: t("models.exportTitle"),
      message: t("models.exportMessage", { count: aiModels.length }),
      warning: t("models.exportWarning"),
      confirmText: t("models.exportConfigs"),
      icon: "warning",
    });
    if (!ok) return;
    setTransferBusy("export");
    try {
      const payload = {
        app: "ai-workbench",
        kind: "ai-models",
        version: 1,
        exportedAt: new Date().toISOString(),
        models: aiModels.map((config) => ({
          name: config.name,
          provider: config.provider,
          apiKey: config.apiKey,
          authType: config.authType,
          baseUrl: config.baseUrl,
          model: config.model,
          temperature: config.temperature,
          maxTokens: config.maxTokens,
        })),
      };
      const path = await invokeSaveTextFile(
        JSON.stringify(payload, null, 2),
        `ai-models-${new Date().toISOString().slice(0, 10)}.json`,
        t("models.exportConfigs")
      );
      showMsg("success", t("models.exportDone", { path }));
    } catch (error) {
      const message = String(error);
      // The native dialog reports a cancelled save as an error; that is not a failure.
      if (message.includes("已取消")) return;
      showMsg("error", t("models.exportFailed", { error: message }));
    } finally {
      setTransferBusy(null);
    }
  };

  const handleImportConfigs = async () => {
    setTransferBusy("import");
    try {
      const raw = await invokePickTextFile(t("models.importConfigs"));
      let parsed: unknown;
      try {
        parsed = JSON.parse(raw);
      } catch {
        showMsg("error", t("models.importInvalid"));
        return;
      }
      const list = Array.isArray(parsed)
        ? parsed
        : ((parsed as { models?: unknown }).models ?? []);
      if (!Array.isArray(list) || list.length === 0) {
        showMsg("error", t("models.importInvalid"));
        return;
      }

      const knownProviders = new Set<string>(AI_PROVIDERS.map((p) => p.value));
      const fingerprint = (config: { name?: string; model?: string; baseUrl?: string }) =>
        `${config.name ?? ""}|${config.model ?? ""}|${config.baseUrl ?? ""}`.trim();
      const existing = new Set(aiModels.map(fingerprint));
      let added = 0;
      let skipped = 0;

      for (const item of list as Array<Record<string, unknown>>) {
        const entry = {
          name: String(item?.name ?? "").trim(),
          model: String(item?.model ?? "").trim(),
          baseUrl: String(item?.baseUrl ?? "").trim(),
        };
        // Same name + model + endpoint already configured: skip instead of duplicating.
        if (!entry.name || !entry.model || !entry.baseUrl || existing.has(fingerprint(entry))) {
          skipped += 1;
          continue;
        }
        existing.add(fingerprint(entry));
        const provider = String(item?.provider ?? "custom");
        await addAIModel({
          name: entry.name,
          provider: (knownProviders.has(provider)
            ? provider
            : "custom") as AIModelConfig["provider"],
          apiKey: String(item?.apiKey ?? ""),
          authType: (item?.authType === "oauth" ? "oauth" : "api") as AuthType,
          baseUrl: entry.baseUrl,
          model: entry.model,
          temperature: finiteOr(item?.temperature, 0.7),
          maxTokens: finiteOr(item?.maxTokens, 4096),
          isDefault: false,
        });
        added += 1;
      }
      showMsg(
        added > 0 ? "success" : "error",
        t("models.importDone", { added, skipped })
      );
    } catch (error) {
      const message = String(error);
      if (message.includes("已取消")) return;
      showMsg("error", t("models.importFailed", { error: message }));
    } finally {
      setTransferBusy(null);
    }
  };

  const handleTestConnection = async () => {
    if (!canTest) {
      showMsg(
        "error",
        isOllama ? t("models.validation.testRequiredOllama") : t("models.validation.testRequired")
      );
      return;
    }
    if (!isValidHttpUrl(formData.baseUrl.trim())) {
      showMsg("error", t("models.validation.urlInvalid"));
      return;
    }
    setTestStatus({ testing: true, result: null });
    try {
      const result = await runConnectionTest(formData as AIModelConfig);
      setTestStatus({ testing: false, result });
      // Only an already-saved config can carry the verdict; a draft has no row.
      if (editingId) await persistTestResult(editingId, result);
    } catch (error) {
      const result = { success: false, message: String(error) };
      setTestStatus({ testing: false, result });
      if (editingId) await persistTestResult(editingId, result);
    }
  };

  const handleListTest = async (config: AIModelConfig) => {
    if (config.provider !== "ollama" && !config.apiKey.trim()) {
      showMsg("error", t("models.validation.testRequired"));
      return;
    }
    if (!config.baseUrl.trim() || !config.model.trim()) {
      showMsg(
        "error",
        config.provider === "ollama"
          ? t("models.validation.testRequiredOllama")
          : t("models.validation.testRequired")
      );
      return;
    }
    setTestingId(config.id);
    try {
      const result = await runConnectionTest(config);
      await persistTestResult(config.id, result);
      showMsg(result.success ? "success" : "error", result.message);
    } catch (error) {
      await persistTestResult(config.id, { success: false, message: String(error) });
      showMsg("error", String(error));
    } finally {
      setTestingId(null);
    }
  };

  const handleClone = async (config: AIModelConfig) => {
    try {
      await addAIModel({
        name: `${config.name}${t("models.cloneSuffix")}`,
        provider: config.provider,
        apiKey: config.apiKey,
        authType: config.authType,
        baseUrl: config.baseUrl,
        model: config.model,
        temperature: config.temperature,
        maxTokens: config.maxTokens,
        isDefault: false,
      });
      showMsg("success", t("models.cloned", { name: config.name }));
    } catch (error) {
      showMsg("error", t("models.cloneFailed", { error: String(error) }));
    }
  };

  const toggleSelected = (id: string) => {
    setSelectedIds((cur) =>
      cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id]
    );
  };

  const toggleSelectFiltered = () => {
    const ids = filteredConfigs.map((config) => config.id);
    setSelectedIds((cur) =>
      allFilteredSelected
        ? cur.filter((id) => !ids.includes(id))
        : [...new Set([...cur, ...ids])]
    );
  };

  /**
   * Bulk test runs 4 at a time (a serial loop over 8 configs could take two
   * minutes at the 15s HTTP timeout), shows a progress line, and writes every
   * verdict back in one pass so the cards settle together.
   */
  const handleBulkTest = async (configs: AIModelConfig[]) => {
    setBulkBusy("test");
    setBulkProgress({ done: 0, total: configs.length });
    const results: Array<{ id: string; ok: boolean; message: string; at: string }> = [];
    try {
      await mapPool(configs, MODEL_TEST_CONCURRENCY, async (config) => {
        let ok = false;
        let message = "";
        try {
          const result = await runConnectionTest(config);
          ok = result.success;
          message = result.message;
        } catch (error) {
          ok = false;
          message = String(error);
        }
        results.push({ id: config.id, ok, message, at: new Date().toISOString() });
        setBulkProgress((cur) => (cur ? { ...cur, done: cur.done + 1 } : cur));
      });
      await recordAIModelTests(results);
      const fail = results.filter((r) => !r.ok).length;
      const firstFail = results.find((r) => !r.ok);
      showMsg(
        fail === 0 ? "success" : "error",
        fail === 0
          ? t("models.bulkTestDone", { ok: results.length, fail: 0 })
          : `${t("models.bulkTestDone", { ok: results.length - fail, fail })}${
              firstFail ? `: ${brief(firstFail.message)}` : ""
            }`
      );
    } finally {
      setBulkBusy(null);
      setBulkProgress(null);
    }
  };

  /** Bulk sync keeps going past failures and reports once at the end. */
  const handleBulkSync = async (configs: AIModelConfig[]) => {
    setBulkBusy("sync");
    let ok = 0;
    try {
      for (const config of configs) {
        setSyncingId(config.id);
        try {
          await invokeSyncModelToDsh({
            name: config.name,
            provider: config.provider,
            apiKey: config.apiKey,
            baseUrl: config.baseUrl,
            model: config.model,
            maxTokens: config.maxTokens,
          });
          ok += 1;
        } catch {
          /* keep going: one bad config must not stop the batch */
        }
      }
      showMsg(ok > 0 ? "success" : "error", t("models.bulkSyncDone", { ok }));
    } finally {
      setSyncingId(null);
      setBulkBusy(null);
    }
  };

  const handleBulkDelete = async (configs: AIModelConfig[]) => {
    const ok = await confirm({
      title: t("models.bulkDeleteTitle"),
      message: t("models.bulkDeleteMessage", { count: configs.length }),
      warning: configs.some((c) => c.isDefault)
        ? t("models.bulkDeleteDefaultWarning")
        : undefined,
      confirmText: t("models.deleteModel"),
      icon: "danger",
    });
    if (!ok) return;
    let deleted = 0;
    try {
      for (const config of configs) {
        await deleteAIModel(config.id);
        deleted += 1;
      }
      showMsg("success", t("models.bulkDeleteDone", { count: deleted }));
    } catch (error) {
      showMsg("error", t("models.bulkDeleteFailed", { error: String(error) }));
    } finally {
      setSelectedIds([]);
    }
  };

  const handleSyncToDsh = async (config: AIModelConfig) => {
    if (config.provider !== "ollama" && !config.apiKey.trim()) {
      showMsg("error", t("models.syncNoKey"));
      return;
    }
    setSyncingId(config.id);
    try {
      const msg = await invokeSyncModelToDsh({
        name: config.name,
        provider: config.provider,
        apiKey: config.apiKey,
        baseUrl: config.baseUrl,
        model: config.model,
        maxTokens: config.maxTokens,
      });
      let text = msg || t("models.synced");
      let running = false;
      try {
        const list = await useGlobalStore.getState().invokeListDsh();
        running = list.length > 0;
        if (running) {
          text += t("models.syncRestartHint");
        }
      } catch {
        /* ignore status check */
      }
      showMsg("success", text);

      if (running) {
        const ok = await confirm({
          title: t("models.syncRestartTitle"),
          message: t("models.syncRestartMessage"),
          confirmText: t("models.syncRestartConfirm"),
          cancelText: t("models.cancel"),
        });
        if (ok) {
          try {
            const list = await useGlobalStore.getState().invokeListDsh();
            const port = list[0]?.port ?? 3080;
            await useGlobalStore.getState().invokeStopDsh(port);
            await useGlobalStore.getState().invokeStartDsh(port);
            showMsg("success", t("models.syncRestarted"));
          } catch (error) {
            showMsg("error", t("models.syncRestartFailed", { error: String(error) }));
          }
        }
      }
    } catch (error) {
      showMsg("error", String(error));
    } finally {
      setSyncingId(null);
    }
  };

  // One config card, reused by both the flat list and each provider group. Inside a group
  // the provider name already sits on the group header, so the card drops it and promotes
  // the test verdict onto the name line — the first thing worth scanning.
  const renderConfigCard = (config: AIModelConfig, inGroup = false) => {
    const meta = getProviderMeta(config.provider);
    const providerLabel = meta?.label || config.provider;
    const iconClass = meta ? `provider-${config.provider}` : "provider-custom";
    const last = config.lastTest ?? null;
    const testState = testStateOf(config);
    const lastTestTitle = last
      ? `${t("models.lastTest")} · ${last.at ? new Date(last.at).toLocaleString() : ""}${
          last.message ? `: ${brief(last.message, 200)}` : ""
        }`
      : t("models.testFromList");
    return (
      <div
        key={config.id}
        className={`ai-model-item ${config.isDefault ? "default" : ""}`}
      >
        <label className="ai-model-select">
          <input
            type="checkbox"
            checked={selectedIds.includes(config.id)}
            onChange={() => toggleSelected(config.id)}
            aria-label={config.name}
          />
        </label>
        <div className={`ai-model-icon ${iconClass}`}>
          {providerIconLabel(config.provider)}
        </div>
        <div className="ai-model-info">
          <div className="ai-model-name">
            <span className="ai-model-name-text">{config.name}</span>
            {config.isDefault && (
              <span className="ai-model-badge">{t("models.default")}</span>
            )}
            <span className={`ai-model-state is-${testState}`} title={lastTestTitle}>
              <span className="ai-model-state-dot" />
              {last
                ? `${last.ok ? t("models.testPassed") : t("models.testFailed")} · ${formatSince(
                    last.at,
                    t
                  )}`
                : t("models.testNever")}
            </span>
          </div>
          <div className="ai-model-detail">
            {!inGroup && <>{providerLabel} · </>}
            <button
              type="button"
              className="ai-model-copyable"
              title={t("models.copyModelId")}
              onClick={() => void copyValue(`${config.id}:model`, config.model)}
            >
              <span className="ai-model-copyable-text">{config.model}</span>
              {copiedField === `${config.id}:model` ? (
                <Check size={11} />
              ) : (
                <Copy size={11} />
              )}
            </button>
          </div>
          <div className="ai-model-meta">
            <button
              type="button"
              className="ai-model-copyable"
              disabled={!config.baseUrl}
              title={config.baseUrl || t("models.urlMissing")}
              onClick={() => void copyValue(`${config.id}:url`, config.baseUrl)}
            >
              <span className="ai-model-copyable-text">
                {config.baseUrl ? truncateUrl(config.baseUrl) : t("models.urlMissing")}
              </span>
              {copiedField === `${config.id}:url` ? (
                <Check size={11} />
              ) : config.baseUrl ? (
                <Copy size={11} />
              ) : null}
            </button>
            <span className="ai-model-meta-sep">·</span>
            <span className="ai-model-meta-fixed">
              {config.apiKey?.trim()
                ? t("models.keyConfigured")
                : t("models.keyMissing")}
            </span>
          </div>
        </div>
        <div className="ai-model-actions">
          {!config.isDefault && (
            <button
              className="btn btn-secondary btn-small"
              onClick={() => handleSetDefault(config.id)}
              title={t("models.setDefaultAction")}
            >
              <Check size={12} />
              <span className="ai-action-label">{t("models.setDefaultAction")}</span>
            </button>
          )}
          <button
            className={`btn btn-secondary btn-small${
              last ? (last.ok ? " is-ok" : " is-fail") : ""
            }`}
            onClick={() => handleListTest(config)}
            disabled={testingId === config.id || bulkBusy !== null}
            title={lastTestTitle}
          >
            {testingId === config.id ? (
              <Loader2 size={12} className="spin" />
            ) : (
              <Zap size={12} />
            )}
            <span className="ai-action-label">{t("models.testFromList")}</span>
          </button>
          <button
            className="btn btn-secondary btn-small"
            onClick={() => handleSyncToDsh(config)}
            disabled={syncingId === config.id}
            title={t("models.syncToDsh")}
          >
            {syncingId === config.id ? (
              <Loader2 size={12} className="spin" />
            ) : (
              <ArrowRightCircle size={12} />
            )}
            <span className="ai-action-label">{t("models.syncToDsh")}</span>
          </button>
          <button
            className="btn btn-secondary btn-small"
            onClick={() => void handleClone(config)}
            title={t("models.clone")}
          >
            <CopyPlus size={12} />
          </button>
          <button
            className="btn btn-secondary btn-small"
            onClick={() => handleEdit(config)}
            title={t("models.edit")}
          >
            <Edit2 size={12} />
            <span className="ai-action-label">{t("models.edit")}</span>
          </button>
          <button
            className="btn btn-danger btn-small"
            onClick={() => handleDelete(config.id)}
            title={t("models.deleteModel")}
          >
            <Trash2 size={12} />
          </button>
        </div>
      </div>
    );
  };

  // One foldable provider panel; shared by the single-column list and each balanced column.
  const renderGroup = (group: (typeof groupedConfigs)[number]) => (
    <section className="ai-model-group" key={group.provider}>
      <button
        type="button"
        className="ai-model-group-header"
        onClick={() => toggleGroup(group.provider)}
        aria-expanded={!collapsedGroups.has(group.provider)}
        title={
          collapsedGroups.has(group.provider)
            ? t("models.groupExpand")
            : t("models.groupCollapse")
        }
      >
        <ChevronDown
          size={14}
          className={`ai-model-group-chevron ${
            collapsedGroups.has(group.provider) ? "collapsed" : ""
          }`}
        />
        <span className="ai-model-group-name">{group.label}</span>
        {group.failed > 0 && (
          <span
            className="ai-model-group-flag is-fail"
            title={t("models.filterFailed", { count: group.failed })}
          >
            <span className="ai-model-group-flag-dot" />
            {group.failed}
          </span>
        )}
        {group.untested > 0 && (
          <span
            className="ai-model-group-flag is-untested"
            title={t("models.filterUntested", { count: group.untested })}
          >
            <span className="ai-model-group-flag-dot" />
            {group.untested}
          </span>
        )}
        <span className="ai-model-group-count">{group.configs.length}</span>
      </button>
      {!collapsedGroups.has(group.provider) && (
        <div className="ai-model-group-body">
          {group.configs.map((config) => renderConfigCard(config, true))}
        </div>
      )}
    </section>
  );

  return (
    <div className="ai-assistant">
      <>
          <div className="models-container">
            <div className="models-header">
              <div className="models-title">
                <h2>{t("models.title")}</h2>
                <span className="models-count">
                  {listQueryNorm
                    ? t("models.countFiltered", {
                        shown: filteredConfigs.length,
                        total: aiModels.length,
                      })
                    : aiModels.length}
                </span>
              </div>
              <div className="models-header-actions">
                {selectedConfigs.length > 0 ? (
                  <>
                    <span className="models-bulk-count">
                      {t("models.selectedCount", { count: selectedConfigs.length })}
                    </span>
                    {bulkBusy === "test" && bulkProgress && (
                      <span className="runtime-muted models-bulk-progress">
                        <Loader2 size={12} className="spin" />
                        {t("models.bulkTestProgress", {
                          done: bulkProgress.done,
                          total: bulkProgress.total,
                        })}
                      </span>
                    )}
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => void handleBulkTest(selectedConfigs)}
                      disabled={bulkBusy !== null}
                      title={t("models.testFromList")}
                    >
                      {bulkBusy === "test" ? (
                        <Loader2 size={12} className="spin" />
                      ) : (
                        <Zap size={12} />
                      )}
                      {t("models.testFromList")} ({selectedConfigs.length})
                    </button>
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => void handleBulkSync(selectedConfigs)}
                      disabled={bulkBusy !== null}
                      title={t("models.syncToDsh")}
                    >
                      {bulkBusy === "sync" ? (
                        <Loader2 size={12} className="spin" />
                      ) : (
                        <ArrowRightCircle size={12} />
                      )}
                      {t("models.syncToDsh")} ({selectedConfigs.length})
                    </button>
                    <button
                      className="btn btn-danger btn-small"
                      onClick={() => void handleBulkDelete(selectedConfigs)}
                      disabled={bulkBusy !== null}
                      title={t("models.bulkDeleteTitle")}
                    >
                      <Trash2 size={12} />
                      {t("models.deleteModel")}
                    </button>
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => setSelectedIds([])}
                      disabled={bulkBusy !== null}
                      title={t("models.bulkClear")}
                    >
                      <X size={12} />
                      {t("models.bulkClear")}
                    </button>
                  </>
                ) : (
                  <>
                    {filteredConfigs.length > 0 && (
                      <label className="models-select-all">
                        <input
                          type="checkbox"
                          checked={allFilteredSelected}
                          onChange={toggleSelectFiltered}
                        />
                        {t("models.selectAll")}
                      </label>
                    )}
                    {aiModels.length > 0 && (
                      <div className="models-search">
                        <Search size={12} aria-hidden />
                        <input
                          type="search"
                          value={listQuery}
                          onChange={(e) => setListQuery(e.target.value)}
                          placeholder={t("models.searchConfigs")}
                          aria-label={t("models.searchConfigs")}
                        />
                      </div>
                    )}
                    {aiModels.length > 0 && (
                      <select
                        className="input-field models-pick"
                        value={statusFilter}
                        onChange={(e) => setStatusFilter(e.target.value)}
                        aria-label={t("models.filterByState")}
                        title={t("models.filterByState")}
                      >
                        <option value="all">{t("models.filterAll")}</option>
                        <option value="failed">
                          {t("models.filterFailed", { count: testStateCounts.failed })}
                        </option>
                        <option value="untested">
                          {t("models.filterUntested", { count: testStateCounts.untested })}
                        </option>
                        <option value="passed">
                          {t("models.filterPassed", { count: testStateCounts.passed })}
                        </option>
                      </select>
                    )}
                    {aiModels.length > 0 && (
                      <select
                        className="input-field models-pick"
                        value={sortBy}
                        onChange={(e) => setSortBy(e.target.value)}
                        aria-label={t("models.sortBy")}
                        title={t("models.sortBy")}
                      >
                        <option value="default">{t("models.sortDefault")}</option>
                        <option value="name">{t("models.sortName")}</option>
                        <option value="provider">{t("models.sortProvider")}</option>
                        <option value="recentTest">{t("models.sortRecentTest")}</option>
                      </select>
                    )}
                    {aiModels.length > 0 && (
                      <button
                        type="button"
                        className={`btn btn-secondary btn-small${groupByProvider ? " is-active" : ""}`}
                        onClick={() => setGroupByProvider((v) => !v)}
                        title={t("models.groupByProvider")}
                        aria-label={t("models.groupByProvider")}
                        aria-pressed={groupByProvider}
                      >
                        <Layers size={12} />
                      </button>
                    )}
                    {groupByProvider && groupedConfigs.length > 1 && (
                      <button
                        type="button"
                        className="btn btn-secondary btn-small"
                        onClick={toggleAllGroups}
                        title={
                          allGroupsCollapsed
                            ? t("models.groupExpandAll")
                            : t("models.groupCollapseAll")
                        }
                        aria-label={
                          allGroupsCollapsed
                            ? t("models.groupExpandAll")
                            : t("models.groupCollapseAll")
                        }
                      >
                        {allGroupsCollapsed ? (
                          <ChevronsUpDown size={12} />
                        ) : (
                          <ChevronsDownUp size={12} />
                        )}
                      </button>
                    )}
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => void handleImportConfigs()}
                      disabled={transferBusy !== null}
                      title={t("models.importConfigs")}
                      aria-label={t("models.importConfigs")}
                    >
                      {transferBusy === "import" ? (
                        <Loader2 size={12} className="spin" />
                      ) : (
                        <Upload size={12} />
                      )}
                    </button>
                    <button
                      className="btn btn-secondary btn-small"
                      onClick={() => void handleExportConfigs()}
                      disabled={transferBusy !== null || aiModels.length === 0}
                      title={t("models.exportConfigs")}
                      aria-label={t("models.exportConfigs")}
                    >
                      {transferBusy === "export" ? (
                        <Loader2 size={12} className="spin" />
                      ) : (
                        <Download size={12} />
                      )}
                    </button>
                    <button
                      className="btn btn-primary btn-small"
                      onClick={openCreateModal}
                      title={t("models.addModel")}
                    >
                      <Plus size={14} />
                      <span className="ai-action-label">{t("models.addModel")}</span>
                    </button>
                  </>
                )}
              </div>
            </div>
            {aiModels.length > 0 && !defaultConfig && (
              <div className="models-default-banner">
                <AlertCircle size={14} className="models-default-banner-icon" />
                <span className="models-default-banner-text">{t("models.noDefaultBanner")}</span>
                {aiModels.length === 1 && (
                  <button
                    className="btn btn-secondary btn-small"
                    onClick={() => void handleSetDefault(aiModels[0].id)}
                  >
                    {t("models.setAsDefault", { name: aiModels[0].name })}
                  </button>
                )}
              </div>
            )}
            {aiModels.length === 0 ? (
              <div className="models-empty">
                <div className="models-empty-icon">
                  <AppLogoMark size={40} />
                </div>
                <div className="models-empty-title">{t("models.noModels")}</div>
                <div className="models-empty-desc">{t("models.emptyDesc")}</div>
                <button className="btn btn-primary" onClick={openCreateModal}>
                  <Plus size={14} /> {t("models.addModel")}
                </button>
              </div>
            ) : filteredConfigs.length === 0 ? (
              <div className="models-empty">
                <div className="models-empty-title">{t("models.noMatch")}</div>
                <div className="models-empty-desc">
                  {statusFilter === "all"
                    ? t("models.noMatchHint")
                    : t("models.noMatchFiltered")}
                </div>
                <button
                  className="btn btn-secondary"
                  onClick={() => {
                    setListQuery("");
                    setStatusFilter("all");
                  }}
                >
                  {statusFilter === "all" ? t("models.clearSearch") : t("models.clearFilters")}
                </button>
              </div>
            ) : (
              <div className={`ai-model-list${groupByProvider ? "" : " is-flat"}`}>
                {groupByProvider
                  ? groupedConfigs.map((group) => renderGroup(group))
                  : filteredConfigs.map((config) => renderConfigCard(config))}
              </div>
            )}
          </div>

          {showModal && (
            <div className="modal-overlay" onClick={closeModal}>
              <div className="modal models-modal" onClick={(e) => e.stopPropagation()}>
                <ModalTitleRow
                  title={editingId ? t("models.editModel") : t("models.addModel")}
                  onClose={closeModal}
                />
                <div className="ai-modal-content">
                  <div className="ai-form">
                    <div className="ai-form-section">
                      <div className="ai-form-section-label">{t("models.sectionBasic")}</div>
                      <div className="input-group">
                        <label className="input-label">{t("models.name")}</label>
                        <input
                          className="input-field"
                          value={formData.name}
                          onChange={(e) => setFormData({ ...formData, name: e.target.value })}
                          placeholder={t("models.namePlaceholder")}
                        />
                      </div>
                      <div className="input-group">
                        <label className="input-label">{t("models.provider")}</label>
                        <select
                          className="input-field"
                          value={formData.provider}
                          onChange={(e) => handleProviderChange(e.target.value)}
                        >
                          {AI_PROVIDERS.map((p) => (
                            <option key={p.value} value={p.value}>
                              {p.label}
                            </option>
                          ))}
                        </select>
                      </div>
                      {isMimo && (
                        <div className="input-group ai-form-span">
                          <label className="input-label">{t("models.mimoAuth")}</label>
                          <select
                            className="input-field"
                            value={formData.authType}
                            onChange={(e) => handleAuthTypeChange(e.target.value as AuthType)}
                          >
                            <option value="api">{t("models.mimoAuthApi")}</option>
                            <option value="token_plan">{t("models.mimoAuthTokenPlan")}</option>
                          </select>
                        </div>
                      )}
                    </div>

                    <div className="ai-form-section">
                      <div className="ai-form-section-label">{t("models.sectionApi")}</div>
                      <div className="input-group ai-form-span">
                        <label className="input-label">
                          {isOllama ? t("models.apiKeyOptional") : t("models.apiKey")}
                        </label>
                        <div className="input-with-toggle">
                          <input
                            className="input-field"
                            type={showApiKey ? "text" : "password"}
                            value={formData.apiKey}
                            onChange={(e) => setFormData({ ...formData, apiKey: e.target.value })}
                            placeholder={
                              isOllama
                                ? t("models.apiKeyOllamaPlaceholder")
                                : t("models.apiKeyPlaceholder")
                            }
                          />
                          <button
                            type="button"
                            className="input-toggle-btn"
                            onClick={() => setShowApiKey(!showApiKey)}
                            title={showApiKey ? t("models.hideKey") : t("models.showKey")}
                          >
                            {showApiKey ? <EyeOff size={14} /> : <Eye size={14} />}
                          </button>
                        </div>
                      </div>
                      <div className="input-group ai-form-span">
                        <label className="input-label">{t("models.baseUrl")}</label>
                        <input
                          className="input-field"
                          value={formData.baseUrl}
                          onChange={(e) => {
                            setFetchedModels(null);
                            setFormData({ ...formData, baseUrl: e.target.value });
                          }}
                          placeholder="https://api.openai.com/v1"
                        />
                      </div>
                      <div className="input-group ai-form-span">
                        <div className="ai-model-preset-header">
                          <label className="input-label">{t("models.modelPreset")}</label>
                          <button
                            type="button"
                            className="btn btn-secondary btn-small"
                            onClick={handleRefreshModels}
                            disabled={!canRefreshModels}
                            title={
                              refreshDisabledReason ||
                              (fetchingModels ? t("models.refreshingList") : t("models.refreshList"))
                            }
                          >
                            {fetchingModels ? (
                              <Loader2 size={12} className="spin" />
                            ) : (
                              <RefreshCw size={12} />
                            )}
                            <span className="ai-action-label">
                              {fetchingModels ? t("models.refreshingList") : t("models.refreshList")}
                            </span>
                          </button>
                        </div>
                        {showModelSelect && (
                          <div className="ai-model-picker" ref={pickerRootRef}>
                            <button
                              type="button"
                              ref={pickerTriggerRef}
                              className="input-field ai-model-trigger"
                              aria-haspopup="dialog"
                              aria-expanded={pickerOpen}
                              title={formData.model || t("models.model")}
                              onClick={() => setPickerOpen((v) => !v)}
                            >
                              <span className="ai-model-trigger-label">
                                {formData.model || t("models.model")}
                              </span>
                              <ChevronDown size={14} aria-hidden />
                            </button>
                            {pickerOpen &&
                              pickerMenuStyle &&
                              createPortal(
                                <div
                                  ref={pickerMenuRef}
                                  className="ai-model-menu is-floating"
                                  style={{
                                    top: pickerMenuStyle.top,
                                    bottom: pickerMenuStyle.bottom,
                                    left: pickerMenuStyle.left,
                                    width: pickerMenuStyle.width,
                                    maxHeight: pickerMenuStyle.maxHeight,
                                  }}
                                >
                                  <input
                                    ref={pickerSearchRef}
                                    className="ai-model-search"
                                    type="search"
                                    value={pickerQuery}
                                    onChange={(e) => setPickerQuery(e.target.value)}
                                    onKeyDown={(e) => {
                                      if (e.key === "Escape") {
                                        closeModelPicker();
                                        return;
                                      }
                                      if (e.key !== "Enter") return;
                                      e.preventDefault();
                                      // Enter takes the exact match, else the typed custom id.
                                      const exact = listedModels.find(
                                        (m) => m.id === customCandidate
                                      );
                                      if (exact) commitModel(exact.id);
                                      else if (showCustomRow) commitModel(customCandidate);
                                    }}
                                    placeholder={t("models.modelSearch")}
                                    aria-label={t("models.modelSearch")}
                                  />
                                  <div className="ai-model-menu-list">
                                    {filteredModels.length === 0 && !showCustomRow ? (
                                      <div className="ai-model-empty">
                                        {t("models.modelNoMatch")}
                                      </div>
                                    ) : (
                                      filteredModels.map((m) => (
                                        <div
                                          key={m.id}
                                          className={`ai-model-option ${
                                            m.id === formData.model ? "is-active" : ""
                                          }`}
                                        >
                                          <button
                                            type="button"
                                            className="ai-model-option-main"
                                            title={m.id}
                                            onClick={() => commitModel(m.id)}
                                          >
                                            <span className="ai-model-option-id">{m.id}</span>
                                            {m.label && m.label !== m.id && (
                                              <span className="ai-model-option-label">
                                                {m.label}
                                              </span>
                                            )}
                                          </button>
                                          <button
                                            type="button"
                                            className="ai-model-option-copy"
                                            title={t("models.copyModelId")}
                                            aria-label={t("models.copyModelId")}
                                            onClick={() => void copyValue(`picker:${m.id}`, m.id)}
                                          >
                                            {copiedField === `picker:${m.id}` ? (
                                              <Check size={12} />
                                            ) : (
                                              <Copy size={12} />
                                            )}
                                          </button>
                                        </div>
                                      ))
                                    )}
                                    {showCustomRow && (
                                      <div className="ai-model-option">
                                        <button
                                          type="button"
                                          className="ai-model-option-main"
                                          title={customCandidate}
                                          onClick={() => commitModel(customCandidate)}
                                        >
                                          <span className="ai-model-option-id">
                                            {t("models.modelCustom")}
                                          </span>
                                          <span className="ai-model-option-label">
                                            {customCandidate}
                                          </span>
                                        </button>
                                      </div>
                                    )}
                                  </div>
                                </div>,
                                document.body
                              )}
                          </div>
                        )}
                      </div>
                      {showCustomModelInput && (
                        <div className="input-group ai-form-span">
                          <label className="input-label">{t("models.model")}</label>
                          <input
                            className="input-field"
                            value={formData.model}
                            onChange={(e) => handleCustomModelChange(e.target.value)}
                            placeholder={t("models.modelCustomPlaceholder")}
                          />
                        </div>
                      )}
                    </div>

                    <div className="ai-form-section">
                      <div className="ai-form-section-label">{t("models.sectionAdvanced")}</div>
                      <div className="ai-preset-grid">
                        <TemperatureField
                          value={formData.temperature}
                          onPick={(temperature) => setFormData({ ...formData, temperature })}
                        />
                        <TokenField
                          value={formData.maxTokens}
                          onPick={(maxTokens) => setFormData({ ...formData, maxTokens })}
                        />
                      </div>
                      <label className="ai-checkbox">
                        <input
                          type="checkbox"
                          checked={formData.isDefault}
                          onChange={(e) =>
                            setFormData({ ...formData, isDefault: e.target.checked })
                          }
                        />
                        {t("models.setDefaultCheckbox")}
                      </label>
                    </div>

                    <div className="ai-test-row">
                      <button
                        className="btn btn-secondary btn-small"
                        onClick={handleTestConnection}
                        disabled={testStatus.testing || !canTest}
                      >
                        {testStatus.testing ? (
                          <Loader2 size={12} className="spin" />
                        ) : (
                          <Check size={12} />
                        )}
                        {testStatus.testing ? t("models.testing") : t("models.testConnection")}
                      </button>
                      {testStatus.result && (
                        <span
                          className={`ai-test-result ${
                            testStatus.result.success ? "success" : "error"
                          }`}
                        >
                          {testStatus.result.message}
                        </span>
                      )}
                    </div>
                  </div>
                </div>
                <div className="modal-actions">
                  <button className="btn btn-secondary" onClick={closeModal}>
                    {t("models.cancel")}
                  </button>
                  <button
                    className="btn btn-primary"
                    onClick={handleSubmit}
                    disabled={!canSubmit}
                  >
                    {editingId ? t("models.save") : t("models.addModel")}
                  </button>
                </div>
              </div>
            </div>
          )}
        </>

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

export default AIAssistant;
