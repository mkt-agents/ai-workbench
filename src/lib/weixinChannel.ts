/**
 * 个人微信 iLink 通道前端封装。
 * 回答设置（engine / answerMode / repo / docDir）复用企微配置，避免两套分叉。
 */
import { invoke } from "@tauri-apps/api/core";
import type { AIModelConfig, AppSettings, McpServer } from "../core/types";
import { resolveFallbackModel, resolveSceneModel } from "./aiSceneModels";

export type WeixinStatus = {
  state: string;
  detail: string;
  enabled: boolean;
  botId: string;
  userId: string;
  baseUrl: string;
  lastError: string;
  running: boolean;
};

export type WeixinQrStart = {
  qrcode: string;
  qrContent: string;
  qrSvg: string;
};

export type WeixinQrPoll = {
  status: string;
  botToken?: string | null;
  botId?: string | null;
  userId?: string | null;
  baseUrl?: string | null;
  detail: string;
  needVerify: boolean;
};

export async function protectWeixinSecret(plain: string): Promise<string> {
  return invoke<string>("weixin_protect_secret", { plain });
}

export async function revealWeixinSecret(stored: string): Promise<string> {
  return invoke<string>("weixin_reveal_secret", { stored });
}

export function startWeixinQr(baseUrl?: string): Promise<WeixinQrStart> {
  return invoke<WeixinQrStart>("weixin_qr_start", { baseUrl: baseUrl || null });
}

export function pollWeixinQr(args: {
  qrcode: string;
  verifyCode?: string;
  baseUrl?: string;
}): Promise<WeixinQrPoll> {
  return invoke<WeixinQrPoll>("weixin_qr_poll", {
    qrcode: args.qrcode,
    verifyCode: args.verifyCode || null,
    baseUrl: args.baseUrl || null,
  });
}

export function fetchWeixinStatus(): Promise<WeixinStatus> {
  return invoke<WeixinStatus>("weixin_channel_status");
}

type ApplyArgs = {
  settings: AppSettings;
  models: AIModelConfig[];
  mcpServers: McpServer[];
  /** freshly scanned plaintext token */
  plainToken?: string;
};

export async function applyWeixinChannel(args: ApplyArgs): Promise<WeixinStatus> {
  const { settings, models, mcpServers } = args;
  let botToken = (settings.weixinBotToken ?? "").trim();
  const plain = args.plainToken?.trim() ?? "";
  if (plain) botToken = await protectWeixinSecret(plain);

  const model =
    resolveSceneModel("aiHub", models, settings.aiSceneModels) ??
    models.find((m) => m.isDefault) ??
    models[0];
  const fallback = resolveFallbackModel(model ?? undefined, models);

  // Answer settings reuse WeCom
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

  return invoke<WeixinStatus>("weixin_channel_apply", {
    req: {
      enabled: Boolean(settings.weixinEnabled),
      botToken,
      botId: (settings.weixinBotId ?? "").trim(),
      userId: (settings.weixinUserId ?? "").trim(),
      baseUrl: (settings.weixinBaseUrl ?? "").trim(),
      answerMode: settings.wecomAnswerMode ?? "auto",
      docDir: (settings.wecomDocDir ?? "").trim(),
      agent,
    },
  });
}

export async function restartWeixinChannel(): Promise<WeixinStatus> {
  return invoke<WeixinStatus>("weixin_channel_restart");
}
