//! Candidate commitment ownership windows (issue #675).
//!
//! The first recruiter to register a `(company, commitment)` pair owns that
//! introduction for the admin-configured window. Other recruiters cannot
//! register the same commitment for the same company while the window is open.

use soroban_sdk::{contractimpl, Address, Env, String, Symbol};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    /// Admin sets the ownership window in ledgers. `0` disables ownership.
    pub fn set_candidate_ownership_window(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage().instance().set(
            &DataKey::Config(ConfigKey::CandidateOwnershipWindow),
            &ledgers,
        );
        env.events().publish(
            (Symbol::new(&env, "candidate_ownership_window_set"),),
            ledgers,
        );
    }

    /// Return the configured ownership window (default: 0, disabled).
    pub fn get_candidate_ownership_window(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::CandidateOwnershipWindow))
            .unwrap_or(0)
    }

    /// Register a candidate commitment for a company. The caller must be the
    /// recruiter of `engagement_id`. Ownership lasts for the configured window;
    /// registering again after expiry transfers ownership to the new recruiter.
    ///
    /// # Panics
    /// - `"CandidateOwned"` when another recruiter owns the commitment and the
    ///   window has not elapsed.
    pub fn register_candidate_commitment(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        commitment: String,
    ) {
        recruiter.require_auth();
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.recruiter != recruiter {
            panic!("unauthorized");
        }
        let window = Self::get_candidate_ownership_window(env.clone());
        let now = env.ledger().sequence();
        let key = DataKey3::CandidateOwner(engagement.company.clone(), commitment.clone());
        if window > 0 {
            if let Some(owner) = env.storage().persistent().get::<_, CandidateOwner>(&key) {
                let expires = owner
                    .registered_at_ledger
                    .saturating_add(window);
                if owner.recruiter != recruiter && now < expires {
                    panic!("CandidateOwned");
                }
            }
            let owner = CandidateOwner {
                recruiter,
                engagement_id: engagement_id.clone(),
                registered_at_ledger: now,
            };
            env.storage().persistent().set(&key, &owner);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
            let index_key =
                DataKey3::CandidateCommitmentIndex(engagement_id.clone());
            env.storage().persistent().set(&index_key, &commitment);
            env.storage()
                .persistent()
                .extend_ttl(&index_key, 100_000, 6_300_000);
        }
        env.events().publish(
            (
                Symbol::new(&env, "candidate_commitment_registered"),
                engagement_id,
            ),
            (engagement.company, commitment),
        );
    }

    /// Return the current owner of `(company, commitment)`, or `None` if the
    /// window has elapsed, ownership was cancelled, or no one registered it.
    pub fn get_candidate_owner(env: Env, company: Address, commitment: String) -> Option<CandidateOwner> {
        let key = DataKey3::CandidateOwner(company, commitment);
        let owner: Option<CandidateOwner> = env.storage().persistent().get(&key);
        match owner {
            Some(owner) => {
                let window = Self::get_candidate_ownership_window(env.clone());
                if window == 0
                    || env.ledger().sequence() >= owner.registered_at_ledger.saturating_add(window)
                {
                    None
                } else {
                    Some(owner)
                }
            }
            None => None,
        }
    }

    /// End ownership early when the owning engagement is cancelled.
    pub(crate) fn clear_candidate_ownership(env: &Env, engagement: &Engagement) {
        if let Some(owner) = env
            .storage()
            .persistent()
            .get::<_, CandidateOwner>(&DataKey3::CandidateOwner(
                engagement.company.clone(),
                Self::commitment_for_engagement(env, &engagement.id),
            )) {
            if owner.engagement_id == engagement.id {
                env.storage().persistent().remove(&DataKey3::CandidateOwner(
                    engagement.company.clone(),
                    Self::commitment_for_engagement(env, &engagement.id),
                ));
            }
        }
    }

    /// Commitments are indexed by `(company, commitment)`; we keep a reverse
    /// lookup so cancelling an engagement can release the introduction without
    /// the caller having to supply the hash again.
    pub(crate) fn commitment_for_engagement(env: &Env, engagement_id: &String) -> String {
        env.storage()
            .persistent()
            .get(&DataKey3::CandidateCommitmentIndex(engagement_id.clone()))
            .unwrap_or_else(|| String::from_str(env, ""))
    }
}
