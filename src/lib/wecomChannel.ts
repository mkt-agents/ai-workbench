/**
 * Build + push WeCom channel config to the Rust long-connection singleton.
 * Settings UI and app boot both call `applyWecomChannel`.
 */
import { invoke } from "@tauri-apps/api/core";
import type { AIModelConfig, AppSettings, McpServer } from "../core/types";
import { resolveFallbackModel, resolveSceneModel } from "./aiSceneModels";

export type WecomStatus = {
  state: string;
  detail: string;
  enabled: boolean;
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
        tools: permission !== "read" ? true : true,
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
