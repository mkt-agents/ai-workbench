//! OSV.dev protocol: batch query bodies + id lists, single-vulnerability detail
//! records normalised for the UI.
//!
//! Pure — the reqwest calls live in `vuln_scan`, so the whole request/response
//! contract is testable from the probe crate without a network. Verified against
//! the live API on 2026-09-22: `/v1/querybatch` answers *id stubs only*
//! (`results[i].vulns[j].id`), so a scan is always two-phase — batch for ids,
//! then `/v1/vulns/<id>` per advisory for summary/severity/fixed versions.
//! One API serves npm / crates.io / PyPI, which is why no local CVE database
//! is shipped.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

use crate::vuln_lock::DepCoord;

pub const OSV_BASE: &str = "https://api.osv.dev";
/// The batch endpoint takes at most 100 queries per POST.
pub const MAX_QUERIES_PER_REQUEST: usize = 100;

/// One advisory as the UI needs it: an id, a one-line summary, a severity band
/// and the versions that fixed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsvVuln {
    pub id: String,
    pub summary: String,
    /// `critical | high | medium | low | unknown` — the bands the UI sorts by.
    pub severity: String,
    pub fixed_versions: Vec<String>,
    pub aliases: Vec<String>,
    /// Which dependency this advisory belongs to.
    pub package: String,
    pub version: String,
}

/// Build the `POST /v1/querybatch` bodies for a dependency set, chunked to the
/// API's limit. Order within a chunk is the order answers come back in.
pub fn build_batch_requests(deps: &[DepCoord]) -> Vec<serde_json::Value> {
    deps.chunks(MAX_QUERIES_PER_REQUEST)
        .map(|chunk| {
            let queries: Vec<serde_json::Value> = chunk
                .iter()
                .map(|d| {
                    serde_json::json!({
                        "package": { "ecosystem": d.ecosystem, "name": d.name },
                        "version": d.version,
                    })
                })
                .collect();
            serde_json::json!({ "queries": queries })
        })
        .collect()
}

