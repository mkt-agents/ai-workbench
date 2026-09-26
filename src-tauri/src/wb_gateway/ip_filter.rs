//! Peer-address gatekeeping for the gateway: pure and unit-tested, no axum in
//! sight. The gateway binds 127.0.0.1 unless LAN mode is on, so this is the
//! second line of defence — it is what makes the IP allow/deny lists mean
//! something once the socket is open to the network.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// Wire form of a rule direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleKind {
    Allow,
    Deny,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IpRuleDto {
    pub id: String,
    pub kind: RuleKind,
    pub ip_or_cidr: String,
    pub enabled: bool,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewIpRule {
    pub kind: RuleKind,
    pub ip_or_cidr: String,
    #[serde(default)]
    pub note: Option<String>,
}

pub fn kind_str(kind: RuleKind) -> &'static str {
    match kind {
        RuleKind::Allow => "allow",
        RuleKind::Deny => "deny",
    }
}

/// Anything unrecognised falls back to deny — the safe direction for a rule we
/// could not read.
pub fn kind_from_str(raw: &str) -> RuleKind {
    if raw.eq_ignore_ascii_case("allow") {
        RuleKind::Allow
    } else {
        RuleKind::Deny
    }
}

/// A parsed rule target. IPv4 is widened into the v6 space by an offset so a
/// `/0` v4 rule cannot accidentally match a real v6 peer (see tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Bits {
    V4(u32),
    V6(u128),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleTarget {
    bits: Bits,
    prefix: u32,
}

pub fn parse_target(raw: &str) -> Result<RuleTarget, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("IP 或 CIDR 不能为空".to_string());
    }
    let (addr_part, prefix) = match raw.split_once('/') {
        Some((addr, len)) => {
            let len: u32 = len.trim().parse().map_err(|_| format!("前缀长度非法: {len}"))?;
            (addr, Some(len))
        }
        None => (raw, None),
    };
    let ip: IpAddr = addr_part
        .trim()
        .parse()
        .map_err(|_| format!("IP 或网段非法: {raw}"))?;
    let bits = match ip {
        IpAddr::V4(v4) => {
            let max = prefix.unwrap_or(32);
            if max > 32 {
                return Err(format!("IPv4 前缀长度不能超过 32（收到 {max}）"));
            }
            Bits::V4(u32::from(v4))
        }
        IpAddr::V6(v6) => {
            let max = prefix.unwrap_or(128);
            if max > 128 {
                return Err(format!("IPv6 前缀长度不能超过 128（收到 {max}）"));
            }
            Bits::V6(u128::from(v6))
        }
    };
    Ok(RuleTarget {
        bits,
        prefix: prefix.unwrap_or(if matches!(bits, Bits::V4(_)) { 32 } else { 128 }),
    })
}

fn bits_of(ip: IpAddr) -> Bits {
    match ip {
        IpAddr::V4(v4) => Bits::V4(u32::from(v4)),
        IpAddr::V6(v6) => Bits::V6(u128::from(v6)),
    }
}

fn masked(bits: Bits, prefix: u32) -> u128 {
    let (value, width) = match bits {
        Bits::V4(v) => (v as u128, 32u32),
        Bits::V6(v) => (v, 128u32),
    };
    let keep = prefix.min(width);
    if keep == 0 {
        return 0;
    }
    // Right-shift away the host bits; the family offset separates v4 from v6.
    value >> (width - keep)
}

pub fn target_matches(target: &RuleTarget, peer: IpAddr) -> bool {
    let peer_bits = bits_of(peer);
    // Different families never match, whatever the numeric values look like.
    let same_family = matches!(
        (target.bits, peer_bits),
        (Bits::V4(_), Bits::V4(_)) | (Bits::V6(_), Bits::V6(_))
    );
    if !same_family {
        return false;
    }
    masked(target.bits, target.prefix) == masked(peer_bits, target.prefix)
}

/// The whole gatekeeping policy in one pure function:
/// - loopback is always allowed (the gateway must stay usable from this box);
/// - a non-loopback peer needs LAN mode;
/// - an explicit deny beats any allow;
/// - a non-empty allow list inverts the default to "deny".
pub fn ip_allowed(peer: IpAddr, lan_enabled: bool, allow: &[RuleTarget], deny: &[RuleTarget]) -> bool {
    if peer.is_loopback() {
        return true;
    }
    if !lan_enabled {
        return false;
    }
    if deny.iter().any(|t| target_matches(t, peer)) {
        return false;
    }
    if !allow.is_empty() {
        return allow.iter().any(|t| target_matches(t, peer));
    }
    true
}

