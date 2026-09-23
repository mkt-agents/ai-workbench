/**
 * Regression report panel — the right-hand detail of the "报告" sub-tab.
 * Two modes via a discriminated prop: a single repo, or a whole folder merged
 * into one report (repos that live together ship related code). Selecting a
 * target auto-collects the report (no click needed); the AI scenarios are generated
 * manually on demand. Nothing is persisted; the backend keeps the collected report
 * in an in-process cache keyed by reportId.
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import { useTranslation } from "react-i18next"
import { listen } from "@tauri-apps/api/event"
import {
  ChevronDown,
  ChevronRight,
  ClipboardList,
  Copy,
  Download,
  GitCommitHorizontal,
  History,
  Loader2,
  Search,
  ShieldAlert,
  Sparkles,
  Trash2,
} from "lucide-react"
import { useGlobalStore } from "../core/store"
import { refreshRepos } from "../core/gitCache"
import { readStoredString, writeStoredString } from "../core/localState"
import { markdownSections } from "../lib/aiText"
import { parseAiLine } from "../lib/reportAi"
import { projectNameFromPath } from "../core/pathUtils"
import { isCancelledError, cleanErrorMessage } from "./testReportTypes"
import type {
  AiResult,
  ChangeReport,
  FileChange,
  FolderRepoRow,
  ReportAiHistoryEntry,
} from "./testReportTypes"
import type { AIModelConfig } from "../core/types"
import { summarizeRisks } from "../lib/reportRisk"
import { useFilteredFiles, EMPTY_FILTER } from "./useFilteredFiles"
import type { FileFilter } from "./useFilteredFiles"
import MultiSelectDropdown from "./MultiSelectDropdown"
import "./TestReportModal.css"

type Props = { repoPath: string } | { folderName: string; repoPaths: string[] }

type Mode = "uncommitted" | "base" | "commits" | "since"
type GroupMode = "module" | "commit"
type Tab = "overview" | "files" | "commits" | "api" | "ai"

/** A merged single/folder view, so the render path is shared. */
type View = {
  report: ChangeReport
  reportId: string
  /** Repo or folder display name — used for the title, export and AI label. */
  name: string
  branch: string
  head: string
  /** Per-repo rollup; non-null only in folder mode. */
  rows: FolderRepoRow[] | null
}

/** The panel renders a bounded list; the full set still counts towards the totals. */
const MAX_VISIBLE_FILES = 150
const MODEL_KEY = "workbench-commit-model"
const SYSTEM_KEY = "workbench-report-ai-system"
/** Pasted requirement/acceptance text — the only non-code input of the AI step. */
const REQUIREMENT_KEY = "workbench-report-requirement"
/** Long enough that writing on every keystroke would stutter a pasted PRD. */
const REQUIREMENT_SAVE_DEBOUNCE_MS = 400

interface ReportModeState {
  mode: Mode
  base: string
  commits: number
  /** `YYYY-MM-DD` bounds of the date window; both ends are inclusive. */
  since: string
  until: string
}

const DAY_MS = 24 * 60 * 60 * 1000
const ISO_DATE = /^\d{4}-\d{2}-\d{2}$/

/** Local `YYYY-MM-DD` — `toISOString` is UTC and shifts the day near midnight. */
function localIsoDate(date: Date): string {
  const month = String(date.getMonth() + 1).padStart(2, "0")
  const day = String(date.getDate()).padStart(2, "0")
  return `${date.getFullYear()}-${month}-${day}`
}

/** A window that is useful the first time the mode is picked: last 7 days, today included. */
function defaultDateWindow(): { since: string; until: string } {
  const today = new Date()
  return { since: localIsoDate(new Date(today.getTime() - 6 * DAY_MS)), until: localIsoDate(today) }
}

function readReportMode(repoPath: string): ReportModeState {
  const window = defaultDateWindow()
  const fallback: ReportModeState = { mode: "uncommitted", base: "", commits: 5, ...window }
  try {
    const parsed: unknown = JSON.parse(readStoredString("workbench-report-mode:" + repoPath))
    if (!parsed || typeof parsed !== "object") return fallback
    const value = parsed as Partial<ReportModeState>
    const mode =
      value.mode === "base" || value.mode === "commits" || value.mode === "since" ? value.mode : "uncommitted"
    const commits = Number(value.commits)
    return {
      mode,
      base: typeof value.base === "string" ? value.base : "",
      commits: Number.isFinite(commits) ? Math.max(1, Math.min(50, Math.trunc(commits))) : 5,
      // A remembered window can be stale but never malformed; anything that does
      // not look like a date falls back to the default rather than reaching git.
      since: typeof value.since === "string" && ISO_DATE.test(value.since) ? value.since : window.since,
      until: typeof value.until === "string" && ISO_DATE.test(value.until) ? value.until : window.until,
    }
  } catch {
    return fallback
  }
}

/**
 * Splits a changed-file path at its last separator so the two halves can be
 * styled and truncated independently: the directory is dimmed and gives up its
 * width first, which keeps the file name — the part that identifies the row —
 * readable even in a deep monorepo path.
 */
function PathLabel({ path }: { path: string }) {
  const cut = Math.max(path.lastIndexOf("/"), path.lastIndexOf("\\"))
  if (cut < 0) return <span className="tr-file-name">{path}</span>
  return (
    <>
      <span className="tr-file-dir">{path.slice(0, cut + 1)}</span>
      <span className="tr-file-name">{path.slice(cut + 1)}</span>
    </>
  )
}

/** `09-23 15:30`. The table only keeps ~50 answers, so the year is noise here;
 *  the full timestamp stays available through the `title` attribute. */
function formatHistoryTime(millis: number): string {
  const date = new Date(millis)
  const pad = (value: number) => String(value).padStart(2, "0")
  return `${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`
}