/// Global query index of everything in one request. The scanner slices `deps`
/// directly; the probe tests use this to assert chunk order survives the JSON
/// round trip.
#[allow(dead_code)]
pub fn queries_of(body: &serde_json::Value) -> Vec<(String, String)> {
    body["queries"]
        .as_array()
        .map(|qs| {
            qs.iter()
                .map(|q| {
                    (
                        q["package"]["name"].as_str().unwrap_or_default().to_string(),
                        q["version"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse a batch response into per-query id lists, positionally aligned with the
/// request's `queries`. Each inner array holds that dependency's advisory ids.
pub fn parse_batch_ids(body: &str) -> Result<Vec<Vec<String>>, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("OSV 响应解析失败: {}", e))?;
    let results = json["results"]
        .as_array()
        .ok_or_else(|| "OSV 响应缺少 results 数组".to_string())?;
    Ok(results
        .iter()
        .map(|entry| {
            // Real shape: `{"vulns": [{"id": "GHSA-…"}]}`. A bare array is also
            // accepted — both have appeared in the wild across OSV deployments.
            let list = entry["vulns"]
                .as_array()
                .or_else(|| entry.as_array())
                .cloned()
                .unwrap_or_default();
            list.iter()
                .filter_map(|v| v["id"].as_str().map(str::to_string))
                .collect()
        })
        .collect())
}

/// GET url for one advisory's full record.
pub fn detail_url(id: &str) -> String {
    format!("{}/v1/vulns/{}", OSV_BASE, id)
}

/// Normalise one `/v1/vulns/<id>` record against the dependency it was found for.
pub fn parse_vuln_record(body: &str, dep: &DepCoord) -> Result<OsvVuln, String> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("OSV 公告解析失败: {}", e))?;
    Ok(vuln_from_json(&json, dep))
}

pub fn vuln_from_json(json: &serde_json::Value, dep: &DepCoord) -> OsvVuln {
    OsvVuln {
        id: json["id"].as_str().unwrap_or_default().to_string(),
        summary: json["summary"]
            .as_str()
            .or_else(|| json["details"].as_str().map(|d| d.lines().next().unwrap_or("")).filter(|s| !s.is_empty()))
            .unwrap_or_default()
            .to_string(),
        severity: normalize_severity(json),
        fixed_versions: fixed_versions(json, &dep.name),
        aliases: json["aliases"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        package: dep.name.clone(),
        version: dep.version.clone(),
    }
}

/// Severity across the ecosystems' habits: an explicit word (GHSA puts
/// `database_specific.severity`), a numeric CVSS score, or a bare CVSS *vector*
/// — which we refuse to re-score by hand, so it lands as `unknown` rather than
/// a made-up band.
pub fn normalize_severity(vuln: &serde_json::Value) -> String {
    if let Some(word) = vuln["database_specific"]["severity"]
        .as_str()
        .or_else(|| vuln["affected"].get(0).and_then(|a| a["database_specific"]["severity"].as_str()))
    {
        return match word.to_ascii_uppercase().as_str() {
            "CRITICAL" => "critical",
            "HIGH" => "high",
            "MODERATE" | "MEDIUM" => "medium",
            "LOW" => "low",
            _ => "unknown",
        }
        .to_string();
    }
    if let Some(scores) = vuln["severity"].as_array() {
        for entry in scores {
            // `score` is either a number ("7.5") or a vector string ("CVSS:3.1/AV:…").
            if let Some(number) = entry["score"].as_str().and_then(|s| s.parse::<f64>().ok()) {
                return band(number).to_string();
            }
            if let Some(number) = entry["score"].as_f64() {
                return band(number).to_string();
            }
        }
    }
    "unknown".to_string()
}

fn band(score: f64) -> &'static str {
    if score >= 9.0 {
        "critical"
    } else if score >= 7.0 {
        "high"
    } else if score >= 4.0 {
        "medium"
    } else if score > 0.0 {
        "low"
    } else {
        "unknown"
    }
}

fn fixed_versions(vuln: &serde_json::Value, dep_name: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    for affected in vuln["affected"].as_array().into_iter().flatten() {
        // Only report fixes for *this* package: one advisory can list many
        // (Go monorepos do exactly that), and another package's versions are noise.
        let name = affected["package"]["name"].as_str().unwrap_or("");
        if !name.is_empty() && name != dep_name {
            continue;
        }
        for range in affected["ranges"].as_array().into_iter().flatten() {
            for event in range["events"].as_array().into_iter().flatten() {
                if let Some(fixed) = event["fixed"].as_str() {
                    out.insert(fixed.to_string());
                }
            }
        }
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vuln_lock::DepCoord;

    fn dep(eco: &str, name: &str, version: &str) -> DepCoord {
        DepCoord {
            ecosystem: eco.to_string(),
            name: name.to_string(),
            version: version.to_string(),
        }
    }

    #[test]
    fn requests_chunk_at_the_api_limit() {
        let deps: Vec<DepCoord> = (0..250).map(|i| dep("npm", &format!("pkg{}", i), "1.0.0")).collect();
        let bodies = build_batch_requests(&deps);
        assert_eq!(bodies.len(), 3);
        assert_eq!(bodies[0]["queries"].as_array().unwrap().len(), 100);
        assert_eq!(bodies[2]["queries"].as_array().unwrap().len(), 50);
        let first = &bodies[0]["queries"][0];
        assert_eq!(first["package"]["ecosystem"], "npm");
        assert_eq!(first["package"]["name"], "pkg0");
        assert_eq!(first["version"], "1.0.0");
        assert_eq!(queries_of(&bodies[0])[1], ("pkg1".to_string(), "1.0.0".to_string()));
    }

    #[test]
    fn batch_answers_are_id_stubs_aligned_per_query() {
        // The real `querybatch` shape: one object per query, `vulns` of stubs.
        let body = r#"{"results":[
            {"vulns":[{"id":"GHSA-8hc4-vh64-cxmj","modified":"2026-09-10T03:51:04Z"},{"id":"OSV-2023-1"}]},
            {"vulns":[]},
            {"vulns":[{"id":"PYSEC-1"}]}
        ]}"#;
        let ids = parse_batch_ids(body).unwrap();
        assert_eq!(ids.len(), 3);
        assert_eq!(ids[0], vec!["GHSA-8hc4-vh64-cxmj".to_string(), "OSV-2023-1".to_string()]);
        assert!(ids[1].is_empty());
        // The legacy array shape parses too.
        assert_eq!(parse_batch_ids(r#"{"results":[[{"id":"X"}]]}"#).unwrap(), vec![vec!["X".to_string()]]);
    }

    #[test]
    fn detail_records_normalise_severity_fixes_and_aliases() {
        // Shaped after the live GET /v1/vulns/GHSA-8hc4-vh64-cxmj.
        let record = r#"{
          "id": "GHSA-8hc4-vh64-cxmj",
          "summary": "Server-Side Request Forgery in axios",
          "aliases": ["CVE-2023-45857"],
          "database_specific": { "severity": "HIGH", "cvss": { "score": "9.9" } },
          "affected": [ { "package": { "ecosystem": "npm", "name": "axios" },
                          "ranges": [ { "type": "ECOSYSTEM",
                                        "events": [ { "introduced": "0" }, { "fixed": "1.6.0" }, { "fixed": "0.28.0" } ] } ] } ]
        }"#;
        let vuln = parse_vuln_record(record, &dep("npm", "axios", "0.21.1")).unwrap();
        assert_eq!(vuln.package, "axios");
        assert_eq!(vuln.version, "0.21.1");
        assert_eq!(vuln.severity, "high", "the word wins over any score");
        assert_eq!(vuln.fixed_versions, vec!["0.28.0".to_string(), "1.6.0".to_string()], "sorted, deduped");
        assert_eq!(vuln.aliases, vec!["CVE-2023-45857".to_string()]);
        assert_eq!(vuln.summary, "Server-Side Request Forgery in axios");
    }

    #[test]
    fn severity_bands_follow_the_cvss_cutoffs_and_unknown_vectors_stay_unknown() {
        assert_eq!(normalize_severity(&serde_json::json!({ "severity": [{ "score": "9.8" }] })), "critical");
        assert_eq!(normalize_severity(&serde_json::json!({ "severity": [{ "score": "7.0" }] })), "high");
        assert_eq!(normalize_severity(&serde_json::json!({ "severity": [{ "score": 4.3 }] })), "medium");
        assert_eq!(normalize_severity(&serde_json::json!({ "severity": [{ "score": "3.9" }] })), "low");
        assert_eq!(
            normalize_severity(&serde_json::json!({ "severity": [{ "score": "CVSS:3.1/AV:N/AC:L" }] })),
            "unknown",
            "a bare vector is not re-scored by hand"
        );
        assert_eq!(
            normalize_severity(&serde_json::json!({ "affected": [{ "database_specific": { "severity": "MODERATE" } }] })),
            "medium",
            "PyPA puts the word under affected[0]"
        );
        assert_eq!(normalize_severity(&serde_json::json!({})), "unknown");
    }

    #[test]
    fn summary_falls_back_to_the_first_line_of_details() {
        let vuln = vuln_from_json(&serde_json::json!({ "id": "X", "details": "long body\nsecond line" }), &dep("npm", "a", "1"));
        assert_eq!(vuln.summary, "long body");
    }

    #[test]
    fn a_malformed_response_is_an_error_the_scan_can_report() {
        assert!(parse_batch_ids("{").is_err());
        assert!(parse_batch_ids(r#"{"nothing": 1}"#).is_err());
        assert!(parse_vuln_record("{", &dep("npm", "a", "1")).is_err());
    }

    #[test]
    fn detail_url_uses_the_documented_endpoint() {
        assert_eq!(detail_url("GHSA-1"), "https://api.osv.dev/v1/vulns/GHSA-1");
    }
}
