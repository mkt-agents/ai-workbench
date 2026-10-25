/**
 * 天玑 OpenAI 兼容端点：把设置推给 Rust HTTP 服务单例。
 * OpenClaw（微信 ClawBot）等外部 agent 以 base_url=http://127.0.0.1:{port}/v1 接入。
 */
import { invoke } from "@tauri-apps/api/core";
import type { AIModelConfig, AppSettings, McpServer } from "../core/types";
import { resolveFallbackModel, resolveSceneModel } from "./aiSceneModels";

export type TianjiApiStatus = {
  state: string;
  detail: string;
  enabled: boolean;
  port: number;
  baseUrl: string;
};

type ApplyArgs = {
  settings: AppSettings;
  models: AIModelConfig[];
  mcpServers: McpServer[];
  plainKey?: string;
};

export async function protectTianjiKey(plain: string): Promise<string> {
  if (!plain.trim()) return "";
  if (plain.startsWith("enc:v1:")) return plain;
  return invoke<string>("wecom_protect_secret", { plain });
}

export async function revealTianjiKey(stored: string): Promise<string> {
  return invoke<string>("wecom_reveal_secret", { stored });
}

export async function applyTianjiApi(args: ApplyArgs): Promise<TianjiApiStatus> {
  const { settings, models, mcpServers } = args;
  let apiKey = (settings.tianjiApiKey ?? "").trim();
  const plain = args.plainKey?.trim() ?? "";
  if (plain) apiKey = await protectTianjiKey(plain);

  const model =
    resolveSceneModel("aiHub", models, settings.aiSceneModels) ??
    models.find((m) => m.isDefault) ??
    models[0];
  const fallback = resolveFallbackModel(model ?? undefined, models);

  const permission = (settings.tianjiApiPermission ?? "read").trim() || "read";
  const repoPath =
    (settings.tianjiApiRepoPath ?? "").trim() || (settings.currentGitRepo ?? "").trim();

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

  return invoke<TianjiApiStatus>("tianji_api_apply", {
    req: {
      enabled: Boolean(settings.tianjiApiEnabled),
      apiKey,
      port: Number(settings.tianjiApiPort ?? 8792) || 8792,
      agent,
    },
  });
}

export async function fetchTianjiApiStatus(): Promise<TianjiApiStatus> {
  return invoke<TianjiApiStatus>("tianji_api_status");
}
