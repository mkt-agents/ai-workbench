// ai-workbench 企微渠道的 DSH 宿主插件：给会话标题加「企业微信 · 」前缀。
//
// 为什么只改标题：DSH 桌面端装着的 dsh-im（@xmanrui/dsh-im）按标题前缀识别 IM
// 通道——`parseSessionChannelTitle` 命中「企业微信 · 」就把前缀文字隐去、在行首画
// 16×16 企微图标。而 ACP 面没有 rename / set_title，`createUserMessage` 的 source
// 也固定是 `{kind:'user'}`（不带 rpcId），dsh-im 自己那套 `wecom-<uuid>` 判定对
// ACP 会话永远不命中，所以前缀只能由这边补。
//
// 怎么补：`session/title` 是 log-only 事件，标题投影是 last-wins 折叠；DSH 自己写完
// 一条之后，再追加一条同源、只换文本的 `session/title` 即可。不用
// `sessionTitle.rename()`——它写 `source:{kind:'user'}`，会把标题钉死、让 DSH 不再
// 按后续用户消息更新标题；这里保留原 source 和 messageSeqs，自动标题照旧。
//
// 失败模式：这个进程就是企微机器人的 DSH 引擎，插件抛错会让 ACP 起不来（等于整个
// 机器人掉线）。所以 apply 全程兜底：任何能力缺失或异常都只降级为「没有前缀」。

export const name = 'ai-workbench-channel-title-prefix';
export const inject = ['sessions'];

/** 与 dsh-im 的中文通道标签逐字一致（`·` 是 U+00B7，两侧各一个半角空格）。 */
const DEFAULT_LABEL = '企业微信';

/**
 * 只认 DSH 写出来的合法标题数据：
 * - `user` 源（显式改名）的 messageSeqs 必须为空；
 * - `fallback` / `provider` 源必须引用到具体用户消息，provider 源还要带 provider 名。
 * 结构不认识就放手，绝不往日志里写半截数据。
 */
function validTitleData(data) {
  if (typeof data?.title !== 'string' || !data.title.trim() || !Array.isArray(data.messageSeqs)) {
    return false;
  }
  const kind = data.source?.kind;
  if (kind === 'user') return data.messageSeqs.length === 0;
  if (kind !== 'fallback' && kind !== 'provider') return false;
  if (data.messageSeqs.length === 0) return false;
  if (data.messageSeqs.some((seq) => !Number.isSafeInteger(seq) || seq < 0)) return false;
  return kind !== 'provider' || Boolean(data.source?.provider);
}

export function apply(ctx, config) {
  try {
    install(ctx, config);
  } catch (error) {
    // 前缀是加分项：装不上就静默降级，但留一条日志方便排查。
    warn(ctx, error);
  }
}

function install(ctx, config) {
  const label = typeof config?.label === 'string' && config.label.trim()
    ? config.label.trim()
    : DEFAULT_LABEL;
  const prefix = `${label} · `;
  if (typeof ctx?.on !== 'function'
    || typeof ctx?.sessions?.get !== 'function'
    || typeof ctx?.sessions?.list !== 'function') return;

  /** 同一会话同一时刻只排一个微任务，避免连续事件叠出多条标题。 */
  const pending = new Map();
  /** 恢复进来的会话不会重发标题事件：每条会话首个普通事件时补查一次。 */
  const touched = new WeakSet();
  let closed = false;

  const latestTitle = (session) => {
    // 优先读标题服务的快照（O(1)）；服务不在位时退回事件日志（DSH 已标记 deprecated，仅兜底）。
    try {
      const snapshot = ctx.sessionTitle?.get?.(session);
      if (snapshot) return snapshot;
    } catch {
      // 快照读失败就继续走日志，别让一次读取异常吃掉整条前缀。
    }
    try {
      const events = typeof session?.snapshotEvents === 'function' ? session.snapshotEvents() : [];
      if (!Array.isArray(events)) return undefined;
      return events.findLast((event) => event.type === 'session/title')?.data;
    } catch {
      return undefined;
    }
  };

  const decorate = (session) => {
    if (closed || !session || typeof session.append !== 'function') return;
    // 已经被关掉 / 被移出 store 的会话不再写：append 会因未宣告而抛错。
    if (ctx.sessions.get(session.id) !== session) return;
    const snapshot = latestTitle(session);
    if (!validTitleData(snapshot)) return;
    if (snapshot.title.startsWith(prefix)) return;
    session.append('session/title', {
      title: `${prefix}${snapshot.title}`,
      messageSeqs: [...snapshot.messageSeqs],
      source: { ...snapshot.source },
    });
  };

  const enqueue = (session) => {
    if (closed || !session || pending.has(session)) return;
    // 必须离开当前发布会话边界：在 session/event 里同步 append 会被 DSH 判为
    // reentrant 直接拒绝（acceptance/publication boundary 未关）。
    const task = Promise.resolve().then(() => {
      pending.delete(session);
      try {
        decorate(session);
      } catch (error) {
        warn(ctx, error);
      }
    });
    pending.set(session, task);
  };

  const onEvent = (session, event) => {
    if (closed || !session) return;
    if (event?.type === 'session/title') {
      enqueue(session);
      return;
    }
    if (touched.has(session)) return;
    touched.add(session);
    enqueue(session);
  };

  ctx.on('session/event', onEvent, { global: true });
  ctx.on('session/created', (session) => enqueue(session), { global: true });
  if (typeof ctx.effect === 'function') {
    ctx.effect(() => () => {
      closed = true;
      pending.clear();
    }, 'ai-workbench: session title prefix');
  }
  // 引擎重启后会话是懒加载的，这里补一遍已在内存里的（正常为空，不漏即可）。
  try {
    for (const session of ctx.sessions.list()) enqueue(session);
  } catch {
    // 会话服务还没就绪：后续事件会补上。
  }
}

function warn(ctx, error) {
  try {
    ctx?.logger?.warn?.(`[ai-workbench] 会话标题前缀未生效：${error?.message ?? error}`);
  } catch {
    // 连日志都打不出去也不能影响引擎。
  }
}