export default function TestReportPanel(props: Props) {
  const { t } = useTranslation("git")

  const isFolder = "repoPaths" in props
  const repoPath = isFolder ? "" : props.repoPath
  const folderName = isFolder ? props.folderName : ""
  const repoPaths = isFolder ? props.repoPaths : []

  const collect = useGlobalStore((s) => s.invokeCollectTestReport)
  const collectFolder = useGlobalStore((s) => s.invokeCollectFolderReport)
  const generateAi = useGlobalStore((s) => s.invokeGenerateTestReportAi)
  const cancelAi = useGlobalStore((s) => s.invokeCancelTestReportAi)
  const saveReportHistory = useGlobalStore((s) => s.invokeSaveReportAiHistory)
  const listReportHistory = useGlobalStore((s) => s.invokeListReportAiHistory)
  const deleteReportHistory = useGlobalStore((s) => s.invokeDeleteReportAiHistory)
  const saveTextFile = useGlobalStore((s) => s.invokeSaveTextFile)
  const copyToClipboard = useGlobalStore((s) => s.invokeCopyToClipboard)
  const aiModels = useGlobalStore((s) => s.aiModels)
  const loadAIModels = useGlobalStore((s) => s.loadAIModels)

  const [mode, setMode] = useState<Mode>("uncommitted")
  const [base, setBase] = useState("")
  const [commits, setCommits] = useState(5)
  const [since, setSince] = useState(() => defaultDateWindow().since)
  const [until, setUntil] = useState(() => defaultDateWindow().until)
  const [busy, setBusy] = useState(false)
  const [view, setView] = useState<View | null>(null)
  const [error, setError] = useState("")
  const [toast, setToast] = useState<{ type: "success" | "error"; text: string } | null>(null)
  const [collectProgress, setCollectProgress] = useState<{ done: number; total: number } | null>(null)

  const [ai, setAi] = useState("")
  const [aiWarnings, setAiWarnings] = useState<string[]>([])
  /** The requirement snapshot the shown AI text was built from. Comparing it with
      the box lets us say "regenerate to pick this up" instead of ignoring the edit. */
  const [aiRequirement, setAiRequirement] = useState("")
  const [aiBusy, setAiBusy] = useState(false)
  const [aiProgress, setAiProgress] = useState<{ done: number; total: number } | null>(null)

  // Past answers, newest first. Loaded lazily when the section is opened and only
  // ever rendered read-only — it never writes back into `ai`, so the live report
  // stays the single source of truth for the panel's current state.
  const [history, setHistory] = useState<ReportAiHistoryEntry[]>([])
  const [historyOpen, setHistoryOpen] = useState(false)
  const [historyExpandedId, setHistoryExpandedId] = useState<number | null>(null)

  const [onlyUntested, setOnlyUntested] = useState(false)
  const [groupMode, setGroupMode] = useState<GroupMode>("module")
  const [selectedModelId, setSelectedModelId] = useState<string>(() => readStoredString(MODEL_KEY))
  const [activeTab, setActiveTab] = useState<Tab>("overview")
  const [fileFilter, setFileFilter] = useState<FileFilter>(EMPTY_FILTER)
  const [expandAll, setExpandAll] = useState(true)
  const [customSystem, setCustomSystem] = useState(() => readStoredString(SYSTEM_KEY))
  const [requirement, setRequirement] = useState(() => readStoredString(REQUIREMENT_KEY))
  const [promptOpen, setPromptOpen] = useState(true)
  const [requirementOpen, setRequirementOpen] = useState(() => readStoredString(REQUIREMENT_KEY).trim().length > 0)

  // Bumped whenever the panel target changes, so a slow collect/AI that lands
  // after the switch is dropped instead of bleeding into the new target.
  const genRef = useRef(0)
  const modeStorageKeyRef = useRef("")

  const report = view?.report ?? null
  const viewingId = view?.reportId ?? ""
  const rows = view?.rows ?? null
  const commitDetails = report?.commitDetails ?? []
  const displayName = isFolder ? folderName : projectNameFromPath(repoPath)
  const modeStorageKey = repoPath ? "workbench-report-mode:" + repoPath : ""

  const defaultModel = useMemo(() => aiModels.find((m) => m.isDefault) || aiModels[0] || null, [aiModels])
  const selectedModel: AIModelConfig | null = useMemo(() => {
    if (selectedModelId) {
      const found = aiModels.find((m) => m.id === selectedModelId)
      if (found) return found
    }
    return defaultModel
  }, [selectedModelId, aiModels, defaultModel])

  const filtered = useFilteredFiles(report?.files ?? [], fileFilter)
  const { visible, total, options } = filtered
  useEffect(() => {
    writeStoredString(MODEL_KEY, selectedModelId)
  }, [selectedModelId])

  useEffect(() => {
    writeStoredString(SYSTEM_KEY, customSystem)
  }, [customSystem])

  // Debounced: this box routinely holds a pasted PRD, and writing a few KB to
  // localStorage on every keystroke is visible. Remembered across targets on
  // purpose — in a folder report the same ask spans several repos.
  useEffect(() => {
    const id = window.setTimeout(() => writeStoredString(REQUIREMENT_KEY, requirement), REQUIREMENT_SAVE_DEBOUNCE_MS)
    return () => window.clearTimeout(id)
  }, [requirement])

  useEffect(() => {
    if (!modeStorageKey) return
    if (modeStorageKeyRef.current !== modeStorageKey) {
      modeStorageKeyRef.current = modeStorageKey
      return
    }
    writeStoredString(modeStorageKey, JSON.stringify({ mode, base, commits, since, until }))
  }, [modeStorageKey, mode, base, commits, since, until])

  useEffect(() => {
    if (aiModels.length === 0) void loadAIModels().catch(() => {})
  }, [aiModels.length, loadAIModels])

  const showMsg = useCallback((type: "success" | "error", text: string) => {
    setToast({ type, text })
    window.setTimeout(() => setToast(null), type === "error" ? 8000 : 3000)
  }, [])

  // A new baseline invalidates any shown report — never let it go stale against
  // the wrong range.
  const resetReport = useCallback(() => {
    setView(null)
    setAi("")
    setAiWarnings([])
    setAiRequirement("")
    setAiProgress(null)
    setCollectProgress(null)
    setError("")
  }, [])

  /** Loaded lazily when the history section opens, and refreshed after a new
   *  answer lands so the list is correct whether or not it is on screen. */
  const loadReportHistory = useCallback(async () => {
    try {
      setHistory(await listReportHistory())
    } catch {
      // A failed history read must never disturb the report — the section simply
      // keeps showing what it already had.
    }
  }, [listReportHistory])

  const removeReportHistory = useCallback(
    async (id: number) => {
      try {
        await deleteReportHistory(id)
        setHistory((prev) => prev.filter((item) => item.id !== id))
        setHistoryExpandedId((current) => (current === id ? null : current))
      } catch (e) {
        showMsg("error", cleanErrorMessage(e))
      }
    },
    [deleteReportHistory, showMsg]
  )

  // The history only leaves the database when the tester asks to see it. Declared
  // after `loadReportHistory` on purpose: the dependency array is evaluated during
  // render, so referencing a later `const` here would hit the temporal dead zone.
  useEffect(() => {
    if (historyOpen) void loadReportHistory()
  }, [historyOpen, loadReportHistory])

  const runAi = useCallback(
    async (idArg?: string) => {
      const id = idArg ?? viewingId
      if (aiBusy || !id || !selectedModel) return
      const gen = genRef.current
      // Snapshot the ask: the result must be stamped with what it actually used,
      // not with whatever the box holds when the response lands.
      const ask = requirement.trim()
      setAiBusy(true)
      setAiProgress(null)
      setError("")
      try {
        const result: AiResult = await generateAi(
          id,
          selectedModel,
          customSystem.trim() || undefined,
          ask || undefined
        )
        if (genRef.current !== gen) return // target switched mid-flight — drop it
        setAi(result.markdown)
        setAiWarnings(result.warnings)
        setAiRequirement(ask)
        // Keep the answer: it cost a map-reduce round trip and it is the artefact
        // the tester works from. The report itself is never stored — it is a
        // projection of the working tree, recomputed on every selection.
        void saveReportHistory({
          targetKind: isFolder ? "folder" : "repo",
          targetLabel: view?.name ?? "",
          baseline: view?.report.base ?? "",
          model: selectedModel.name,
          requirement: ask,
          markdown: result.markdown,
        })
          .then(() => void loadReportHistory())
          .catch(() => {
            // A failed history write must not read as a failed generation: the
            // panel already has the text, which is what the user asked for.
          })
      } catch (e) {
        if (genRef.current !== gen) return
        // A cancel is a user action, not a failure; the static report stays usable.
        if (isCancelledError(e)) {
          showMsg("success", t("testReport.aiCancelled"))
        } else {
          setError(cleanErrorMessage(e))
          showMsg("error", t("testReport.aiFailed", { error: cleanErrorMessage(e) }))
        }
      } finally {
        if (genRef.current === gen) {
          setAiBusy(false)
          setAiProgress(null)
        }
      }
    },
    [
      aiBusy,
      viewingId,
      selectedModel,
      generateAi,
      customSystem,
      requirement,
      view,
      isFolder,
      saveReportHistory,
      loadReportHistory,
      showMsg,
      t,
    ]
  )

  const generate = async (force = false, scope?: ReportModeState) => {
    if (busy && !force) return
    const gen = genRef.current
    setBusy(true)
    setError("")
    setCollectProgress(null)
    const activeMode = scope?.mode ?? mode
    const baseArg = activeMode === "base" ? (scope?.base ?? base).trim() : undefined
    const commitsArg = activeMode === "commits" ? scope?.commits ?? commits : undefined
    const sinceArg = activeMode === "since" ? (scope?.since ?? since).trim() : undefined
    const untilArg = activeMode === "since" ? (scope?.until ?? until).trim() : undefined
    try {
      if (isFolder) {
        const r = await collectFolder(folderName, repoPaths, baseArg, commitsArg, sinceArg, untilArg)
        if (genRef.current !== gen) return // switched away while collecting
        setView({ report: r.report, reportId: r.reportId, name: r.folderName, branch: "", head: "", rows: r.repos })
        void refreshRepos(repoPaths, { withStatus: true, force: true }).catch(() => {})
        setAi("")
        setAiWarnings([])
        if (r.report.stats.files === 0) showMsg("success", t("testReport.empty"))
      } else {
        const r = await collect(repoPath, baseArg, commitsArg, sinceArg, untilArg)
        if (genRef.current !== gen) return
        setView({ report: r.report, reportId: r.reportId, name: r.repoName, branch: r.branch, head: r.head, rows: null })
        void refreshRepos([repoPath], { withStatus: true, force: true }).catch(() => {})
        setAi("")
        setAiWarnings([])
        if (r.report.stats.files === 0) showMsg("success", t("testReport.empty"))
      }
    } catch (e) {
      if (genRef.current !== gen) return
      setError(cleanErrorMessage(e))
      showMsg("error", t("testReport.failed", { error: cleanErrorMessage(e) }))
    } finally {
      if (genRef.current === gen) setBusy(false)
    }
  }

  // Switching target: cancel any live AI for the old report, then wipe the view
  // and the filters (meaningless for a different target). Auto-collect for the
  // new target — `force` bypasses the busy guard so a fresh report appears
  // without a click. The genRef guard drops stale results, so rapid switching is safe.
  const targetKey = isFolder ? `f:${folderName}|${repoPaths.join("|")}` : `r:${repoPath}`
  useEffect(() => {
    genRef.current += 1
    if (aiBusy && viewingId) void cancelAi(viewingId).catch(() => {})
    const remembered = readReportMode(repoPath)
    setMode(remembered.mode)
    setBase(remembered.base)
    setCommits(remembered.commits)
    setSince(remembered.since)
    setUntil(remembered.until)
    setGroupMode("module")
    setOnlyUntested(false)
    setActiveTab("overview")
    setFileFilter(EMPTY_FILTER)
    setExpandAll(true)
    resetReport()
    void generate(true, remembered)
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [targetKey])

  // The folder collect is one slow pass over every repo; keep the count live.
  useEffect(() => {
    if (!busy || !isFolder) return
    const un = listen<{ done: number; total: number }>("test-report-collect-progress", (event) => {
      setCollectProgress({ done: event.payload.done, total: event.payload.total })
    })
    return () => {
      void un.then((dispose) => dispose())
    }
  }, [busy, isFolder])

  // The map-reduce AI pass emits one event per finished chunk.
  useEffect(() => {
    if (!aiBusy || !viewingId) return
    const un = listen<{ reportId: string; done: number; total: number }>(
      "test-report-ai-progress",
      (event) => {
        if (event.payload.reportId !== viewingId) return
        setAiProgress({ done: event.payload.done, total: event.payload.total })
      }
    )
    return () => {
      void un.then((dispose) => dispose())
    }
  }, [aiBusy, viewingId])

  const cancelAiRun = async () => {
    if (!viewingId) return
    try {
      await cancelAi(viewingId)
      showMsg("success", t("testReport.aiCancelSent"))
    } catch (e) {
      showMsg("error", cleanErrorMessage(e))
    }
  }

  const copySection = useCallback(
    async (text: string) => {
      try {
        await copyToClipboard(text)
        showMsg("success", t("testReport.copied"))
      } catch (e) {
        showMsg("error", cleanErrorMessage(e))
      }
    },
    [copyToClipboard, showMsg, t]
  )

  const fileMatches = useMemo(() => {
    if (!report) return []
    return onlyUntested ? visible.filter((f) => f.risks.includes("untested")) : visible
  }, [report, visible, onlyUntested])

  const truncated = fileMatches.length > MAX_VISIBLE_FILES
  const shownFiles = truncated ? fileMatches.slice(0, MAX_VISIBLE_FILES) : fileMatches

  const riskSummary = useMemo(() => summarizeRisks(report), [report])

  const fileGroups = useMemo(() => {
    if (!report) return []
    if (groupMode === "commit" && commitDetails.length > 0) {
      return commitDetails
        .map((commit) => ({
          kind: "commit" as const,
          key: commit.sha,
          commit,
          files: shownFiles.filter((f) => (f.commits ?? []).includes(commit.shortSha)),
        }))
        .filter((g) => g.files.length > 0)
    }
    return report.groups
      .map((group) => ({
        kind: "module" as const,
        key: group.name,
        group,
        files: shownFiles.filter((f) => f.module === group.name),
      }))
      .filter((g) => g.files.length > 0)
  }, [report, shownFiles, groupMode, commitDetails])

  const ungroupedCommits = useMemo(() => {
    if (!report) return []
    return commitDetails.filter(
      (commit) => report.files.length > 0 && !fileGroups.some((g) => g.kind === "commit" && g.commit.shortSha === commit.shortSha)
    )
  }, [report, commitDetails, fileGroups])

  const aiSections = useMemo(() => Object.entries(markdownSections(ai)), [ai])

  /** Shared by the live answer and the history entries: both are the same shape of
   *  markdown, so both get P0/P1/P2 case cards and per-section copy. */
  const renderAiSections = (markdownText: string) => (
    <div className="tr-ai-body">
      {Object.entries(markdownSections(markdownText)).map(([title, body]) => (
        <section key={title} className="tr-ai-block">
          <h4>
            {title}
            <button
              type="button"
              className="btn btn-secondary btn-small tr-ai-block-copy"
              onClick={() => void copySection(`## ${title}` + `\n\n` + body)}
              title={t("testReport.ai.copySection", { defaultValue: "复制该章节" })}
            >
              <Copy size={12} />
            </button>
          </h4>
          <ul>
            {body
              .split("\n")
              .map((line) => line.replace(/^[-*]\s*/, "").trim())
              .filter(Boolean)
              .map((item, index) => {
                // A formatted scenario/coverage line becomes a case card;
                // anything else stays a plain bullet.
                const parsed = parseAiLine(item)
                if (!parsed) return <li key={`${title}-${index}`}>{item}</li>
                return (
                  <li key={`${title}-${index}`} className="tr-ai-case">
                    <div className="tr-ai-case-head">
                      {parsed.priority && (
                        <span className={`tr-ai-chip tr-ai-priority is-${parsed.priority.toLowerCase()}`}>
                          {parsed.priority}
                        </span>
                      )}
                      {parsed.verdict && (
                        <span className={`tr-ai-chip tr-ai-verdict is-${parsed.verdict.key}`}>
                          {parsed.verdict.text}
                        </span>
                      )}
                      {parsed.title && <span className="tr-ai-case-title">{parsed.title}</span>}
                    </div>
                    <dl className="tr-ai-case-fields">
                      {parsed.fields.map((field) => (
                        <div key={field.label} className="tr-ai-case-field">
                          <dt>{field.label}</dt>
                          <dd>{field.value}</dd>
                        </div>
                      ))}
                    </dl>
                  </li>
                )
              })}
          </ul>
        </section>
      ))}
    </div>
  )

  const applyOnlyUntested = (files: FileChange[]) =>
    files.filter((f) => !onlyUntested || f.risks.includes("untested")).slice(0, MAX_VISIBLE_FILES)

  const markdown = useMemo(() => {
    if (!report) return ""
    const lines: string[] = []
    lines.push(`# ${t(isFolder ? "testReport.folderTitle" : "testReport.title")} · ${view?.name ?? ""}`)
    lines.push("")
    lines.push(`- ${t("testReport.baseline")}: ${report.base || t("testReport.modeUncommitted")}`)
    if (view?.branch)
      lines.push(`- ${t("testReport.branch")}: ${view.branch}${view.head ? ` @ ${view.head}` : ""}`)
    lines.push(
      `- ${t("testReport.stats")}: ${report.stats.files} ${t("testReport.files")}, +${report.stats.adds} -${report.stats.dels}, ` +
        `${report.stats.modules} ${t("testReport.modules")}, ${report.stats.untested} ${t("testReport.untested")}` +
        (report.stats.ignored ? `, ${t("testReport.ignored")} ${report.stats.ignored}` : "")
    )
    if (rows && rows.length) {
      lines.push("", `## ${t("testReport.repoSummary")}`)
      for (const row of rows) {
        lines.push(
          row.error
            ? `- ${row.name}: ${t("testReport.repoError", { error: row.error })}`
            : `- ${row.name}: ${row.files} ${t("testReport.files")}, +${row.adds} -${row.dels}${
                row.untested ? `, ${row.untested} ${t("testReport.untested")}` : ""
              }`
        )
      }
    }
    if (report.scope.length)
      lines.push(
        `- ${t("testReport.scopeTitle")}: ${report.scope.map((s) => t(`testReport.scope.${s}`, { defaultValue: s })).join(" / ")}`
      )
    if (report.commits.length) {
      lines.push("", `## ${t("testReport.commits")}`)
      for (const c of report.commits) lines.push(`- ${c}`)
    }
    for (const group of report.groups) {
      lines.push("", `## ${group.name} (${group.files} ${t("testReport.files")}, +${group.adds} -${group.dels})`)
      if (group.risks.length)
        lines.push(`- ${t("testReport.risks")}: ${group.risks.map((r) => t(`testReport.risk.${r}`, { defaultValue: r })).join(" / ")}`)
      for (const file of applyOnlyUntested(report.files.filter((f) => f.module === group.name))) {
        const flags = [
          file.layer && t(`testReport.layer.${file.layer}`, { defaultValue: file.layer }),
          ...file.risks.map((r) => t(`testReport.risk.${r}`, { defaultValue: r })),
          (file.commits ?? []).length ? `@${(file.commits ?? []).join("/")}` : null,
        ]
          .filter(Boolean)
          .join(" · ")
        lines.push(`- [${file.status}] ${file.path} (+${file.adds} -${file.dels}) ${flags}`)
      }
    }
    if (report.apiChanges.length) {
      lines.push("", `## ${t("testReport.apiChanges")}`)
      for (const change of report.apiChanges)
        lines.push(`- [${t(`testReport.api.${change.kind}`, { defaultValue: change.kind })}] ${change.name} @ ${change.path}`)
    }
    // The ask is part of the deliverable: whoever reads the exported report has
    // to see what was being tested against, not just what the AI concluded.
    if (requirement.trim())
      lines.push(
        "",
        `## ${t("testReport.ai.requirement", { defaultValue: "需求与验收标准" })}`,
        "",
        requirement.trim()
      )
    if (ai) lines.push("", `## ${t("testReport.aiSection")}`, "", ai.trim())
    return lines.join("\n")
  }, [report, view, rows, ai, requirement, onlyUntested, isFolder, t])

  const copy = async () => {
    try {
      await copyToClipboard(markdown)
      showMsg("success", t("testReport.copied"))
    } catch (e) {
      showMsg("error", cleanErrorMessage(e))
    }
  }

  const exportMd = async () => {
    try {
      const name = `${view?.name ?? "folder"}-${t(isFolder ? "testReport.folderTitle" : "testReport.title")}-${new Date().toISOString().slice(0, 10)}.md`
      const path = await saveTextFile(markdown, name, t("testReport.export"))
      showMsg("success", t("testReport.savedTo", { path }))
    } catch (e) {
      const text = cleanErrorMessage(e)
      if (!/cancel/i.test(text)) showMsg("error", text)
    }
  }

  /** An interface change lives in a file; the useful next step is seeing that
   *  file in context, not reading its path off a list. */
  const jumpToFile = (path: string) => {
    setFileFilter({ ...EMPTY_FILTER, search: path })
    setExpandAll(true)
    setActiveTab("files")
  }

  const renderFile = (file: FileChange) => (
    <li key={`${groupMode}-${file.path}`} className="tr-file">
      <span className={`tr-file-status tr-status-${file.status}`}>{file.status}</span>
      <span
        className="tr-file-path"
        title={`${file.path}${file.testPath ? ` → ${file.testPath}` : file.hasTest ? "" : ` · ${t("testReport.noTestHint")}`}`}
      >
        <PathLabel path={file.path} />
      </span>
      <span className="tr-file-layer">{t(`testReport.layer.${file.layer}`, { defaultValue: file.layer })}</span>
      <span className="tr-file-delta">
        +{file.adds} -{file.dels}
      </span>
      <span className="tr-file-flags">
        {file.hasTest ? <span className="tr-ok">✓</span> : <span className="tr-warn">—</span>}
        {file.risks
          .filter((r) => r !== "untested" && r !== "newFile")
          .map((r) => (
            <span key={r} className={`tr-risk tr-risk-${r}`}>
              {t(`testReport.risk.${r}`, { defaultValue: r })}
            </span>
          ))}
        {(file.hints ?? []).map((h) => (
          <span key={h} className="tr-risk tr-risk-hint" title={t("testReport.hintWhy")}>
            {t(`testReport.risk.${h}`, { defaultValue: h })}
          </span>
        ))}
        {(file.commits ?? []).map((sha) => (
          <span key={sha} className="tr-file-sha" title={sha}>
            {sha}
          </span>
        ))}
      </span>
    </li>
  )

  const folderCollect = isFolder ? collectProgress : null
  const aiChunk = aiProgress && aiProgress.total > 1 ? aiProgress : null
  const progressPct =
    busy && folderCollect && folderCollect.total > 0
      ? Math.round((folderCollect.done / folderCollect.total) * 100)
      : aiBusy && aiChunk
        ? Math.round((aiChunk.done / aiChunk.total) * 100)
        : null
  const showProgress = (busy || aiBusy) && (!report || busy)

  const renderGroup = (g: typeof fileGroups[number]) => {
    if (g.kind === "commit") {
      const { commit, files } = g
      return (
        <details key={commit.sha} className="tr-group" open={expandAll}>
          <summary className="tr-group-head">
            <ChevronRight size={12} className="tr-group-caret" aria-hidden />
            <span className="tr-group-name tr-commit-subject" title={commit.subject}>
              {commit.subject}
            </span>
            <code className="tr-file-sha">{commit.shortSha}</code>
            <span className="tr-group-stat">
              {files.length} · {commit.author} · {commit.date.slice(0, 10)}
            </span>
          </summary>
          {commit.body.trim() && <p className="tr-commit-body">{commit.body.trim()}</p>}
          <ul className="tr-files">{files.map(renderFile)}</ul>
        </details>
      )
    }
    const { group, files } = g
    return (
      <details key={group.name} className="tr-group" open={expandAll}>
        <summary className="tr-group-head">
          <ChevronRight size={12} className="tr-group-caret" aria-hidden />
          <span className="tr-group-name">{group.name}</span>
          <span className="tr-group-stat">
            {group.files} · +{group.adds} -{group.dels}
          </span>
          {group.risks.map((risk) => (
            <span key={risk} className={`tr-risk tr-risk-${risk}`}>
              {t(`testReport.risk.${risk}`, { defaultValue: risk })}
            </span>
          ))}
        </summary>
        <ul className="tr-files">{files.map(renderFile)}</ul>
      </details>
    )
  }

  return (
    <section className="tr-panel">
      {showProgress && (
        <div className="tr-progress" aria-hidden>
          <div
            className={`tr-progress-bar${progressPct == null ? " is-indeterminate" : ""}`}
            style={progressPct == null ? undefined : { width: `${progressPct}%` }}
          />
        </div>
      )}
      <header className="tr-panel-head">
        <span className="tr-panel-head-icon" aria-hidden>
          <ClipboardList size={15} />
        </span>
        <h3 className="tr-panel-title">
          {isFolder ? `${t("testReport.folderTitle")} · ${displayName}` : `${t("testReport.title")} · ${displayName}`}
        </h3>
        <span className="tr-actions-spacer" />
        <button type="button" className="btn btn-secondary btn-small" onClick={() => void copy()} disabled={!report}>
          <Copy size={13} /> {t("testReport.copy")}
        </button>
        <button type="button" className="btn btn-secondary btn-small" onClick={() => void exportMd()} disabled={!report}>
          <Download size={13} /> {t("testReport.export")}
        </button>
      </header>

      <div className="tr-toolbar">
        <select
          className="input-field tr-mode"
          value={mode}
          onChange={(e) => {
            setMode(e.target.value as Mode)
            resetReport()
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter") void generate()
          }}
          disabled={busy || aiBusy}
          aria-label={t("testReport.mode")}
        >
          <option value="uncommitted">{t("testReport.modeUncommitted")}</option>
          <option value="base">{t("testReport.modeBase")}</option>
          <option value="commits">{t("testReport.modeCommits")}</option>
          <option value="since">{t("testReport.modeSince")}</option>
        </select>
        {mode === "base" && (
          <input
            className="input-field tr-base"
            value={base}
            placeholder={t("testReport.basePlaceholder")}
            onChange={(e) => {
              setBase(e.target.value)
              resetReport()
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void generate()
            }}
            disabled={busy || aiBusy}
          />
        )}
        {mode === "commits" && (
          <input
            className="input-field tr-count"
            type="number"
            min={1}
            max={50}
            value={commits}
            onChange={(e) => {
              setCommits(Math.max(1, Math.min(50, Number(e.target.value) || 1)))
              resetReport()
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter") void generate()
            }}
            disabled={busy || aiBusy}
          />
        )}
        {mode === "since" && (
          <>
            <input
              className="input-field tr-date"
              type="date"
              value={since}
              max={until || undefined}
              onChange={(e) => {
                setSince(e.target.value)
                resetReport()
              }}
              disabled={busy || aiBusy}
              aria-label={t("testReport.dateFrom")}
            />
            <span className="tr-date-sep" aria-hidden>
              ~
            </span>
            <input
              className="input-field tr-date"
              type="date"
              value={until}
              min={since || undefined}
              onChange={(e) => {
                setUntil(e.target.value)
                resetReport()
              }}
              disabled={busy || aiBusy}
              aria-label={t("testReport.dateTo")}
            />
          </>
        )}
        {/* The readout and the primary action travel as one group holding the
            right edge — on a narrow window they wrap together instead of
            scattering to the left. */}
        <span className="tr-actions">
          {mode === "since" && !since && !until ? (
            <span className="tr-hint">{t("testReport.dateRequired")}</span>
          ) : null}
          {report && (
            <label className="tr-toggle">
              <input type="checkbox" checked={onlyUntested} onChange={(e) => setOnlyUntested(e.target.checked)} />
              {t("testReport.onlyUntested")}
            </label>
          )}
          <button
            type="button"
            className="btn btn-primary btn-small"
            onClick={() => void generate()}
            disabled={busy || aiBusy || (mode === "since" && !since && !until)}
          >
            {busy ? <Loader2 size={12} className="spin" /> : null}
            {report ? t("testReport.regenerate") : t("testReport.generate")}
          </button>
        </span>
      </div>

      {report && (
        <div className="tr-tabs" role="tablist">
          <button
            type="button"
            role="tab"
            className={`tr-tab${activeTab === "overview" ? " is-active" : ""}`}
            aria-selected={activeTab === "overview"}
            onClick={() => setActiveTab("overview")}
          >
            {t("testReport.tabs.overview", { defaultValue: "概览" })}
          </button>
          <button
            type="button"
            role="tab"
            className={`tr-tab${activeTab === "files" ? " is-active" : ""}`}
            aria-selected={activeTab === "files"}
            onClick={() => setActiveTab("files")}
          >
            {t("testReport.tabs.files", { defaultValue: "改动文件" })}
          </button>
          <button
            type="button"
            role="tab"
            className={`tr-tab${activeTab === "commits" ? " is-active" : ""}`}
            aria-selected={activeTab === "commits"}
            onClick={() => setActiveTab("commits")}
          >
            {t("testReport.tabs.commits", { defaultValue: "提交" })}
          </button>
          <button
            type="button"
            role="tab"
            className={`tr-tab${activeTab === "api" ? " is-active" : ""}`}
            aria-selected={activeTab === "api"}
            onClick={() => setActiveTab("api")}
          >
            {t("testReport.tabs.api", { defaultValue: "API变更" })}
          </button>
          <button
            type="button"
            role="tab"
            className={`tr-tab${activeTab === "ai" ? " is-active" : ""}`}
            aria-selected={activeTab === "ai"}
            onClick={() => setActiveTab("ai")}
          >
            {t("testReport.tabs.ai", { defaultValue: "AI建议" })}
          </button>
        </div>
      )}

      {isFolder && <p className="tr-hint tr-baseline-note">{t("testReport.baselineNote")}</p>}
      {error && <div className="repos-modal-error">{error}</div>}
      {toast && <div className={`toast toast-${toast.type}`}><span className="toast-text">{toast.text}</span></div>}

      {!report && !busy && <p className="tr-hint">{t(isFolder ? "testReport.folderIntro" : "testReport.intro")}</p>}
      {busy && !report && (
        <div className="tr-loading">
          <Loader2 size={18} className="spin" />
          <span>
            {isFolder && collectProgress
              ? t("testReport.collectProgress", { done: collectProgress.done, total: collectProgress.total })
              : t("testReport.collecting")}
          </span>
        </div>
      )}

      {report && (
        <div className={`tr-body${busy ? " is-refreshing" : ""}`} key={viewingId}>
          {activeTab === "overview" && (
            <>
              {rows && rows.length > 0 && (
                <div className="tr-repos">
                  <span className="tr-label">{t("testReport.repoSummary")}</span>
                  <ul>
                    {rows.map((row) => (
                      <li key={row.name} className={`tr-repo${row.error ? " is-error" : ""}`}>
                        <span className="tr-repo-name" title={row.name}>
                          {row.name}
                        </span>
                        {row.error ? (
                          <span className="tr-repo-error">{t("testReport.repoError", { error: row.error })}</span>
                        ) : (
                          <span className="tr-repo-stat">
                            {row.files} {t("testReport.files")} · +{row.adds} -{row.dels}
                            {row.untested > 0 && (
                              <span className="tr-repo-warn">
                                {" "}
                                · {row.untested} {t("testReport.untested")}
                              </span>
                            )}
                          </span>
                        )}
                      </li>
                    ))}
                  </ul>
                </div>
              )}

              <div className="tr-risk-banner">
                {riskSummary.length === 0 ? (
                  <span className="tr-hint">{t("testReport.riskBanner.none", { defaultValue: "暂未识别高风险改动" })}</span>
                ) : (
                  <>
                    <span className="tr-label">{t("testReport.riskBanner.title", { defaultValue: "高风险改动" })}</span>
                    {riskSummary.map(({ type, count }) => (
                      <button
                        key={type}
                        type="button"
                        className={`tr-risk-chip tr-risk-${type}`}
                        onClick={() => {
                          setFileFilter((_) => ({ ...EMPTY_FILTER, risks: [type] }))
                          setActiveTab("files")
                        }}
                      >
                        {t("testReport.riskBanner.count", {
                          count,
                          label: t(`testReport.risk.${type}`, { defaultValue: type }),
                        })}
                      </button>
                    ))}
                  </>
                )}
              </div>

              <div className="tr-section-card tr-stats">
                <span className="tr-stat">
                  <span className="tr-stat-value">{report.stats.files}</span> {t("testReport.files")}
                </span>
                <span className="tr-stat-sep" aria-hidden />
                <span className="tr-stat is-add">
                  <span className="tr-stat-value">+{report.stats.adds}</span>
                </span>
                <span className="tr-stat is-del">
                  <span className="tr-stat-value">-{report.stats.dels}</span>
                </span>
                <span className="tr-stat-sep" aria-hidden />
                <span className="tr-stat">
                  <span className="tr-stat-value">{report.stats.modules}</span> {t("testReport.modules")}
                </span>
                {report.stats.untested > 0 && (
                  <>
                    <span className="tr-stat-sep" aria-hidden />
                    {/* "Which changes have no test?" is the question a tester opens
                        the overview for, so the number is the shortcut to the list
                        rather than a read-only figure. */}
                    <button
                      type="button"
                      className="tr-stat is-warn is-action"
                      onClick={() => {
                        setOnlyUntested(true)
                        setFileFilter(EMPTY_FILTER)
                        setExpandAll(true)
                        setActiveTab("files")
                      }}
                      title={t("testReport.untestedJump", { defaultValue: "只看没有配对测试的改动" })}
                    >
                      <span className="tr-stat-value">{report.stats.untested}</span> {t("testReport.untested")}
                    </button>
                  </>
                )}
                {report.stats.testFiles > 0 && (
                  <span className="tr-stat is-muted">
                    {report.stats.testFiles} {t("testReport.testFiles")}
                  </span>
                )}
                {report.stats.ignored > 0 && (
                  <span className="tr-stat is-muted" title={t("testReport.ignoredHint")}>
                    {t("testReport.ignored")} {report.stats.ignored}
                  </span>
                )}
                {view?.branch && (
                  <span className="tr-stat is-muted">
                    {view.branch}
                    {view.head ? ` @ ${view.head}` : ""}
                  </span>
                )}
              </div>

              {/* Zero changes is usually a wrong-baseline symptom, not a result:
                  it reads as a state with a next step, not a stray grey line. */}
              {report.stats.files === 0 && (
                <div className="tr-blank">
                  <span className="tr-blank-icon" aria-hidden>
                    <ClipboardList size={18} />
                  </span>
                  <span className="tr-blank-title">{t("testReport.empty")}</span>
                  <span className="tr-hint">
                    {t("testReport.emptyHint", { defaultValue: "换个改动基准，或确认改动是否已经提交。" })}
                  </span>
                </div>
              )}

              {report.scope.length > 0 && (
                <div className="tr-section-card tr-scope">
                  <span className="tr-label">
                    {t("testReport.scopeTitle")}
                    <span className="tr-count-badge">{report.scope.length}</span>
                  </span>
                  {report.scope.map((s) => (
                    <span key={s} className={`tr-scope-chip tr-scope-${s}`}>
                      {t(`testReport.scope.${s}`, { defaultValue: s })}
                    </span>
                  ))}
                </div>
              )}

              {report.truncated && <p className="tr-hint">{t("testReport.truncated")}</p>}

              {report.apiChanges.length > 0 && (
                <div className="tr-section-card tr-api">
                  <span className="tr-label">{t("testReport.apiChanges")}</span>
                  <ul>
                    {report.apiChanges.slice(0, 3).map((c) => (
                      <li key={`ov-${c.kind}-${c.name}-${c.path}`}>
                        <button
                          type="button"
                          className="tr-api-row"
                          onClick={() => jumpToFile(c.path)}
                          title={t("testReport.apiJump", { defaultValue: "在改动文件中查看该文件" })}
                        >
                          <span className={`tr-api-kind tr-api-${c.kind}`}>
                            {t(`testReport.api.${c.kind}`, { defaultValue: c.kind })}
                          </span>
                          <code>{c.name}</code>
                          <span className="tr-api-path">{c.path}</span>
                        </button>
                      </li>
                    ))}
                  </ul>
                  {report.apiChanges.length > 0 && (
                    <button type="button" className="tr-view-all" onClick={() => setActiveTab("api")}>
                      {t("testReport.overview.viewAll", { defaultValue: "查看全部 →" })}
                    </button>
                  )}
                </div>
              )}
            </>
          )}

          {activeTab === "files" && (
            <>
              <div className="tr-subtoolbar">
                <span className="tr-subtoolbar-field">
                  <Search size={13} aria-hidden />
                  <input
                    className="input-field"
                    value={fileFilter.search}
                    onChange={(e) => setFileFilter((f) => ({ ...f, search: e.target.value }))}
                    placeholder={t("testReport.subtoolbar.searchPlaceholder", { defaultValue: "搜索文件路径…" })}
                  />
                </span>
                <MultiSelectDropdown
                  label={t("testReport.subtoolbar.layer", { defaultValue: "层级" })}
                  options={options.layers.map((layer) => ({
                    value: layer,
                    label: t(`testReport.layer.${layer}`, { defaultValue: layer }),
                  }))}
                  selected={fileFilter.layers}
                  onChange={(layers) => setFileFilter((f) => ({ ...f, layers }))}
                />
                <MultiSelectDropdown
                  label={t("testReport.subtoolbar.risk", { defaultValue: "风险" })}
                  options={options.risks.map((risk) => ({
                    value: risk,
                    label: t(`testReport.risk.${risk}`, { defaultValue: risk }),
                  }))}
                  selected={fileFilter.risks}
                  onChange={(risks) => setFileFilter((f) => ({ ...f, risks }))}
                />
                <button type="button" className="btn btn-secondary btn-small" onClick={() => setExpandAll((e) => !e)}>
                  {expandAll
                    ? t("testReport.subtoolbar.collapseAll", { defaultValue: "全部折叠" })
                    : t("testReport.subtoolbar.expandAll", { defaultValue: "全部展开" })}
                </button>
                <button type="button" className="btn btn-secondary btn-small" onClick={() => setFileFilter(EMPTY_FILTER)}>
                  {t("testReport.subtoolbar.reset", { defaultValue: "重置" })}
                </button>
                <span className="tr-subtoolbar-count">
                  {t("testReport.subtoolbar.showing", {
                    defaultValue: "已筛选 {{n}} / 共 {{total}} 个文件",
                    n: fileMatches.length,
                    total,
                  })}
                </span>
              </div>

              {commitDetails.length > 0 && report.stats.files > 0 && (
                <div className="tr-groupbar" role="group" aria-label={t("testReport.groupBy")}>
                  <button
                    type="button"
                    className={`btn btn-small ${groupMode === "module" ? "btn-primary" : "btn-secondary"}`}
                    onClick={() => setGroupMode("module")}
                  >
                    {t("testReport.groupModule")}
                  </button>
                  <button
                    type="button"
                    className={`btn btn-small ${groupMode === "commit" ? "btn-primary" : "btn-secondary"}`}
                    onClick={() => setGroupMode("commit")}
                  >
                    <GitCommitHorizontal size={11} />
                    {t("testReport.groupCommit")}
                  </button>
                  <span className="tr-groupbar-hint">{t("testReport.groupCommitHint")}</span>
                </div>
              )}

              {fileMatches.length === 0 ? (
                <p className="tr-hint">{t("testReport.subtoolbar.empty", { defaultValue: "无匹配文件" })}</p>
              ) : (
                <>
                  {truncated && (
                    <p className="tr-hint">
                      {t("testReport.subtoolbar.truncated", {
                        defaultValue: "显示前 150 / {{n}} 个文件 · 使用搜索缩小范围",
                        n: fileMatches.length,
                      })}
                    </p>
                  )}
                  {fileGroups.map(renderGroup)}
                  {groupMode === "commit" &&
                    ungroupedCommits.map((commit) => (
                      <div key={commit.sha} className="tr-commit-empty">
                        <code className="tr-file-sha">{commit.shortSha}</code>
                        <span title={commit.subject}>{commit.subject}</span>
                        <span className="tr-groupbar-hint">{t("testReport.commitNoFiles")}</span>
                      </div>
                    ))}
                </>
              )}
            </>
          )}

          {activeTab === "commits" && (
            <div className="tr-section-card tr-commits">
              <span className="tr-label">
                {t("testReport.commits")}
                <span className="tr-count-badge">{commitDetails.length || report.commits.length}</span>
              </span>
              <ul>
                {commitDetails.length > 0
                  ? commitDetails.slice(0, 20).map((c) => (
                      <li key={c.sha} className="tr-commit-line">
                        <code className="tr-file-sha">{c.shortSha}</code>
                        <span title={c.body.trim() || undefined}>{c.subject}</span>
                        <span className="tr-commit-meta">
                          {c.author} · {c.date.slice(0, 10)}
                        </span>
                      </li>
                    ))
                  : report.commits.slice(0, 20).map((c, index) => <li key={`${c}-${index}`}>{c}</li>)}
              </ul>
            </div>
          )}

          {activeTab === "api" && (
            <div className="tr-section-card tr-api">
              <span className="tr-label">
                {t("testReport.apiChanges")}
                <span className="tr-count-badge">{report.apiChanges.length}</span>
              </span>
              <ul>
                {report.apiChanges.slice(0, 40).map((c) => (
                  <li key={`${c.kind}-${c.name}-${c.path}`}>
                    <button
                      type="button"
                      className="tr-api-row"
                      onClick={() => jumpToFile(c.path)}
                      title={t("testReport.apiJump", { defaultValue: "在改动文件中查看该文件" })}
                    >
                      <span className={`tr-api-kind tr-api-${c.kind}`}>
                        {t(`testReport.api.${c.kind}`, { defaultValue: c.kind })}
                      </span>
                      <code>{c.name}</code>
                      <span className="tr-api-path">{c.path}</span>
                    </button>
                  </li>
                ))}
              </ul>
            </div>
          )}

          {activeTab === "ai" && (
            <>
              {/* The only non-code input: with it the model answers "was the ask
                  covered", without it the panel behaves exactly as before. */}
              <div className="tr-ai-prompt">
                <button type="button" className="tr-ai-prompt-toggle" onClick={() => setRequirementOpen((o) => !o)}>
                  <span className="tr-ai-prompt-name">
                    {t("testReport.ai.requirement", { defaultValue: "需求与验收标准" })}
                    {requirement.trim() ? <span className="tr-ai-prompt-filled" aria-hidden /> : null}
                  </span>
                  <ChevronDown size={12} className={requirementOpen ? "is-open" : ""} />
                </button>
                {requirementOpen && (
                  <div className="tr-ai-prompt-body">
                    <textarea
                      className="input-field tr-requirement"
                      value={requirement}
                      onChange={(e) => setRequirement(e.target.value)}
                      placeholder={t("testReport.ai.requirementPlaceholder", {
                        defaultValue: "粘贴需求描述、验收标准或 PR 说明（可选）…",
                      })}
                      rows={6}
                    />
                    <p className="tr-hint">
                      {t("testReport.ai.requirementHint", {
                        defaultValue: "本机保存；仅在生成时随请求发送给你配置的模型。填写后会追加「需求覆盖对照」章节。",
                      })}
                    </p>
                  </div>
                )}
              </div>

              <div className="tr-ai-prompt">
                <button type="button" className="tr-ai-prompt-toggle" onClick={() => setPromptOpen((o) => !o)}>
                  {t("testReport.ai.systemPrompt", { defaultValue: "自定义提示词" })}
                  <ChevronDown size={12} className={promptOpen ? "is-open" : ""} />
                </button>
                {promptOpen && (
                  <div className="tr-ai-prompt-body">
                    <textarea
                      className="input-field"
                      value={customSystem}
                      onChange={(e) => setCustomSystem(e.target.value)}
                      placeholder={t("testReport.ai.systemPlaceholder", {
                        defaultValue: "可选：覆盖默认 system 提示词…",
                      })}
                      rows={4}
                    />
                    <p className="tr-hint">
                      {t("testReport.ai.systemHint", {
                        defaultValue: "留空则使用内置提示词；该内容仅保存在本机。",
                      })}
                    </p>
                  </div>
                )}
              </div>

              <div className="tr-ai">
                <div className="tr-ai-head">
                  <span className="tr-label">
                    <Sparkles size={12} />
                    {t("testReport.aiSection")}
                  </span>
                  <select
                    className="input-field tr-model"
                    value={selectedModel?.id ?? ""}
                    onChange={(e) => setSelectedModelId(e.target.value)}
                    disabled={aiBusy}
                    aria-label={t("testReport.aiModel")}
                  >
                    {aiModels.length === 0 && <option value="">{t("testReport.aiNeedModel")}</option>}
                    {aiModels.map((m) => (
                      <option key={m.id} value={m.id}>
                        {m.name}
                      </option>
                    ))}
                  </select>
                  {aiBusy && (aiProgress?.total ?? 1) > 1 && (
                    <span className="tr-ai-progress">
                      {t("testReport.aiProgress", { done: aiProgress?.done ?? 0, total: aiProgress?.total ?? 1 })}
                    </span>
                  )}
                  {aiBusy && (
                    <button type="button" className="btn btn-secondary btn-small" onClick={() => void cancelAiRun()}>
                      {t("testReport.aiCancel")}
                    </button>
                  )}
                  <button
                    type="button"
                    className="btn btn-secondary btn-small"
                    onClick={() => void runAi()}
                    disabled={aiBusy || !viewingId || !selectedModel || report.stats.files === 0}
                  >
                    {aiBusy ? <Loader2 size={12} className="spin" /> : <Sparkles size={12} />}
                    {ai ? t("testReport.aiRegenerate") : t("testReport.aiGenerate")}
                  </button>
                </div>
                {!ai && !aiBusy && <p className="tr-hint">{selectedModel ? t("testReport.aiHint") : t("testReport.aiNeedModel")}</p>}
                {ai && !aiBusy && aiRequirement !== requirement.trim() && (
                  <p className="tr-hint tr-ai-stale">
                    {t("testReport.ai.requirementStale", {
                      defaultValue: "需求已改动，重新生成才能更新「需求覆盖对照」。",
                    })}
                  </p>
                )}
                {aiSections.length > 0 && renderAiSections(ai)}
                {ai && !aiBusy && aiSections.length === 0 && (
                  <div className="tr-ai-raw">
                    {ai.split("\n").map((line, i) => (
                      <p key={i}>{line || " "}</p>
                    ))}
                  </div>
                )}
                {ai && aiWarnings.length > 0 && (
                  <p className="tr-ai-warn">
                    <ShieldAlert size={12} />
                    {t("testReport.aiWarn", { list: aiWarnings.join("、") })}
                  </p>
                )}
              </div>

              {/* Past answers. Read-only by construction: it renders saved markdown
                  and never writes back into the live `ai` state, so the current
                  report stays the single source of truth for the panel. */}
              <div className="tr-ai-prompt tr-history">
                <button type="button" className="tr-ai-prompt-toggle" onClick={() => setHistoryOpen((open) => !open)}>
                  <span className="tr-ai-prompt-name">
                    <History size={12} />
                    {t("testReport.history.title", { defaultValue: "历史记录" })}
                    {history.length > 0 ? <span className="tr-count-badge">{history.length}</span> : null}
                  </span>
                  <ChevronDown size={12} className={historyOpen ? "is-open" : ""} />
                </button>
                {historyOpen && (
                  <div className="tr-ai-prompt-body">
                    {history.length === 0 ? (
                      <p className="tr-hint">
                        {t("testReport.history.empty", { defaultValue: "还没有生成记录；生成成功后会留在这里。" })}
                      </p>
                    ) : (
                      <ul className="tr-history-list">
                        {history.map((item) => {
                          const expanded = historyExpandedId === item.id
                          return (
                            <li key={item.id} className={`tr-history-item${expanded ? " is-open" : ""}`}>
                              <div className="tr-history-head">
                                <button
                                  type="button"
                                  className="tr-history-summary"
                                  onClick={() => setHistoryExpandedId(expanded ? null : item.id)}
                                  aria-expanded={expanded}
                                >
                                  <ChevronRight size={12} className="tr-history-caret" aria-hidden />
                                  <span
                                    className="tr-history-when"
                                    title={new Date(item.createdAt).toLocaleString()}
                                  >
                                    {formatHistoryTime(item.createdAt)}
                                  </span>
                                  <span className="tr-history-target">
                                    {item.targetKind === "folder"
                                      ? t("testReport.history.kindFolder", { defaultValue: "文件夹" })
                                      : t("testReport.history.kindRepo", { defaultValue: "仓库" })}
                                    {item.targetLabel ? ` · ${item.targetLabel}` : ""}
                                  </span>
                                  {item.baseline ? <code className="tr-history-baseline">{item.baseline}</code> : null}
                                  <span className="tr-history-model">{item.model}</span>
                                  {item.requirement ? (
                                    <span className="tr-history-req" title={item.requirement}>
                                      {t("testReport.history.withRequirement", { defaultValue: "含需求" })}
                                    </span>
                                  ) : null}
                                </button>
                                <button
                                  type="button"
                                  className="tr-history-action"
                                  onClick={() => void copySection(item.markdown)}
                                  title={t("testReport.history.copy", { defaultValue: "复制内容" })}
                                >
                                  <Copy size={12} />
                                </button>
                                <button
                                  type="button"
                                  className="tr-history-action is-danger"
                                  onClick={() => void removeReportHistory(item.id)}
                                  title={t("testReport.history.delete", { defaultValue: "删除该记录" })}
                                >
                                  <Trash2 size={12} />
                                </button>
                              </div>
                              {expanded && <div className="tr-history-body">{renderAiSections(item.markdown)}</div>}
                            </li>
                          )
                        })}
                      </ul>
                    )}
                  </div>
                )}
              </div>
            </>
          )}
        </div>
      )}
    </section>
  )
}
