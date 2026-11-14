// ai-workbench 企微通道的 DSH 宿主插件：把「别的进程新建的会话」挂到对应工作区。
//
// 为什么需要它：DSH 的工作区表（~/.dsh/storages/workspace.json）在宿主启动时读进
// 内存（dsh-workspace 的 `[Service.init]` → `rebuildEntities()`），之后只读内存，
// 也不监听文件；会话归属还要过 `candidateMatches` 这个启动时扫盘建的索引。所以
// 企微引擎（独立的 `dsh --profile acp` 进程）写出的会话日志，桌面端宿主看不见，
// 表现为「企微里已经回话，DSH 侧边栏却要重启才出现」。
//
// 这里用宿主自己的入口补上：`Workspace.attachSession(id)` 会读会话 header 校验
// cwd、把 id 前插进 sessionIds、落盘并刷新内存快照，UI 的 workspace.follow 流随即
// 就能看到。插件只做这一件事，不碰会话内容、不删不归档。
//
// 失败模式：插件抛错会让宿主启动失败，所以 apply 全程兜底；任何能力缺失都只降级为
// 「不同步」，不能影响 DSH 本身。

import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const name = 'ai-workbench-workspace-sync';
export const inject = ['workspaceRegistry'];

/** 兜底扫描周期：正常靠 fs.watch 触发，这里只防丢事件。 */
const DEFAULT_SCAN_MS = 30000;
/** fs.watch 的防抖：一次成稿会连着写好几次文件。 */
const DEBOUNCE_MS = 800;
/** 挂不上去的会话（cwd 不属于任何工作区）隔这么久再试，避免每轮都去解压 header。 */
const RETRY_MS = 60000;

export function apply(ctx, config) {
  try {
    install(ctx, config);
  } catch (error) {
    warn(ctx, error);
  }
}

