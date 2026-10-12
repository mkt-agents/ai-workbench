// 清理旧版本安装包，只保留当前版本
// 用法: node scripts/clean-installers.mjs
import { readdir, unlink } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';

const BUNDLES_DIR = join(process.cwd(), 'src-tauri', 'target', 'release', 'bundle');

function getCurrentVersion() {
  const pkg = JSON.parse(readFileSync(join(process.cwd(), 'package.json'), 'utf8'));
  return pkg.version;
}

async function cleanDir(dir, keepPattern) {
  try {
    const files = await readdir(dir);
    let kept = 0;
    let removed = 0;
    for (const file of files) {
      if (file.includes(keepPattern)) {
        kept++;
        continue;
      }
      await unlink(join(dir, file));
      removed++;
    }
    return { kept, removed };
  } catch {
    return { kept: 0, removed: 0 };
  }
}

async function main() {
  const version = getCurrentVersion();
  const keepPattern = `_${version}_`;

  const msiDir = join(BUNDLES_DIR, 'msi');
  const nsisDir = join(BUNDLES_DIR, 'nsis');

  const msiResult = await cleanDir(msiDir, keepPattern);
  const nsisResult = await cleanDir(nsisDir, keepPattern);

  console.log(`已清理旧版本安装包（保留 v${version}）:`);
  if (msiResult.removed > 0) console.log(`  MSI: 删除 ${msiResult.removed} 个旧版本`);
  if (nsisResult.removed > 0) console.log(`  NSIS: 删除 ${nsisResult.removed} 个旧版本`);
  if (msiResult.removed === 0 && nsisResult.removed === 0) console.log('  无需清理');
}

main().catch(console.error);
