//! Account selection for the gateway: round-robin with per-(key,model)
//! stickiness, plus the unhealthy/expired bookkeeping that lets a failing
//! account be skipped without dropping it from the pool.
//!
//! Pure state over `PoolAccount` values the caller loaded from SQLite — the
//! Tauri command layer owns the lock, this owns the decision.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

/// A pool member as far as routing is concerned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PoolAccount {
    pub id: i64,
    /// "active" | "unverified" | "expired" | "banned"
    pub status: String,
    pub enabled: bool,
    pub exp_unix: Option<i64>,
}

impl PoolAccount {
    pub fn is_expired_at(&self, now_unix: i64) -> bool {
        self.exp_unix.is_some_and(|exp| exp <= now_unix)
    }

    /// Routing-eligible: turned on, not banned, credential not expired.
    /// `unverified` still routes — a protocol that has not been probed yet is
    /// not evidence the account is dead.
    pub fn eligible_at(&self, now_unix: i64) -> bool {
        self.enabled && self.status != "banned" && !self.is_expired_at(now_unix)
    }
}

/// Sticky routing key: the same client key + model keeps hitting the same
/// account, which is what preserves upstream prompt caching.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StickKey {
    pub key_id: i64,
    pub model: String,
}

#[derive(Default)]
pub struct Pool {
    /// Accounts that failed recently, mapped to the unix time they may retry.
    unhealthy: HashMap<i64, i64>,
    /// Which account a stick key currently owns.
    sticky: HashMap<StickKey, i64>,
    /// Round-robin cursor over the eligible set.
    cursor: usize,
}

/// Why an account was marked unhealthy. Retrying a quota exhaustion sooner than
/// a hard auth failure would just burn the same upstream twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// 401/403 — the credential itself is no good.
    Auth,
    /// 402/429 — rate or credit limit.
    Quota,
    /// 5xx / transport error.
    Transient,
}

const UNHEALTHY_SECS_AUTH: i64 = 30 * 60;
const UNHEALTHY_SECS_QUOTA: i64 = 10 * 60;
const UNHEALTHY_SECS_TRANSIENT: i64 = 60;

pub fn failure_status_code(code: u16) -> Option<Failure> {
    match code {
        401 | 403 => Some(Failure::Auth),
        402 | 429 => Some(Failure::Quota),
        c if c >= 500 => Some(Failure::Transient),
        _ => None,
    }
}

pub fn unhealthy_secs(failure: Failure) -> i64 {
    match failure {
        Failure::Auth => UNHEALTHY_SECS_AUTH,
        Failure::Quota => UNHEALTHY_SECS_QUOTA,
        Failure::Transient => UNHEALTHY_SECS_TRANSIENT,
    }
}

impl Pool {
    pub fn mark_unhealthy(&mut self, account_id: i64, failure: Failure, now_unix: i64) {
        self.unhealthy
            .insert(account_id, now_unix + unhealthy_secs(failure));
    }

    /// A credential that just worked cannot still be in its penalty box.
    pub fn mark_healthy(&mut self, account_id: i64) {
        self.unhealthy.remove(&account_id);
    }

    pub fn is_unhealthy(&self, account_id: i64, now_unix: i64) -> bool {
        match self.unhealthy.get(&account_id) {
            Some(until) => *until > now_unix,
            None => false,
        }
    }

    fn eligible_ids(&self, accounts: &[PoolAccount], now_unix: i64) -> Vec<i64> {
        accounts
            .iter()
            .filter(|a| a.eligible_at(now_unix) && !self.is_unhealthy(a.id, now_unix))
            .map(|a| a.id)
            .collect()
    }

