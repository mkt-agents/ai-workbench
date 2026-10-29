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
} from "../core/types";
import { resolveFallbackModel, resolveSceneModel } from "./aiSceneModels";

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

export type WecomStatus = {
  processId: number;
  buildLabel: string;
  lastInvestigation?: WecomInvestigationStatus | null;
  lastPerformance?: WecomPerformanceStatus | null;
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

export type WecomSessionLogEntry = {
  session: string;
  ts: string;
  user: string;
  assistant: string;
};

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
  let webhookUrl = (settings.wecomWebhookUrl ?? "").trim();
  if (webhookUrl && !webhookUrl.startsWith("enc:v1:")) {
    webhookUrl = await protectSecret(webhookUrl);
  }

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

  const status = await invoke<WecomStatus>("wecom_channel_apply", {
    req: {
      enabled: Boolean(settings.wecomEnabled),
      botId: (settings.wecomBotId ?? "").trim(),
      botSecret,
      corpId: (settings.wecomCorpId ?? "").trim(),
      corpSecret,
      botDisplayName: (settings.wecomBotDisplayName ?? "").trim(),
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
      // 文档问答 + 排障 + 自动更新
      answerMode: settings.wecomAnswerMode ?? "auto",
      docDir: (settings.wecomDocDir ?? "").trim(),
      skywalkingAddr: (settings.wecomSkywalkingAddr ?? "").trim(),
      skywalkingHeader: (settings.wecomSkywalkingHeader ?? "").trim(),
      skywalkingTimezoneOffsetMinutes: settings.wecomSkywalkingTimezoneOffsetMinutes ?? null,
      autoUpdateEnabled: Boolean(settings.wecomAutoUpdateEnabled),
      autoUpdateTime: (settings.wecomAutoUpdateTime ?? "").trim(),
      releaseBranch: (settings.wecomReleaseBranch ?? "").trim(),
      webhookUrl,
      autoCommit: settings.wecomAutoCommit !== false,
      autoPush: Boolean(settings.wecomAutoPush),
      maxJobs: normalizeWecomMaxJobs(settings.wecomMaxJobs),
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

export async function fetchWecomStatus(): Promise<WecomStatus> {
  return invoke<WecomStatus>("wecom_channel_status");
}

/** 读 DSH 引擎可用的模型目录（ACP configOptions）。`refresh` 会丢掉缓存重拉。 */
export async function fetchDshModels(refresh = false): Promise<DshModelCatalog> {
  return invoke<DshModelCatalog>("wecom_dsh_models", { refresh });
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
