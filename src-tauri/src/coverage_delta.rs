//! Incremental (diff-line) coverage: the intersection of "lines this change
//! touched" with "lines the coverage report instrumented".
//!
//! Pure functions only — the diff text and the parsed line hits come from the
//! caller (`change_commands` re-runs git, `coverage_parsers::parse_*_lines`
//! reads the reports). This keeps the whole join testable from the probe crate.
//!
//! Honesty rule: a changed line with *no* instrumentation data is counted
//! `unknown`, never `missed`. Overstating "untested" would burn the one metric
//! the QA workflow trusts, so the UI always shows the unknown count next to
//! the percentage.

use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

/// One changed source file's share of the coverage.
#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct FileDelta {
    pub path: String,
    /// Lines the diff touched on the new side.
    pub changed_lines: u32,
    pub covered: u32,
    pub missed: u32,
    /// Changed lines the coverage report says nothing about (not instrumented
    /// or the file could not be matched to a report entry).
    pub unknown: u32,
    /// covered / (covered + missed); 0 when nothing is instrumented.
    pub ratio: f64,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Hotspot {
    pub path: String,
    /// Missed line numbers, ascending, capped per file.
    pub lines: Vec<u32>,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeltaCoverage {
    pub files: Vec<FileDelta>,
    pub changed_lines: u32,
    pub covered: u32,
    pub missed: u32,
    pub unknown: u32,
    pub ratio: f64,
    pub uncovered_hotspots: Vec<Hotspot>,
}

/// Cap a pathological single-hunk range so a 200k-line generated file cannot
/// dominate the walk; the excess counts as unknown instead.
const MAX_FILE_LINES: usize = 20_000;
const HOTSPOT_FILES: usize = 10;
const HOTSPOT_LINES: usize = 60;

fn canon_rel_path(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while let Some(rest) = s.strip_prefix("./") {
        s = rest.to_string();
    }
    while let Some(rest) = s.strip_prefix('/') {
        s = rest.to_string();
    }
    s.to_ascii_lowercase()
}

/// Parse the NEW-side line ranges of a unified diff (any context width).
/// Only `@@ ... +c,d @@` ranges with d > 0 are kept — a pure-deletion hunk has
/// no new lines to cover. Path comes from `+++ b/<path>` so renames attribute
/// to their destination; `/dev/null` (new deletions) ends the current file.
pub fn parse_hunk_ranges(diff: &str) -> BTreeMap<String, Vec<(u32, u32)>> {
    let mut out: BTreeMap<String, Vec<(u32, u32)>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in diff.lines() {
        if let Some(rest) = line.strip_prefix("+++ ") {
            // Strip the "b/" prefix and any trailing "\t<timestamp>" tab field.
            let path = rest.split('\t').next().unwrap_or(rest).trim();
            current = match path.strip_prefix("b/") {
                Some(p) if p != "/dev/null" => Some(p.to_string()),
                _ => None, // /dev/null (deletion) or exotic quoting: not a new-side file
            };
            continue;
        }
        let Some(path) = &current else { continue };
        let Some(hunk) = line.strip_prefix("@@ ") else { continue };
        // "<old> +<new> <tail>"; we only need the +<new> part.
        let Some(plus) = hunk.split(" +").nth(1) else { continue };
        let range = plus.split(' ').next().unwrap_or("");
        let (start, count) = match range.split_once(',') {
            Some((a, b)) => (a.parse::<u32>().unwrap_or(0), b.parse::<u32>().unwrap_or(1)),
            None => (range.parse::<u32>().unwrap_or(0), 1),
        };
        if start == 0 || count == 0 {
            continue;
        }
        out.entry(path.clone()).or_default().push((start, start + count - 1));
    }
    out
}

/// Match a diff path to a coverage-report path. Diff paths are repo-root
/// relative; lcov SF: entries may be absolute or module-relative, so a
/// boundary-checked suffix match either way is the only portable rule.
fn match_coverage_path<'a>(changed: &str, hits: &'a BTreeMap<String, BTreeMap<u32, u64>>) -> Option<&'a str> {
    let want = canon_rel_path(changed);
    let mut best: Option<(&'a str, usize)> = None;
    for key in hits.keys() {
        let have = canon_rel_path(key);
        let matched = if have == want {
            Some(want.len())
        } else if have.ends_with(&format!("/{want}")) {
            Some(want.len())
        } else if want.ends_with(&format!("/{have}")) {
            Some(have.len())
        } else {
            None
        };
        if let Some(len) = matched {
            if best.map(|(_, l)| len > l).unwrap_or(true) {
                best = Some((key.as_str(), len));
            }
        }
    }
    best.map(|(k, _)| k)
}

