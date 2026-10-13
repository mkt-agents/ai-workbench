/** Last successful "sync model → DSH" metadata (localStorage). */

const KEY = "workbench-dsh-sync-meta";

export type DshSyncMeta = {
  at: string;
  modelName: string;
  provider: string;
  modelId: string;
};

export function readDshSyncMeta(): DshSyncMeta | null {
  try {
    const raw = localStorage.getItem(KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as DshSyncMeta;
    if (!parsed?.at || !parsed?.modelName) return null;
    return parsed;
  } catch {
    return null;
  }
}

export function writeDshSyncMeta(meta: Omit<DshSyncMeta, "at"> & { at?: string }): void {
  try {
    const payload: DshSyncMeta = {
      at: meta.at || new Date().toISOString(),
      modelName: meta.modelName,
      provider: meta.provider,
      modelId: meta.modelId,
    };
    localStorage.setItem(KEY, JSON.stringify(payload));
    window.dispatchEvent(new CustomEvent("dsh-sync-meta-changed", { detail: payload }));
  } catch {
    /* ignore quota / private mode */
  }
}
