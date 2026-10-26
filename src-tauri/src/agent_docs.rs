//! 文档与表格读取：docx（zip + XML 文本层）、xlsx/xls（calamine）、csv/tsv/md/txt 直读。
//!
//! 取舍：只取「文本层」，不做版式还原；扫描件 PDF 不在此列（没有 OCR）。
//! 单文件上限 10MB，抽取结果由调用方截断——文档可能很长，别一次灌进上下文。

use std::io::Read;
use std::path::Path;

use calamine::{open_workbook_auto, Reader};

/// 单个文档的体积上限：再大就不该整份塞进上下文了。
pub(crate) const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// 表格最多读多少行（含表头）。
pub(crate) const MAX_TABLE_ROWS: usize = 5_000;

/// 读文档成纯文本（docx 取正文段落；xlsx 取第一个表的 TSV）。
pub(crate) fn read_document(path: &Path, cap: usize) -> Result<String, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("读不到文件：{e}"))?;
    if !meta.is_file() {
        return Err("不是文件".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("文件超过 {}MB，先拆分再读", MAX_FILE_BYTES / 1024 / 1024));
    }
    let ext = extension(path);
    let text = match ext.as_str() {
        "docx" => docx_text(path)?,
        "xlsx" | "xls" => table_text(path)?,
        "pdf" => pdf_text(path)?,
        _ => std::fs::read_to_string(path).map_err(|e| format!("不是可读的文本文件：{e}"))?,
    };
    Ok(truncate(text, cap))
}

/// PDF 文本层抽取（无 OCR：扫描件会得到空/很少字）。
fn pdf_text(path: &Path) -> Result<String, String> {
    let doc = lopdf::Document::load(path).map_err(|e| format!("打不开 PDF：{e}"))?;
    let mut pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    pages.sort_unstable();
    let mut out = String::new();
    for (i, page_id) in pages.iter().enumerate() {
        // extract_text 要 1-based 页码列表
        let chunk = doc
            .extract_text(&[*page_id])
            .unwrap_or_default();
        let trimmed = chunk.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("--- 第 {} 页 ---\n{}", i + 1, trimmed));
        if out.chars().count() > 200_000 {
            out.push_str("\n…(后续页省略)");
            break;
        }
    }
    if out.trim().is_empty() {
        return Err("这份 PDF 没有可抽取的文字层（可能是扫描件，本机不做 OCR）".into());
    }
    Ok(out)
}

/// 读表格：返回 (列名, 数据行)。首行当表头，空名字补 `col_N`。
pub(crate) fn read_rows(path: &Path, limit: usize) -> Result<(Vec<String>, Vec<Vec<String>>), String> {
    // 和 read_document 同一道体积闸门：以前这里没有，2GB 的 CSV 会先全量读入再全量解析，
    // 最后才 truncate 丢掉多余行。
    let meta = std::fs::metadata(path).map_err(|e| format!("读不到表：{e}"))?;
    if !meta.is_file() {
        return Err("不是文件".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err(format!("文件超过 {}MB，先拆分再读", MAX_FILE_BYTES / 1024 / 1024));
    }
    let ext = extension(path);
    let mut rows: Vec<Vec<String>> = match ext.as_str() {
        "xlsx" | "xls" => calamine_rows(path)?,
        _ => {
            let raw = std::fs::read_to_string(path).map_err(|e| format!("读不到表：{e}"))?;
            let delimiter = if ext == "tsv" { '\t' } else { ',' };
            csv_rows(&raw, delimiter)
        }
    };
    if rows.is_empty() {
        return Err("表是空的".into());
    }
    let header = rows
        .remove(0)
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let name = name.trim();
            if name.is_empty() {
                format!("col_{}", i + 1)
            } else {
                sanitize_column(name)
            }
        })
        .collect();
    rows.truncate(limit.clamp(1, MAX_TABLE_ROWS));
    Ok((header, rows))
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase()
}

fn truncate(text: String, cap: usize) -> String {
    if text.chars().count() <= cap {
        return text;
    }
    text.chars().take(cap).collect()
}

