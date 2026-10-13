import type { QuickAskProfile } from "../core/types";

/** Built-in profiles (not persisted; merged at runtime). */
export const BUILTIN_PROFILES: QuickAskProfile[] = [
  {
    id: "builtin:code",
    name: "代码助手",
    nameKey: "profile.code",
    systemRole:
      "Prefer precise technical answers: cite files when present, propose concrete diffs or steps, avoid fluff.",
    temperature: 0.3,
    builtin: true,
  },
  {
    id: "builtin:translate",
    name: "翻译",
    nameKey: "profile.translate",
    systemRole:
      "You are a careful translator. Preserve meaning, formatting, and technical terms. Output only the translation unless asked otherwise.",
    temperature: 0.2,
    builtin: true,
  },
  {
    id: "builtin:polish",
    name: "润色",
    nameKey: "profile.polish",
    systemRole:
      "You polish writing for clarity and professionalism while keeping the author's intent and language. Return only the improved text unless asked otherwise.",
    temperature: 0.5,
    builtin: true,
  },
];

export function mergeProfiles(custom: QuickAskProfile[] | undefined): QuickAskProfile[] {
  const extras = (custom || []).filter((p) => p.id && !p.id.startsWith("builtin:"));
  return [...BUILTIN_PROFILES, ...extras];
}

export function findProfile(
  id: string | undefined,
  custom: QuickAskProfile[] | undefined
): QuickAskProfile | null {
  if (!id) return null;
  return mergeProfiles(custom).find((p) => p.id === id) || null;
}

export function newCustomProfile(partial?: Partial<QuickAskProfile>): QuickAskProfile {
  return {
    id: `profile-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
    name: partial?.name?.trim() || "Custom",
    systemRole: partial?.systemRole?.trim() || "",
    temperature: partial?.temperature,
    modelId: partial?.modelId,
    builtin: false,
  };
}
