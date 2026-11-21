//! Issue #676: recruiter counter-offers on an `Offered` engagement, and
//! issue #677: open offers that any of several eligible recruiters may claim.
//!
//! Both build on the recruiter acceptance step (issue #569): an engagement
//! created with `require_recruiter_acceptance` sits in `Offered` with its
//! escrow locked until the recruiter accepts. Here the recruiter may instead
//! propose different terms, and an open offer names a set of recruiters
//! rather than one.

use crate::*;
use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol, Vec};

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #677 — OPEN OFFERS
    // ----------------------------------------------------------

    /// Create an open offer: an engagement whose recruiter is not chosen yet.
    /// Takes the same arguments as `create_engagement`, except
    /// `eligible_recruiters` replaces `recruiter`. The engagement is created
    /// `Offered` (whatever `config.require_recruiter_acceptance` says), and
    /// the first eligible recruiter (or the co-signer of one) to call
    /// `accept_engagement` is bound as its recruiter.
    ///
    /// Until then `Engagement::recruiter` holds the contract's own address
    /// as a placeholder, and the engagement does not appear in any
    /// recruiter's engagement index; see `get_eligible_recruiters`.
    ///
    /// The company's active engagement slot is taken at creation, as for any
    /// `Offered` engagement. The recruiter's caps are checked at acceptance.
    /// `withdraw_offer` and `expire_engagement` work as for any other offer.
    ///
    /// # Panics
    /// Everything `create_engagement` can raise, plus:
    /// - `"NoEligibleRecruiters"` — `eligible_recruiters` is empty.
    /// - `"TooManyEligibleRecruiters"` — more than 20 addresses.
    /// - `"DuplicateEligibleRecruiter"` — an address appears twice.
    /// - `"CompanyRecruiterCollision"` / `"RecruiterArbiterCollision"` — an
    ///   eligible recruiter is the company or an arbiter.
    /// - `"OpenOfferBondUnsupported"` — `config.recruiter_bond_amount` is set;
    ///   there is no recruiter to post it yet.
    ///
    /// # Events
    /// The usual `engagement_created` / `engagement_offered`, plus
    /// `("open_offer_created", engagement_id)` with
    /// `(company, eligible_recruiters, total_amount)`.
    pub fn create_open_engagement(
        env: Env,
        engagement_id: String,
        company: Address,
        eligible_recruiters: Vec<Address>,
        arbiter_setup: ArbiterSetup,
        token: Address,
        total_amount: i128,
        job_title: String,
        milestones: Vec<Milestone>,
        retention_days: Vec<u32>,
        config: EngagementConfig,
    ) -> String {
        Self::assert_not_paused(&env);
        company.require_auth();

        if eligible_recruiters.is_empty() {
            panic!("NoEligibleRecruiters");
        }
        if eligible_recruiters.len() > MAX_ELIGIBLE_RECRUITERS {
            panic!("TooManyEligibleRecruiters");
        }
        for i in 0..eligible_recruiters.len() {
            let candidate = eligible_recruiters.get(i).unwrap();
            if candidate == company {
                panic!("CompanyRecruiterCollision");
            }
            if arbiter_setup.arbiters.contains(&candidate) {
                panic!("RecruiterArbiterCollision");
            }
            for j in (i + 1)..eligible_recruiters.len() {
                if eligible_recruiters.get(j).unwrap() == candidate {
                    panic!("DuplicateEligibleRecruiter");
                }
            }
        }
        if config.recruiter_bond_amount.is_some() {
            panic!("OpenOfferBondUnsupported");
        }

        let mut config = config;
        config.require_recruiter_acceptance = true;
        let placeholder = env.current_contract_address();
        let id = Self::create_engagement_impl(
            env.clone(),
            engagement_id,
            company.clone(),
            placeholder,
            arbiter_setup,
            token,
            total_amount,
            job_title,
            milestones,
            retention_days,
            config,
            true,
        );

        let key = DataKey3::EligibleRecruiters(id.clone());
        env.storage().persistent().set(&key, &eligible_recruiters);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "open_offer_created"), id.clone()),
            (company, eligible_recruiters, total_amount),
        );
        id
    }

    /// Recruiters who may still claim an open offer; empty once it has been
    /// claimed, withdrawn or expired, and for an ordinary engagement.
    pub fn get_eligible_recruiters(env: Env, engagement_id: String) -> Vec<Address> {
        Self::eligible_recruiters_internal(&env, &engagement_id).unwrap_or_else(|| Vec::new(&env))
    }

    /// Whether the engagement is an open offer no recruiter has claimed yet.
    pub fn is_open_offer(env: Env, engagement_id: String) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey3::EligibleRecruiters(engagement_id))
    }

    pub(crate) fn eligible_recruiters_internal(
        env: &Env,
        engagement_id: &String,
    ) -> Option<Vec<Address>> {
        env.storage()
            .persistent()
            .get(&DataKey3::EligibleRecruiters(engagement_id.clone()))
    }

    /// The eligible recruiter `caller` acts for: itself, or the eligible
    /// recruiter whose co-signer it is.
    ///
    /// # Panics
    /// - `"RecruiterNotEligible"`
    pub(crate) fn resolve_eligible_recruiter(
        env: &Env,
        eligible: &Vec<Address>,
        caller: &Address,
    ) -> Address {
        for candidate in eligible.iter() {
            if Self::is_authorized_recruiter(env, caller, &candidate) {
                return candidate;
            }
        }
        panic!("RecruiterNotEligible");
    }

    /// Bind `recruiter` to an open offer: check its active engagement caps,
    /// re-check role collisions (the company or the panel may have changed
    /// since creation), set it on the engagement and index the engagement
    /// under it. The caller saves `engagement`.
    pub(crate) fn bind_open_offer_recruiter(
        env: &Env,
        engagement: &mut Engagement,
        engagement_id: &String,
        recruiter: &Address,
    ) {
        if *recruiter == engagement.company {
            panic!("CompanyRecruiterCollision");
        }
        if engagement.arbiters.contains(recruiter) {
            panic!("RecruiterArbiterCollision");
        }
        Self::assert_recruiter_below_cap(env, recruiter);
        engagement.recruiter = recruiter.clone();
        Self::index_recruiter_engagement(env, recruiter, engagement_id);
    }

    /// Move an `Offered` engagement to `Active`, save it, and discard any
    /// leftover offer state. Shared by `accept_engagement` and
    /// `accept_counter_offer`.
    pub(crate) fn activate_offered_engagement(
        env: &Env,
        engagement: &mut Engagement,
        engagement_id: &String,
        actor: &Address,
    ) {
        for i in 0..engagement.milestones.len() {
            let m = engagement.milestones.get(i).unwrap();
            if m.kind == MilestoneKind::Placement && m.status == MilestoneStatus::Pending {
                Self::mark_milestone_pending_since(env, engagement_id, i);
            }
        }

        engagement.status = EngagementStatus::Active;
        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), engagement);
        Self::extend_engagement_ttl(env, engagement_id);
        Self::clear_offer_state(env, engagement_id);

        Self::emit_engagement_status_changed(
            env,
            engagement_id,
            EngagementStatus::Offered,
            EngagementStatus::Active,
            Some(actor.clone()),
        );
    }

    /// Remove the pending counter-offer and the open-offer allowlist, if
    /// any. Called whenever an engagement leaves `Offered`.
    pub(crate) fn clear_offer_state(env: &Env, engagement_id: &String) {
        env.storage()
            .persistent()
            .remove(&DataKey3::CounterOffer(engagement_id.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey3::EligibleRecruiters(engagement_id.clone()));
    }

    // ----------------------------------------------------------
    // ISSUE #676 — COUNTER-OFFERS
    // ----------------------------------------------------------

    /// Recruiter (or its co-signer) proposes different terms for an
    /// `Offered` engagement: a new `total_amount` and a new
    /// `payment_percent` for each milestone, by index. On an open offer any
    /// eligible recruiter may counter, and is the one bound if the company
    /// accepts.
    ///
    /// Only one counter-offer may be pending per engagement. It stays
    /// pending until the company accepts or rejects it, the recruiter
    /// withdraws it, or the engagement leaves `Offered`. The recruiter can
    /// still `accept_engagement` on the original terms in the meantime,
    /// which discards the counter.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"EngagementNotOffered"` — the engagement is not in `Offered` status.
    /// - `"unauthorized"` / `"RecruiterNotEligible"` — caller may not counter.
    /// - `"CounterOfferPending"` — a counter-offer is already pending.
    /// - `"amount must be greater than zero"` / `"AmountBelowMinimum"`
    /// - `"CounterOfferMilestoneMismatch"` — `milestone_percents` does not
    ///   have one entry per milestone.
    /// - `"milestone percentages must sum to 100"`
    /// - `"CounterOfferUnchanged"` — the terms equal the current ones.
    ///
    /// # Events
    /// `("counter_offer_proposed", engagement_id)` with
    /// `(recruiter, total_amount, milestone_percents)`.
    pub fn counter_offer(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        total_amount: i128,
        milestone_percents: Vec<u32>,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Offered {
            panic!("EngagementNotOffered");
        }
        let bound = match Self::eligible_recruiters_internal(&env, &engagement_id) {
            Some(eligible) => Self::resolve_eligible_recruiter(&env, &eligible, &recruiter),
            None => {
                if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
                    panic!("{}", ERR_UNAUTHORIZED);
                }
                engagement.recruiter.clone()
            }
        };

        let key = DataKey3::CounterOffer(engagement_id.clone());
        if env.storage().persistent().has(&key) {
            panic!("CounterOfferPending");
        }

        if total_amount <= 0 {
            panic!("amount must be greater than zero");
        }
        if total_amount < Self::get_effective_min_amount(env.clone(), engagement.token.clone()) {
            panic!("AmountBelowMinimum");
        }
        if milestone_percents.len() != engagement.milestones.len() {
            panic!("CounterOfferMilestoneMismatch");
        }
        let mut percent_sum: u32 = 0;
        let mut unchanged = total_amount == engagement.total_amount;
        for i in 0..milestone_percents.len() {
            let pct = milestone_percents.get(i).unwrap();
            percent_sum = percent_sum.saturating_add(pct);
            if pct != engagement.milestones.get(i).unwrap().payment_percent {
                unchanged = false;
            }
        }
        if percent_sum != 100 {
            panic!("milestone percentages must sum to 100");
        }
        if unchanged {
            panic!("CounterOfferUnchanged");
        }

        let counter = CounterOffer {
            proposer: recruiter,
            recruiter: bound.clone(),
            total_amount,
            milestone_percents: milestone_percents.clone(),
            proposed_at_ledger: env.ledger().sequence(),
        };
        env.storage().persistent().set(&key, &counter);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "counter_offer_proposed"), engagement_id),
            (bound, total_amount, milestone_percents),
        );
    }

    /// Company (or its co-signer) accepts the pending counter-offer. The
    /// escrow is adjusted to the new `total_amount` in the same call — the
    /// increase is pulled from the caller (from the company pool for a
    /// pool-funded engagement), or the decrease is returned to the company
    /// (to its pool for a pool-funded engagement). The new milestone
    /// percentages are applied, and the engagement becomes `Active` with the
    /// counter's recruiter bound, since the recruiter already agreed to
    /// these terms.
    ///
    /// For an engagement created with `snapshot_fee_tier`, the fee tier is
    /// re-resolved against the new total.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"EngagementNotOffered"` — the engagement is not in `Offered` status.
    /// - `"unauthorized"` — caller is not the company or its co-signer.
    /// - `"NoCounterOffer"` — no counter-offer is pending.
    /// - `"InsufficientCompanyBalance"` — pool-funded, and the pool cannot
    ///   cover the increase.
    /// - `"RecruiterActiveLimitReached"` and the collision panics — open
    ///   offer, and the recruiter can no longer be bound.
    ///
    /// # Events
    /// `("counter_offer_accepted", engagement_id)` with
    /// `(company, old_total, new_total)`, then `engagement_accepted` with the
    /// bound recruiter and the `Offered → Active` status-change event.
    pub fn accept_counter_offer(env: Env, company: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Offered {
            panic!("EngagementNotOffered");
        }
        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        let counter: CounterOffer = env
            .storage()
            .persistent()
            .get(&DataKey3::CounterOffer(engagement_id.clone()))
            .unwrap_or_else(|| panic!("NoCounterOffer"));

        if Self::eligible_recruiters_internal(&env, &engagement_id).is_some() {
            Self::bind_open_offer_recruiter(
                &env,
                &mut engagement,
                &engagement_id,
                &counter.recruiter,
            );
        }

        // Nothing is released or refunded while `Offered`, so the escrow is
        // exactly `total_amount`.
        let old_total = engagement.total_amount;
        let delta = counter.total_amount - old_total;
        if delta > 0 {
            if Self::is_pool_funded(&env, &engagement_id) {
                Self::debit_company_pool(&env, &engagement.company, &engagement.token, delta);
            } else {
                token::Client::new(&env, &engagement.token).transfer(
                    &company,
                    &env.current_contract_address(),
                    &delta,
                );
            }
        } else if delta < 0 {
            Self::return_escrow_to_company(&env, &engagement, &engagement_id, -delta);
        }
        if delta != 0 {
            Self::adjust_escrow(&env, &engagement_id, &engagement.token, delta);
        }

        engagement.total_amount = counter.total_amount;
        for i in 0..engagement.milestones.len() {
            let mut m = engagement.milestones.get(i).unwrap();
            m.payment_percent = counter.milestone_percents.get(i).unwrap();
            engagement.milestones.set(i, m);
        }

        let snapshot_key = DataKey2::FeeTierSnapshot(engagement_id.clone());
        if env.storage().persistent().has(&snapshot_key) {
            let base_bps = Self::get_platform_fee_internal(&env).bps;
            let bps = Self::resolve_platform_fee_bps(&env, base_bps, counter.total_amount);
            env.storage().persistent().set(&snapshot_key, &bps);
        }

        Self::activate_offered_engagement(&env, &mut engagement, &engagement_id, &company);

        env.events().publish(
            (
                Symbol::new(&env, "counter_offer_accepted"),
                engagement_id.clone(),
            ),
            (company, old_total, counter.total_amount),
        );
        env.events().publish(
            (Symbol::new(&env, "engagement_accepted"), engagement_id),
            counter.recruiter,
        );
    }

    /// Company (or its co-signer) rejects the pending counter-offer. The
    /// engagement stays `Offered` on its original terms, and a new
    /// counter-offer may be made.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"EngagementNotOffered"` / `"unauthorized"` / `"NoCounterOffer"`
    ///
    /// # Events
    /// `("counter_offer_rejected", engagement_id)` with `(company, recruiter)`.
    pub fn reject_counter_offer(env: Env, company: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Offered {
            panic!("EngagementNotOffered");
        }
        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        let key = DataKey3::CounterOffer(engagement_id.clone());
        let counter: CounterOffer = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("NoCounterOffer"));
        env.storage().persistent().remove(&key);

        env.events().publish(
            (Symbol::new(&env, "counter_offer_rejected"), engagement_id),
            (company, counter.recruiter),
        );
    }

    /// The recruiter that made the pending counter-offer (or its co-signer)
    /// withdraws it, so it can make a different one.
    ///
    /// # Panics
    /// - `"NoCounterOffer"` / `"unauthorized"`
    ///
    /// # Events
    /// `("counter_offer_withdrawn", engagement_id)` with the recruiter.
    pub fn withdraw_counter_offer(env: Env, recruiter: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        recruiter.require_auth();

        let key = DataKey3::CounterOffer(engagement_id.clone());
        let counter: CounterOffer = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("NoCounterOffer"));
        if !Self::is_authorized_recruiter(&env, &recruiter, &counter.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        env.storage().persistent().remove(&key);

        env.events().publish(
            (Symbol::new(&env, "counter_offer_withdrawn"), engagement_id),
            counter.recruiter,
        );
    }

    /// The pending counter-offer on an engagement, if any.
    pub fn get_counter_offer(env: Env, engagement_id: String) -> Option<CounterOffer> {
        env.storage()
            .persistent()
            .get(&DataKey3::CounterOffer(engagement_id))
    }
}
