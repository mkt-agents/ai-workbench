/**
 * Build + push WeCom channel config to the Rust long-connection singleton.
 * Settings UI and app boot both call `applyWecomChannel`.
 */
import { invoke } from "@tauri-apps/api/core";
import type {
  AIModelConfig,
  AppSettings,
  DshMirrorReport,
  DshModelCatalog,
  McpServer,
  WecomSkywalkingEnv,
  WecomSkywalkingProfile,
  WecomSkywalkingProfiles,
} from "../core/types";
import { resolveFallbackModel, resolveSceneModel } from "./aiSceneModels";

export type { WecomSkywalkingEnv, WecomSkywalkingProfile, WecomSkywalkingProfiles };

/** 非法 / 空值回落到生产，避免误打测试 OAP。 */
export function normalizeWecomSkywalkingEnv(raw: unknown): WecomSkywalkingEnv {
  return raw === "test" ? "test" : "prod";
}

export function flatFromSkywalkingProfile(profile: WecomSkywalkingProfile): Pick<
  AppSettings,
  "wecomSkywalkingAddr" | "wecomSkywalkingHeader" | "wecomSkywalkingTimezoneOffsetMinutes"
> {
  return {
    wecomSkywalkingAddr: profile.addr?.trim() || undefined,
    wecomSkywalkingHeader: profile.header?.trim() || undefined,
    wecomSkywalkingTimezoneOffsetMinutes: profile.timezoneOffsetMinutes,
  };
}

function profileFromFlatSettings(settings: AppSettings): WecomSkywalkingProfile {
  return {
    addr: settings.wecomSkywalkingAddr,
    header: settings.wecomSkywalkingHeader,
    timezoneOffsetMinutes: settings.wecomSkywalkingTimezoneOffsetMinutes,
  };
}

function profilesHaveAny(profiles: WecomSkywalkingProfiles | undefined): boolean {
  if (!profiles) return false;
  return Boolean(profiles.test || profiles.prod);
}

export type ResolvedWecomSkywalking = {
  env: WecomSkywalkingEnv;
  profile: WecomSkywalkingProfile;
  /** 旧扁平配置尚未写入 profiles 时为 true */
  needsMigrate: boolean;
  /** needsMigrate 时一次性写回的补丁 */
  migrated: Partial<AppSettings>;
};

/**
 * 解析当前 SkyWalking 环境与档案。
 * 若只有旧扁平字段、尚无 profiles，则归入 prod 并给出迁移补丁（调用方写回一次即可）。
 */
export function resolveWecomSkywalking(settings: AppSettings): ResolvedWecomSkywalking {
  if (!profilesHaveAny(settings.wecomSkywalkingProfiles)) {
    const flat = profileFromFlatSettings(settings);
    const hasFlat = Boolean(
      flat.addr?.trim() ||
        flat.header?.trim() ||
        flat.timezoneOffsetMinutes !== undefined,
    );
    if (hasFlat) {
      const migrated: Partial<AppSettings> = {
        wecomSkywalkingEnv: "prod",
        wecomSkywalkingProfiles: { prod: flat },
        ...flatFromSkywalkingProfile(flat),
      };
      return { env: "prod", profile: flat, needsMigrate: true, migrated };
    }
    const env = normalizeWecomSkywalkingEnv(settings.wecomSkywalkingEnv);
    return { env, profile: {}, needsMigrate: false, migrated: {} };
  }

  const env = normalizeWecomSkywalkingEnv(settings.wecomSkywalkingEnv);
  const profile = settings.wecomSkywalkingProfiles?.[env] ?? {};
  return { env, profile, needsMigrate: false, migrated: {} };
}

/**
 * 切换环境或改写某环境档案；返回应 `setSettings` 的补丁（含扁平镜像）。
 * `profilePatch` 与当前档案浅合并；切环境时不传 patch 则只换当前指针并镜像目标档案。
 */
