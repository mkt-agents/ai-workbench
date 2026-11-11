/**
 * 企微多机器人：profile 列表 + active 指针 + 扁平 wecom* 镜像（兼容旧读者）。
 */
import type { AppSettings, WecomBotProfile } from "../core/types";

function newId(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `wecom-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 10)}`;
}

/** 从旧扁平 wecom* 生成一条 profile（迁移用）。 */
export function profileFromFlatSettings(settings: AppSettings): WecomBotProfile {
  return {
    id: newId(),
    botId: settings.wecomBotId,
    botSecret: settings.wecomBotSecret,
    botDisplayName: settings.wecomBotDisplayName,
    remark: settings.wecomBotRemark,
    enabled: Boolean(settings.wecomEnabled),
    corpId: settings.wecomCorpId,
    corpSecret: settings.wecomCorpSecret,
    selfIntro: settings.wecomSelfIntro,
    dmPolicy: settings.wecomDmPolicy,
    groupPolicy: settings.wecomGroupPolicy,
    requireMention: settings.wecomRequireMention,
    allowUsers: settings.wecomAllowUsers,
    allowGroups: settings.wecomAllowGroups,
    pairedUsers: settings.wecomPairedUsers,
    permission: settings.wecomPermission,
    repoPath: settings.wecomRepoPath,
    engine: settings.wecomEngine,
    dshModel: settings.wecomDshModel,
    dshEffort: settings.wecomDshEffort,
    answerMode: settings.wecomAnswerMode,
    docDir: settings.wecomDocDir,
    enhanceDmEnabled: settings.wecomEnhanceDmEnabled,
    enhanceGroupEnabled: settings.wecomEnhanceGroupEnabled,
    enhanceDm: settings.wecomEnhanceDm,
    enhanceGroup: settings.wecomEnhanceGroup,
    skywalkingAddr: settings.wecomSkywalkingAddr,
    skywalkingHeader: settings.wecomSkywalkingHeader,
    skywalkingTimezoneOffsetMinutes: settings.wecomSkywalkingTimezoneOffsetMinutes,
    skywalkingEnv: settings.wecomSkywalkingEnv,
    skywalkingProfiles: settings.wecomSkywalkingProfiles,
    autoUpdateEnabled: settings.wecomAutoUpdateEnabled,
    autoUpdateTime: settings.wecomAutoUpdateTime,
    releaseBranch: settings.wecomReleaseBranch,
    webhookUrl: settings.wecomWebhookUrl,
    webhookFeishu: settings.wecomWebhookFeishu,
    webhookDingtalk: settings.wecomWebhookDingtalk,
    docgenCron: settings.wecomDocgenCron,
    docgenEngine: settings.wecomDocgenEngine,
    docgenDshModel: settings.wecomDocgenDshModel,
    docgenDshEffort: settings.wecomDocgenDshEffort,
    docgenRepoPath: settings.wecomDocgenRepoPath,
    docgenDocDir: settings.wecomDocgenDocDir,
    docgenLookbackDays: settings.wecomDocgenLookbackDays,
    docgenSyncSources: settings.wecomDocgenSyncSources,
    docgenRepoAllowlist: settings.wecomDocgenRepoAllowlist,
    docgenSystemPrompt: settings.wecomDocgenSystemPrompt,
    docgenQualityPrompt: settings.wecomDocgenQualityPrompt,
    docgenPreview: settings.wecomDocgenPreview,
    docgenFullInit: settings.wecomDocgenFullInit,
    docgenQualityCheck: settings.wecomDocgenQualityCheck,
    docgenQualityGate: settings.wecomDocgenQualityGate,
    docgenPollRelease: settings.wecomDocgenPollRelease,
    autoCommit: settings.wecomAutoCommit,
    autoPush: settings.wecomAutoPush,
    maxJobs: settings.wecomMaxJobs,
    createdAt: Date.now(),
  };
}

