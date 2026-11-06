// DSH 文档任务的宿主工具闸门；不加载项目脚本，不允许命令或权限提升。
import fs from 'node:fs';
import path from 'node:path';

export const name = 'ai-workbench-docgen-guard';
export const inject = ['tools'];

function realTarget(target) {
  let current = path.resolve(target);
  const rest = [];
  while (!fs.existsSync(current)) {
    const parent = path.dirname(current);
    if (parent === current) throw new Error('Unresolvable target');
    rest.unshift(path.basename(current));
    current = parent;
  }
  return path.join(fs.realpathSync(current), ...rest);
}

function within(root, target) {
  const rel = path.relative(root, target);
  return rel !== '' && rel !== '..' && !rel.startsWith(`..${path.sep}`) && !path.isAbsolute(rel);
}

export function apply(ctx, config) {
  const root = fs.realpathSync(config.workspace);
  const doc = realTarget(path.join(root, config.docDir));
  if (!within(root, doc)) throw new Error('Document directory must be inside isolated workspace');
  ctx.tools.guard((execution) => {
    const args = execution.arguments ?? {};
    if (['read', 'glob', 'grep'].includes(execution.name)) {
      const requested = execution.name === 'read' ? args.file_path : (args.path ?? '.');
      if (typeof requested !== 'string') return 'Missing read path';
      try {
        const target = realTarget(path.resolve(root, requested));
        if (target !== root && !within(root, target)) return 'Read must stay inside isolated workspace';
        const parts = path.relative(root, target).split(path.sep);
        if (parts.some((part) => part.toLowerCase() === '.git')) return 'Git metadata is not part of the document source';
      } catch {
        return 'Cannot safely resolve read path';
      }
      return;
    }
    if (config.readonly || !['write', 'edit'].includes(execution.name)) {
      return 'Document task denies commands, deletion, delegation and all other side effects';
    }
    if (args.sandbox_permissions || args.sandbox_mode || args.sandboxMode) return 'Sandbox escalation is forbidden';
    if (typeof args.file_path !== 'string' || !args.file_path.trim()) return 'Missing document file path';
    try {
      const target = realTarget(path.resolve(root, args.file_path));
      if (path.relative(root, target).split(path.sep).some((part) => part.toLowerCase() === '.git')) return 'Git metadata cannot be written';
      if (!within(doc, target)) return 'Write must stay inside the document directory';
    } catch {
      return 'Cannot safely resolve document path';
    }
  });
  // 宿主在发送模型任务前核对这份回执；插件未加载时 fail-closed。
  if (config.readyFile) {
    fs.writeFileSync(config.readyFile, JSON.stringify({ workspace: config.workspace, docDir: config.docDir, readonly: config.readonly }));
  }
}
