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
  WecomBotProfile,
  WecomSkywalkingEnv,
  WecomSkywalkingProfile,
  WecomSkywalkingProfiles,
} from "../core/types";
import { resolveFallbackModel, resolveSceneModel } from "./aiSceneModels";
import {
  getActiveWecomBot,
  listWecomBots,
  migrateWecomBotsSettings,
} from "./wecomBots";

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

export type WecomBotRuntimeStatus = {
  botId: string;
  displayName?: string;
  state: string;
  detail?: string;
  enabled: boolean;
  engine?: string;
  connectedMs?: number | null;
  lastError?: string;
  /** 这台机器人自己的 DSH 引擎状态（每台一个 ACP 进程） */
  dsh?: WecomDshStatus | null;
  /** 这台机器人自己的并发占用 / 排队 / 生效上限 / 共享任务数 */
  activeJobs?: number;
  waitingJobs?: number;
  maxJobs?: number;
  sharedTasks?: number;
}; // 与后端 WecomBotRuntimeStatus camelCase 对齐

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
  /** 多 bot：在线数 / 已启用数（后端 bots_online / bots_total） */
  botsOnline?: number;
  botsTotal?: number;
  bots?: WecomBotRuntimeStatus[];
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
  /** 发起请求的机器人 bot_id；空串 = 未知（按全通道处理） */
  botId?: string;
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
 * 占位符：{doc_dir} {branch} {commits} {files} {stat} {diff} {range} {index} {file_count} {mapping} {date} {updates_dir}
 * 有只读 git 工具时 {index} 只渲染「采集区间 + 本批文件」；没有时回落旧三节。
 * 业务摘要给人读；「## 代码引用」仅映射落库（群通知侧会剥离）。
 */