/// Split rule rows into the two matcher lists the gate needs.
pub fn compile_rules(rows: &[(String, bool)]) -> (Vec<RuleTarget>, Vec<RuleTarget>) {
    let (mut allow, mut deny) = (Vec::new(), Vec::new());
    for (raw, is_allow) in rows {
        match parse_target(raw) {
            Ok(target) => {
                if *is_allow {
                    allow.push(target);
                } else {
                    deny.push(target);
                }
            }
            // A rule that no longer parses is skipped rather than locking the
            // whole gateway out; the panel shows the offending row verbatim.
            Err(_) => continue,
        }
    }
    (allow, deny)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn targets(items: &[&str]) -> Vec<RuleTarget> {
        items.iter().map(|s| parse_target(s).unwrap()).collect()
    }

    #[test]
    fn parses_single_cidr_and_rejects_junk() {
        assert!(parse_target("10.0.0.1").is_ok());
        assert!(parse_target(" 10.0.0.0/8 ").is_ok());
        assert!(parse_target("::1").is_ok());
        assert!(parse_target("fe80::/10").is_ok());
        for bad in ["", "abc", "10.0.0.1/33", "10.0.0.1/x", "::/129", "10.0.0.1/999", "1.2.3"] {
            assert!(parse_target(bad).is_err(), "{bad} must be rejected");
        }
    }

    #[test]
    fn cidr_bounds_are_exact() {
        let lan = targets(&["192.168.1.0/24"]);
        assert!(target_matches(&lan[0], ip("192.168.1.1")));
        assert!(target_matches(&lan[0], ip("192.168.1.255")));
        assert!(!target_matches(&lan[0], ip("192.168.2.1")));
        assert!(!target_matches(&lan[0], ip("192.168.0.255")));

        let wide = targets(&["10.0.0.0/8"]);
        assert!(target_matches(&wide[0], ip("10.255.255.255")));
        assert!(!target_matches(&wide[0], ip("11.0.0.1")));

        let v6 = targets(&["fe80::/10"]);
        assert!(target_matches(&v6[0], ip("fe80::1")));
        assert!(target_matches(&v6[0], ip("febf::1")));
        assert!(!target_matches(&v6[0], ip("fec0::1")));
    }

    #[test]
    fn single_host_rule_only_matches_itself() {
        let one = targets(&["10.1.2.3"]);
        assert!(target_matches(&one[0], ip("10.1.2.3")));
        assert!(!target_matches(&one[0], ip("10.1.2.4")));
    }

    #[test]
    fn families_do_not_cross() {
        // A v4 /0 must not become a v6 wildcard by way of both masking to 0.
        let v4_any = targets(&["0.0.0.0/0"]);
        assert!(target_matches(&v4_any[0], ip("8.8.8.8")));
        assert!(!target_matches(&v4_any[0], ip("2001:db8::1")));

        let one = targets(&["::1"]);
        assert!(!target_matches(&one[0], ip("127.0.0.1")));
    }

    #[test]
    fn loopback_is_exempt_from_everything() {
        let deny_all = targets(&["127.0.0.0/8", "::1"]);
        assert!(ip_allowed(ip("127.0.0.1"), false, &[], &deny_all));
        assert!(ip_allowed(ip("::1"), false, &[], &deny_all));
    }

    #[test]
    fn lan_mode_is_the_master_switch() {
        assert!(!ip_allowed(ip("192.168.1.5"), false, &[], &[]));
        assert!(ip_allowed(ip("192.168.1.5"), true, &[], &[]));
    }

    #[test]
    fn deny_wins_then_allow_list_inverts_the_default() {
        let allow = targets(&["192.168.1.0/24"]);
        let deny = targets(&["192.168.1.66"]);

        // Empty allow list = allow anything not denied.
        assert!(ip_allowed(ip("10.0.0.9"), true, &[], &deny));
        // Non-empty allow list = nothing outside it gets in.
        assert!(!ip_allowed(ip("10.0.0.9"), true, &allow, &deny));
        assert!(ip_allowed(ip("192.168.1.20"), true, &allow, &deny));
        // Deny overrides membership in the allow list.
        assert!(!ip_allowed(ip("192.168.1.66"), true, &allow, &deny));
    }

    #[test]
    fn compile_skips_unparsable_rules_and_honours_direction() {
        let rows = vec![
            ("192.168.0.0/16".to_string(), true),
            ("garbage".to_string(), true),
            ("192.168.9.9".to_string(), false),
            ("not-an-ip".to_string(), false),
        ];
        let (allow, deny) = compile_rules(&rows);
        assert_eq!(allow.len(), 1);
        assert_eq!(deny.len(), 1);
        assert!(!ip_allowed(ip("192.168.9.9"), true, &allow, &deny));
        assert!(ip_allowed(ip("192.168.9.10"), true, &allow, &deny));
    }
}
