// 内置只读 git MCP server（stdio，换行分隔 JSON-RPC 2.0）。
//
// 由宿主（AI Workbench）随 DSH 文档引擎的 ACP `session/new|resume` 的 mcpServers 下发，
// 让「逆向文档」任务自己查完整 diff / 历史 / blame，而不是由宿主把 git 输出拼进提示词。
//
// 安全边界：
// - 只暴露只读子命令（log / diff / show / blame / status / names）；
// - 参数结构化（range / path / max / lines），不接收自由命令行，不拼 shell；
// - `execFile('git', args)` 直接执行，`path` 必须落在 workspace 内；
// - 拒绝以 `-` 开头的 range / rev（防止被当成 flag）；
// - 输出按字符截断，失败返回 stderr 尾部。
//
// 参数：argv[2] = workspace 绝对路径。

import { createInterface } from 'node:readline';
import { execFile } from 'node:child_process';
import path from 'node:path';

const WORKSPACE = process.argv[2];
if (!WORKSPACE || !path.isAbsolute(WORKSPACE)) {
  process.stderr.write('docgen-git-mcp: missing workspace argument\n');
  process.exit(2);
}

const PROTOCOL_VERSION = '2024-11-05';
const OUTPUT_CAP = 30_000;
const MAX_BUFFER = 4 * 1024 * 1024;
const GIT_TIMEOUT_MS = 30_000;

const RANGE_RE = /^[A-Za-z0-9_.~^@/:-]+$/;
const LINES_RE = /^\d+(,\d+)?$/;

function textResult(text, isError = false) {
  return { content: [{ type: 'text', text }], ...(isError ? { isError: true } : {}) };
}

function clip(text) {
  if (text.length <= OUTPUT_CAP) return text;
  return `${text.slice(0, OUTPUT_CAP)}\n…（输出已截断，请缩小 range 或 path 重查）`;
}

function strArg(args, key) {
  const v = args?.[key];
  return typeof v === 'string' ? v.trim() : '';
}

function rangeArg(args, key = 'range') {
  const raw = strArg(args, key);
  if (!raw) return '';
  if (raw.startsWith('-') || !RANGE_RE.test(raw)) {
    throw new Error(`invalid ${key}: 只允许提交/区间字符（不含空格与引号）`);
  }
  return raw;
}

function pathArg(args, key = 'path') {
  const raw = strArg(args, key);
  if (!raw) return '';
  const resolved = path.resolve(WORKSPACE, raw);
  const rel = path.relative(WORKSPACE, resolved);
  if (rel.startsWith('..') || path.isAbsolute(rel)) {
    throw new Error(`path 必须位于工作区内：${raw}`);
  }
  if (rel === '') return '.';
  return rel.split(path.sep).join('/');
}

function intArg(args, key, fallback, min, max) {
  const v = args?.[key];
  let n = fallback;
  if (typeof v === 'number' && Number.isInteger(v)) n = v;
  else if (typeof v === 'string' && /^\d+$/.test(v.trim())) n = Number(v.trim());
  return Math.min(max, Math.max(min, n));
}

function runGit(args) {
  return new Promise((resolve, reject) => {
    execFile(
      'git',
      args,
      {
        cwd: WORKSPACE,
        maxBuffer: MAX_BUFFER,
        timeout: GIT_TIMEOUT_MS,
        windowsHide: true,
        env: { ...process.env, GIT_PAGER: 'cat', PAGER: 'cat', GIT_TERMINAL_PROMPT: '0' },
      },
      (err, stdout, stderr) => {
        if (err) {
          const tail = String(stderr || err.message || '')
            .trim()
            .split('\n')
            .slice(-6)
            .join('\n');
          reject(new Error(`git_error: ${tail}`));
          return;
        }
        resolve(clip(String(stdout || '')));
      },
    );
  });
}