export function buildWecomSkywalkingPatch(
  settings: AppSettings,
  next: { env?: WecomSkywalkingEnv; profilePatch?: WecomSkywalkingProfile },
): Partial<AppSettings> {
  const resolved = resolveWecomSkywalking(settings);
  let profiles: WecomSkywalkingProfiles = {
    ...(resolved.needsMigrate
      ? ((resolved.migrated.wecomSkywalkingProfiles as WecomSkywalkingProfiles | undefined) ?? {})
      : (settings.wecomSkywalkingProfiles ?? {})),
  };
  const env = next.env ?? resolved.env;
  if (next.profilePatch) {
    const merged: WecomSkywalkingProfile = {
      ...(profiles[env] ?? {}),
      ...next.profilePatch,
    };
    // 显式 undefined = 清除（跟随本机时区 / 清空地址或请求头）
    if ("addr" in next.profilePatch && !next.profilePatch.addr?.trim()) {
      delete merged.addr;
    }
    if ("header" in next.profilePatch && !next.profilePatch.header?.trim()) {
      delete merged.header;
    }
    if (
      "timezoneOffsetMinutes" in next.profilePatch &&
      next.profilePatch.timezoneOffsetMinutes === undefined
    ) {
      delete merged.timezoneOffsetMinutes;
    }
    profiles = { ...profiles, [env]: merged };
  }
  const profile = profiles[env] ?? {};
  return {
    wecomSkywalkingEnv: env,
    wecomSkywalkingProfiles: profiles,
    ...flatFromSkywalkingProfile(profile),
  };
}

export type WecomDshStatus = {
  /** stopped | starting | ready | error */
  state: string;
  detail: string;
  /** ACP 进程 initialize 自报的版本（桥接层自己的号） */
  version: string | null;
  /** 本机安装的 @deepseek-ai/dsh 版本；界面优先显示它 */
  cliVersion?: string | null;
  mirror: DshMirrorReport | null;
};

export type WecomInvestigationStatus = {
  status: string;
  requestId: string;
  /** 当前后端实例实际计数；旧实例缺字段保持未提供，不从状态或回复文案推测。 */
  requestCount?: number | null;
  /** 本轮成功响应复用次数（含 singleflight 等待后复用），不计实际 HTTP。 */
  cacheHits?: number | null;
  /** 采集日志 + trace spans；累计补查可含重复。 */
  evidenceCount?: number | null;
  /** 已展示日志条目数，不含 trace spans。 */
  shownCount?: number | null;
  /** 采集 WARN 数，不推测其中已展示多少。 */
  warnCount?: number | null;
  truncatedCount?: number | null;
  /** 后端报告的未完成分页标记数，不是精确剩余页总数。 */
  incompletePages?: number | null;
  /** 空数组表示后端未报告未解释决策；缺字段不是空数组。 */
  decisionGaps?: string[] | null;
  sourceReady?: boolean | null;
  detail: string;
  /** 本机核对的未执行原因（host 文本，非 OAP 原文）；缺字段表示后端实例未提供。 */
  clarification?: string | null;
};

export type WecomPerformancePhase = {
  phase: string;
  elapsedMs: number;
  calls: number;
  inputChars: number;
  outputChars: number;
  toolEvents: number;
  firstEventMs?: number | null;
};

export type WecomPerformanceSummary = {
  requestId: string;
  /** 后端独立计时；不由 phases 或父子阶段相加推算。 */
  totalMs?: number | null;
  queueMs?: number | null;
  /** 后端报告的含排队耗时；前端不自行相加，亦非送达时延。 */
  elapsedWithQueueMs?: number | null;
  phases: WecomPerformancePhase[];
  sessionAction: string;
  model: string;
  effort: string;
  /** Total failures, independent of the bounded validationFailures sample. */
  validationFailureCount?: number | null;
  validationFailures: string[];
};

export type WecomPerformanceStatus = {
  requestId: string;
  /** 同 request 的累计快照；旧后端可缺省。 */
  summary?: WecomPerformanceSummary | null;
  /** 最近完成的阶段；total 表示执行耗时，排队与消息尾注口径另计。 */
  phase: string;
  elapsedMs: number;
  inputChars: number;
  outputChars: number;
  toolEvents: number;
  /** 阶段开始至首事件；未观察到事件时后端可省略或返回 null。 */
  firstEventMs?: number | null;
};

export type WecomCrossQueryStatus = {
  requestId: string;
  engine: string;
  docHits: number;
  codeHits: number;
  logHits: number;
  summary: string;
  linked: boolean;
};

