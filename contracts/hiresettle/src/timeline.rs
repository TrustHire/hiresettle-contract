//! Engagement timeline (issue #501): a chronological feed merged at read time
//! from the per-kind histories, plus the recorders and queries for the
//! histories that did not previously survive their event.

use soroban_sdk::{contractimpl, Address, Env, Map, String, Symbol, TryFromVal, TryIntoVal, Val, Vec};
use crate::*;

/// Decode a dispute-history entry, tolerating entries written before the
/// `category` field was introduced (issue #658). Generated `#[contracttype]`
/// struct decoding requires an exact field set, so old entries are read as a
/// raw map and missing fields get their defaults.
fn decode_dispute_entry(env: &Env, val: &Val) -> DisputeHistoryEntry {
    let map: Map<Symbol, Val> = Map::try_from_val(env, val).unwrap();
    let field = |name: &str| -> Val {
        map.try_get(Symbol::new(env, name))
            .ok()
            .flatten()
            .unwrap_or_else(|| panic!("invalid dispute history entry"))
    };
    let category = match map.try_get(Symbol::new(env, "category")) {
        Ok(Some(v)) => v.try_into_val(env).unwrap(),
        _ => DisputeCategory::default(),
    };
    let withdrawn_at = match map.try_get(Symbol::new(env, "withdrawn_at")) {
        Ok(Some(v)) => v.try_into_val(env).unwrap_or(None),
        _ => None,
    };
    DisputeHistoryEntry {
        milestone_index: field("milestone_index").try_into_val(env).unwrap(),
        raised_by: field("raised_by").try_into_val(env).unwrap(),
        reason: field("reason").try_into_val(env).unwrap(),
        ledger: field("ledger").try_into_val(env).unwrap(),
        withdrawn_at,
        category,
    }
}

/// Load the whole history through the compatibility decoder above.
fn load_dispute_entries(env: &Env, engagement_id: &String) -> Vec<DisputeHistoryEntry> {
    let raw: Vec<Val> = env
        .storage()
        .persistent()
        .get(&DataKey2::DisputeHistory(engagement_id.clone()))
        .unwrap_or_else(|| Vec::new(env));
    let mut out = Vec::new(env);
    for i in 0..raw.len() {
        out.push_back(decode_dispute_entry(env, &raw.get(i).unwrap()));
    }
    out
}