/** 把一条 profile 镜像回扁平 wecom*（旧页面/boot 兼容）。 */
export function flatFromWecomBot(bot: WecomBotProfile): Partial<AppSettings> {
  return {
    wecomEnabled: Boolean(bot.enabled),
    wecomBotId: bot.botId,
    wecomBotSecret: bot.botSecret,
    wecomBotDisplayName: bot.botDisplayName,
    wecomBotRemark: bot.remark,
    wecomCorpId: bot.corpId,
    wecomCorpSecret: bot.corpSecret,
    wecomSelfIntro: bot.selfIntro,
    wecomDmPolicy: bot.dmPolicy,
    wecomGroupPolicy: bot.groupPolicy,
    wecomRequireMention: bot.requireMention,
    wecomAllowUsers: bot.allowUsers,
    wecomAllowGroups: bot.allowGroups,
    wecomPairedUsers: bot.pairedUsers,
    wecomPermission: bot.permission,
    wecomRepoPath: bot.repoPath,
    wecomEngine: bot.engine,
    wecomDshModel: bot.dshModel,
    wecomDshEffort: bot.dshEffort,
    wecomAnswerMode: bot.answerMode,
    wecomDocDir: bot.docDir,
    wecomEnhanceDmEnabled: bot.enhanceDmEnabled,
    wecomEnhanceGroupEnabled: bot.enhanceGroupEnabled,
    wecomEnhanceDm: bot.enhanceDm,
    wecomEnhanceGroup: bot.enhanceGroup,
    wecomSkywalkingAddr: bot.skywalkingAddr,
    wecomSkywalkingHeader: bot.skywalkingHeader,
    wecomSkywalkingTimezoneOffsetMinutes: bot.skywalkingTimezoneOffsetMinutes,
    wecomSkywalkingEnv: bot.skywalkingEnv,
    wecomSkywalkingProfiles: bot.skywalkingProfiles,
    wecomAutoUpdateEnabled: bot.autoUpdateEnabled,
    wecomAutoUpdateTime: bot.autoUpdateTime,
    wecomReleaseBranch: bot.releaseBranch,
    wecomWebhookUrl: bot.webhookUrl,
    wecomWebhookFeishu: bot.webhookFeishu,
    wecomWebhookDingtalk: bot.webhookDingtalk,
    wecomDocgenCron: bot.docgenCron,
    wecomDocgenEngine: bot.docgenEngine,
    wecomDocgenDshModel: bot.docgenDshModel,
    wecomDocgenDshEffort: bot.docgenDshEffort,
    wecomDocgenRepoPath: bot.docgenRepoPath,
    wecomDocgenDocDir: bot.docgenDocDir,
    wecomDocgenLookbackDays: bot.docgenLookbackDays,
    wecomDocgenSyncSources: bot.docgenSyncSources,
    wecomDocgenRepoAllowlist: bot.docgenRepoAllowlist,
    wecomDocgenSystemPrompt: bot.docgenSystemPrompt,
    wecomDocgenQualityPrompt: bot.docgenQualityPrompt,
    wecomDocgenPreview: bot.docgenPreview,
    wecomDocgenFullInit: bot.docgenFullInit,
    wecomDocgenQualityCheck: bot.docgenQualityCheck,
    wecomDocgenQualityGate: bot.docgenQualityGate,
    wecomDocgenPollRelease: bot.docgenPollRelease,
    wecomAutoCommit: bot.autoCommit,
    wecomAutoPush: bot.autoPush,
    wecomMaxJobs: bot.maxJobs,
  };
}

/**
 * 一次性：wecomBots 空且旧 botId 非空 → 收成一条；并设 active。
 * 已有列表时只保证 active 有效，并把 active 镜像回扁平字段。
 */
export function migrateWecomBotsSettings(settings: AppSettings): AppSettings {
  let bots = Array.isArray(settings.wecomBots) ? [...settings.wecomBots] : [];
  let activeId = settings.wecomActiveBotId;
  let changed = false;

  if (bots.length === 0) {
    const legacyId = (settings.wecomBotId ?? "").trim();
    const legacySecret = (settings.wecomBotSecret ?? "").trim();
    if (legacyId || legacySecret || settings.wecomEnabled) {
      const profile = profileFromFlatSettings(settings);
      bots = [profile];
      activeId = profile.id;
      changed = true;
    }
  }

  if (bots.length > 0) {
    if (!activeId || !bots.some((b) => b.id === activeId)) {
      activeId = bots[0].id;
      changed = true;
    }
  } else {
    activeId = undefined;
  }

  const active = bots.find((b) => b.id === activeId);
  const mirrored = active ? flatFromWecomBot(active) : {};

  if (!changed && bots === settings.wecomBots && activeId === settings.wecomActiveBotId) {
    return { ...settings, ...mirrored, wecomBots: bots, wecomActiveBotId: activeId };
  }

  return {
    ...settings,
    ...mirrored,
    wecomBots: bots,
    wecomActiveBotId: activeId,
  };
}

export function listWecomBots(settings: AppSettings): WecomBotProfile[] {
  return Array.isArray(settings.wecomBots) ? settings.wecomBots : [];
}

/** 已接入：有 Bot ID 才算添加成功，草稿（扫码/手动未完成）不进列表。 */
export function isWecomBotBound(bot: WecomBotProfile | undefined): boolean {
  return Boolean((bot?.botId ?? "").trim());
}

/** 列表只展示已接入的机器人。 */
export function listBoundWecomBots(settings: AppSettings): WecomBotProfile[] {
  return listWecomBots(settings).filter(isWecomBotBound);
}

