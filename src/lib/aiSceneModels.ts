import type { AIModelConfig, AiScene, AiSceneModels } from "../core/types";

/** Scenes that can bind a preferred model (falls back to default). */
export const AI_SCENES: AiScene[] = ["quickAsk", "report", "commit", "optimize"];

export const AI_SCENE_I18N_KEY: Record<AiScene, string> = {
  quickAsk: "models.sceneQuickAsk",
  report: "models.sceneReport",
  commit: "models.sceneCommit",
  optimize: "models.sceneOptimize",
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
