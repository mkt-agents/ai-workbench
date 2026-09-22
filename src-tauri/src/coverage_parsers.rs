//! Coverage readers for the formats that are not "one JSON per project".
//!
//! Pure (no tauri, no sqlite) so they can be compiled and *run* from a scratch crate:
//! every parser here is exercised by a test, not merely type-checked.
//!
//! These are the files a developer actually ends up with:
//! - `lcov.info` — jest/vitest `--coverage`, pytest-cov, `cargo llvm-cov --lcov`, gcovr
//! - `jacoco.xml` — Maven JaCoCo (`target/site/jacoco/jacoco.xml`)
//! - `cobertura.xml` — .NET, `coverage xml-cobertura`, `gocover-cobertura`

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use crate::test_output_parsers::{attr, next_xml_tag};

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    pub lines: CoverageMetric,
    pub statements: CoverageMetric,
    pub branches: CoverageMetric,
    pub functions: CoverageMetric,
    pub files: Vec<CoverageFile>,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CoverageMetric {
    pub total: u32,
    pub covered: u32,
    pub percentage: f64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CoverageFile {
    pub path: String,
    pub lines: CoverageMetric,
    pub statements: CoverageMetric,
    pub branches: CoverageMetric,
    pub functions: CoverageMetric,
}

pub fn metric(total: u32, covered: u32) -> CoverageMetric {
    CoverageMetric {
        total,
        covered,
        percentage: if total > 0 {
            (covered as f64 / total as f64) * 100.0
        } else {
            0.0
        },
    }
}

fn roll_up(files: &[CoverageFile], pick: fn(&CoverageFile) -> &CoverageMetric) -> CoverageMetric {
    let total: u32 = files.iter().map(|f| pick(f).total).sum();
    let covered: u32 = files.iter().map(|f| pick(f).covered).sum();
    metric(total, covered)
}

fn parse_u32(value: Option<&str>) -> u32 {
    value.and_then(|v| v.trim().parse().ok()).unwrap_or(0)
}

/// Locate `<dir_name>/<file_name>` below `root`. Maven writes one report per module,
/// so a fixed relative path is not enough for a multi-module build.
pub fn find_report_file(root: &Path, dir_name: &str, file_name: &str, max_depth: usize) -> Option<PathBuf> {
    let mut frontier: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut best: Option<PathBuf> = None;
    let mut best_depth = usize::MAX;

    while let Some((dir, depth)) = frontier.pop() {
        if depth > max_depth || depth >= best_depth {
            continue;
        }
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let path = entry.path();
            if name == dir_name {
                let candidate = path.join(file_name);
                if candidate.is_file() {
                    best = Some(candidate);
                    best_depth = depth;
                }
                continue;
            }
            // Neither a report location nor worth descending into.
            if matches!(name.as_str(), ".git" | "node_modules" | "src" | ".gradle" | "__pycache__") {
                continue;
            }
            frontier.push((path, depth + 1));
        }
    }
    best
}

// --------------------------------------------------------------------------- lcov

#[derive(Default)]
struct LcovRecord {
    lines: HashMap<u32, u64>,
    branches: HashMap<(String, String), u64>,
    functions: HashMap<String, u64>,
    fn_found: Option<u32>,
    fn_hit: Option<u32>,
    line_total: Option<u32>,
    line_hit: Option<u32>,
    branch_total: Option<u32>,
    branch_hit: Option<u32>,
}

impl LcovRecord {
    fn finish(self) -> CoverageFile {
        let lines_total = self.line_total.unwrap_or(self.lines.len() as u32);
        let lines_hit = self
            .line_hit
            .unwrap_or(self.lines.values().filter(|hits| **hits > 0).count() as u32);
        let branches_total = self.branch_total.unwrap_or(self.branches.len() as u32);
        let branches_hit = self
            .branch_hit
            .unwrap_or(self.branches.values().filter(|hits| **hits > 0).count() as u32);
        let fn_total = self.fn_found.unwrap_or(self.functions.len() as u32);
        let fn_hit = self
            .fn_hit
            .unwrap_or(self.functions.values().filter(|hits| **hits > 0).count() as u32);
        CoverageFile {
            path: String::new(), // filled in by the caller, which owns the key
            lines: metric(lines_total, lines_hit.min(lines_total)),
            statements: metric(lines_total, lines_hit.min(lines_total)),
            branches: metric(branches_total, branches_hit.min(branches_total)),
            functions: metric(fn_total, fn_hit.min(fn_total)),
        }
    }
}