#[contractimpl]
impl HireSettleContract {
    /// Return one page of everything that happened on an engagement, oldest
    /// first: accepted split amendments, replacement requests, disputes raised
    /// and engagement status transitions.
    ///
    /// Built at read time by merging `get_split_amendment_log`,
    /// `get_replacement_record`, `get_dispute_history` and
    /// `get_status_history`, so it always agrees with those queries. Entries
    /// are sorted by ledger; entries on the same ledger are ordered by
    /// [`TimelineKind`] declaration order, and entries of the same kind keep
    /// their order in the underlying history. `source_index` locates each
    /// entry in its underlying history for full detail.
    ///
    /// The dispute, status and split-amendment histories are FIFO-capped, so
    /// very old entries may have been dropped from them and therefore from
    /// here. Replacements requested before replacement records existed have
    /// no ledger and are omitted.
    ///
    /// `page` is 0-indexed. A `page_size` of 0, a page past the end, an
    /// engagement with no history, or an unknown engagement ID returns an
    /// empty vec.
    pub fn get_engagement_timeline(
        env: Env,
        engagement_id: String,
        page: u32,
        page_size: u32,
    ) -> Vec<TimelineEntry> {
        let entries = Self::build_engagement_timeline(&env, &engagement_id);

        let total = entries.len();
        let start = page.saturating_mul(page_size);
        if page_size == 0 || start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(entries.get(i).unwrap());
        }
        result
    }

    /// Return who requested replacement `replacement_index` and when, or
    /// `None` if there is no such replacement (or it predates these records).
    /// Companion to `get_replacement_reason` (issue #501).
    pub fn get_replacement_record(
        env: Env,
        engagement_id: String,
        replacement_index: u32,
    ) -> Option<ReplacementRecord> {
        env.storage().persistent().get(&DataKey2::ReplacementRecord(
            engagement_id,
            replacement_index,
        ))
    }

    /// Return every dispute raised on an engagement, oldest first, including
    /// ones already resolved (issue #501). Capped at the most recent
    /// `MAX_DISPUTE_HISTORY_ENTRIES`.
    pub fn get_dispute_history(env: Env, engagement_id: String) -> Vec<DisputeHistoryEntry> {
        load_dispute_entries(&env, &engagement_id)
    }

    /// Return every engagement status transition, oldest first (issue #501).
    /// Creation itself is not a transition and is not listed. Capped at the
    /// most recent `MAX_STATUS_HISTORY_ENTRIES`.
    pub fn get_status_history(env: Env, engagement_id: String) -> Vec<StatusChangeEntry> {
        env.storage()
            .persistent()
            .get(&DataKey2::StatusHistory(engagement_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    pub(crate) fn record_replacement(
        env: &Env,
        engagement_id: &String,
        replacement_index: u32,
        requested_by: &Address,
    ) {
        let key = DataKey2::ReplacementRecord(engagement_id.clone(), replacement_index);
        env.storage().persistent().set(
            &key,
            &ReplacementRecord {
                requested_by: requested_by.clone(),
                ledger: env.ledger().sequence(),
            },
        );
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    pub(crate) fn record_dispute_raised(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
        raised_by: &Address,
        reason: &String,
        category: &DisputeCategory,
    ) {
        let key = DataKey2::DisputeHistory(engagement_id.clone());
        let mut log = load_dispute_entries(env, engagement_id);
        if log.len() >= MAX_DISPUTE_HISTORY_ENTRIES {
            log.remove(0);
        }
        log.push_back(DisputeHistoryEntry {
            milestone_index,
            raised_by: raised_by.clone(),
            reason: reason.clone(),
            ledger: env.ledger().sequence(),
            withdrawn_at: None,
            category: category.clone(),
        });
        env.storage().persistent().set(&key, &log);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Mark the most recent dispute on `milestone_index` as withdrawn
    /// (issue #565). A no-op if that entry already aged out of the FIFO cap.
    pub(crate) fn record_dispute_withdrawn(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
    ) {
        let key = DataKey2::DisputeHistory(engagement_id.clone());
        let mut log = load_dispute_entries(env, engagement_id);
        let mut i = log.len();
        while i > 0 {
            i -= 1;
            let mut entry = log.get(i).unwrap();
            if entry.milestone_index == milestone_index {
                entry.withdrawn_at = Some(env.ledger().sequence());
                log.set(i, entry);
                env.storage().persistent().set(&key, &log);
                env.storage()
                    .persistent()
                    .extend_ttl(&key, 100_000, 6_300_000);
                return;
            }
        }
    }

    pub(crate) fn record_status_change(
        env: &Env,
        engagement_id: &String,
        old_status: EngagementStatus,
        new_status: EngagementStatus,
        actor: Option<Address>,
    ) {
        let key = DataKey2::StatusHistory(engagement_id.clone());
        let mut log: Vec<StatusChangeEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        if log.len() >= MAX_STATUS_HISTORY_ENTRIES {
            log.remove(0);
        }
        log.push_back(StatusChangeEntry {
            old_status,
            new_status,
            actor,
            ledger: env.ledger().sequence(),
        });
        env.storage().persistent().set(&key, &log);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Collect every history into one vec sorted by `(ledger, kind)`. Each
    /// source is appended in kind order and is already chronological, so a
    /// stable insertion sort on `ledger` alone yields that ordering.
    fn build_engagement_timeline(env: &Env, engagement_id: &String) -> Vec<TimelineEntry> {
        let mut entries: Vec<TimelineEntry> = Vec::new(env);

        let amendments = Self::get_split_amendment_log(env.clone(), engagement_id.clone());
        for i in 0..amendments.len() {
            let a = amendments.get(i).unwrap();
            entries.push_back(TimelineEntry {
                kind: TimelineKind::Amendment,
                milestone_index: None,
                actor: Some(a.proposer),
                ledger: a.ledger,
                source_index: i,
            });
        }

        let replacements = Self::get_replacement_count(env.clone(), engagement_id.clone());
        for i in 0..replacements {
            if let Some(r) = Self::get_replacement_record(env.clone(), engagement_id.clone(), i) {
                entries.push_back(TimelineEntry {
                    kind: TimelineKind::Replacement,
                    milestone_index: None,
                    actor: Some(r.requested_by),
                    ledger: r.ledger,
                    source_index: i,
                });
            }
        }

        let disputes = Self::get_dispute_history(env.clone(), engagement_id.clone());
        for i in 0..disputes.len() {
            let d = disputes.get(i).unwrap();
            entries.push_back(TimelineEntry {
                kind: TimelineKind::Dispute,
                milestone_index: Some(d.milestone_index),
                actor: Some(d.raised_by),
                ledger: d.ledger,
                source_index: i,
            });
        }

        let statuses = Self::get_status_history(env.clone(), engagement_id.clone());
        for i in 0..statuses.len() {
            let s = statuses.get(i).unwrap();
            entries.push_back(TimelineEntry {
                kind: TimelineKind::StatusChange,
                milestone_index: None,
                actor: s.actor,
                ledger: s.ledger,
                source_index: i,
            });
        }

        // Stable insertion sort by ledger. Every source is bounded (FIFO caps
        // and the admin replacement limit), so the quadratic worst case stays
        // small.
        for i in 1..entries.len() {
            let current = entries.get(i).unwrap();
            let mut j = i;
            while j > 0 && entries.get(j - 1).unwrap().ledger > current.ledger {
                entries.set(j, entries.get(j - 1).unwrap());
                j -= 1;
            }
            entries.set(j, current);
        }

        entries
    }
}