/// Join changed ranges with line hits. `changed` keys are the raw diff paths;
/// the per-file result keeps the raw path for display.
pub fn intersect(
    changed: &BTreeMap<String, Vec<(u32, u32)>>,
    hits: &BTreeMap<String, BTreeMap<u32, u64>>,
) -> DeltaCoverage {
    let mut files: Vec<FileDelta> = Vec::new();
    let (mut t_changed, mut t_covered, mut t_missed, mut t_unknown) = (0u32, 0u32, 0u32, 0u32);

    for (path, ranges) in changed {
        let mut lines: BTreeSet<u32> = BTreeSet::new();
        let mut overflow = 0u32;
        for (lo, hi) in ranges {
            for n in *lo..=*hi {
                if lines.len() < MAX_FILE_LINES {
                    lines.insert(n);
                } else {
                    overflow += 1;
                }
            }
        }
        if lines.is_empty() {
            continue;
        }
        let (mut covered, mut missed) = (0u32, 0u32);
        let mut missed_lines: Vec<u32> = Vec::new();
        let unknown = match match_coverage_path(path, hits) {
            Some(key) => {
                let map = &hits[key];
                let mut unknown = 0u32;
                for n in &lines {
                    match map.get(n) {
                        Some(h) if *h > 0 => covered += 1,
                        Some(_) => {
                            missed += 1;
                            if missed_lines.len() < HOTSPOT_LINES {
                                missed_lines.push(*n);
                            }
                        }
                        None => unknown += 1,
                    }
                }
                unknown + overflow
            }
            // Whole file absent from the report: everything is unknown, not missed.
            None => lines.len() as u32 + overflow,
        };
        let instrumented = covered + missed;
        t_changed += lines.len() as u32 + overflow;
        t_covered += covered;
        t_missed += missed;
        t_unknown += unknown;
        files.push(FileDelta {
            path: path.clone(),
            changed_lines: lines.len() as u32 + overflow,
            covered,
            missed,
            unknown,
            ratio: if instrumented > 0 {
                (covered as f64 / instrumented as f64) * 100.0
            } else {
                0.0
            },
        });
    }

    let mut hotspots: Vec<Hotspot> = files
        .iter()
        .filter(|f| f.missed > 0)
        .map(|f| Hotspot {
            path: f.path.clone(),
            lines: hotspots_lines_for(changed, hits, &f.path),
        })
        .collect();
    hotspots.sort_by(|a, b| {
        let am = a.lines.len();
        let bm = b.lines.len();
        bm.cmp(&am).then_with(|| a.path.cmp(&b.path))
    });
    hotspots.truncate(HOTSPOT_FILES);

    let instrumented = t_covered + t_missed;
    DeltaCoverage {
        files,
        changed_lines: t_changed,
        covered: t_covered,
        missed: t_missed,
        unknown: t_unknown,
        ratio: if instrumented > 0 {
            (t_covered as f64 / instrumented as f64) * 100.0
        } else {
            0.0
        },
        uncovered_hotspots: hotspots,
    }
}

