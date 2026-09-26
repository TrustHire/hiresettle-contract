//! Admin-curated arbiter pool and randomized, response-time-weighted panel
//! assignment (issues #467, #468).
//!
//! ## Randomness caveat
//! Panels are drawn with `env.prng()`, Soroban's per-invocation PRNG. It is a
//! ChaCha20 stream, but its seed is derived from ledger data that is **public
//! as soon as the ledger is nominated** and is **under the influence of
//! validators**. A sufficiently motivated validator (or anyone who can choose
//! which ledger a transaction lands in and simulate the outcome first) can bias
//! or predict which arbiters are drawn. The draw therefore only makes it harder
//! for an ordinary company to hand-pick a panel; it is not a cryptographic
//! guarantee of fairness. Integrators should not rely on it alone for
//! high-value disputes — pair it with a large, vetted pool, or supply an
//! explicitly agreed panel via `create_engagement` instead.

use soroban_sdk::{contractimpl, Address, Env, String, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #467 — ARBITER POOL
    // ----------------------------------------------------------

    /// Admin adds an address to the arbiter pool.
    ///
    /// # Panics
    /// - `"AlreadyInPool"` — the address is already a pool member.
    pub fn add_arbiter_pool_member(env: Env, admin: Address, address: Address) {
        Self::assert_admin(&env, &admin);
        let mut pool = Self::get_arbiter_pool(env.clone());
        if pool.contains(&address) {
            panic!("AlreadyInPool");
        }
        pool.push_back(address.clone());
        Self::set_arbiter_pool(&env, &pool);
        env.events()
            .publish((Symbol::new(&env, "arbiter_pool_added"),), address);
    }

    /// Admin removes an address from the arbiter pool. Panels already drawn
    /// onto existing engagements are unaffected.
    ///
    /// # Panics
    /// - `"NotInPool"` — the address is not a pool member.
    pub fn remove_arbiter_pool_member(env: Env, admin: Address, address: Address) {
        Self::assert_admin(&env, &admin);
        let mut pool = Self::get_arbiter_pool(env.clone());
        let index = pool
            .first_index_of(&address)
            .unwrap_or_else(|| panic!("NotInPool"));
        pool.remove(index);
        Self::set_arbiter_pool(&env, &pool);
        env.events()
            .publish((Symbol::new(&env, "arbiter_pool_removed"),), address);
    }

    /// Return the current arbiter pool.
    pub fn get_arbiter_pool(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::ArbiterPool)
            .unwrap_or_else(|| Vec::new(&env))
    }

    fn set_arbiter_pool(env: &Env, pool: &Vec<Address>) {
        env.storage().persistent().set(&DataKey::ArbiterPool, pool);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::ArbiterPool, 100_000, 6_300_000);
    }

    /// Same as [`Self::create_engagement`], except the arbiter panel is drawn
    /// from the admin-curated arbiter pool instead of being supplied by the
    /// company: `panel_size` distinct pool members are drawn, biased by
    /// [`Self::get_arbiter_selection_weight`], and `quorum` of them must agree
    /// to resolve a dispute.
    ///
    /// The engagement's `company` and `recruiter` are never drawn (they would
    /// be rejected as colliding parties). See the module docs for the
    /// manipulability limits of the on-chain randomness source.
    ///
    /// # Panics
    /// - `"ArbiterPoolTooSmall"` — the pool (excluding the company and
    ///   recruiter) has fewer than `panel_size` members.
    /// - Everything [`Self::create_engagement`] panics on, including
    ///   `"invalid quorum"` when `quorum` is 0 or exceeds `panel_size`.
    pub fn create_engagement_random_panel(
        env: Env,
        engagement_id: String,
        company: Address,
        recruiter: Address,
        random_setup: RandomArbiterSetup,
        token: Address,
        total_amount: i128,
        job_title: String,
        milestones: Vec<Milestone>,
        retention_days: Vec<u32>,
        config: EngagementConfig,
    ) -> String {
        Self::assert_not_paused(&env);
        company.require_auth();

        let pool = Self::get_arbiter_pool(env.clone());
        let arbiters = Self::draw_arbiter_panel(
            &env,
            &pool,
            random_setup.panel_size,
            &company,
            &recruiter,
        );

        env.events().publish(
            (
                Symbol::new(&env, "arbiters_drawn"),
                engagement_id.clone(),
            ),
            arbiters.clone(),
        );

        Self::create_engagement_impl(
            env,
            engagement_id,
            company,
            recruiter,
            ArbiterSetup {
                arbiters,
                quorum: random_setup.quorum,
            },
            token,
            total_amount,
            job_title,
            milestones,
            retention_days,
            config,
        )
    }

    /// Draw `panel_size` distinct members of `pool` (skipping `company` and
    /// `recruiter`) by weighted sampling without replacement, each member's
    /// chance proportional to its selection weight (issue #468). Members with
    /// no history get the default weight, so with no history anywhere this is
    /// a uniform draw.
    pub(crate) fn draw_arbiter_panel(
        env: &Env,
        pool: &Vec<Address>,
        panel_size: u32,
        company: &Address,
        recruiter: &Address,
    ) -> Vec<Address> {
        if pool.len() < panel_size {
            panic!("ArbiterPoolTooSmall");
        }

        let mut candidates: Vec<Address> = Vec::new(env);
        let mut weights: Vec<u32> = Vec::new(env);
        for member in pool.iter() {
            if member != *company && member != *recruiter {
                weights.push_back(Self::selection_weight(env, &member));
                candidates.push_back(member);
            }
        }
        if candidates.len() < panel_size {
            panic!("ArbiterPoolTooSmall");
        }

        let mut panel: Vec<Address> = Vec::new(env);
        for _ in 0..panel_size {
            let total: u64 = weights.iter().map(|w| w as u64).sum();
            let mut ticket: u64 = env.prng().gen_range(0..total);
            let mut chosen = 0;
            for (i, w) in weights.iter().enumerate() {
                if ticket < w as u64 {
                    chosen = i as u32;
                    break;
                }
                ticket -= w as u64;
            }
            panel.push_back(candidates.get(chosen).unwrap());
            candidates.remove(chosen);
            weights.remove(chosen);
        }
        panel
    }

    // ----------------------------------------------------------
    // ISSUE #468 — RESPONSE-TIME-WEIGHTED SELECTION
    // ----------------------------------------------------------

    /// Return the weight (1–100) with which `address` is drawn by
    /// `create_engagement_random_panel`.
    ///
    /// # Formula
    /// - No dispute history yet → `50` (the midpoint), so new members are
    ///   eligible at a neutral weight rather than excluded.
    /// - Otherwise, with `completion = 100 * votes_cast / disputes_assigned`
    ///   and `speed = 100 * R / (R + avg_response_ledgers)` where `R` is
    ///   17 280 ledgers (~1 day; `speed` is 100 for instant votes, 50 at a
    ///   one-day average, and 0 if the arbiter never voted):
    ///   `weight = max(1, completion * speed / 100)`.
    ///
    /// The floor of 1 keeps even consistently absent arbiters drawable (just
    /// rarely) so the admin, not the formula, decides pool membership.
    pub fn get_arbiter_selection_weight(env: Env, address: Address) -> u32 {
        Self::selection_weight(&env, &address)
    }

    /// Return the raw dispute-response record for an arbiter, if any.
    pub fn get_arbiter_stats(env: Env, address: Address) -> Option<ArbiterStats> {
        env.storage()
            .persistent()
            .get(&DataKey::ArbiterStats(address))
    }

    pub(crate) fn selection_weight(env: &Env, address: &Address) -> u32 {
        let stats: ArbiterStats = match env
            .storage()
            .persistent()
            .get(&DataKey::ArbiterStats(address.clone()))
        {
            Some(s) => s,
            None => return DEFAULT_ARBITER_SELECTION_WEIGHT,
        };
        if stats.disputes_assigned == 0 {
            return DEFAULT_ARBITER_SELECTION_WEIGHT;
        }
        let votes = stats.votes_cast.min(stats.disputes_assigned) as u64;
        let completion = votes * 100 / stats.disputes_assigned as u64;
        let speed = if votes == 0 {
            0
        } else {
            let avg = stats.total_response_ledgers / votes;
            100 * ARBITER_RESPONSE_REFERENCE_LEDGERS / (ARBITER_RESPONSE_REFERENCE_LEDGERS + avg)
        };
        ((completion * speed / 100) as u32).max(1)
    }

    fn load_arbiter_stats(env: &Env, arbiter: &Address) -> ArbiterStats {
        env.storage()
            .persistent()
            .get(&DataKey::ArbiterStats(arbiter.clone()))
            .unwrap_or(ArbiterStats {
                disputes_assigned: 0,
                votes_cast: 0,
                total_response_ledgers: 0,
            })
    }

    fn store_arbiter_stats(env: &Env, arbiter: &Address, stats: &ArbiterStats) {
        let key = DataKey::ArbiterStats(arbiter.clone());
        env.storage().persistent().set(&key, stats);
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Count a newly raised dispute against every member of its panel.
    pub(crate) fn record_arbiter_assignments(env: &Env, arbiters: &Vec<Address>) {
        for arbiter in arbiters.iter() {
            let mut stats = Self::load_arbiter_stats(env, &arbiter);
            stats.disputes_assigned += 1;
            Self::store_arbiter_stats(env, &arbiter, &stats);
        }
    }

    /// Record a cast vote and how many ledgers after the dispute was raised
    /// it arrived. Must run before `DisputeRaisedAt` is cleared.
    pub(crate) fn record_arbiter_vote(
        env: &Env,
        arbiter: &Address,
        engagement_id: &String,
        milestone_index: u32,
    ) {
        let now = env.ledger().sequence();
        let raised_at: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DisputeRaisedAt(engagement_id.clone(), milestone_index))
            .unwrap_or(now);
        let mut stats = Self::load_arbiter_stats(env, arbiter);
        stats.votes_cast += 1;
        stats.total_response_ledgers += now.saturating_sub(raised_at) as u64;
        Self::store_arbiter_stats(env, arbiter, &stats);
    }
}