export const DEFAULT_DOCGEN_SYSTEM_PROMPT = `本次更新：逆向文档同步 · 分支 {branch} · {date} · 本批 {file_count} 个文件
（索引见下；完整 diff、提交语义与历史用只读 git 工具自查）

你是代码逆向文档维护助手。工作区是一个代码仓库，有两类文档产物：
- \`{doc_dir}/\`：按模块沉淀的逆向工程说明书（长期知识）
- \`{updates_dir}/{date}.md\`：按日沉淀的「今天改了哪些功能」（面向人读的变更日志）
当前在 release 分支「{branch}」，今日日期 \`{date}\`。

{index}{mapping}

工作流程（严格按顺序）：
第一步·规划：把「本批文件」逐个归类，先写出「文件 → 目标文档」清单（可用 update_plan 展示）：
- 按变更文件的路径层级推断归属模块（如 \`service/order/\`、\`controller/quote/\`），优先匹配 \`{doc_dir}/\` 下同名或同层级的文档与子目录；
- 用 grep / symbols 验证：拿变更文件名、所在目录名、新增/改动的类名与方法名去搜，命中的才算找到；
- 同一模块的多个文件合并到同一篇文档，不要一个文件一篇文档；
- 找不到对应文档且该变更重要（新增能力 / 行为变化）时才新建一篇，文件名贴合模块目录名（不要建在 updates/ 下）。
第二步·调查（写任何文档之前必须做，否则视为未完成）：
- 看本轮完整变更：天玑用 \`git_read\`（先 \`{"sub":"log","args":"-5 --oneline"}\` 找到范围，再 \`{"sub":"diff","args":"-p <旧提交>..HEAD -- <路径>"}\`）；Harness 用 \`git_log\` / \`git_diff\`（如 \`{"range":"<旧提交>..HEAD","path":"src/order"}\`）；必要时 \`show\` / \`blame\`；
- 索引只有采集区间与本批文件，可能不完整：提交语义、完整 diff、历史都要自己查，禁止只按文件名猜内容；
- 用 read_file / grep / symbols / lsp 读透相关源码：入口、调用链、校验与错误分支、边界条件；
- 只写读到的行为，禁止补未证实的架构或流程。
第三步·写作：
一、模块文档（\`{doc_dir}/\`）——写「行为」不写「动作」：
- 讲清本批变更改变了什么业务行为——新增/修改/废弃了什么能力、入口在哪、调用链怎么走、有哪些校验、边界条件和错误处理；禁止把 commit 标题或 diff 片段原样抄进文档。
- 已有文档：保持原结构，只改与代码不符的段落，在对应章节里就地更新；禁止重写全文、禁止堆砌项目背景和无关介绍。
- 新建文档：覆盖职责、关键入口与调用链、核心类型或表、边界与错误四块；不写空泛的「概述 / 总结」套话。
- 删除或重命名：同步改文档或标明已移除；纯格式/依赖噪音且无行为变化 → 模块文档跳过，日报记「无行为变更」。
二、当日更新（\`{updates_dir}/{date}.md\`）：
- 打开或新建，在文末追加一节（不要整文件覆盖）；节标题：\`## HH:MM · {branch}\`。
- 正文用条目列出本批功能点（用人话，如「订单结算」「文档中心门户」），并注明各点更新了哪篇文档；只写本批出现的变更，禁止编造未出现的模块。
第四步·收尾（必须输出；只写下面三个小节，不要写其它小节，也不要叙述工作过程）：
1) 「## 功能点」：3–8 条，每条一行，格式 \`- 功能名：做完了什么、影响谁\`。只列本轮确实完成的功能（用人话概括能力，不写内部实现）；禁止行号、函数/符号名、JSON 字段、commit hash；禁止描述工作过程（规划、读文件、自查、落盘）；禁止写批次号、文件数量与代码路径。
2) 「## 代码引用」每行一条且必须用箭头（**仅供系统映射落库，不要写进业务摘要正文**；群通知会自动省略该节）：\`源码路径:符号\` → \`文档相对路径\`
3) 「## 覆盖」每行一条，覆盖清单里的每个业务文件：\`源码路径\` → \`文档相对路径\`，或 \`源码路径\` → \`跳过：原因\`

硬规则：
1. **只允许修改/新建 \`{doc_dir}/\` 目录内的文件**（含 \`{updates_dir}/\`）；绝对不要改任何业务代码、配置或其它目录（越界操作会被拒绝，产物不会发布）。
2. 不要删除或移动文件；不要运行命令（只读 git 用 git_read / git_log 等工具），只用读文件 / 搜索 / 写文件 / 改文件。写当日更新前先读已有文件再追加。
3. 本批只改规划清单里的文档与当日 updates 追加节；不要通读全库文档。
4. 变更很小 / 与文档无关时可以不改模块文档，但须在当日更新文件记一笔或写明跳过原因。
5. 控制工具往返：无关文件跳过并在摘要说明；优先完成落盘。`;

/**
 * 内置质检 system 模板（与 Rust `default_docgen_quality_prompt` 保持一致）。
 * 占位符：{doc_dir} {branch} {files} {summary} {coverage} {date} {updates_dir}
 */
export const DEFAULT_DOCGEN_QUALITY_PROMPT = `你是逆向文档质量审查员（只读）。工作区分支「{branch}」，文档目录 \`{doc_dir}/\`，今日 \`{date}\`。
本轮更新摘要：
{summary}

本轮覆盖台账（模型自报；缺行不等于没做，但要点名核对）：
{coverage}

本次代码变更文件：
{files}

任务：只审**本轮更新**。优先根据摘要、台账与文件列表下结论；需要时最多抽查 3 个相关文档（含当日 \`{updates_dir}/{date}.md\` 相关节）核对源码，可用只读 git 工具（\`git_read\` 或 \`git_log\` / \`git_diff\` 等）看本轮完整 diff / 历史。用 3–8 条中文结论；禁止只复述摘要，禁止广搜或通读 \`{doc_dir}/\`，禁止把整库历史欠账当本轮事故。
范围（逐项核对）：
- 覆盖：本轮 \`{files}\` 里的业务文件是否都在摘要与台账里有着落（更新了哪篇，或写明跳过原因）；完全没着落的要点名缺哪些；
- 深度：本轮新写的内容是否讲清了业务行为、调用链与边界，还是只复述了 commit 标题式的一句话；
- 矛盾：新写的描述与现行源码是否一致；
- 历史欠账、无关模块、总索引陈旧 → 一律 \`[建议]\`；
- 吃不准就不要 \`[阻断]\`；超时前必须先给出结论列表。
\`[阻断]\` 仅限（须同时满足「本轮新写/新改内容」）：
- 与本轮 \`{files}\` 源码直接矛盾；
- 引用了本轮文件中不存在的符号；
- 日报编造了本轮未出现的模块。
输出格式（每条单独一行）：
- \`[阻断] …\`；
- \`[建议] …\`；
- 无本轮硬伤写：\`未发现过时或矛盾\`。
硬规则：只读文件/搜索/只读 git（git_read 或 git_log / git_diff 等），禁止写文件、改文件、跑命令、删除。`;

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