export type WecomStatus = {
  processId: number;
  buildLabel: string;
  lastInvestigation?: WecomInvestigationStatus | null;
  lastPerformance?: WecomPerformanceStatus | null;
  lastCrossQuery?: WecomCrossQueryStatus | null;
  state: string;
  detail: string;
  enabled: boolean;
  /** 当前实例实际持有全局回合许可的任务数；不是 HTTP 并发数。 */
  activeJobs: number;
  /** 当前实例等待同会话上一轮完成的消息数。 */
  waitingJobs: number;
  /** 当前后端生效的全局回合上限；不从前端设置推算。 */
  maxJobs: number;
  /** 当前有效共享任务数，不是正在运行的模型回合数。 */
  sharedTasks: number;
  /** tianji | dsh */
  engine: string;
  dsh: WecomDshStatus;
  /** 最近一次长连接错误（空串 = 无） */
  lastError?: string;
  /** 已连接时长（毫秒）；未连上为 null */
  connectedMs?: number | null;
};

export type WecomDiagnosticEvent = {
  schemaVersion: 1;
  timestamp: string;
  requestId: string;
  phase: string;
  status: string;
  code?: string | null;
  durationMs?: number | null;
  metadata: Record<string, unknown>;
};

export type WecomDiagnosticFilter = {
  requestId?: string;
  phase?: string;
  status?: string;
  from?: string;
  to?: string;
};

export type WecomDiagnosticPage = {
  events: WecomDiagnosticEvent[];
  nextCursor: string | null;
  partial: boolean;
  scannedBytes: number;
  readErrors: string[];
  /** Schema is independent of the event schema; render only recognized fields. */
  health: unknown;
};

/** Copy summaries deliberately omit free text, paths and arbitrary metadata values. */
export function wecomDiagnosticSummary(events: WecomDiagnosticEvent[]): string {
  const token = (value: string) => /^[a-zA-Z0-9_.:-]{1,160}$/.test(value) ? value : "[redacted]";
  return JSON.stringify({
    schemaVersion: 1,
    scope: "loaded-events-only",
    events: events.map((event) => ({
      timestamp: Number.isFinite(Date.parse(event.timestamp)) ? new Date(event.timestamp).toISOString() : null,
      requestId: token(event.requestId),
      phase: token(event.phase),
      status: token(event.status),
      code: event.code ? token(event.code) : null,
      durationMs: typeof event.durationMs === "number" && Number.isFinite(event.durationMs) && event.durationMs >= 0
        ? event.durationMs : null,
      // Do not copy metadata: even innocently named strings may contain user content.
    })),
  }, null, 2);
}

export function fetchWecomDiagnostics(
  filter: WecomDiagnosticFilter,
  cursor?: string,
  limit = 50,
): Promise<WecomDiagnosticPage> {
  return invoke<WecomDiagnosticPage>("wecom_diagnostics_query", { filter, cursor, limit });
}

export function fetchWecomDiagnosticRequest(requestId: string): Promise<WecomDiagnosticPage> {
  return invoke<WecomDiagnosticPage>("wecom_diagnostic_request", { requestId });
}

/** Returns a metadata-only JSON package; the caller must use the normal save dialog. */
export function exportWecomDiagnosticRequest(requestId: string): Promise<string> {
  return invoke<string>("wecom_diagnostic_export", { requestId });
}

export type WecomSessionLogEntry = {
  requestId?: string;
  session: string;
  ts: string;
  user: string;
  assistant: string;
};

export type WecomSessionLogPage = {
  entries: WecomSessionLogEntry[];
  partial: boolean;
  readErrors: string[];
  scannedBytes: number;
};

export function fetchWecomSessionLogPage(limit = 30, key?: string): Promise<WecomSessionLogPage> {
  return invoke<WecomSessionLogPage>("wecom_session_recent_page", { key, limit });
}

export type WecomPairing = {
  userid: string;
  code: string;
  createdMs: number;
};

type ApplyArgs = {
  settings: AppSettings;
  models: AIModelConfig[];
  mcpServers: McpServer[];
  /** When saving a new plaintext secret from the form */
  plainSecret?: string;
  /** When saving a new plaintext business-API secret (自建应用) from the form */
  plainCorpSecret?: string;
};