fn field(value: &str, index: usize) -> &str {
    value.split(',').nth(index).map(str::trim).unwrap_or("")
}

fn hits_of(raw: &str) -> u64 {
    // `-` marks "instrumented but never executed" in some writers.
    let raw = raw.trim();
    if raw.is_empty() || raw == "-" {
        return 0;
    }
    raw.parse().unwrap_or(0)
}

/// Merge records by source path: `cargo llvm-cov --lcov` emits one record per test
/// binary for the same file, and taking the last would lose hits.
pub fn parse_lcov_coverage(content: &str) -> Result<CoverageReport, String> {
    let mut records: BTreeMap<String, LcovRecord> = BTreeMap::new();
    let mut current = String::new();

    for raw in content.lines() {
        let line = raw.trim();
        if line == "end_of_record" {
            current.clear();
            continue;
        }
        let Some((tag, value)) = line.split_once(':') else { continue };
        if tag == "SF" {
            current = value.trim().replace('\\', "/");
            records.entry(current.clone()).or_default();
            continue;
        }
        // TN/unused keys before any SF belong to no file.
        if current.is_empty() {
            continue;
        }
        let Some(record) = records.get_mut(&current) else {
            continue;
        };
        match tag {
            "DA" => {
                let number: u32 = field(value, 0).parse().unwrap_or(0);
                let hits = hits_of(field(value, 1));
                let entry = record.lines.entry(number).or_insert(0);
                *entry = (*entry).max(hits);
            }
            "BRDA" => {
                let key = (field(value, 1).to_string(), field(value, 2).to_string());
                let hits = hits_of(field(value, 3));
                let entry = record.branches.entry(key).or_insert(0);
                *entry = (*entry).max(hits);
            }
            // `FN:<line>,<name>` declares, `FNDA:<hits>,<name>` counts.
            "FN" => {
                let name = field(value, 1);
                if !name.is_empty() {
                    record.functions.entry(name.to_string()).or_insert(0);
                }
            }
            "FNDA" => {
                let hits = hits_of(field(value, 0));
                let name = field(value, 1).to_string();
                if !name.is_empty() {
                    let entry = record.functions.entry(name).or_insert(0);
                    *entry = (*entry).max(hits);
                }
            }
            "FNF" => record.fn_found = value.trim().parse().ok(),
            "FNH" => record.fn_hit = value.trim().parse().ok(),
            "LF" => record.line_total = value.trim().parse().ok(),
            "LH" => record.line_hit = value.trim().parse().ok(),
            "BRF" => record.branch_total = value.trim().parse().ok(),
            "BRH" => record.branch_hit = value.trim().parse().ok(),
            _ => {}
        }
    }

    let mut files: Vec<CoverageFile> = Vec::new();
    for (path, record) in records {
        let mut file = record.finish();
        if file.lines.total == 0 && file.branches.total == 0 && file.functions.total == 0 {
            continue;
        }
        file.path = path;
        files.push(file);
    }
    if files.is_empty() {
        return Err("lcov 文件里没有任何文件记录".to_string());
    }

    Ok(CoverageReport {
        lines: roll_up(&files, |f| &f.lines),
        // lcov has no statement concept; instrumented lines are the closest match.
        statements: roll_up(&files, |f| &f.statements),
        branches: roll_up(&files, |f| &f.branches),
        functions: roll_up(&files, |f| &f.functions),
        files,
    })
}

// -------------------------------------------------------------------------- jacoco

#[derive(Default)]
struct Counters {
    entries: Vec<(String, u32, u32)>, // type -> (missed, covered)
}

impl Counters {
    fn add(&mut self, kind: &str, missed: u32, covered: u32) {
        if kind.is_empty() {
            return;
        }
        match self.entries.iter_mut().find(|(name, _, _)| name == kind) {
            Some((_, m, c)) => {
                *m += missed;
                *c += covered;
            }
            None => self.entries.push((kind.to_string(), missed, covered)),
        }
    }

