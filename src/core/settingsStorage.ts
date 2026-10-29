import type { StateStorage } from 'zustand/middleware';
import { queueSettingsPatch } from '../lib/settingsDb';

/**
 * 主窗、天玑窗、桌宠窗各持一份 store，却共用 localStorage 的同一个键。persist 默认整份覆盖：
 * 桌宠拿着启动时的旧设置写一次好感度，就会把主窗刚改的企微 / 天玑配置盖回去，重启才暴露。
 * 这里改成按键合并：只写本窗口真正改过的键，其余键以磁盘上的最新值为准。
 */

type Settings = Record<string, unknown>;
type Envelope = { state?: { settings?: Settings } & Record<string, unknown>; version?: number };

/** 本窗口最后一次从磁盘读到 / 写入磁盘的设置，用来判断哪些键是本窗口改的。 */
let lastSynced: Settings = {};

function same(a: unknown, b: unknown): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

function parse(raw: string | null): Envelope | null {
  if (!raw) return null;
  try {
    return JSON.parse(raw) as Envelope;
  } catch {
    return null;
  }
}

/** 相对磁盘上那份，本次真正变化的键；值为 undefined 表示这个键被删了。 */
export function changedSettings(disk: Settings, merged: Settings): Settings {
  const out: Settings = {};
  for (const key of Object.keys(merged)) {
    if (!same(merged[key], disk[key])) out[key] = merged[key];
  }
  for (const key of Object.keys(disk)) {
    if (!(key in merged)) out[key] = undefined;
  }
  return out;
}

/** ours 相对 base 改过的键用 ours，其它键用 disk；值为 undefined 表示删除。 */
export function mergeSettings(base: Settings, ours: Settings, disk: Settings): Settings {
  const out: Settings = { ...disk };
  const keys = new Set([...Object.keys(base), ...Object.keys(ours)]);
  for (const key of keys) {
    if (same(ours[key], base[key])) continue;
    if (ours[key] === undefined) delete out[key];
    else out[key] = ours[key];
  }
  return out;
}

/** 另一个窗口写入后调用：把磁盘值记为基线，接下来的写入就不会把它当成本窗口的改动。 */
export function markSettingsSynced(settings: Settings): void {
  lastSynced = settings;
}

export const mergingSettingsStorage: StateStorage = {
  getItem: (name) => {
    const raw = localStorage.getItem(name);
    lastSynced = parse(raw)?.state?.settings ?? {};
    return raw;
  },
  setItem: (name, value) => {
    const next = parse(value);
    const ours = next?.state?.settings;
    if (!next || !ours) {
      localStorage.setItem(name, value);
      return;
    }
    const disk = parse(localStorage.getItem(name))?.state?.settings ?? {};
    const merged = mergeSettings(lastSynced, ours, disk);
    lastSynced = merged;
    localStorage.setItem(name, JSON.stringify({ ...next, state: { ...next.state, settings: merged } }));
    // 顺手把变化的键推到 SQLite 副本（防抖）：重装 / 清缓存后靠它回填
    queueSettingsPatch(changedSettings(disk, merged));
  },
  removeItem: (name) => {
    lastSynced = {};
    localStorage.removeItem(name);
  },
};