/** 保留任意合法整数（含旧档 3/6）；缺省或非法值回落 2，不主动抬高并发。 */
export function normalizeWecomMaxJobs(value: number | undefined): number {
  return typeof value === "number" && Number.isInteger(value) && value >= 1 && value <= 32
    ? value
    : 2;
}

/** 文档回溯天数：0=不限制，默认 7，上限 30。 */
export function normalizeDocgenLookbackDays(value: number | undefined): number {
  if (typeof value !== "number" || !Number.isFinite(value)) return 7;
  const n = Math.trunc(value);
  if (n < 0) return 7;
  return Math.min(30, n);
}

/** 子仓白名单：去重、trim；相对路径保留（后端再校验落在 workspace 下）。 */
export function normalizeDocgenRepoAllowlist(list: string[] | undefined): string[] {
  const seen = new Set<string>();
  const out: string[] = [];
  for (const raw of list ?? []) {
    const p = raw.trim();
    if (!p) continue;
    const key = p.replace(/\//g, "\\").toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(p);
  }
  return out;
}

/** 与后端 `PROMPT_CHAR_CAP` 一致。 */
export const DOCGEN_PROMPT_CHAR_CAP = 12_000;

/**
 * 内置更新 system 模板（与 Rust `default_docgen_system_prompt` 保持一致）。
 * 占位符：{doc_dir} {branch} {commits} {files} {diff}
 */
export const DEFAULT_DOCGEN_SYSTEM_PROMPT = `你是代码逆向文档维护助手。工作区是一个代码仓库，\`{doc_dir}/\` 下是基于代码的逆向工程文档。
当前在 release 分支「{branch}」。

## 提交摘要
{commits}

## 变更文件
{files}

## 变更 diff 摘录（定向线索；完整内容请用工具读文件）
{diff}

任务：阅读这些变更涉及的代码，更新 \`{doc_dir}/\` 下**对应**的逆向文档，使其与最新代码一致。
如何找对应文档（按顺序尝试）：
- 先在 \`{doc_dir}/\` 下 grep 变更文件名、所在模块名、关键函数/类名，命中的文档就是要更新的；
- 文件名/模块名找不到时，按业务功能关键词再搜一次；
- 仍找不到且该变更重要时，才在 \`{doc_dir}/\` 下新建一篇，文件名贴合模块。
硬规则：
1. **只允许修改/新建 \`{doc_dir}/\` 目录内的文件**；绝对不要改任何业务代码、配置或其它目录（越界操作会被拒绝，产物不会发布）。
2. 变更很小 / 与文档无关时可以不改，并在摘要里说明原因。
3. 不要运行命令、不要删除或移动文件，只用读文件 / 搜索 / 写文件 / 改文件。
4. 最后用 3-6 行中文总结「更新了哪些文档、对应哪些变更」，这段会推送到群里。`;

/**
 * 内置质检 system 模板（与 Rust `default_docgen_quality_prompt` 保持一致）。
 * 占位符：{doc_dir} {branch} {files} {summary}
 */
export const DEFAULT_DOCGEN_QUALITY_PROMPT = `你是逆向文档质量审查员（只读）。工作区分支「{branch}」，文档目录 \`{doc_dir}/\`。
上一轮更新摘要：
{summary}

本次代码变更文件：
{files}

任务：对照变更与 \`{doc_dir}/\` 现有文档，用 3–8 条中文检查结论指出：是否缺更、是否过时、是否矛盾。
硬规则：只读文件/搜索，禁止写文件、改文件、跑命令、删除。`;

/** trim + 字符上限；空串表示走后端默认。 */
export function normalizeDocgenPrompt(value: string | undefined): string {
  const t = (value ?? "").trim();
  if (!t) return "";
  return Array.from(t).slice(0, DOCGEN_PROMPT_CHAR_CAP).join("");
}

function splitCsv(list: string[] | undefined): string[] {
  return (list ?? []).map((s) => s.trim()).filter(Boolean);
}

export async function protectSecret(plain: string): Promise<string> {
  if (!plain.trim()) return "";
  if (plain.startsWith("enc:v1:")) return plain;
  return invoke<string>("wecom_protect_secret", { plain });
}

/** Decrypt for on-screen reveal only. Do not write the plaintext back into settings. */
export async function revealWecomSecret(stored: string): Promise<string> {
  return invoke<string>("wecom_reveal_secret", { stored });
}

export async function applyWecomChannel(args: ApplyArgs): Promise<WecomStatus> {
  const { settings, models, mcpServers } = args;
  let botSecret = (settings.wecomBotSecret ?? "").trim();
  // undefined = leave stored secret; empty string must not wipe it
  const plain = args.plainSecret?.trim() ?? "";
  if (plain) botSecret = await protectSecret(plain);
  let corpSecret = (settings.wecomCorpSecret ?? "").trim();
  const plainCorp = args.plainCorpSecret?.trim() ?? "";
  if (plainCorp) corpSecret = await protectSecret(plainCorp);

  // webhook 当密文存：已是 enc:v1: 直接用，明文则加密后下发。
  const sealWebhook = async (raw: string | undefined) => {
    let v = (raw ?? "").trim();
    if (v && !v.startsWith("enc:v1:")) v = await protectSecret(v);
    return v;
  };
  const webhookUrl = await sealWebhook(settings.wecomWebhookUrl);
  const webhookFeishu = await sealWebhook(settings.wecomWebhookFeishu);
  const webhookDingtalk = await sealWebhook(settings.wecomWebhookDingtalk);

  const model =
    resolveSceneModel("aiHub", models, settings.aiSceneModels) ??
    models.find((m) => m.isDefault) ??
    models[0];
  const fallback = resolveFallbackModel(model ?? undefined, models);

  const permission = (settings.wecomPermission ?? "read").trim() || "read";
  const repoPath =
    (settings.wecomRepoPath ?? "").trim() || (settings.currentGitRepo ?? "").trim();

  const mcp = mcpServers
    .filter((s) => s.enabled && s.name.trim() && s.command.trim())
    .map((s) => ({ name: s.name.trim(), command: s.command.trim(), args: s.args ?? "" }));

  const agent = model
    ? {
        config: model,
        fallbackConfig: fallback,
        repoPath,
        services: [] as string[],
        permission,
        // 「有没有工具」与「能做什么」是两件事：只读档也要读写文件/搜索，
        // 真正的边界由 permission 在闸门里决定（这里本来就是恒 true）。
        tools: true,
        mode: "",
        vision: Boolean(model.vision),
        diffConfirm: Boolean(settings.agentDiffConfirm),
        alwaysAllow: settings.agentAlwaysAllow ?? [],
        webAllow: settings.agentWebAllow ?? [],
        webTier: settings.agentWebTier || "allowlist",
        webSearchProvider: settings.agentWebSearchProvider || "",
        webSearchKey: settings.agentWebSearchKey || "",
        subagents: Boolean(settings.agentSubagents),
        parallelReads: settings.agentParallelReads !== false,
        retries: settings.agentRetries !== false,
        grepIgnore: settings.agentGrepIgnore !== false,
        planFirst: Boolean(settings.agentPlanFirst),
        contextTokens: settings.agentContextTokens ?? 0,
        resultCap: settings.agentResultCap ?? 0,
        anthropicStream: settings.agentAnthropicStream !== false,
        mcpEnabled: settings.agentMcpEnabled !== false,
        mcp,
        systemExtra: "",
      }
    : null;

  const skywalking = resolveWecomSkywalking(settings).profile;

  const status = await invoke<WecomStatus>("wecom_channel_apply", {
    req: {
      enabled: Boolean(settings.wecomEnabled),
      botId: (settings.wecomBotId ?? "").trim(),
      botSecret,
      corpId: (settings.wecomCorpId ?? "").trim(),
      corpSecret,
      botDisplayName: (settings.wecomBotDisplayName ?? "").trim(),
      selfIntro: Array.from((settings.wecomSelfIntro ?? "").trim()).slice(0, 2000).join(""),
      dmPolicy: settings.wecomDmPolicy ?? "pairing",
      groupPolicy: settings.wecomGroupPolicy ?? "allowlist",
      requireMention: settings.wecomRequireMention !== false,
      allowUsers: splitCsv(settings.wecomAllowUsers),
      allowGroups: splitCsv(settings.wecomAllowGroups),
      pairedUsers: splitCsv(settings.wecomPairedUsers),
      // 引擎与档位另带一份：DSH 档不需要天玑的模型快照，但要工作区、档位与模型选择。
      engine: settings.wecomEngine === "dsh" ? "dsh" : "tianji",
      permission,
      workspace: repoPath,
      dshModel: (settings.wecomDshModel ?? "").trim(),
      dshEffort: (settings.wecomDshEffort ?? "").trim(),
      // 文档问答 + 排障 + 自动更新（SkyWalking 取当前环境档案）
      answerMode: settings.wecomAnswerMode ?? "auto",
      docDir: (settings.wecomDocDir ?? "").trim(),
      enhanceDmEnabled: Boolean(settings.wecomEnhanceDmEnabled),
      enhanceGroupEnabled: Boolean(settings.wecomEnhanceGroupEnabled),
      enhanceDm: Array.from((settings.wecomEnhanceDm ?? "").trim()).slice(0, 2000).join(""),
      enhanceGroup: Array.from((settings.wecomEnhanceGroup ?? "").trim()).slice(0, 2000).join(""),
      skywalkingAddr: (skywalking.addr ?? "").trim(),
      skywalkingHeader: (skywalking.header ?? "").trim(),
      skywalkingTimezoneOffsetMinutes: skywalking.timezoneOffsetMinutes ?? null,
      autoUpdateEnabled: Boolean(settings.wecomAutoUpdateEnabled),
      autoUpdateTime: (settings.wecomAutoUpdateTime ?? "").trim(),
      releaseBranch: (settings.wecomReleaseBranch ?? "").trim(),
      webhookUrl,
      webhookFeishu,
      webhookDingtalk,
      docgenCron: (settings.wecomDocgenCron ?? "").trim(),
      // 文档更新引擎与回话引擎分开：`inherit` 让后端跟随上面的 engine 解析。
      docgenEngine:
        settings.wecomDocgenEngine === "dsh"
          ? "dsh"
          : settings.wecomDocgenEngine === "tianji"
            ? "tianji"
            : "inherit",
      // 文档更新（DSH 档）的模型/思考强度；空 = 后端沿用上面的 dshModel / dshEffort。
      docgenDshModel: (settings.wecomDocgenDshModel ?? "").trim(),
      docgenDshEffort: (settings.wecomDocgenDshEffort ?? "").trim(),
      // 文档自动更新自己的工作区 / 文档目录：留空才回落到上面「回答设置」的同名字段。
      docgenRepoPath: (settings.wecomDocgenRepoPath ?? "").trim(),
      docgenDocDir:
        (settings.wecomDocgenDocDir ?? "").trim() || (settings.wecomDocDir ?? "").trim(),
      docgenLookbackDays: normalizeDocgenLookbackDays(settings.wecomDocgenLookbackDays),
      docgenSyncSources: Boolean(settings.wecomDocgenSyncSources),
      docgenRepoAllowlist: normalizeDocgenRepoAllowlist(settings.wecomDocgenRepoAllowlist),
      docgenSystemPrompt: normalizeDocgenPrompt(settings.wecomDocgenSystemPrompt),
      docgenQualityPrompt: normalizeDocgenPrompt(settings.wecomDocgenQualityPrompt),
      docgenPreview: Boolean(settings.wecomDocgenPreview),
      docgenQualityCheck: Boolean(settings.wecomDocgenQualityCheck),
      docgenPollRelease: Boolean(settings.wecomDocgenPollRelease),
      autoCommit: settings.wecomAutoCommit !== false,
      autoPush: Boolean(settings.wecomAutoPush),
      maxJobs: normalizeWecomMaxJobs(settings.wecomMaxJobs),
      // DSH still needs MCP settings when no AI Hub model is configured.
      mcpEnabled: settings.agentMcpEnabled !== false,
      mcp,
      agent,
    },
  });
  // Ensure the desktop pet window is up so it can react to wecom- agent broadcasts.
  if (settings.wecomEnabled && settings.aiHubBubbleEnabled !== false) {
    const pos = settings.aiHubBubblePos;
    await invoke("set_ai_hub_bubble_visible", {
      visible: true,
      x: pos?.x ?? null,
      y: pos?.y ?? null,
    }).catch(() => {});
  }
  return status;
}

/**
 * 「逆向工程」页专用：只下发文档自动更新 / 群通知的配置。
 *
 * 与 `applyWecomChannel` 分家的理由：那是通道级 apply，会连带校验 bot 凭据、
 * 重启长连接；文档更新页每改一个字段就重启一次 IM 通道，既慢又会踢掉正在聊的会话。
 * 后端 `wecom_docgen_apply` 只写 `state.docgen`，两条路径收敛到同一份配置。
 */
export async function applyWecomDocgen(settings: AppSettings): Promise<void> {
  const seal = async (raw: string | undefined) => {
    let v = (raw ?? "").trim();
    if (v && !v.startsWith("enc:v1:")) v = await protectSecret(v);
    return v;
  };
  // 工作区留空 = 跟随「回答设置」：这里解析成具体路径再下发（与通道 apply 同一套回落）。
  const repoPath =
    (settings.wecomDocgenRepoPath ?? "").trim() ||
    (settings.wecomRepoPath ?? "").trim() ||
    (settings.currentGitRepo ?? "").trim();
  const dshModel = (settings.wecomDocgenDshModel ?? "").trim() || (settings.wecomDshModel ?? "").trim();
  const dshEffort =
    (settings.wecomDocgenDshEffort ?? "").trim() || (settings.wecomDshEffort ?? "").trim();
  // 文档目录同样留空跟随「回答设置」，两处各自一套。
  const docDir = (settings.wecomDocgenDocDir ?? "").trim() || (settings.wecomDocDir ?? "").trim();
  await invoke("wecom_docgen_apply", {
    req: {
      repoPath,
      docDir,
      releaseBranch: (settings.wecomReleaseBranch ?? "").trim(),
      autoUpdateEnabled: Boolean(settings.wecomAutoUpdateEnabled),
      autoUpdateTime: (settings.wecomAutoUpdateTime ?? "").trim(),
      cron: (settings.wecomDocgenCron ?? "").trim(),
      webhookUrl: await seal(settings.wecomWebhookUrl),
      webhookFeishu: await seal(settings.wecomWebhookFeishu),
      webhookDingtalk: await seal(settings.wecomWebhookDingtalk),
      preview: Boolean(settings.wecomDocgenPreview),
      qualityCheck: Boolean(settings.wecomDocgenQualityCheck),
      pollRelease: Boolean(settings.wecomDocgenPollRelease),
      syncSources: Boolean(settings.wecomDocgenSyncSources),
      lookbackDays: normalizeDocgenLookbackDays(settings.wecomDocgenLookbackDays),
      repoAllowlist: normalizeDocgenRepoAllowlist(settings.wecomDocgenRepoAllowlist),
      systemPrompt: normalizeDocgenPrompt(settings.wecomDocgenSystemPrompt),
      qualityPrompt: normalizeDocgenPrompt(settings.wecomDocgenQualityPrompt),
      engine:
        settings.wecomDocgenEngine === "dsh"
          ? "dsh"
          : settings.wecomDocgenEngine === "tianji"
            ? "tianji"
            : "inherit",
      dshModel,
      dshEffort,
      autoCommit: settings.wecomAutoCommit !== false,
      autoPush: Boolean(settings.wecomAutoPush),
    },
  });
}

export async function fetchWecomStatus(): Promise<WecomStatus> {
  return invoke<WecomStatus>("wecom_channel_status");
}

/** 读 DSH 引擎可用的模型目录（ACP configOptions）。`refresh` 会丢掉缓存重拉。
 * `scope = "docgen"` 时读文档自动更新那套引擎（通道可能还在天玑档）。 */
export async function fetchDshModels(
  refresh = false,
  scope?: "channel" | "docgen"
): Promise<DshModelCatalog> {
  return invoke<DshModelCatalog>("wecom_dsh_models", { refresh, scope: scope ?? null });
}

/** 重新把 DSH 的 web 配置镜像到 acp profile（引擎随后自动重启）。 */
export async function syncDshProfile(): Promise<DshMirrorReport> {
  return invoke<DshMirrorReport>("wecom_dsh_sync");
}

export async function fetchWecomPairings(): Promise<WecomPairing[]> {
  return invoke<WecomPairing[]>("wecom_pairing_list");
}

export async function approveWecomPairing(code: string): Promise<string> {
  return invoke<string>("wecom_pairing_approve", { code });
}

export async function revokeWecomUser(userid: string): Promise<void> {
  return invoke("wecom_pairing_revoke", { userid });
}

export async function restartWecomChannel(): Promise<WecomStatus> {
  return invoke<WecomStatus>("wecom_channel_restart");
}

/** 「立即更新一次」逆向文档：手动触发自动更新流程，返回人类可读的结果摘要。 */
export async function runWecomDocgenNow(): Promise<string> {
  return invoke<string>("wecom_docgen_run_now");
}

/** 连通自检：校验 release 分支 / webhook / SkyWalking，返回多行结果。 */
export async function testWecomDocgen(): Promise<string> {
  return invoke<string>("wecom_docgen_test");
}

/**
 * 重置采集点：不传 `repos` 就清全部仓。
 * 清完下次运行会从「回溯天数」窗口重新采集（已采集过的提交会再跑一遍）。
 */
export async function resetWecomDocgenCursor(repos?: string[]): Promise<string> {
  return invoke<string>("wecom_docgen_reset_cursor", {
    repos: repos && repos.length ? repos : null,
  });
}

export type WecomDocgenHistoryItem = {
  at: string;
  trigger: string;
  branch: string;
  status: string;
  changedFiles: number;
  updatedDocs: string;
  commitNote?: string;
  quality?: string;
  pushed: boolean;
  error?: string;
  preview: boolean;
  webhookErrors?: string[];
  /** 整轮耗时（毫秒）；老记录没有，0 = 未知。 */
  durationMs?: number;
};

/** 最近 30 条逆向文档更新历史（新 → 旧）。 */
export async function fetchWecomDocgenHistory(): Promise<WecomDocgenHistoryItem[]> {
  return invoke<WecomDocgenHistoryItem[]>("wecom_docgen_history");
}

/**
 * 最近一次文档更新进度快照 + 「现在是否真的在跑」。
 * 页面切走再回来组件会重新挂载，靠它把进度卡恢复出来。
 */
export type WecomDocgenProgressSnapshot = {
  progress: WecomDocgenProgress | null;
  running: boolean;
};
export async function fetchWecomDocgenProgress(): Promise<WecomDocgenProgressSnapshot> {
  return invoke<WecomDocgenProgressSnapshot>("wecom_docgen_progress");
}

export const WECOM_DOCGEN_PROGRESS_EVENT = "wecom-docgen-progress";

export type WecomDocgenProgress = {
  runId: string;
  trigger: string;
  phase: string;
  percent: number;
  status: "running" | "ok" | "skip" | "fail";
  detail?: string;
  /** 本轮开始时间（毫秒时间戳）：运行中拿它做实时计时；旧快照没有。 */
  startedAtMs?: number | null;
  /** 结束事件才有：整轮耗时（毫秒）。 */
  durationMs?: number | null;
};

/** 读通道排障日志最后 N 行（设置页「通道日志」折叠区）。 */
export async function fetchWecomLog(lines = 200): Promise<string> {
  return invoke<string>("wecom_log_tail", { lines });
}

/** 读最近若干条企微会话旁证（设置页「会话日志」折叠区）。 */
export async function fetchWecomSessionLog(
  limit = 30,
): Promise<WecomSessionLogEntry[]> {
  return invoke<WecomSessionLogEntry[]>("wecom_session_recent", { limit });
}


export type WecomQrStart = {
  scode: string;
  authUrl: string;
  qrPageUrl: string;
  qrSvg: string;
};

export type WecomQrPoll = {
  status: string;
  botId?: string | null;
  secret?: string | null;
  detail: string;
};

/** 申请企微扫码绑定会话（返回 scode + 授权 URL）。 */
export function startWecomQr(): Promise<WecomQrStart> {
  return invoke<WecomQrStart>("wecom_qr_start");
}

/** 轮询扫码结果；成功时带回 botId / secret（明文，调用方负责加密落盘）。 */
export function pollWecomQr(scode: string): Promise<WecomQrPoll> {
  return invoke<WecomQrPoll>("wecom_qr_poll", { scode });
}