async function sealWebhook(raw: string | undefined): Promise<string> {
  let v = (raw ?? "").trim();
  if (v && !v.startsWith("enc:v1:")) v = await protectSecret(v);
  return v;
}

/** 单 bot 的 apply 载荷（与后端 WecomApplyRequest 对齐）。 */
async function buildBotApplyPayload(args: {
  bot: WecomBotProfile;
  settings: AppSettings;
  models: AIModelConfig[];
  mcpServers: McpServer[];
  plainSecret?: string;
  plainCorpSecret?: string;
}): Promise<Record<string, unknown>> {
  const { bot, settings, models, mcpServers } = args;
  let botSecret = (bot.botSecret ?? "").trim();
  const plain = args.plainSecret?.trim() ?? "";
  if (plain) botSecret = await protectSecret(plain);
  let corpSecret = (bot.corpSecret ?? "").trim();
  const plainCorp = args.plainCorpSecret?.trim() ?? "";
  if (plainCorp) corpSecret = await protectSecret(plainCorp);

  const webhookUrl = await sealWebhook(bot.webhookUrl);
  const webhookFeishu = await sealWebhook(bot.webhookFeishu);
  const webhookDingtalk = await sealWebhook(bot.webhookDingtalk);

  const model =
    resolveSceneModel("aiHub", models, settings.aiSceneModels) ??
    models.find((m) => m.isDefault) ??
    models[0];
  const fallback = resolveFallbackModel(model ?? undefined, models);

  const permission = (bot.permission ?? "read").trim() || "read";
  const repoPath =
    (bot.repoPath ?? "").trim() || (settings.currentGitRepo ?? "").trim();

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

  const skyEnv = bot.skywalkingEnv === "test" ? "test" : "prod";
  const skywalking =
    bot.skywalkingProfiles?.[skyEnv] ??
    ({
      addr: bot.skywalkingAddr,
      header: bot.skywalkingHeader,
      timezoneOffsetMinutes: bot.skywalkingTimezoneOffsetMinutes,
    } satisfies WecomSkywalkingProfile);

  return {
    localId: bot.id,
    enabled: Boolean(bot.enabled),
    botId: (bot.botId ?? "").trim(),
    botSecret,
    corpId: (bot.corpId ?? "").trim(),
    corpSecret,
    botDisplayName: (bot.botDisplayName ?? "").trim(),
    selfIntro: Array.from((bot.selfIntro ?? "").trim()).slice(0, 2000).join(""),
    dmPolicy: bot.dmPolicy ?? "pairing",
    groupPolicy: bot.groupPolicy ?? "allowlist",
    requireMention: bot.requireMention !== false,
    allowUsers: splitCsv(bot.allowUsers),
    allowGroups: splitCsv(bot.allowGroups),
    pairedUsers: splitCsv(bot.pairedUsers),
    engine: bot.engine === "dsh" ? "dsh" : "tianji",
    permission,
    workspace: repoPath,
    dshModel: (bot.dshModel ?? "").trim(),
    dshEffort: (bot.dshEffort ?? "").trim(),
    answerMode: bot.answerMode ?? "auto",
    docDir: (bot.docDir ?? "").trim(),
    enhanceDmEnabled: Boolean(bot.enhanceDmEnabled),
    enhanceGroupEnabled: Boolean(bot.enhanceGroupEnabled),
    enhanceDm: Array.from((bot.enhanceDm ?? "").trim()).slice(0, 2000).join(""),
    enhanceGroup: Array.from((bot.enhanceGroup ?? "").trim()).slice(0, 2000).join(""),
    skywalkingAddr: (skywalking.addr ?? "").trim(),
    skywalkingHeader: (skywalking.header ?? "").trim(),
    skywalkingTimezoneOffsetMinutes: skywalking.timezoneOffsetMinutes ?? null,
    autoUpdateEnabled: Boolean(bot.autoUpdateEnabled),
    autoUpdateTime: (bot.autoUpdateTime ?? "").trim(),
    releaseBranch: (bot.releaseBranch ?? "").trim(),
    webhookUrl,
    webhookFeishu,
    webhookDingtalk,
    docgenCron: (bot.docgenCron ?? "").trim(),
    docgenEngine:
      bot.docgenEngine === "dsh"
        ? "dsh"
        : bot.docgenEngine === "tianji"
          ? "tianji"
          : "inherit",
    docgenDshModel: (bot.docgenDshModel ?? "").trim(),
    docgenDshEffort: (bot.docgenDshEffort ?? "").trim(),
    docgenRepoPath: (bot.docgenRepoPath ?? "").trim(),
    docgenDocDir: (bot.docgenDocDir ?? "").trim() || (bot.docDir ?? "").trim(),
    docgenLookbackDays: normalizeDocgenLookbackDays(bot.docgenLookbackDays),
    docgenSyncSources: Boolean(bot.docgenSyncSources),
    docgenRepoAllowlist: normalizeDocgenRepoAllowlist(bot.docgenRepoAllowlist),
    docgenSystemPrompt: normalizeDocgenPrompt(bot.docgenSystemPrompt),
    docgenQualityPrompt: normalizeDocgenPrompt(bot.docgenQualityPrompt),
    docgenPreview: Boolean(bot.docgenPreview),
    docgenQualityCheck: Boolean(bot.docgenQualityCheck),
    docgenPollRelease: Boolean(bot.docgenPollRelease),
    autoCommit: bot.autoCommit !== false,
    autoPush: Boolean(bot.autoPush),
    maxJobs: normalizeWecomMaxJobs(bot.maxJobs),
    mcpEnabled: settings.agentMcpEnabled !== false,
    mcp,
    agent,
  };
}

