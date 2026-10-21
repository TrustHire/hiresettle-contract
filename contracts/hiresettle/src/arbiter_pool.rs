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

    // ----------------------------------------------------------
    // ISSUE #562 — ARBITER POOL MEMBER SUSPENSION
    // ----------------------------------------------------------

    /// Suspend a pool member from new panel draws until `until_ledger`.
    /// Suspended members keep their stats history and stay seated on panels
    /// already drawn; only future draws skip them. Suspension expires
    /// automatically once `until_ledger` is reached (lazy check, no cleanup).
    ///
    /// # Panics
    /// - `"NotInPool"` — the address is not a pool member.
    /// - `"InvalidUntilLedger"` — `until_ledger` is not in the future.
    pub fn suspend_arbiter_pool_member(
        env: Env,
        admin: Address,
        arbiter: Address,
        until_ledger: u32,
    ) {
        Self::assert_admin(&env, &admin);
        if !Self::get_arbiter_pool(env.clone()).contains(&arbiter) {
            panic!("NotInPool");
        }
        if until_ledger <= env.ledger().sequence() {
            panic!("InvalidUntilLedger");
        }
        let key = DataKey2::ArbiterSuspension(arbiter.clone());
        env.storage().persistent().set(&key, &until_ledger);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
        env.events().publish(
            (Symbol::new(&env, "arbiter_suspended"),),
            (arbiter, until_ledger),
        );
    }

    /// Lift a pool member's suspension early. No-op when not suspended.
    ///
    /// # Panics
    /// - `"NotInPool"` — the address is not a pool member.
    pub fn unsuspend_arbiter_pool_member(env: Env, admin: Address, arbiter: Address) {
        Self::assert_admin(&env, &admin);
        if !Self::get_arbiter_pool(env.clone()).contains(&arbiter) {
            panic!("NotInPool");
        }
        env.storage()
            .persistent()
            .remove(&DataKey2::ArbiterSuspension(arbiter.clone()));
        env.events()
            .publish((Symbol::new(&env, "arbiter_unsuspended"),), arbiter);
    }

    /// Return the ledger until which `arbiter` is suspended, or `None` when
    /// not suspended (including when a past suspension has expired).
    pub fn get_arbiter_suspension(env: Env, arbiter: Address) -> Option<u32> {
        let until: u32 = env
            .storage()
            .persistent()
            .get(&DataKey2::ArbiterSuspension(arbiter))?;
        if until > env.ledger().sequence() {
            Some(until)
        } else {
            None
        }
    }

    /// Whether `arbiter` is currently suspended from new panel draws.
    pub(crate) fn is_arbiter_suspended(env: &Env, arbiter: &Address) -> bool {
        let until: Option<u32> = env
            .storage()
            .persistent()
            .get(&DataKey2::ArbiterSuspension(arbiter.clone()));
        until.map_or(false, |u| u > env.ledger().sequence())
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
                weights: None,
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
            // Issue #562: suspended members sit out future draws but keep
            // their stats (and stay seated on already-drawn panels).
            if member != *company
                && member != *recruiter
                && !Self::is_arbiter_suspended(env, &member)
            {
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
    /// - Otherwise, with `completion = 100 * votes_cast / (disputes_assigned +
    ///   missed_vote_deadlines)` (a missed vote deadline, issue #560, counts as
    ///   a second unanswered assignment)
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
        let assigned = stats.disputes_assigned as u64 + stats.missed_vote_deadlines as u64;
        let completion = votes * 100 / assigned;
        let speed = stats.total_response_ledgers.checked_div(votes).map_or(0, |avg| {
            100 * ARBITER_RESPONSE_REFERENCE_LEDGERS / (ARBITER_RESPONSE_REFERENCE_LEDGERS + avg)
        });
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
                missed_vote_deadlines: 0,
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

    /// Record that `arbiter` was recused for missing a vote deadline (issue #560).
    pub(crate) fn record_missed_vote_deadline(env: &Env, arbiter: &Address) {
        let mut stats = Self::load_arbiter_stats(env, arbiter);
        stats.missed_vote_deadlines += 1;
        Self::store_arbiter_stats(env, arbiter, &stats);
    }

    // ----------------------------------------------------------
    // ARBITER CONFLICT-OF-INTEREST DECLARATIONS
    // ----------------------------------------------------------

    /// Arbiter declares a conflict of interest on an engagement, recusing
    /// themselves from voting on all disputes for that engagement.
    ///
    /// # Caller
    /// Must be an arbiter on the engagement's panel and must sign the transaction.
    ///
    /// # Behaviour
    /// - The arbiter must be on the engagement's arbiter panel.
    /// - The declaration is permanent for this engagement (cannot be undone by
    ///   the arbiter, only by admin via `clear_conflict_declaration`).
    /// - Once declared, the arbiter cannot vote on any disputes for this engagement.
    /// - The declaration does not affect other engagements.
    /// - `reason` is limited to 256 characters for storage efficiency.
    ///
    /// # Panics
    /// - `"engagement not found"` — no engagement exists for `engagement_id`.
    /// - `"unauthorized"` — the caller is not on the engagement's arbiter panel.
    /// - `"ConflictReasonTooLong"` — `reason` exceeds 256 characters.
    /// - `"ConflictAlreadyDeclared"` — an active declaration already exists.
    ///
    /// # Events
    /// Emits `("conflict_declared", engagement_id)` with `arbiter`.
    pub fn declare_conflict_of_interest(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        reason: String,
    ) {
        arbiter.require_auth();

        if reason.len() > 256 {
            panic!("ConflictReasonTooLong");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        // Verify arbiter is on the panel
        if !engagement.arbiters.contains(&arbiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey2::ConflictDeclaration(engagement_id.clone(), arbiter.clone());

        // Check if a declaration already exists
        if let Some(existing) = env.storage().persistent().get::<_, ConflictDeclaration>(&key) {
            if existing.active {
                panic!("ConflictAlreadyDeclared");
            }
        }

        let declaration = ConflictDeclaration {
            arbiter: arbiter.clone(),
            reason,
            declared_at_ledger: env.ledger().sequence(),
            active: true,
        };

        env.storage().persistent().set(&key, &declaration);
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "conflict_declared"), engagement_id),
            arbiter,
        );
    }

    /// Return the conflict-of-interest declaration for an arbiter on an
    /// engagement, if one exists and is active.
    pub fn get_conflict_declaration(
        env: Env,
        engagement_id: String,
        arbiter: Address,
    ) -> Option<ConflictDeclaration> {
        let key = DataKey2::ConflictDeclaration(engagement_id, arbiter);
        let decl: Option<ConflictDeclaration> = env.storage().persistent().get(&key);
        decl.filter(|d| d.active)
    }

    /// Return all active conflict declarations for an engagement.
    /// Useful for displaying which arbiters have recused themselves.
    pub fn get_engagement_conflicts(
        env: Env,
        engagement_id: String,
    ) -> Vec<ConflictDeclaration> {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let mut conflicts: Vec<ConflictDeclaration> = Vec::new(&env);

        for arbiter in engagement.arbiters.iter() {
            if let Some(decl) = Self::get_conflict_declaration(env.clone(), engagement_id.clone(), arbiter) {
                conflicts.push_back(decl);
            }
        }

        conflicts
    }

    /// Admin clears a conflict-of-interest declaration (e.g., if it was made
    /// in error). The historical record is preserved but marked inactive.
    ///
    /// # Panics
    /// - `"engagement not found"` — no engagement exists for `engagement_id`.
    /// - `"unauthorized"` — caller is not the admin.
    /// - `"NoConflictDeclaration"` — no active declaration exists.
    ///
    /// # Events
    /// Emits `("conflict_cleared", engagement_id)` with `arbiter`.
    pub fn clear_conflict_declaration(
        env: Env,
        admin: Address,
        engagement_id: String,
        arbiter: Address,
    ) {
        Self::assert_admin(&env, &admin);

        let _ = Self::get_engagement_internal(&env, &engagement_id);

        let key = DataKey2::ConflictDeclaration(engagement_id.clone(), arbiter.clone());
        let mut declaration: ConflictDeclaration = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("NoConflictDeclaration"));

        if !declaration.active {
            panic!("NoConflictDeclaration");
        }

        declaration.active = false;
        env.storage().persistent().set(&key, &declaration);
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "conflict_cleared"), engagement_id),
            arbiter,
        );
    }

    /// Check if an arbiter has an active conflict declaration for an engagement.
    /// Used internally to prevent conflicted arbiters from voting.
    pub(crate) fn has_conflict_of_interest(
        env: &Env,
        engagement_id: &String,
        arbiter: &Address,
    ) -> bool {
        let key = DataKey2::ConflictDeclaration(engagement_id.clone(), arbiter.clone());
        if let Some(decl) = env.storage().persistent().get::<_, ConflictDeclaration>(&key) {
            decl.active
        } else {
            false
        }
    }
}
