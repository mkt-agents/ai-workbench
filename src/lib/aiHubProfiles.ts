import type { AiHubProfile } from "../core/types";

/** Built-in profiles (not persisted; merged at runtime). */
export const BUILTIN_PROFILES: AiHubProfile[] = [
  {
    id: "builtin:code",
    name: "代码助手",
    nameKey: "profile.code",
    systemRole:
      "Prefer precise technical answers: cite files when present, propose concrete diffs or steps, avoid fluff.",
    temperature: 0.3,
    builtin: true,
  },
];

export function mergeProfiles(custom: AiHubProfile[] | undefined): AiHubProfile[] {
  const extras = (custom || []).filter((p) => p.id && !p.id.startsWith("builtin:"));
  return [...BUILTIN_PROFILES, ...extras];
}

export function findProfile(
  id: string | undefined,
  custom: AiHubProfile[] | undefined
): AiHubProfile | null {
  if (!id) return null;
  return mergeProfiles(custom).find((p) => p.id === id) || null;
}

export function newCustomProfile(partial?: Partial<AiHubProfile>): AiHubProfile {
  return {
    id: `profile-${Date.now()}-${Math.random().toString(36).slice(2, 7)}`,
    name: partial?.name?.trim() || "Custom",
    systemRole: partial?.systemRole?.trim() || "",
    temperature: partial?.temperature,
    modelId: partial?.modelId,
    builtin: false,
  };
}
