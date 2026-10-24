//! `symbols`: where is this thing defined? No LSP in the app, so this is a deliberately
//! small heuristic — keyword words (`fn`, `class`, `def`, `struct`…) followed by a name —
//! and the tool text says so. Cheaper and far less noisy than grepping the whole repo for
//! a common identifier, and easy to swap for tree-sitter later.

use std::path::Path;

use serde_json::Value;

use crate::agent_fs::{arg_str, walk, Scope};

/// Words that start a definition, and the `kind` we report for them.
const KEYWORDS: &[(&str, &str)] = &[
    ("fn", "fn"),
    ("func", "fn"),
    ("fun", "fn"),
    ("def", "def"),
    ("function", "function"),
    ("class", "class"),
    ("interface", "interface"),
    ("struct", "struct"),
    ("enum", "enum"),
    ("trait", "trait"),
    ("impl", "impl"),
    ("type", "type"),
    ("const", "const"),
    ("mod", "mod"),
    ("namespace", "namespace"),
    ("protocol", "protocol"),
    ("record", "record"),
];

/// Extensions worth scanning; everything else is skipped without being read.
const SOURCE_EXT: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "vue", "svelte", "py", "go", "java", "kt", "kts", "cs", "cpp", "cc",
    "c", "h", "hpp", "rb", "php", "swift", "scala", "dart", "lua", "sh", "ps1",
];

const HIT_CAP: usize = 50;
/// One definition line, clipped: a signature is a pointer, not a quote.
const LINE_CAP: usize = 160;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Symbol {
    pub(crate) line: usize,
    pub(crate) kind: String,
    pub(crate) name: String,
    pub(crate) text: String,
}

/// Definition lines in `source` whose declared name contains `query` (case-insensitive).
/// Pure: no filesystem, no cancellation — the walker owns those.
pub(crate) fn scan_text(source: &str, query: &str, kind: Option<&str>) -> Vec<Symbol> {
    let needle = query.trim().to_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    let want = kind.map(str::trim).filter(|k| !k.is_empty());
    let mut out = Vec::new();
    for (idx, raw) in source.lines().enumerate() {
        let line = raw.trim();
        // Minified / generated lines and comments are not definitions worth citing.
        if line.len() < 4 || line.len() > 300 || is_comment(line) {
            continue;
        }
        if !line.to_lowercase().contains(&needle) {
            continue;
        }
        let Some((kind_name, name)) = definition_of(line) else {
            continue;
        };
        if let Some(want) = want {
            if !kind_name.eq_ignore_ascii_case(want) {
                continue;
            }
        }
        if !name.to_lowercase().contains(&needle) {
            continue;
        }
        out.push(Symbol {
            line: idx + 1,
            kind: kind_name.to_string(),
            name,
            text: clip(line, LINE_CAP),
        });
    }
    out
}

fn is_comment(line: &str) -> bool {
    line.starts_with("//")
        || line.starts_with("/*")
        || line.starts_with('*')
        || line.starts_with("--")
        // `#` 注释，但保留 Rust 属性 `#[derive(...)]`
        || (line.starts_with('#') && !line.starts_with("#["))
}

/// `(kind, name)` when the line declares something. Only whole words count, so
/// `my_function_name` inside a string does not look like a `function` declaration.
fn definition_of(line: &str) -> Option<(&'static str, String)> {
    let words = words(line);
    for (i, word) in words.iter().enumerate() {
        let Some((_, kind)) = KEYWORDS.iter().find(|(kw, _)| *kw == word.as_str()) else {
            continue;
        };
        let Some(next) = words.get(i + 1) else { continue };
        // The name has to start with an identifier character: `fn(` is a call, `fn foo` a def.
        let first = next.chars().next()?;
        if !(first.is_alphabetic() || first == '_' || first == '$') {
            continue;
        }
        // `type` / `const` show up in JSON-ish and object literals; those lines quote keys.
        if line.starts_with('"') || line.starts_with('{') || line.starts_with('<') {
            continue;
        }
        return Some((kind, next.to_string()));
    }
    None
}

/// Identifier-ish words in order, split on everything else (`pub(crate) fn foo<T>` →
/// pub, crate, fn, foo, T).
fn words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    for ch in line.chars() {
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            cur.push(ch);
        } else if !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn clip(text: &str, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text.to_string();
    }
    let cut: String = text.chars().take(cap).collect();
    format!("{cut}…")
}

fn is_source_file(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    SOURCE_EXT.contains(&ext.as_str())
}

