//! 企微渠道访问策略（pairing / allowlist / open / disabled）。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rand::Rng;
use serde::{Deserialize, Serialize};

pub const PAIRING_TTL: Duration = Duration::from_secs(3600);
pub const MAX_PENDING_PER_ACCOUNT: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DmPolicy {
    #[default]
    Pairing,
    Allowlist,
    Open,
    Disabled,
}

impl DmPolicy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "allowlist" => Self::Allowlist,
            "open" => Self::Open,
            "disabled" => Self::Disabled,
            _ => Self::Pairing,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum GroupPolicy {
    #[default]
    Allowlist,
    Open,
    Disabled,
}

impl GroupPolicy {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "open" => Self::Open,
            "disabled" => Self::Disabled,
            _ => Self::Allowlist,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPair {
    pub userid: String,
    pub code: String,
    #[serde(skip)]
    pub created: Option<Instant>,
    pub created_ms: u64,
}

#[derive(Debug, Default)]
pub struct PolicyState {
    pub dm_policy: DmPolicy,
    pub group_policy: GroupPolicy,
    pub require_mention: bool,
    pub allow_users: Vec<String>,
    pub allow_groups: Vec<String>,
    pub paired_users: Vec<String>,
    pub pending: Vec<PendingPair>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum GateDecision {
    Allow,
    DenySilent,
    DenyWith(String),
    /// 需要发出配对码；`String` 是码本身
    NeedPairing(String),
}

impl PolicyState {
    pub fn prune_pending(&mut self) {
        let now = Instant::now();
        self.pending.retain(|p| {
            p.created
                .map(|t| now.duration_since(t) < PAIRING_TTL)
                .unwrap_or(true)
        });
    }

    pub fn is_user_allowed(&self, userid: &str) -> bool {
        if userid.is_empty() {
            return false;
        }
        self.allow_users.iter().any(|u| u == userid)
            || self.paired_users.iter().any(|u| u == userid)
    }

    pub fn is_group_allowed(&self, chatid: &str) -> bool {
        if chatid.is_empty() {
            return false;
        }
        self.allow_groups.iter().any(|g| g == chatid)
    }

    pub fn gate_dm(&mut self, userid: &str) -> GateDecision {
        self.prune_pending();
        match self.dm_policy {
            DmPolicy::Disabled => GateDecision::DenySilent,
            DmPolicy::Open => GateDecision::Allow,
            DmPolicy::Allowlist => {
                if self.is_user_allowed(userid) {
                    GateDecision::Allow
                } else {
                    GateDecision::DenySilent
                }
            }
            DmPolicy::Pairing => {
                if self.is_user_allowed(userid) {
                    GateDecision::Allow
                } else if let Some(existing) = self.pending.iter().find(|p| p.userid == userid) {
                    GateDecision::NeedPairing(existing.code.clone())
                } else {
                    // 每账号最多 3 条待配对
                    self.pending.retain(|p| p.userid != userid);
                    while self.pending.len() >= MAX_PENDING_PER_ACCOUNT {
                        self.pending.remove(0);
                    }
                    let code = gen_pair_code();
                    self.pending.push(PendingPair {
                        userid: userid.to_string(),
                        code: code.clone(),
                        created: Some(Instant::now()),
                        created_ms: now_ms(),
                    });
                    GateDecision::NeedPairing(code)
                }
            }
        }
    }

    pub fn gate_group(&self, chatid: &str, mentioned: bool) -> GateDecision {
        match self.group_policy {
            GroupPolicy::Disabled => GateDecision::DenySilent,
            GroupPolicy::Open => {
                if self.require_mention && !mentioned {
                    GateDecision::DenySilent
                } else {
                    GateDecision::Allow
                }
            }
            GroupPolicy::Allowlist => {
                if !self.is_group_allowed(chatid) {
                    GateDecision::DenySilent
                } else if self.require_mention && !mentioned {
                    GateDecision::DenySilent
                } else {
                    GateDecision::Allow
                }
            }
        }
    }

    pub fn approve_pair(&mut self, code: &str) -> Result<String, String> {
        self.prune_pending();
        let idx = self
            .pending
            .iter()
            .position(|p| p.code.eq_ignore_ascii_case(code.trim()))
            .ok_or_else(|| "配对码无效或已过期".to_string())?;
        let pair = self.pending.remove(idx);
        if !self.paired_users.iter().any(|u| u == &pair.userid) {
            self.paired_users.push(pair.userid.clone());
        }
        Ok(pair.userid)
    }

    pub fn revoke_user(&mut self, userid: &str) {
        self.paired_users.retain(|u| u != userid);
        self.pending.retain(|p| p.userid != userid);
    }
}

/// msgid 去重（环形，保留最近 N 条）。
#[derive(Debug, Default)]
pub struct MsgDeduper {
    order: Vec<String>,
    set: HashMap<String, ()>,
    cap: usize,
}

impl MsgDeduper {
    pub fn new(cap: usize) -> Self {
        Self {
            order: Vec::new(),
            set: HashMap::new(),
            cap: cap.max(16),
        }
    }

    /// 若已见过返回 true（应丢弃）。
    pub fn seen_or_insert(&mut self, msgid: &str) -> bool {
        if msgid.is_empty() {
            return false;
        }
        if self.set.contains_key(msgid) {
            return true;
        }
        self.set.insert(msgid.to_string(), ());
        self.order.push(msgid.to_string());
        while self.order.len() > self.cap {
            if let Some(old) = self.order.first().cloned() {
                self.order.remove(0);
                self.set.remove(&old);
            } else {
                break;
            }
        }
        false
    }
}

fn gen_pair_code() -> String {
    let n: u32 = rand::rng().random_range(0..1_000_000);
    format!("{n:06}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairing_issues_code_then_allows_after_approve() {
        let mut p = PolicyState {
            dm_policy: DmPolicy::Pairing,
            ..Default::default()
        };
        match p.gate_dm("alice") {
            GateDecision::NeedPairing(code) => {
                assert_eq!(code.len(), 6);
                let uid = p.approve_pair(&code).unwrap();
                assert_eq!(uid, "alice");
            }
            other => panic!("expected pairing, got {other:?}"),
        }
        assert_eq!(p.gate_dm("alice"), GateDecision::Allow);
    }

    #[test]
    fn group_allowlist_requires_mention() {
        let p = PolicyState {
            group_policy: GroupPolicy::Allowlist,
            require_mention: true,
            allow_groups: vec!["g1".into()],
            ..Default::default()
        };
        assert_eq!(p.gate_group("g1", false), GateDecision::DenySilent);
        assert_eq!(p.gate_group("g1", true), GateDecision::Allow);
        assert_eq!(p.gate_group("g2", true), GateDecision::DenySilent);
    }

    /// Orchestrator always passes mentioned=true for group callbacks (platform already
    /// filtered). Body may lack a leading @ after strip — still Allow on allowlist hit.
    #[test]
    fn group_platform_delivered_allowlist() {
        let p = PolicyState {
            group_policy: GroupPolicy::Allowlist,
            require_mention: true,
            allow_groups: vec!["g1".into()],
            ..Default::default()
        };
        assert_eq!(p.gate_group("g1", true), GateDecision::Allow);
    }

    #[test]
    fn deduper_drops_duplicates() {
        let mut d = MsgDeduper::new(4);
        assert!(!d.seen_or_insert("a"));
        assert!(d.seen_or_insert("a"));
        assert!(!d.seen_or_insert("b"));
    }
}