    /// Choose an account for `stick`. Returns None only when nothing is
    /// eligible, which the caller turns into a 503.
    pub fn pick(&mut self, accounts: &[PoolAccount], stick: &StickKey, now_unix: i64) -> Option<i64> {
        let eligible = self.eligible_ids(accounts, now_unix);
        if eligible.is_empty() {
            return None;
        }
        if let Some(pinned) = self.sticky.get(stick) {
            if eligible.contains(pinned) {
                return Some(*pinned);
            }
            self.sticky.remove(stick);
        }
        let index = self.cursor % eligible.len();
        self.cursor = self.cursor.wrapping_add(1);
        let chosen = eligible[index];
        self.sticky.insert(stick.clone(), chosen);
        Some(chosen)
    }

    /// Next candidate after `failed_id` for the same request, used for the
    /// bounded failover retry. `tried` prevents looping back onto an account
    /// the same request already burned.
    pub fn pick_next(
        &mut self,
        accounts: &[PoolAccount],
        now_unix: i64,
        tried: &HashSet<i64>,
    ) -> Option<i64> {
        self.eligible_ids(accounts, now_unix)
            .into_iter()
            .find(|id| !tried.contains(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn account(id: i64) -> PoolAccount {
        PoolAccount { id, status: "active".into(), enabled: true, exp_unix: None }
    }

    fn banned(id: i64) -> PoolAccount {
        PoolAccount { id, status: "banned".into(), enabled: true, exp_unix: None }
    }

    fn off(id: i64) -> PoolAccount {
        PoolAccount { id, status: "active".into(), enabled: false, exp_unix: None }
    }

    fn expired(id: i64, at: i64) -> PoolAccount {
        PoolAccount { id, status: "expired".into(), enabled: true, exp_unix: Some(at) }
    }

    fn stick(key_id: i64, model: &str) -> StickKey {
        StickKey { key_id, model: model.into() }
    }

    const NOW: i64 = 1_000_000;

    #[test]
    fn eligibility_respects_disabled_banned_and_expired() {
        assert!(account(1).eligible_at(NOW));
        assert!(PoolAccount { id: 1, status: "unverified".into(), enabled: true, exp_unix: None }
            .eligible_at(NOW));
        assert!(!off(1).eligible_at(NOW));
        assert!(!banned(1).eligible_at(NOW));
        assert!(!expired(1, NOW - 1).eligible_at(NOW));
        assert!(account(1).eligible_at(NOW) && !account(1).is_expired_at(NOW));
        // An account whose token is still in the future is eligible even if the
        // stored status row has not been refreshed yet.
        assert!(PoolAccount { id: 2, status: "expired".into(), enabled: true, exp_unix: Some(NOW + 99) }
            .eligible_at(NOW));
    }

    #[test]
    fn no_eligible_account_returns_none() {
        let mut pool = Pool::default();
        let accounts = vec![off(1), banned(2), expired(3, NOW - 5)];
        assert_eq!(pool.pick(&accounts, &stick(1, "m"), NOW), None);
        assert_eq!(pool.pick(&[], &stick(1, "m"), NOW), None);
    }

    #[test]
    fn sticky_key_pins_one_account_until_it_leaves_the_pool() {
        let mut pool = Pool::default();
        let accounts = vec![account(1), account(2), account(3)];
        let s = stick(7, "deepseek-chat");
        let first = pool.pick(&accounts, &s, NOW).unwrap();
        for _ in 0..5 {
            assert_eq!(pool.pick(&accounts, &s, NOW), Some(first));
        }
        // Another client key is free to land elsewhere (round-robin moves on).
        let other = stick(8, "deepseek-chat");
        assert!(pool.pick(&accounts, &other, NOW).is_some());
    }

    #[test]
    fn round_robin_spreads_across_the_eligible_set() {
        let mut pool = Pool::default();
        let accounts = vec![account(1), account(2)];
        let seen: Vec<i64> = (0..6)
            .map(|i| pool.pick(&accounts, &stick(100 + i, "m"), NOW).unwrap())
            .collect();
        assert_eq!(seen, vec![1, 2, 1, 2, 1, 2]);
    }

    #[test]
    fn sticky_binding_follows_a_shrinking_pool() {
        let mut pool = Pool::default();
        let all = vec![account(1), account(2)];
        let s = stick(1, "m");
        assert_eq!(pool.pick(&all, &s, NOW), Some(1));
        // Account 1 goes away -> the next pick must not hand it out.
        let now_only_2 = vec![off(1), account(2)];
        assert_eq!(pool.pick(&now_only_2, &s, NOW), Some(2));
        // And it stays on 2 afterwards.
        assert_eq!(pool.pick(&now_only_2, &s, NOW), Some(2));
    }

    #[test]
    fn unhealthy_accounts_are_skipped_until_their_window_passes() {
        let mut pool = Pool::default();
        let accounts = vec![account(1), account(2)];
        pool.mark_unhealthy(1, Failure::Transient, NOW);
        assert_eq!(pool.pick(&accounts, &stick(1, "a"), NOW), Some(2));
        assert_eq!(pool.pick(&accounts, &stick(2, "a"), NOW), Some(2));
        // 60s later the transient penalty is over.
        assert_eq!(pool.pick(&accounts, &stick(3, "a"), NOW + 61), Some(1));
        assert!(pool.is_unhealthy(1, NOW));
        assert!(!pool.is_unhealthy(1, NOW + 61));
    }

    #[test]
    fn failure_classes_scale_the_penalty() {
        let mut pool = Pool::default();
        pool.mark_unhealthy(1, Failure::Auth, NOW);
        pool.mark_unhealthy(2, Failure::Quota, NOW);
        pool.mark_unhealthy(3, Failure::Transient, NOW);
        // Transient is the short one: a 5xx should cost ~1 minute, not half an hour.
        assert!(pool.is_unhealthy(3, NOW + 59));
        assert!(!pool.is_unhealthy(3, NOW + 61));
        // Quota waits out ~10 minutes.
        assert!(pool.is_unhealthy(2, NOW + 599));
        assert!(!pool.is_unhealthy(2, NOW + 601));
        // A dead credential stays out for ~30 minutes.
        assert!(pool.is_unhealthy(1, NOW + 1_799));
        assert!(!pool.is_unhealthy(1, NOW + 1_801));
    }

    #[test]
    fn a_success_clears_the_penalty() {
        let mut pool = Pool::default();
        pool.mark_unhealthy(1, Failure::Auth, NOW);
        pool.mark_healthy(1);
        assert!(!pool.is_unhealthy(1, NOW));
    }

    #[test]
    fn status_codes_map_to_the_right_failure_class() {
        assert_eq!(failure_status_code(401), Some(Failure::Auth));
        assert_eq!(failure_status_code(403), Some(Failure::Auth));
        assert_eq!(failure_status_code(402), Some(Failure::Quota));
        assert_eq!(failure_status_code(429), Some(Failure::Quota));
        assert_eq!(failure_status_code(500), Some(Failure::Transient));
        assert_eq!(failure_status_code(503), Some(Failure::Transient));
        assert_eq!(failure_status_code(200), None);
        assert_eq!(failure_status_code(400), None, "a bad client request is not the account's fault");
        assert_eq!(failure_status_code(404), None);
    }

    #[test]
    fn pick_next_skips_everything_already_tried() {
        let mut pool = Pool::default();
        let accounts = vec![account(1), account(2), account(3)];
        let mut tried = HashSet::new();
        let mut order = Vec::new();
        while let Some(id) = pool.pick_next(&accounts, NOW, &tried) {
            tried.insert(id);
            order.push(id);
        }
        assert_eq!(order, vec![1, 2, 3]);
        assert_eq!(order.len(), HashSet::<i64>::from_iter(order.clone()).len());
    }

    #[test]
    fn penalty_expires_and_the_account_rejoins_rotation() {
        let mut pool = Pool::default();
        let accounts = vec![account(1), account(2)];
        pool.mark_unhealthy(1, Failure::Quota, NOW);
        assert_eq!(pool.pick_next(&accounts, NOW, &[1].iter().copied().collect()), Some(2));
        assert_eq!(pool.pick_next(&accounts, NOW + 11 * 60, &HashSet::new()), Some(1));
    }
}
