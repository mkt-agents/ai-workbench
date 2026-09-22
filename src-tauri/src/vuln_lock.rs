//! Lock-file readers for the dependency vulnerability audit.
//!
//! Pure: text in, `DepCoord` list out — the OSV round trip lives in `vuln_osv`,
//! the filesystem walk in `vuln_scan`. Hand-rolled parsing (no toml/regex crates)
//! follows the same rule as every other parser in this crate.
//!
//! Only *exact* pins can be queried: OSV's batch API answers per version, so a
//! `^1.2.3` range or an unpinned requirement is skipped rather than guessed —
//! a wrong version would report vulnerabilities the project does not have.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// One exact dependency coordinate, in the OSV API's own ecosystem names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DepCoord {
    /// "npm" | "crates.io" | "PyPI" (maven deliberately absent: see the audit plan).
    pub ecosystem: String,
    pub name: String,
    pub version: String,
}

/// `package-lock.json` v1 (nested `dependencies`) and v2/v3 (`packages`).
pub fn parse_package_lock(content: &str) -> Result<Vec<DepCoord>, String> {
    let json: serde_json::Value =
        serde_json::from_str(content).map_err(|e| format!("package-lock.json 解析失败: {}", e))?;
    let mut out = BTreeSet::new();
    if let Some(packages) = json.get("packages").and_then(|v| v.as_object()) {
        for (key, entry) in packages {
            // "" is the root package; a node_modules entry always has a version,
            // links/dev-link entries do not and are skipped with it.
            if key.is_empty() {
                continue;
            }
            let Some(version) = entry.get("version").and_then(|v| v.as_str()) else { continue };
            let name = match key.rsplit_once("node_modules/") {
                Some((_prefix, rest)) => rest,
                // v2 also lists the workspace roots themselves; not a dependency.
                None => continue,
            };
            if name.is_empty() || version.is_empty() {
                continue;
            }
            out.insert(DepCoord {
                ecosystem: "npm".to_string(),
                name: name.to_string(),
                version: version.to_string(),
            });
        }
    } else if let Some(deps) = json.get("dependencies").and_then(|v| v.as_object()) {
        walk_v1(deps, &mut out);
    } else {
        return Err("package-lock.json 里没有 packages 或 dependencies 段".to_string());
    }
    Ok(out.into_iter().collect())
}

fn walk_v1(deps: &serde_json::Map<String, serde_json::Value>, out: &mut BTreeSet<DepCoord>) {
    for (name, entry) in deps {
        if let Some(version) = entry.get("version").and_then(|v| v.as_str()) {
            out.insert(DepCoord {
                ecosystem: "npm".to_string(),
                name: name.clone(),
                version: version.to_string(),
            });
        }
        if let Some(nested) = entry.get("dependencies").and_then(|v| v.as_object()) {
            walk_v1(nested, out);
        }
    }
}

/// `Cargo.lock`: `[[package]]` blocks with `name = ".."` / `version = ".."`.
pub fn parse_cargo_lock(content: &str) -> Vec<DepCoord> {
    let mut out = BTreeSet::new();
    let mut name: Option<String> = None;
    let mut version: Option<String> = None;

    let flush = |out: &mut BTreeSet<DepCoord>, name: &mut Option<String>, version: &mut Option<String>| {
        if let (Some(n), Some(v)) = (name.take(), version.take()) {
            out.insert(DepCoord {
                ecosystem: "crates.io".to_string(),
                name: n,
                version: v,
            });
        }
    };

    for line in content.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line == "[[package]]" {
            flush(&mut out, &mut name, &mut version);
            continue;
        }
        if line.starts_with('[') {
            // Any other header (`[metadata]`, …) ends the current package block.
            flush(&mut out, &mut name, &mut version);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let value = value.trim().trim_matches('"').to_string();
        match key.trim() {
            "name" => name = Some(value),
            "version" => version = Some(value),
            _ => {}
        }
    }
    flush(&mut out, &mut name, &mut version);
    out.into_iter().collect()
}