export async function applyWecomChannel(args: ApplyArgs): Promise<WecomStatus> {
  const migrated = migrateWecomBotsSettings(args.settings);
  const bots = listWecomBots(migrated);
  const active = getActiveWecomBot(migrated);
  const plain = args.plainSecret?.trim() ?? "";
  const plainCorp = args.plainCorpSecret?.trim() ?? "";

  const payloads = await Promise.all(
    bots.map((bot) =>
      buildBotApplyPayload({
        bot,
        settings: migrated,
        models: args.models,
        mcpServers: args.mcpServers,
        plainSecret: active?.id === bot.id ? plain || undefined : undefined,
        plainCorpSecret: active?.id === bot.id ? plainCorp || undefined : undefined,
      }),
    ),
  );

  const status = await invoke<WecomStatus>("wecom_channel_apply", {
    bots: payloads,
    req: null,
  });

  const anyEnabled = bots.some((b) => b.enabled);
  if (anyEnabled && migrated.aiHubBubbleEnabled !== false) {
    const pos = migrated.aiHubBubblePos;
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
export async function applyWecomDocgen(
  settings: AppSettings,
  models: AIModelConfig[],
  botId?: string,
  mcpServers: McpServer[] = [],
): Promise<void> {
  const migrated = migrateWecomBotsSettings(settings);
  const active = getActiveWecomBot(migrated);
  const targetBotId =
    (botId ?? "").trim() ||
    (active?.botId ?? "").trim() ||
    (migrated.wecomBotId ?? "").trim();
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
  // 文档轮次的 MCP：只下发用户勾选且启用/填全的服务（服务级白名单，fail-closed）。
  const selectedMcp = new Set(
    (settings.wecomDocgenMcpServers ?? []).map((s) => (s ?? "").trim()).filter(Boolean),
  );
  const docgenMcp = settings.wecomDocgenMcpEnabled
    ? mcpServers
        .filter((s) => s.enabled && selectedMcp.has(s.name.trim()) && s.name.trim() && s.command.trim())
        .map((s) => ({ name: s.name.trim(), command: s.command.trim(), args: s.args ?? "" }))
    : [];
  const docgenModel =
    resolveSceneModel("aiHub", models, settings.aiSceneModels) ??
    models.find((m) => m.isDefault) ??
    models[0];
  const docgenFallback = resolveFallbackModel(docgenModel ?? undefined, models);
  const docgenAgent = docgenModel
    ? {
        config: docgenModel,
        fallbackConfig: docgenFallback,
        repoPath,
        services: [] as string[],
        permission: "workspace",
        tools: true,
        mode: "doc",
        vision: Boolean(docgenModel.vision),
        diffConfirm: false,
        alwaysAllow: [],
        webAllow: settings.agentWebAllow ?? [],
        webTier: settings.agentWebTier || "allowlist",
        webSearchProvider: settings.agentWebSearchProvider || "",
        webSearchKey: settings.agentWebSearchKey || "",
        subagents: false,
        parallelReads: settings.agentParallelReads !== false,
        retries: settings.agentRetries !== false,
        grepIgnore: settings.agentGrepIgnore !== false,
        planFirst: false,
        contextTokens: settings.agentContextTokens ?? 0,
        resultCap: settings.agentResultCap ?? 0,
        anthropicStream: settings.agentAnthropicStream !== false,
        mcpEnabled: false,
        mcp: [],
        systemExtra: "",
      }
    : null;

  await invoke("wecom_docgen_apply", {
    req: {
      botId: targetBotId,
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
      fullInit: Boolean(settings.wecomDocgenFullInit),
      qualityCheck: Boolean(settings.wecomDocgenQualityCheck),
      qualityGate: settings.wecomDocgenQualityGate === true,
      pollRelease: Boolean(settings.wecomDocgenPollRelease),
      syncSources: Boolean(settings.wecomDocgenSyncSources),
      lookbackDays: normalizeDocgenLookbackDays(settings.wecomDocgenLookbackDays),
      repoAllowlist: normalizeDocgenRepoAllowlist(settings.wecomDocgenRepoAllowlist),
      systemPrompt: normalizeDocgenPrompt(settings.wecomDocgenSystemPrompt),
      qualityPrompt: normalizeDocgenPrompt(settings.wecomDocgenQualityPrompt),
      docSkill: (settings.wecomDocgenSkill ?? "").trim(),
      mcpEnabled: Boolean(settings.wecomDocgenMcpEnabled) && docgenMcp.length > 0,
      mcp: docgenMcp,
      gitTools: settings.wecomDocgenGitTools !== false,
      engine:
        settings.wecomDocgenEngine === "dsh"
          ? "dsh"
          : settings.wecomDocgenEngine === "tianji"
            ? "tianji"
            : "inherit",
      agent: docgenAgent,
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
  scope?: "channel" | "docgen",
  botId?: string,
): Promise<DshModelCatalog> {
  return invoke<DshModelCatalog>("wecom_dsh_models", {
    refresh,
    scope: scope ?? null,
    botId: (botId ?? "").trim() || null,
  });
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

/** 停止当前文档任务；返回失败表示没有运行中的任务。 */
export async function cancelWecomDocgen(): Promise<void> {
  return invoke("wecom_docgen_cancel");
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
  /** 本轮记录或复用的代码—文档引用；用于追溯，不证明调用关系。 */
  references?: Array<{
    repo: string;
    sha: string;
    sourcePath: string;
    symbol: string;
    docPath: string;
    section: string;
  }>;
  commitNote?: string;
  quality?: string;
  /** 质检识别出的明确阻断项数量。 */
  qualityBlocking?: number;
  /** 模型自报的覆盖台账（`## 覆盖` 小节原文）；老记录没有。 */
  coverage?: string;
  pushed: boolean;
  /** ok | fail | not-run | reused；旧记录为 undefined。 */
  commitStatus?: string;
  /** ok | fail | not-run；旧记录为 undefined。 */
  pushStatus?: string;
  /** sent | not-sent；旧记录为 undefined。 */
  notifyStatus?: string;
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
  /** 后端计算的下一计划时间；cron 是实际下一次命中。 */
  nextRunAt?: string | null;
  pendingDailySlot?: string | null;
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