fn hotspots_lines_for(
    changed: &BTreeMap<String, Vec<(u32, u32)>>,
    hits: &BTreeMap<String, BTreeMap<u32, u64>>,
    path: &str,
) -> Vec<u32> {
    let Some(key) = match_coverage_path(path, hits) else {
        return Vec::new();
    };
    let map = &hits[key];
    changed
        .get(path)
        .into_iter()
        .flatten()
        .flat_map(|(lo, hi)| *lo..=*hi)
        .filter(|n| matches!(map.get(n), Some(0)))
        .take(HOTSPOT_LINES)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits(entries: &[(&str, &[(u32, u64)])]) -> BTreeMap<String, BTreeMap<u32, u64>> {
        entries
            .iter()
            .map(|(p, lines)| {
                (
                    p.to_string(),
                    lines.iter().map(|(n, h)| (*n, *h)).collect::<BTreeMap<_, _>>(),
                )
            })
            .collect()
    }

    const DIFF: &str = "diff --git a/src/a.ts b/src/a.ts\n--- a/src/a.ts\n+++ b/src/a.ts\n@@ -0,0 +2,2 @@\n+y1\n+y2\n@@ -10,2 +12,2 @@\n-a\n-b\n+b1\n+b2\ndiff --git a/src/b.py b/src/b.py\n--- a/src/b.py\n+++ b/src/b.py\n@@ -5,1 +4,0 @@\n-deleted only\n";

    #[test]
    fn hunk_headers_give_new_side_ranges() {
        let ranges = parse_hunk_ranges(DIFF);
        assert_eq!(ranges.len(), 1, "the pure-deletion file contributes nothing");
        assert_eq!(ranges["src/a.ts"], vec![(2, 3), (12, 13)]);
    }

    #[test]
    fn absolute_lcov_paths_align_by_suffix_and_zero_hits_are_missed() {
        let ranges = parse_hunk_ranges(DIFF);
        let h = hits(&[(
            "/home/dev/proj/src/a.ts",
            &[(2, 5), (3, 0), (12, 1), (13, 0)],
        )]);
        let d = intersect(&ranges, &h);
        assert_eq!(d.covered, 2);
        assert_eq!(d.missed, 2);
        assert_eq!(d.unknown, 0);
        assert_eq!(d.files[0].path, "src/a.ts");
        assert_eq!(d.uncovered_hotspots[0].lines, vec![3, 13]);
    }

    #[test]
    fn unmatched_files_count_unknown_not_missed() {
        let ranges = parse_hunk_ranges(DIFF);
        let h = hits(&[("other/x.ts", &[(2, 1)])]);
        let d = intersect(&ranges, &h);
        assert_eq!((d.covered, d.missed, d.unknown), (0, 0, 4));
        assert!(d.uncovered_hotspots.is_empty(), "unknown is not a hotspot");
        assert_eq!(d.ratio, 0.0);
    }

    #[test]
    fn partial_instrumentation_splits_unknown_from_missed() {
        let ranges = parse_hunk_ranges(DIFF);
        let h = hits(&[("src/a.ts", &[(2, 1), (3, 0)])]); // 12/13 not instrumented
        let d = intersect(&ranges, &h);
        assert_eq!((d.covered, d.missed, d.unknown), (1, 1, 2));
        // ratio ignores the unknown lines: 1 of 2 instrumented = 50%.
        assert_eq!(d.files[0].ratio.round() as u32, 50);
    }

    #[test]
    fn cjk_paths_match_case_insensitively_with_boundary() {
        let diff = "--- a/x\n+++ b/模块/用户中心.ts\n@@ -1,0 +1,3 @@\n";
        let ranges = parse_hunk_ranges(diff);
        let h = hits(&[("D:/repo/模块/用户中心.ts", &[(1, 1), (2, 0), (3, 0)])]);
        let d = intersect(&ranges, &h);
        assert_eq!((d.covered, d.missed), (1, 2));
        // No false prefix match: "中心.ts" must not steal "用户中心.ts".
        let h2 = hits(&[("中心.ts", &[(1, 1)])]);
        let d2 = intersect(&ranges, &h2);
        assert_eq!(d2.unknown, 3);
    }
}