const TOOLS = [
  {
    name: 'git_log',
    description:
      'Read recent commit history: `git log --format="%h %ad %s" --date=short`. Optional range (e.g. "abc123..HEAD"), path filter and max count (default 20, up to 50).',
    inputSchema: {
      type: 'object',
      properties: {
        range: { type: 'string', description: 'Commit range, e.g. abc123..HEAD (optional)' },
        path: { type: 'string', description: 'Limit to this file or folder (optional, workspace-relative)' },
        max: { type: 'number', description: 'Max commits (default 20, up to 50)' },
      },
    },
  },
  {
    name: 'git_diff',
    description:
      'Read the real diff (not a truncated excerpt): `git diff [-p|--stat] [range] -- path`. No range = working tree vs index. Use stat=true for per-file line counts.',
    inputSchema: {
      type: 'object',
      properties: {
        range: { type: 'string', description: 'Commit range, e.g. abc123..HEAD (optional)' },
        path: { type: 'string', description: 'Limit to this file or folder (optional)' },
        stat: { type: 'boolean', description: 'Return --stat summary instead of the full patch' },
      },
    },
  },
  {
    name: 'git_show',
    description: 'Show one commit with its patch: `git show --stat -p <rev>`.',
    inputSchema: {
      type: 'object',
      properties: { rev: { type: 'string', description: 'Commit hash or ref (must not start with -)' } },
      required: ['rev'],
    },
  },
  {
    name: 'git_blame',
    description: 'Blame one file, optionally a line range like "10,40".',
    inputSchema: {
      type: 'object',
      properties: {
        path: { type: 'string', description: 'File path (workspace-relative)' },
        lines: { type: 'string', description: 'Line range "start,end" (optional)' },
      },
      required: ['path'],
    },
  },
  {
    name: 'git_status',
    description: 'Working tree status: `git status --porcelain=v1 -b`.',
    inputSchema: { type: 'object', properties: {} },
  },
  {
    name: 'git_names',
    description: 'Changed file names with status: `git diff --name-status [range]`.',
    inputSchema: {
      type: 'object',
      properties: { range: { type: 'string', description: 'Commit range, e.g. abc123..HEAD (optional)' } },
    },
  },
];

async function callTool(name, args) {
  switch (name) {
    case 'git_log': {
      const range = rangeArg(args);
      const p = pathArg(args);
      const max = intArg(args, 'max', 20, 1, 50);
      const argv = ['--no-pager', 'log', '--format=%h %ad %s', '--date=short', '-n', String(max)];
      if (range) argv.push(range);
      argv.push('--');
      if (p) argv.push(p);
      return runGit(argv);
    }
    case 'git_diff': {
      const range = rangeArg(args);
      const p = pathArg(args);
      const argv = ['--no-pager', 'diff'];
      argv.push(args?.stat === true ? '--stat' : '-p');
      if (range) argv.push(range);
      argv.push('--');
      if (p) argv.push(p);
      return runGit(argv);
    }
    case 'git_show': {
      const rev = rangeArg(args, 'rev');
      if (!rev) throw new Error('缺少 rev');
      return runGit(['--no-pager', 'show', '--stat', '-p', rev]);
    }
    case 'git_blame': {
      const p = pathArg(args);
      if (!p) throw new Error('缺少 path');
      const lines = strArg(args, 'lines');
      if (lines && !LINES_RE.test(lines)) throw new Error('lines 必须是 "start,end" 形式');
      const argv = ['--no-pager', 'blame'];
      if (lines) argv.push(`-L`, lines);
      argv.push('--', p);
      return runGit(argv);
    }
    case 'git_status':
      return runGit(['--no-pager', 'status', '--porcelain=v1', '-b']);
    case 'git_names': {
      const range = rangeArg(args);
      const argv = ['--no-pager', 'diff', '--name-status'];
      if (range) argv.push(range);
      return runGit(argv);
    }
    default:
      throw new Error(`unknown tool: ${name}`);
  }
}

function rpcResult(id, result) {
  return { jsonrpc: '2.0', id, result };
}

function rpcError(id, code, message) {
  return { jsonrpc: '2.0', id, error: { code, message } };
}

async function handleMessage(msg, write) {
  const { id, method, params } = msg || {};
  const isNotification = id === undefined || id === null;
  switch (method) {
    case 'initialize':
      write(rpcResult(id, {
        protocolVersion: PROTOCOL_VERSION,
        capabilities: { tools: {} },
        serverInfo: { name: 'docgen-git', version: '1.0.0' },
      }));
      return;
    case 'notifications/initialized':
    case 'notifications/cancelled':
      return;
    case 'ping':
      if (!isNotification) write(rpcResult(id, {}));
      return;
    case 'tools/list':
      write(rpcResult(id, { tools: TOOLS }));
      return;
    case 'tools/call': {
      const name = params?.name;
      if (!TOOLS.some((t) => t.name === name)) {
        write(rpcError(id, -32602, `unknown tool: ${name}`));
        return;
      }
      try {
        const text = await callTool(name, params.arguments || {});
        write(rpcResult(id, textResult(text || '（无输出）')));
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        write(rpcResult(id, textResult(message, true)));
      }
      return;
    }
    default:
      if (!isNotification) write(rpcError(id, -32601, `method not found: ${method}`));
  }
}

function main() {
  const rl = createInterface({ input: process.stdin });
  rl.on('line', (line) => {
    let msg;
    try {
      msg = JSON.parse(line);
    } catch {
      return; // 非 JSON 行直接丢（与宿主侧 mcp.rs 行为一致）
    }
    handleMessage(msg, (out) => {
      process.stdout.write(`${JSON.stringify(out)}\n`);
    }).catch((e) => {
      process.stderr.write(`docgen-git-mcp: ${e?.message || e}\n`);
    });
  });
  rl.on('close', () => {
    // 不用 process.exit：等 stdout 写完自然退出。
  });
}

main();