/// `requirements.txt`: keep only exact `==` pins; everything looser is skipped.
pub fn parse_requirements_txt(content: &str) -> Vec<DepCoord> {
    let mut out = BTreeSet::new();
    for line in content.lines() {
        let line = line.trim_end_matches('\r').trim();
        if line.is_empty()
            || line.starts_with('#')
            || line.starts_with('-')
            || line.contains("://")
            || line.starts_with('.')
        {
            continue;
        }
        // Trailing comments and environment markers are not part of the pin.
        let line = line.split('#').next().unwrap_or("").trim();
        let line = line.split(';').next().unwrap_or("").trim();
        // `foo[extra]==1.0` names the package `foo` for OSV purposes.
        let Some((spec, version)) = line.split_once("==") else { continue };
        let name = spec.split('[').next().unwrap_or(spec).trim();
        let version = version.split("==").next().unwrap_or(version).trim();
        if name.is_empty() || version.is_empty() {
            continue;
        }
        // PyPI's canonical form is lowercase with `-` separators.
        out.insert(DepCoord {
            ecosystem: "PyPI".to_string(),
            name: name.to_lowercase().replace('_', "-"),
            version: version.to_string(),
        });
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn coord(eco: &str, name: &str, version: &str) -> DepCoord {
        DepCoord {
            ecosystem: eco.to_string(),
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn package_lock_v2_packages_section_includes_scoped_and_nested_entries() {
        let lock = r#"{
  "name": "app",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "app", "version": "1.0.0" },
    "node_modules/@vue/shared": { "version": "3.4.21" },
    "node_modules/vite": { "version": "5.0.0", "dev": true },
    "node_modules/foo/node_modules/bar": { "version": "0.9.1" },
    "node_modules/local-link": { "link": true },
    "packages/inner": { "name": "inner", "version": "2.0.0" }
  }
}"#;
        assert_eq!(
            parse_package_lock(lock).unwrap(),
            vec![
                coord("npm", "@vue/shared", "3.4.21"),
                coord("npm", "bar", "0.9.1"),
                coord("npm", "vite", "5.0.0"),
            ],
            "root, link and workspace entries are not dependencies"
        );
    }

    #[test]
    fn package_lock_v1_walks_the_nested_dependencies_tree() {
        let lock = r#"{
  "name": "app",
  "lockfileVersion": 1,
  "dependencies": {
    "async": { "version": "2.6.1", "dependencies": { "lodash": { "version": "4.17.10" } } },
    "no-version-entry": { "from": "x" }
  }
}"#;
        assert_eq!(
            parse_package_lock(lock).unwrap(),
            vec![coord("npm", "async", "2.6.1"), coord("npm", "lodash", "4.17.10")]
        );
    }

    #[test]
    fn cargo_lock_reads_package_blocks_and_survives_other_sections() {
        let lock = "# comment\n[[package]]\nname = \"serde\"\nversion = \"1.0.200\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"ai_workbench\"\nversion = \"0.1.7\"\n\n[metadata]\nchecksum = \"whatever\"\n";
        assert_eq!(
            parse_cargo_lock(lock),
            vec![
                coord("crates.io", "ai_workbench", "0.1.7"),
                coord("crates.io", "serde", "1.0.200")
            ]
        );
    }

    #[test]
    fn requirements_keeps_only_exact_pins_and_canonicalises_names() {
        let req = "# deps\nrequests==2.31.0\nFlask[async]==3.0.0\nnumpy >= 1.24\nsome_pkg==1.2.3 ; python_version < '3.12'\n-r other.txt\nhttps://example.tgz#egg=weird\npandas==2.0.0 # pinned inline\n";
        assert_eq!(
            parse_requirements_txt(req),
            vec![
                coord("PyPI", "flask", "3.0.0"),
                coord("PyPI", "pandas", "2.0.0"),
                coord("PyPI", "requests", "2.31.0"),
                coord("PyPI", "some-pkg", "1.2.3"),
            ]
        );
    }

    #[test]
    fn a_broken_lock_file_is_an_error_not_an_empty_list() {
        assert!(parse_package_lock("{ not json").is_err());
        assert!(parse_package_lock(r#"{"lockfileVersion": 2}"#).is_err());
    }
}
