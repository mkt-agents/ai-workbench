import type { AIModelConfig, AiScene, AiSceneModels } from "../core/types";

/** Scenes that can bind a preferred model (falls back to default). */
export const AI_SCENES: AiScene[] = ["aiHub", "report", "commit"];

export const AI_SCENE_I18N_KEY: Record<AiScene, string> = {
  aiHub: "models.sceneAiHub",
  report: "models.sceneReport",
  commit: "models.sceneCommit",
};

/** Resolve the model for a scene: explicit binding → default → first. */
export function resolveSceneModel(
  scene: AiScene,
  models: AIModelConfig[],
  bindings: AiSceneModels | undefined,
  overrideId?: string | null
): AIModelConfig | null {
  if (!models.length) return null;
  if (overrideId) {
    const found = models.find((m) => m.id === overrideId);
    if (found) return found;
  }
  const boundId = bindings?.[scene];
  if (boundId) {
    const found = models.find((m) => m.id === boundId);
    if (found) return found;
  }
  return models.find((m) => m.isDefault) || models[0] || null;
}

/** Preferred model id for initial UI state (binding or empty → consumer uses default). */
export function sceneModelId(
  scene: AiScene,
  bindings: AiSceneModels | undefined
): string {
  return bindings?.[scene]?.trim() || "";
}

/** Wire protocol a model speaks; the backend branches on exactly this (`agent.rs`). */
export function modelProtocol(model: AIModelConfig): "anthropic" | "openai" {
  return model.provider === "anthropic" ? "anthropic" : "openai";
}

/**
 * A second model for the agent to fall back to when the primary fails a whole round
 * (connection / 4xx / 5xx / empty answer) before emitting any text.
 *
 * Must speak the same protocol: a turn's messages and tool schemas are already formatted
 * for one of them, so swapping across `/chat/completions` and `/v1/messages` mid-turn
 * cannot work. Must also be a different endpoint — retrying the same URL + model name
 * would fail the same way. `null` when nothing else is usable.
 */
export function resolveFallbackModel(
  primary: AIModelConfig | undefined,
  models: AIModelConfig[]
): AIModelConfig | null {
  if (!primary) return null;
  const protocol = modelProtocol(primary);
  const primaryModel = primary.model?.trim() || "";
  const primaryBase = primary.baseUrl?.trim() || "";
  const usable = models.filter((m) => {
    if (m.id === primary.id) return false;
    if (!m.model?.trim() || !m.baseUrl?.trim()) return false;
    if (modelProtocol(m) !== protocol) return false;
    // 同一个地址 + 同一个模型名就是同一路，再试一次也是同样的错
    return !(m.model.trim() === primaryModel && m.baseUrl.trim() === primaryBase);
  });
  return usable.find((m) => m.isDefault) || usable[0] || null;
}