function install(ctx, config) {
  const registry = ctx?.workspaceRegistry;
  if (typeof registry?.list !== 'function') return;

  const sessionsRoot = typeof config?.sessionsRoot === 'string' && config.sessionsRoot.trim()
    ? config.sessionsRoot.trim()
    : path.join(os.homedir(), '.dsh', 'sessions');
  const scanMs = clampInterval(config?.scanIntervalMs);

  let stopped = false;
  let running = false;
  let watcher = null;
  let debounceTimer = null;
  let scanTimer = null;
  const failedUntil = new Map();

  const listWorkspaces = () => {
    try {
      const workspaces = registry.list();
      return Array.isArray(workspaces) ? workspaces : [];
    } catch {
      return [];
    }
  };

  // 与后端 session_store_dir_name 一致：去尾部 `\`（长度 > 3 才去，盘根保留），
  // 否则带尾分隔符的 workspace.path 编出来的目录名和 DSH 实盘对不上。
  const encodedName = (workspacePath) => {
    let target = String(workspacePath ?? '').trim().replace(/\//g, '\\');
    while (target.length > 3 && target.endsWith('\\')) target = target.slice(0, -1);
    let name = '--';
    for (const ch of target) {
      if (ch === '\\' || ch === '/') name += '-';
      else if (ch !== ':') name += ch;
    }
    return `${name}--`;
  };

  const archivedIds = () => {
    try {
      const ids = registry.archivedSessionIds;
      return Array.isArray(ids) ? ids : [];
    } catch {
      return [];
    }
  };

  const attach = async (sessionId, candidates) => {
    for (const workspace of candidates) {
      if (typeof workspace?.attachSession !== 'function') continue;
      // 宿主内存快照里的 sessionIds 已被 candidateMatches 过滤：里面有它
      // 说明 cwd→会话索引认识它，无需处理。磁盘上登记过但索引不认识的，
      // 这里看不到，会落到下面的先摘再挂。
      try {
        if ((workspace.sessionIds ?? []).includes(sessionId)) return true;
      } catch {
        // 读不到就继续按未归组处理。
      }
      try {
        // 关键：外部进程建的会话可能已经躺在 sessionIds 里（我们登记的），但宿主
        // 的 cwd→会话索引还不认识它。这种情况直接 attachSession 会走「已登记」分支，
        // 跳过 header 读取，接着被 candidateMatches 剪掉。先摘再挂，才会重新读 header、
        // 重新建立索引，会话这才真正显示在工作区下。
        if (typeof workspace.detachSession === 'function') {
          await workspace.detachSession(sessionId);
        }
        await workspace.attachSession(sessionId);
        return true;
      } catch {
        // cwd 不匹配 / session 已失效：换下一个候选，全部失败由调用方退避。
      }
    }
    return false;
  };

  const scan = async () => {
    let entries;
    try {
      entries = fs.readdirSync(sessionsRoot, { withFileTypes: true });
    } catch {
      return; // DSH 还没落过盘 / HOME 不对：静默跳过。
    }
    const workspaces = listWorkspaces();
    if (workspaces.length === 0) return;
    const byDir = new Map();
    for (const workspace of workspaces) byDir.set(encodedName(workspace.path).toLowerCase(), workspace);
    const archived = new Set(archivedIds());
    const attached = new Set();
    for (const workspace of workspaces) {
      try {
        for (const id of workspace.sessionIds ?? []) attached.add(id);
      } catch {
        // 取不到的会话清单就跳过这个工作区，不影响其它工作区同步。
      }
    }
    const now = Date.now();
    for (const entry of entries) {
      if (stopped) return;
      if (!entry.isDirectory()) continue;
      const matched = byDir.get(entry.name.toLowerCase());
      // 编码对不上时退回「逐个工作区试 attach」：attachSession 自己会按 cwd 校验。
      const candidates = matched ? [matched] : workspaces;
      let sessionEntries;
      try {
        sessionEntries = fs.readdirSync(path.join(sessionsRoot, entry.name), { withFileTypes: true });
      } catch {
        continue;
      }
      for (const session of sessionEntries) {
        if (stopped) return;
        if (!session.isDirectory()) continue;
        const id = session.name;
        if (!id || attached.has(id) || archived.has(id)) continue;
        const until = failedUntil.get(id);
        if (until !== undefined && until > now) continue;
        if (await attach(id, candidates)) {
          attached.add(id);
          failedUntil.delete(id);
        } else {
          failedUntil.set(id, now + RETRY_MS);
        }
      }
    }
  };

  const run = () => {
    if (stopped || running) return;
    running = true;
    Promise.resolve()
      .then(scan)
      .catch((error) => warn(ctx, error))
      .finally(() => {
        running = false;
      });
  };

  const schedule = () => {
    if (stopped) return;
    if (debounceTimer !== null) clearTimeout(debounceTimer);
    debounceTimer = setTimeout(() => {
      debounceTimer = null;
      run();
    }, DEBOUNCE_MS);
    debounceTimer.unref?.();
  };

  // 落一份「已加载」回执：设置页/排障可以据此判断宿主到底有没有装起插件
  // （宿主只会在启动时读 profile，读不到插件时这里是空的）。
  try {
    const marker = path.join(path.dirname(fileURLToPath(import.meta.url)), 'applied.json');
    fs.writeFileSync(
      marker,
      JSON.stringify({ at: new Date().toISOString(), pid: process.pid, sessionsRoot }),
    );
  } catch {
    // 写不进去只影响排障，不影响同步。
  }

  scanTimer = setInterval(run, scanMs);
  scanTimer.unref?.();
  try {
    watcher = fs.watch(sessionsRoot, { recursive: true }, schedule);
    watcher.on?.('error', () => {
      try { watcher?.close?.(); } catch { /* 关闭失败无所谓 */ }
      watcher = null;
    });
  } catch {
    watcher = null; // 平台不支持 recursive：退化为定时扫描。
  }
  run();

  if (typeof ctx.effect === 'function') {
    ctx.effect(() => () => {
      stopped = true;
      if (debounceTimer !== null) clearTimeout(debounceTimer);
      if (scanTimer !== null) clearInterval(scanTimer);
      try { watcher?.close?.(); } catch { /* 关闭失败无所谓 */ }
      watcher = null;
    }, 'ai-workbench: workspace sync');
  }
}

function clampInterval(value) {
  const parsed = Number(value);
  if (!Number.isFinite(parsed) || parsed < 5000) return DEFAULT_SCAN_MS;
  return Math.min(parsed, 300000);
}

function warn(ctx, error) {
  try {
    ctx?.logger?.warn?.(`[ai-workbench] 工作区同步未生效：${error?.message ?? error}`);
  } catch {
    // 连日志都打不出去也不能影响宿主。
  }
}