/**
 * 丢掉未完成接入的草稿。
 * `keepId`：编辑弹框打开时保留当前这条草稿，其它空壳仍清掉。
 */
export function buildPurgeIncompleteWecomBotsPatch(
  settings: AppSettings,
  keepId?: string,
): Partial<AppSettings> | null {
  const bots = listWecomBots(settings);
  const next = bots.filter((b) => isWecomBotBound(b) || (keepId != null && b.id === keepId));
  if (next.length === bots.length) return null;
  const activeId =
    keepId && next.some((b) => b.id === keepId)
      ? keepId
      : next.some((b) => b.id === settings.wecomActiveBotId)
        ? settings.wecomActiveBotId
        : next[0]?.id;
  const active = next.find((b) => b.id === activeId);
  return {
    wecomBots: next,
    wecomActiveBotId: activeId,
    ...(active
      ? flatFromWecomBot(active)
      : {
          wecomEnabled: false,
          wecomBotId: "",
          wecomBotSecret: "",
          wecomBotDisplayName: "",
          wecomBotRemark: "",
        }),
  };
}

/** 列表/编辑标题：备注 → 显示名 → 未命名（由调用方补 i18n 默认文案）。 */
export function wecomBotLabel(bot: WecomBotProfile | undefined, untitled: string): string {
  if (!bot) return untitled;
  const remark = (bot.remark ?? "").trim();
  if (remark) return remark;
  const name = (bot.botDisplayName ?? "").trim();
  if (name) return name;
  return untitled;
}

export const WECOM_BOT_REMARK_MAX = 40;

export function clampWecomBotRemark(raw: string): string {
  return Array.from((raw ?? "").trim()).slice(0, WECOM_BOT_REMARK_MAX).join("");
}

export function getActiveWecomBot(settings: AppSettings): WecomBotProfile | undefined {
  const bots = listWecomBots(settings);
  if (!bots.length) return undefined;
  const id = settings.wecomActiveBotId;
  return bots.find((b) => b.id === id) ?? bots[0];
}

/** 新建空机器人（未启用）；调用方负责 setSettings。 */
export function createEmptyWecomBot(partial?: Partial<WecomBotProfile>): WecomBotProfile {
  return {
    id: newId(),
    enabled: false,
    engine: "tianji",
    permission: "read",
    dmPolicy: "pairing",
    groupPolicy: "allowlist",
    requireMention: true,
    answerMode: "auto",
    createdAt: Date.now(),
    ...partial,
  };
}

/**
 * 更新列表中某 bot；同时镜像到扁平 wecom*（若是 active）。
 * 返回应写入 store 的 patch。
 */
export function buildWecomBotPatch(
  settings: AppSettings,
  botId: string,
  patch: Partial<WecomBotProfile>,
): Partial<AppSettings> {
  const bots = listWecomBots(settings).map((b) =>
    b.id === botId ? { ...b, ...patch } : b,
  );
  const activeId = settings.wecomActiveBotId ?? bots[0]?.id;
  const active = bots.find((b) => b.id === activeId);
  return {
    wecomBots: bots,
    wecomActiveBotId: activeId,
    ...(active ? flatFromWecomBot(active) : {}),
  };
}

/** 增 bot 并选中。 */
export function buildAddWecomBotPatch(
  settings: AppSettings,
  bot: WecomBotProfile,
): Partial<AppSettings> {
  const bots = [...listWecomBots(settings), bot];
  return {
    wecomBots: bots,
    wecomActiveBotId: bot.id,
    ...flatFromWecomBot(bot),
  };
}

/** 删 bot；若删的是 active 则选中下一项。 */
export function buildRemoveWecomBotPatch(
  settings: AppSettings,
  botLocalId: string,
): Partial<AppSettings> {
  const bots = listWecomBots(settings).filter((b) => b.id !== botLocalId);
  const activeId =
    settings.wecomActiveBotId === botLocalId ? bots[0]?.id : settings.wecomActiveBotId;
  const active = bots.find((b) => b.id === activeId);
  return {
    wecomBots: bots,
    wecomActiveBotId: activeId,
    ...(active
      ? flatFromWecomBot(active)
      : {
          wecomEnabled: false,
          wecomBotId: "",
          wecomBotSecret: "",
        }),
  };
}

export function buildSelectWecomBotPatch(
  settings: AppSettings,
  botLocalId: string,
): Partial<AppSettings> {
  const bot = listWecomBots(settings).find((b) => b.id === botLocalId);
  if (!bot) return {};
  return {
    wecomActiveBotId: botLocalId,
    ...flatFromWecomBot(bot),
  };
}

