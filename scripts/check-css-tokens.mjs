// Guards the design-token conventions in AGENTS.md. Run: npm run check:css
// A line ending with `/* token-ok: <reason> */` is exempt (keep the reason honest).
import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";

const ROOT = new URL("..", import.meta.url).pathname.replace(/^\/([A-Za-z]:)/, "$1");
const SRC = join(ROOT, "src");
// tokens.css is where literal values are supposed to live.
const SKIP = new Set([join(SRC, "styles", "tokens.css")]);
// Custom properties set at runtime from TSX, so their fallback is live.
const RUNTIME_VARS = new Set(["--ai-slider-progress"]);

const BLUES = /#(5b8def|58a6ff|3b82f6|4a90e2)\b|rgba?\(\s*(91,\s*141,\s*239|88,\s*166,\s*255|59,\s*130,\s*246|100,\s*180,\s*255|53,\s*106,\s*158)\b/i;

const RULES = [
  { id: "transition-all", re: /transition\s*:[^;]*\ball\b/, msg: "禁止 transition: all（窗口 resize 性能）" },
  { id: "hard-blue", re: BLUES, msg: "写死蓝色，改用 var(--accent) 或 color-mix" },
  {
    id: "radius-px",
    re: /border(-[a-z]+)*-radius\s*:[^;]*?(?<![\w.-])(3|4|5|6|7|8|9|10|11|12|13|14|15|16|999|9999)px/,
    msg: "圆角用 var(--radius-sm|md|lg|xl|full)",
  },
];

function walk(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walk(p, out);
    else if (p.endsWith(".css") && !SKIP.has(p)) out.push(p);
  }
  return out;
}

function walkSources(dir, out = []) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) walkSources(p, out);
    else if (/\.(css|tsx?)$/.test(p)) out.push(p);
  }
  return out;
}

// Anything declared as `--x:` in CSS or written as a "--x" string in TS/TSX (inline style).
const defined = new Set(RUNTIME_VARS);
for (const file of walkSources(SRC)) {
  const text = readFileSync(file, "utf8");
  const re = file.endsWith(".css") ? /(--[\w-]+)\s*:/g : /["'`](--[\w-]+)["'`]/g;
  for (const m of text.matchAll(re)) defined.add(m[1]);
}

const problems = [];
for (const file of [...walk(SRC), join(SRC, "styles", "tokens.css")]) {
  const lines = readFileSync(file, "utf8").split(/\r?\n/);
  lines.forEach((line, i) => {
    if (/token-ok:/.test(line)) return;
    const code = line.replace(/\/\*.*?\*\//g, "");
    for (const m of code.matchAll(/var\(\s*(--[\w-]+)/g)) {
      if (!defined.has(m[1])) problems.push([file, i + 1, `变量 ${m[1]} 未定义（会静默失效）`, line.trim()]);
    }
    if (SKIP.has(file)) return;
    for (const r of RULES) {
      if (r.re.test(code)) problems.push([file, i + 1, r.msg, line.trim()]);
    }
    for (const m of code.matchAll(/var\(\s*(--[\w-]+)\s*,/g)) {
      if (!RUNTIME_VARS.has(m[1])) problems.push([file, i + 1, `变量 ${m[1]} 带回退值：已定义就删回退，未定义就改用现有 token`, line.trim()]);
    }
  });
}

if (problems.length) {
  for (const [f, l, msg, src] of problems) console.log(`${relative(ROOT, f)}:${l}  ${msg}\n    ${src}`);
  console.log(`\n${problems.length} 处不符合样式约定`);
  process.exit(1);
}
console.log("check:css 通过");