/// 解压后的 `document.xml` 上限：磁盘上 10MB 的 zip 可以膨胀成几 GB XML（zip 炸弹）。
const DOCX_XML_CAP: u64 = 32 * 1024 * 1024;

fn docx_text(path: &Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("打开失败：{e}"))?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| format!("不是有效的 docx：{e}"))?;
    let entry = archive
        .by_name("word/document.xml")
        .map_err(|_| "docx 里没有正文（document.xml）".to_string())?;
    let clipped = entry.size() > DOCX_XML_CAP;
    let mut xml = String::new();
    entry
        .take(DOCX_XML_CAP)
        .read_to_string(&mut xml)
        .map_err(|e| format!("读取 docx 正文失败：{e}"))?;
    let mut text = xml_to_text(&xml);
    if clipped {
        text.push_str("\n…(正文超过 32MB，后面省略)");
    }
    Ok(text)
}

/// Word 的段落 / 换行 / 制表符 → 文本，再剥标签、解实体。
fn xml_to_text(xml: &str) -> String {
    let staged = xml
        .replace("</w:p>", "\n")
        .replace("<w:br/>", "\n")
        .replace("<w:br />", "\n")
        .replace("<w:tab/>", "\t")
        .replace("<w:tab />", "\t");
    let mut out = String::with_capacity(staged.len() / 2);
    let mut in_tag = false;
    for ch in staged.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    let decoded = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'");
    // 压掉多余空行，保留段落边界
    let mut lines: Vec<&str> = decoded.lines().map(str::trim_end).collect();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

fn calamine_rows(path: &Path) -> Result<Vec<Vec<String>>, String> {
    let mut workbook = open_workbook_auto(path).map_err(|e| format!("打开表格失败：{e}"))?;
    let sheet = workbook
        .worksheet_range_at(0)
        .ok_or("表里没有工作表")?
        .map_err(|e| format!("读取工作表失败：{e}"))?;
    let rows: Vec<Vec<String>> = sheet
        .rows()
        .take(MAX_TABLE_ROWS)
        .map(|row| row.iter().map(|cell| cell.to_string()).collect())
        .collect();
    Ok(rows)
}

fn table_text(path: &Path) -> Result<String, String> {
    let rows = calamine_rows(path)?;
    Ok(rows
        .into_iter()
        .map(|row| row.join("\t"))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// CSV / TSV：支持双引号包裹与 `""` 转义，够应付导出的报表。
fn csv_rows(raw: &str, delimiter: char) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = raw.trim_start_matches('\u{feff}').chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => quoted = true,
            c if c == delimiter => {
                row.push(std::mem::take(&mut field));
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|r| r.iter().any(|cell| !cell.trim().is_empty()));
    rows
}

/// 列名要能直接进 SQL 标识符：去掉引号与换行，其余保留（中文列名也合法）。
fn sanitize_column(name: &str) -> String {
    name.replace(['"', '\r', '\n'], " ").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aw-docs-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn csv_handles_quotes_and_crlf() {
        let rows = csv_rows("name,note\r\n张三,\"说,你好\"\r\n李四,\"带\"\"引号\"\"\"\r\n", ',');
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec!["张三", "说,你好"]);
        assert_eq!(rows[2][1], "带\"引号\"");
    }

    #[test]
    fn table_uses_first_row_as_header_and_fills_blanks() {
        let dir = temp_dir("table");
        let path = dir.join("data.csv");
        std::fs::write(&path, "name,,age\n张三,x,30\n").unwrap();
        let (header, rows) = read_rows(&path, 100).unwrap();
        assert_eq!(header, vec!["name", "col_2", "age"]);
        assert_eq!(rows[0], vec!["张三", "x", "30"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn docx_xml_becomes_readable_text() {
        let xml = "<w:body><w:p><w:r><w:t>第一段</w:t></w:r></w:p><w:p><w:r><w:t>第二段</w:t></w:r></w:p></w:body>";
        assert_eq!(xml_to_text(xml), "第一段\n第二段");
    }

    #[test]
    fn truncation_keeps_the_head() {
        let text = truncate("一二三四五".into(), 3);
        assert_eq!(text, "一二三");
    }
}
