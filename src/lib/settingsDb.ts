/**
 * 设置的 SQLite 副本（`app_settings` 表）。
 *
 * localStorage（`workbench-settings`）仍是主存：所有窗口照旧同步读写它。
 * 这里做两件事：
 * - 每次写入后把「本次真正变化的键」防抖推到库里（`settings_patch`）；
 * - 启动时若本地为空（重装 / 清了 WebView2 缓存），用库里的那份回填。
 *
 * 覆盖的是 `settings` 里那些只此一份的配置：企微凭据、天玑权限、联网扩展、
 * 天玑 API 端口与密钥等。业务数据本来就在 SQLite，不受影响。
 */
import { invoke } from "@tauri-apps/api/core";

const SETTINGS_KEY = "workbench-settings";
/** 连续改动（拖滑块、连点开关）合并成一次写库。 */
const PATCH_DELAY_MS = 500;

let pending: Record<string, unknown> = {};
let timer: number | undefined;

/** 把变化的键攒起来，稍后统一写库。`undefined` 表示这个键被删了。 */
export function queueSettingsPatch(changed: Record<string, unknown>): void {
  if (typeof window === "undefined" || Object.keys(changed).length === 0) return;
  Object.assign(pending, changed);
  if (timer !== undefined) window.clearTimeout(timer);
  timer = window.setTimeout(() => {
    timer = undefined;
    void flushSettingsPatch();
  }, PATCH_DELAY_MS);
}

/** 立即写库（启动迁移、窗口关闭前用）。 */
export async function flushSettingsPatch(): Promise<void> {
  if (typeof window === "undefined") return;
  if (timer !== undefined) {
    window.clearTimeout(timer);
    timer = undefined;
  }
  const entries = Object.entries(pending).map(([key, value]) => ({
    key,
    value: value === undefined ? null : value,
  }));
  pending = {};
  if (entries.length === 0) return;
  try {
    await invoke("settings_patch", { entries });
  } catch {
    /*
     * 写库失败（比如后端还没接上真正的数据库）：把这批改动放回待写队列，
     * 下次改动或窗口关闭时再试。期间被新值覆盖过的键跳过，避免旧值盖新值。
     */
    for (const entry of entries) {
      if (!(entry.key in pending)) {
        pending[entry.key] = entry.value === null ? undefined : entry.value;
      }
    }
  }
}

/** 读库里的设置副本；老库没有这张表、或命令缺席时返回空对象。 */
export async function loadSettingsFromDb(): Promise<Record<string, unknown>> {
  try {
    const raw = await invoke<Record<string, unknown>>("settings_load");
    return raw && typeof raw === "object" ? raw : {};
  } catch {
    return {};
  }
}

/** 读 localStorage 里现有的设置（只读，不改写）。 */
export function readLocalSettings(): Record<string, unknown> {
  if (typeof window === "undefined") return {};
  const raw = localStorage.getItem(SETTINGS_KEY);
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as { state?: { settings?: Record<string, unknown> } };
    const settings = parsed.state?.settings;
    return settings && typeof settings === "object" ? settings : {};
  } catch {
    return {};
  }
}

/** 把库里那份写回 localStorage 的信封（信封里的其它键与本地独有的键原样保留）。 */
export function adoptDbSettings(db: Record<string, unknown>): void {
  if (typeof window === "undefined") return;
  const raw = localStorage.getItem(SETTINGS_KEY);
  let envelope: { state?: Record<string, unknown>; version?: number } = { state: {}, version: 0 };
  if (raw) {
    try {
      envelope = JSON.parse(raw) as typeof envelope;
    } catch {
      /* 坏 JSON：下面直接用库里的值覆盖 */
    }
  }
  const state = (envelope.state ?? {}) as Record<string, unknown>;
  const settings = { ...((state.settings as Record<string, unknown>) ?? {}), ...db };
  envelope.state = { ...state, settings };
  localStorage.setItem(SETTINGS_KEY, JSON.stringify(envelope));
}

/** 首次升级（库里为空）：把本地现有设置整份灌进库。 */
export async function seedSettingsToDb(settings: Record<string, unknown>): Promise<void> {
  queueSettingsPatch(settings);
  await flushSettingsPatch();
}

if (typeof window !== "undefined") {
  // 窗口被关掉前把攒下的改动落库，避免最后半秒的修改只留在 localStorage
  window.addEventListener("pagehide", () => {
    void flushSettingsPatch();
  });
}