    fn metric(&self, kind: &str) -> CoverageMetric {
        match self.entries.iter().find(|(name, _, _)| name == kind) {
            Some((_, missed, covered)) => metric(missed + covered, *covered),
            None => metric(0, 0),
        }
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// JaCoCo's XML report. Counters appear at report, package, class and source-file
/// level; the report level is the total and the source-file level feeds the table.
pub fn parse_jacoco_xml(content: &str) -> Result<CoverageReport, String> {
    let mut files: Vec<CoverageFile> = Vec::new();
    let mut totals = Counters::default();
    let mut package = String::new();
    let mut current: Option<(String, Counters)> = None;

    let mut pos = 0usize;
    // JaCoCo repeats the same counters at report / package / class / source level;
    // only the report level is the total and only the source level feeds the table.
    let mut nesting = 0usize;
    while let Some(tag) = next_xml_tag(content, pos) {
        pos = tag.end;
        if tag.close {
            match tag.name {
                "package" | "class" | "sourcefile" => nesting = nesting.saturating_sub(1),
                _ => {}
            }
            match tag.name {
                "sourcefile" => {
                    if let Some((name, counters)) = current.take() {
                        files.push(jacoco_file(&package, name, counters));
                    }
                }
                "package" => package.clear(),
                _ => {}
            }
            continue;
        }
        match tag.name {
            "package" | "class" | "sourcefile" => nesting += 1,
            _ => {}
        }
        match tag.name {
            "package" => package = attr(&tag.attrs, "name").unwrap_or("").to_string(),
            "sourcefile" => {
                if let Some((name, counters)) = current.take() {
                    files.push(jacoco_file(&package, name, counters));
                }
                current = Some((
                    attr(&tag.attrs, "name").unwrap_or("").to_string(),
                    Counters::default(),
                ));
            }
            "counter" => {
                let missed = parse_u32(attr(&tag.attrs, "missed"));
                let covered = parse_u32(attr(&tag.attrs, "covered"));
                let kind = attr(&tag.attrs, "type").unwrap_or("");
                match &mut current {
                    Some((_, counters)) => counters.add(kind, missed, covered),
                    None if nesting == 0 => totals.add(kind, missed, covered),
                    None => {}
                }
            }
            _ => {}
        }
    }
    if let Some((name, counters)) = current.take() {
        files.push(jacoco_file(&package, name, counters));
    }

    files.retain(|f| f.lines.total > 0);
    if files.is_empty() && totals.is_empty() {
        return Err("jacoco 报告里没有任何计数".to_string());
    }

    Ok(CoverageReport {
        lines: totals.metric("LINE"),
        statements: totals.metric("INSTRUCTION"),
        branches: totals.metric("BRANCH"),
        functions: totals.metric("METHOD"),
        files,
    })
}

fn jacoco_file(package: &str, name: String, counters: Counters) -> CoverageFile {
    let path = if package.is_empty() {
        name
    } else {
        format!("{}/{}", package.trim_matches('/'), name)
    };
    CoverageFile {
        path,
        lines: counters.metric("LINE"),
        statements: counters.metric("INSTRUCTION"),
        branches: counters.metric("BRANCH"),
        functions: counters.metric("METHOD"),
    }
}

// ----------------------------------------------------------------------- cobertura

#[derive(Default)]
struct CoberturaFile {
    lines: u32,
    lines_hit: u32,
    branches: u32,
    branches_hit: u32,
}

/// Cobertura XML: rates on the root, executable lines as `<line number hits>`, and
/// branch coverage as `condition-coverage="50% (1 of 2)"` on the line.
pub fn parse_cobertura_xml(content: &str) -> Result<CoverageReport, String> {
    let mut files: BTreeMap<String, CoberturaFile> = BTreeMap::new();
    let mut current: Option<String> = None;
    let mut root: Option<(u32, u32, u32, u32)> = None; // lines-valid, lines-covered, branches-valid, branches-covered

    let mut pos = 0usize;
    while let Some(tag) = next_xml_tag(content, pos) {
        pos = tag.end;
        match (tag.name, tag.close) {
            ("coverage", false) => root = Some((
                parse_u32(attr(&tag.attrs, "lines-valid")),
                parse_u32(attr(&tag.attrs, "lines-covered")),
                parse_u32(attr(&tag.attrs, "branches-valid")),
                parse_u32(attr(&tag.attrs, "branches-covered")),
            )),
            ("class", false) => {
                let filename = attr(&tag.attrs, "filename").unwrap_or("").replace('\\', "/");
                current = if filename.is_empty() { None } else { Some(filename) };
            }
            ("class", true) => current = None,
            ("line", false) => {
                if let Some(path) = &current {
                    let file = files.entry(path.clone()).or_default();
                    file.lines += 1;
                    if hits_of(field(attr(&tag.attrs, "hits").unwrap_or("0"), 0)) > 0 {
                        file.lines_hit += 1;
                    }
                    if let Some((covered, total)) = condition_coverage(attr(&tag.attrs, "condition-coverage")) {
                        file.branches += total;
                        file.branches_hit += covered;
                    }
                }
            }
            _ => {}
        }
    }

    if files.is_empty() {
        return Err("cobertura 报告里没有任何文件".to_string());
    }

    let files: Vec<CoverageFile> = files
        .into_iter()
        .map(|(path, file)| CoverageFile {
            path,
            lines: metric(file.lines, file.lines_hit),
            statements: metric(file.lines, file.lines_hit),
            branches: metric(file.branches, file.branches_hit.min(file.branches)),
            functions: metric(0, 0),
        })
        .collect();

    let (lines_valid, lines_covered, branches_valid, branches_covered) = root.unwrap_or((0, 0, 0, 0));
    let lines = roll_up(&files, |f| &f.lines);
    let branches = roll_up(&files, |f| &f.branches);

    Ok(CoverageReport {
        lines: if lines_valid > 0 { metric(lines_valid, lines_covered) } else { lines.clone() },
        statements: lines,
        branches: if branches_valid > 0 {
            metric(branches_valid, branches_covered)
        } else {
            branches.clone()
        },
        functions: metric(0, 0),
        files,
    })
}

// ----------------------------------------------------------------------- line level

/// File path -> (line number -> hit count). Only instrumented lines appear, so a
/// missing key means "no data", which is NOT the same as zero. Feeds
/// `coverage_delta::intersect`; the count reports themselves stay at file level.
pub type LineHits = BTreeMap<String, BTreeMap<u32, u64>>;

pub fn parse_lcov_lines(content: &str) -> LineHits {
    let mut out: LineHits = BTreeMap::new();
    let mut current = String::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line == "end_of_record" {
            current.clear();
            continue;
        }
        let Some((tag, value)) = line.split_once(':') else { continue };
        if tag == "SF" {
            current = value.trim().replace('\\', "/");
            out.entry(current.clone()).or_default();
            continue;
        }
        if current.is_empty() || tag != "DA" {
            continue;
        }
        let number: u32 = field(value, 0).parse().unwrap_or(0);
        let hits = hits_of(field(value, 1));
        if number == 0 {
            continue;
        }
        // Same merge rule as the count parser: duplicate SF records take the max.
        let entry = out.get_mut(&current).expect("entry created with SF");
        let slot = entry.entry(number).or_insert(0);
        *slot = (*slot).max(hits);
    }
    out
}

pub fn parse_jacoco_lines(content: &str) -> LineHits {
    let mut out: LineHits = BTreeMap::new();
    let mut package = String::new();
    let mut current: Option<String> = None;

    let mut pos = 0usize;
    while let Some(tag) = next_xml_tag(content, pos) {
        pos = tag.end;
        if tag.close {
            match tag.name {
                "sourcefile" => current = None,
                "package" => package.clear(),
                _ => {}
            }
            continue;
        }
        match tag.name {
            "package" => package = attr(&tag.attrs, "name").unwrap_or("").to_string(),
            "sourcefile" => {
                let name = attr(&tag.attrs, "name").unwrap_or("").to_string();
                current = Some(if package.is_empty() {
                    name
                } else {
                    format!("{}/{}", package.trim_matches('/'), name)
                });
            }
            // <line nr= mi= ci=> — instrumented iff mi+ci > 0; covered iff ci > 0.
            "line" => {
                if let Some(path) = &current {
                    let mi = parse_u32(attr(&tag.attrs, "mi"));
                    let ci = parse_u32(attr(&tag.attrs, "ci"));
                    let nr = parse_u32(attr(&tag.attrs, "nr"));
                    if nr > 0 && mi + ci > 0 {
                        out.entry(path.clone()).or_default().insert(nr, ci as u64);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

pub fn parse_cobertura_lines(content: &str) -> LineHits {
    let mut out: LineHits = BTreeMap::new();
    let mut current: Option<String> = None;

    let mut pos = 0usize;
    while let Some(tag) = next_xml_tag(content, pos) {
        pos = tag.end;
        match (tag.name, tag.close) {
            ("class", false) => {
                let filename = attr(&tag.attrs, "filename").unwrap_or("").replace('\\', "/");
                current = if filename.is_empty() { None } else { Some(filename) };
            }
            ("class", true) => current = None,
            ("line", false) => {
                if let Some(path) = &current {
                    let number = parse_u32(attr(&tag.attrs, "number"));
                    if number == 0 {
                        continue;
                    }
                    let hits = hits_of(field(attr(&tag.attrs, "hits").unwrap_or("0"), 0));
                    out.entry(path.clone()).or_default().insert(number, hits);
                }
            }
            _ => {}
        }
    }
    out
}

/// `"50% (1 of 2)"` → `(1, 2)`; absent or malformed → not a branch.
fn condition_coverage(value: Option<&str>) -> Option<(u32, u32)> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    let inner = value.rsplit_once('(')?.1.split_once(')')?.0;
    let covered = inner.split_whitespace().next()?.parse().ok()?;
    let total = inner.split_whitespace().last()?.parse().ok()?;
    if total == 0 {
        return None;
    }
    Some((covered, total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn percent(metric: &CoverageMetric) -> u32 {
        metric.percentage.round() as u32
    }

    #[test]
    fn lcov_records_become_per_file_metrics() {
        let lcov = "TN:\nSF:src/core/store.ts\nFN:10,loadAll\nFN:20,saveAll\nFNDA:5,loadAll\nFNDA:0,saveAll\n\
                     FNF:2\nFNH:1\nBRDA:12,0,0,3\nBRDA:12,0,1,0\nBRF:2\nBRH:1\nDA:10,5\nDA:12,3\nDA:20,0\n\
                     LF:3\nLH:2\nend_of_record\nSF:src/core/pathUtils.ts\nDA:1,7\nLF:1\nLH:1\nend_of_record\n";
        let report = parse_lcov_coverage(lcov).unwrap();

        assert_eq!(report.lines, metric(4, 3));
        assert_eq!(report.branches, metric(2, 1));
        assert_eq!(report.functions, metric(2, 1));
        assert_eq!(report.files.len(), 2, "sorted by path");
        assert_eq!(report.files[0].path, "src/core/pathUtils.ts");
        assert_eq!(report.files[0].lines, metric(1, 1));
        assert_eq!(report.files[1].path, "src/core/store.ts");
        assert_eq!(percent(&report.files[1].functions), 50);
        assert_eq!(report.files[1].branches, metric(2, 1));
    }

    #[test]
    fn duplicate_lcov_records_for_one_file_merge_instead_of_overwrite() {
        // `cargo llvm-cov --lcov` writes one record per test binary.
        let lcov = "SF:src/lib.rs\nDA:1,0\nDA:2,0\nLF:2\nLH:0\nend_of_record\n\
                    SF:src/lib.rs\nDA:1,4\nDA:2,0\nLF:2\nLH:1\nend_of_record\n";
        let report = parse_lcov_coverage(lcov).unwrap();
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].lines, metric(2, 1), "the summary of the last record wins, hits are merged");
    }

    #[test]
    fn lcov_without_any_record_is_an_error_not_a_zero_report() {
        assert!(parse_lcov_coverage("TN:\n").is_err());
    }

    #[test]
    fn jacoco_counters_are_read_at_both_levels() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<report name="account-service">
  <package name="com/x/web">
    <class name="com/x/web/AccountController" sourcefilename="AccountController.java">
      <counter type="INSTRUCTION" missed="10" covered="90"/>
      <counter type="LINE" missed="2" covered="18"/>
      <counter type="METHOD" missed="1" covered="5"/>
    </class>
    <sourcefile name="AccountController.java">
      <counter type="INSTRUCTION" missed="10" covered="90"/>
      <counter type="BRANCH" missed="1" covered="3"/>
      <counter type="LINE" missed="2" covered="18"/>
      <counter type="METHOD" missed="1" covered="5"/>
    </sourcefile>
  </package>
  <counter type="INSTRUCTION" missed="10" covered="90"/>
  <counter type="BRANCH" missed="1" covered="3"/>
  <counter type="LINE" missed="2" covered="18"/>
  <counter type="METHOD" missed="1" covered="5"/>
  <counter type="CLASS" missed="0" covered="1"/>
</report>"#;
        let report = parse_jacoco_xml(xml).unwrap();
        assert_eq!(report.lines, metric(20, 18));
        assert_eq!(report.statements, metric(100, 90));
        assert_eq!(report.branches, metric(4, 3));
        assert_eq!(report.functions, metric(6, 5));
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].path, "com/x/web/AccountController.java");
        assert_eq!(report.files[0].lines, metric(20, 18));
        assert_eq!(percent(&report.lines), 90);
    }

    #[test]
    fn cobertura_lines_and_condition_coverage_are_counted() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<coverage line-rate="0.75" branch-rate="0.5" lines-covered="3" lines-valid="4" branches-covered="1" branches-valid="2" version="0.5">
  <packages>
    <package name="com.x">
      <classes>
        <class name="Foo" filename="com/x/Foo.java" line-rate="0.75">
          <lines>
            <line number="1" hits="2"/>
            <line number="2" hits="1" branch="true" condition-coverage="50% (1 of 2)"/>
            <line number="3" hits="0"/>
            <line number="4" hits="5"/>
          </lines>
        </class>
      </classes>
    </package>
  </packages>
</coverage>"#;
        let report = parse_cobertura_xml(xml).unwrap();
        assert_eq!(report.lines, metric(4, 3), "the root counters are authoritative");
        assert_eq!(report.branches, metric(2, 1));
        assert_eq!(report.files[0].path, "com/x/Foo.java");
        assert_eq!(report.files[0].lines, metric(4, 3));
        assert_eq!(report.files[0].branches, metric(2, 1));
    }

    #[test]
    fn condition_coverage_parses_only_real_branches() {
        assert_eq!(condition_coverage(Some("50% (1 of 2)")), Some((1, 2)));
        assert_eq!(condition_coverage(Some("100% (3 of 3)")), Some((3, 3)));
        assert_eq!(condition_coverage(None), None);
        assert_eq!(condition_coverage(Some("")), None);
        assert_eq!(condition_coverage(Some("0% (0 of 0)")), None);
    }

    #[test]
    fn a_module_report_is_found_below_the_project_root_and_junk_is_skipped() {
        let root = std::env::temp_dir().join(format!("aiwb-cov-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let dir = root.join("account.service").join("target").join("site").join("jacoco");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("jacoco.xml"), "<report/>").unwrap();
        let decoy = root.join("node_modules").join("pkg").join("target").join("site").join("jacoco");
        fs::create_dir_all(&decoy).unwrap();
        fs::write(decoy.join("jacoco.xml"), "<report/>").unwrap();

        let found = find_report_file(&root, "jacoco", "jacoco.xml", 8).unwrap();
        assert!(found.ends_with("account.service/target/site/jacoco/jacoco.xml"), "got {:?}", found);
        assert!(find_report_file(&root, "surefire-reports", "x.xml", 8).is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn lcov_lines_keep_zero_hit_entries_and_merge_by_max() {
        let lcov = "SF:src/a.ts\nDA:1,0\nDA:2,3\nend_of_record\nSF:src/a.ts\nDA:1,5\nend_of_record\n";
        let lines = parse_lcov_lines(lcov);
        let a = &lines["src/a.ts"];
        assert_eq!(a.get(&1), Some(&5), "duplicate SF record merges by max");
        assert_eq!(a.get(&2), Some(&3));
    }

    #[test]
    fn jacoco_lines_join_package_path_and_skip_uninstrumented() {
        let xml = r#"<report><package name="com/x">
            <sourcefile name="A.java">
                <line nr="3" mi="2" ci="0"/>
                <line nr="4" mi="0" ci="1"/>
                <line nr="5" mi="0" ci="0"/>
            </sourcefile>
        </package></report>"#;
        let lines = parse_jacoco_lines(xml);
        let a = &lines["com/x/A.java"];
        assert_eq!(a.get(&3), Some(&0));
        assert_eq!(a.get(&4), Some(&1));
        assert!(!a.contains_key(&5), "mi+ci==0 is not instrumented");
    }

    #[test]
    fn cobertura_lines_map_number_to_hits() {
        let xml = r#"<coverage><packages><package><classes>
            <class filename="a/ctx.c"><lines>
                <line number="5" hits="0"/>
                <line number="6" hits="2"/>
            </lines></class>
        </classes></package></packages></coverage>"#;
        let lines = parse_cobertura_lines(xml);
        let f = &lines["a/ctx.c"];
        assert_eq!(f.get(&5), Some(&0));
        assert_eq!(f.get(&6), Some(&2));
    }
}