const FLAT_TO_BOT: Array<[keyof AppSettings, keyof WecomBotProfile]> = [
  ["wecomEnabled", "enabled"],
  ["wecomBotId", "botId"],
  ["wecomBotSecret", "botSecret"],
  ["wecomBotDisplayName", "botDisplayName"],
  ["wecomBotRemark", "remark"],
  ["wecomCorpId", "corpId"],
  ["wecomCorpSecret", "corpSecret"],
  ["wecomSelfIntro", "selfIntro"],
  ["wecomDmPolicy", "dmPolicy"],
  ["wecomGroupPolicy", "groupPolicy"],
  ["wecomRequireMention", "requireMention"],
  ["wecomAllowUsers", "allowUsers"],
  ["wecomAllowGroups", "allowGroups"],
  ["wecomPairedUsers", "pairedUsers"],
  ["wecomPermission", "permission"],
  ["wecomRepoPath", "repoPath"],
  ["wecomEngine", "engine"],
  ["wecomDshModel", "dshModel"],
  ["wecomDshEffort", "dshEffort"],
  ["wecomAnswerMode", "answerMode"],
  ["wecomDocDir", "docDir"],
  ["wecomEnhanceDmEnabled", "enhanceDmEnabled"],
  ["wecomEnhanceGroupEnabled", "enhanceGroupEnabled"],
  ["wecomEnhanceDm", "enhanceDm"],
  ["wecomEnhanceGroup", "enhanceGroup"],
  ["wecomSkywalkingAddr", "skywalkingAddr"],
  ["wecomSkywalkingHeader", "skywalkingHeader"],
  ["wecomSkywalkingTimezoneOffsetMinutes", "skywalkingTimezoneOffsetMinutes"],
  ["wecomSkywalkingEnv", "skywalkingEnv"],
  ["wecomSkywalkingProfiles", "skywalkingProfiles"],
  ["wecomAutoUpdateEnabled", "autoUpdateEnabled"],
  ["wecomAutoUpdateTime", "autoUpdateTime"],
  ["wecomReleaseBranch", "releaseBranch"],
  ["wecomWebhookUrl", "webhookUrl"],
  ["wecomWebhookFeishu", "webhookFeishu"],
  ["wecomWebhookDingtalk", "webhookDingtalk"],
  ["wecomDocgenCron", "docgenCron"],
  ["wecomDocgenEngine", "docgenEngine"],
  ["wecomDocgenDshModel", "docgenDshModel"],
  ["wecomDocgenDshEffort", "docgenDshEffort"],
  ["wecomDocgenRepoPath", "docgenRepoPath"],
  ["wecomDocgenDocDir", "docgenDocDir"],
  ["wecomDocgenLookbackDays", "docgenLookbackDays"],
  ["wecomDocgenSyncSources", "docgenSyncSources"],
  ["wecomDocgenRepoAllowlist", "docgenRepoAllowlist"],
  ["wecomDocgenSystemPrompt", "docgenSystemPrompt"],
  ["wecomDocgenQualityPrompt", "docgenQualityPrompt"],
  ["wecomDocgenPreview", "docgenPreview"],
  ["wecomDocgenFullInit", "docgenFullInit"],
  ["wecomDocgenQualityCheck", "docgenQualityCheck"],
  ["wecomDocgenQualityGate", "docgenQualityGate"],
  ["wecomDocgenPollRelease", "docgenPollRelease"],
  ["wecomAutoCommit", "autoCommit"],
  ["wecomAutoPush", "autoPush"],
  ["wecomMaxJobs", "maxJobs"],
];

/** 把设置页仍在写的扁平 wecom* 补丁折进当前 active bot（并镜像回扁平）。 */
export function foldWecomFlatPatch(
  settings: AppSettings,
  flat: Partial<AppSettings>,
): Partial<AppSettings> {
  const botPatch: Partial<WecomBotProfile> = {};
  let touched = false;
  for (const [flatKey, botKey] of FLAT_TO_BOT) {
    if (Object.prototype.hasOwnProperty.call(flat, flatKey)) {
      (botPatch as Record<string, unknown>)[botKey as string] = flat[flatKey];
      touched = true;
    }
  }

  const rest: Partial<AppSettings> = { ...flat };
  for (const [flatKey] of FLAT_TO_BOT) {
    delete rest[flatKey];
  }

  if (!touched) {
    return flat;
  }

  const active = getActiveWecomBot(settings);
  if (!active) {
    const created = createEmptyWecomBot(botPatch);
    return {
      ...rest,
      ...buildAddWecomBotPatch(settings, created),
    };
  }

  return {
    ...rest,
    ...buildWecomBotPatch(settings, active.id, botPatch),
  };
}