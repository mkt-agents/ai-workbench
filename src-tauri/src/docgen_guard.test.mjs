import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { apply } from './docgen_guard.mjs';

test('document guard allows only scoped writes and readonly review', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'docgen-guard-test-'));
  try {
    fs.mkdirSync(path.join(root, 'doc'));
    let guard;
    const ctx = { tools: { guard(fn) { guard = fn; } } };
    const readyFile = path.join(root, 'guard-ready.json');
    apply(ctx, { workspace: root, docDir: 'doc', readonly: false, readyFile });
    assert.deepEqual(JSON.parse(fs.readFileSync(readyFile, 'utf8')), { workspace: root, docDir: 'doc', readonly: false });
    const check = (name, args = {}) => guard({ name, arguments: args });
    assert.equal(check('write', { file_path: 'doc/new.md' }), undefined);
    assert.equal(check('edit', { file_path: path.join(root, 'doc', 'a.md') }), undefined);
    assert.ok(check('write', { file_path: 'src/code.rs' }));
    assert.ok(check('write', { file_path: 'doc/../../user.txt' }));
    assert.ok(check('write', { file_path: 'doc' }));
    assert.ok(check('write', { file_path: 'docs/a.md' }));
    assert.ok(check('write', { file_path: 'doc/.git/config' }));
    for (const name of ['bash', 'pwsh', 'apply_patch', 'subagent', 'unknown']) assert.ok(check(name));
    // 只读 git MCP：这些工具放行（服务自身只读），其余 git 名字/命令仍拒绝。
    for (const name of ['git_log', 'git_diff', 'git_show', 'git_blame', 'git_status', 'git_names']) {
      assert.equal(check(name, { range: 'a..b' }), undefined, name);
      assert.equal(check(`mcp__docgen-git__${name}`, {}), undefined, `${name} 命名空间`);
    }
    assert.ok(check('git_push'));
    assert.ok(check('mcp__docgen-git__git_commit'));
    assert.ok(check('write', { file_path: 'doc/a.md', sandbox_permissions: 'require_escalated' }));
    apply(ctx, { workspace: root, docDir: 'doc', readonly: true });
    assert.ok(check('write', { file_path: 'doc/a.md' }));
    assert.ok(check('edit', { file_path: 'doc/a.md' }));
    for (const name of ['read', 'grep', 'glob']) assert.equal(check(name, { file_path: 'doc/a.md' }), undefined);
    assert.ok(check('read', { file_path: '../outside.txt' }));
    assert.ok(check('grep', { path: '../' }));
    assert.ok(check('read', { file_path: '.git' }));
    assert.throws(() => apply(ctx, { workspace: root, docDir: '../escape', readonly: false }));
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test('document guard rejects symlink or junction escape', () => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'docgen-link-test-'));
  const outside = fs.mkdtempSync(path.join(os.tmpdir(), 'docgen-outside-test-'));
  try {
    fs.mkdirSync(path.join(root, 'doc'));
    fs.symlinkSync(outside, path.join(root, 'doc', 'link'), process.platform === 'win32' ? 'junction' : 'dir');
    let guard;
    apply({ tools: { guard(fn) { guard = fn; } } }, { workspace: root, docDir: 'doc', readonly: false });
    assert.ok(guard({ name: 'write', arguments: { file_path: 'doc/link/user.txt' } }));
    assert.equal(fs.existsSync(path.join(outside, 'user.txt')), false);
  } finally {
    fs.rmSync(root, { recursive: true, force: true });
    fs.rmSync(outside, { recursive: true, force: true });
  }
});