pub(crate) fn symbols(scope: &Scope, args: &Value, request_id: &str) -> Result<String, String> {
    let query = arg_str(args, "query");
    if query.is_empty() {
        return Err("缺少符号名（query）".into());
    }
    let kind = arg_str(args, "kind");
    let kind = if kind.is_empty() { None } else { Some(kind.as_str()) };
    let max = args
        .get("max")
        .and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())))
        .map(|n| n as usize)
        .unwrap_or(20)
        .clamp(1, HIT_CAP);
    let start = scope.resolve(&arg_str(args, "path"))?;
    if !start.exists() {
        return Err(format!("路径不存在：{}", scope.show(&start)));
    }

    let mut hits: Vec<String> = Vec::new();
    let mut scanned = 0usize;
    let mut skipped_big = 0usize;
    let mut capped = false;
    let mut scan = |path: &Path| {
        if !is_source_file(path) {
            return true;
        }
        let Ok(meta) = path.metadata() else { return true };
        // 生成物 / 打包产物读进来只会拖慢搜索
        if meta.len() > 1_000_000 {
            skipped_big += 1;
            return true;
        }
        let Ok(text) = std::fs::read_to_string(path) else { return true };
        scanned += 1;
        let shown = scope.show(path);
        for sym in scan_text(&text, &query, kind) {
            hits.push(format!("{shown}:{}: {}  [{}]", sym.line, sym.text, sym.kind));
            if hits.len() >= max {
                capped = true;
                return false;
            }
        }
        true
    };
    if start.is_dir() {
        walk(&start, request_id, false, &mut scan)?;
    } else {
        scan(&start);
    }

    if hits.is_empty() {
        return Ok(format!(
            "没有找到「{query}」的定义行（启发式：只看关键字 + 名字，扫了 {scanned} 个源文件{}）。\
             可以换 grep 搜引用，或用 find_files 缩小范围。",
            if skipped_big > 0 { format!("，跳过 {skipped_big} 个大文件") } else { String::new() }
        ));
    }
    let mut out = format!("「{query}」的定义（启发式，扫了 {scanned} 个源文件）：\n");
    out.push_str(&hits.join("\n"));
    if capped {
        out.push_str(&format!("\n（已到上限 {max} 条，可能有更多：用 path 缩小范围或调大 max）"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_definitions_in_js_and_rs() {
        let js = r#"
import { readFile } from "fs";
// function loadConfig is mentioned in a comment
export async function loadConfig(path) {
  const cached = 1;
}
class ConfigStore {}
const helper = () => {};
"#;
        let hits = scan_text(js, "loadconfig", None);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].line, 4);
        assert_eq!(hits[0].kind, "function");
        assert!(hits[0].text.starts_with("export async function loadConfig"));

        let rs = r#"
pub(crate) struct Scope { root: Option<PathBuf> }
impl Scope {
    pub(crate) fn resolve(&self, user_path: &str) -> Result<PathBuf, String> { }
}
fn scope() {}
"#;
        let hits = scan_text(rs, "scope", None);
        let kinds: Vec<_> = hits.iter().map(|h| h.kind.as_str()).collect();
        assert_eq!(kinds, vec!["struct", "impl", "fn"], "{hits:?}");
        // kind 过滤：只要实现块
        let impls = scan_text(rs, "scope", Some("impl"));
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].line, 3);
    }

    #[test]
    fn ignores_calls_comments_and_json_keys() {
        let src = r#"
fn main() { load(); }
    load_config(path);
// fn load_config() is commented out
* function load_config
"type": "load_config",
"#;
        assert!(scan_text(src, "load_config", None).is_empty());
        // 名字里带查询词但不是同一个符号也算命中（启发式，比漏掉好）
        let near = scan_text("fn load_config_all() {}\n", "load_config", None);
        assert_eq!(near.len(), 1);
        assert_eq!(near[0].name, "load_config_all");
    }

    #[test]
    fn clips_long_lines_and_respects_kind() {
        // 160 < 行长 < 300：既被裁剪，又没到「这行是生成物」的丢弃线
        let long = format!("fn {}({}) {{}}", "x".repeat(40), "a: u32, ".repeat(15));
        let hits = scan_text(&long, "xxxx", None);
        assert_eq!(hits.len(), 1, "{}", long.len());
        assert!(hits[0].text.chars().count() <= LINE_CAP + 1);
        assert!(scan_text("class Foo {}\n", "foo", Some("fn")).is_empty());
        // 超过 300 字符的行当作压缩产物，直接丢
        let minified = format!("fn {}(a: u32) {{}}", "y".repeat(400));
        assert!(scan_text(&minified, "yyy", None).is_empty());
    }
}
