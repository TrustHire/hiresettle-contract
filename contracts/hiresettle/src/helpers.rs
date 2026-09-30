use soroban_sdk::{contractimpl, token, Address, Env, String, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // INTERNAL HELPERS
    // ----------------------------------------------------------

    pub(crate) fn get_engagement_internal(env: &Env, engagement_id: &String) -> Engagement {
        env.storage()
            .persistent()
            .get(&DataKey::Engagement(engagement_id.clone()))
            .unwrap_or_else(|| panic!("engagement not found"))
    }

    /// Shared "load milestone by index or panic" helper (issue #170) — mirrors
    /// `get_engagement_internal` for the milestone lookup, so every call site
    /// panics with the same `"invalid milestone index"` message instead of a
    /// bare Vec index-out-of-bounds panic.
    pub(crate) fn get_milestone_or_panic(engagement: &Engagement, milestone_index: u32) -> Milestone {
        engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("{}", ERR_INVALID_MILESTONE_INDEX))
    }



    pub(crate) fn get_admin_internal(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("admin not initialized"))
    }

    pub(crate) fn assert_admin(env: &Env, admin: &Address) {
        let renounced: bool = env
            .storage()
            .instance()
            .get(&DataKey::AdminRenounced)
            .unwrap_or(false);
        if renounced {
            panic!("NoAdmin");
        }
        admin.require_auth();
        if *admin != Self::get_admin_internal(env) {
            panic!("{}", ERR_UNAUTHORIZED);
        }
    }

    pub(crate) fn is_paused_internal(env: &Env) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    pub(crate) fn assert_not_paused(env: &Env) {
        if Self::is_paused_internal(env) {
            panic!("ContractPaused");
        }
    }

    pub(crate) fn is_engagement_paused_internal(env: &Env, engagement_id: &String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::EngagementPaused(engagement_id.clone()))
            .unwrap_or(false)
    }

    /// Per-engagement quarantine guard (issue #239). Layered on top of — not a
    /// replacement for — `assert_not_paused`: the global pause halts every
    /// engagement, this one halts a single quarantined ID.
    pub(crate) fn assert_engagement_not_paused(env: &Env, engagement_id: &String) {
        if Self::is_engagement_paused_internal(env, engagement_id) {
            panic!("{}", ERR_ENGAGEMENT_PAUSED);
        }
    }

    pub(crate) fn is_milestone_on_hold_internal(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
    ) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey2::MilestoneHold(engagement_id.clone(), milestone_index))
    }

    /// Per-milestone compliance-hold guard (issue #492). Layered under — not a
    /// replacement for — `assert_not_paused` and `assert_engagement_not_paused`:
    /// callers check all three, and none of them clears another.
    pub(crate) fn assert_milestone_not_on_hold(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
    ) {
        if Self::is_milestone_on_hold_internal(env, engagement_id, milestone_index) {
            panic!("{}", ERR_MILESTONE_ON_HOLD);
        }
    }


    /// Terminal engagement states — no further state transitions are possible.
    pub(crate) fn is_terminal_status(status: &EngagementStatus) -> bool {
        matches!(
            status,
            EngagementStatus::Completed | EngagementStatus::Cancelled | EngagementStatus::Expired
        )
    }





    pub(crate) fn get_platform_fee_internal(env: &Env) -> PlatformFee {
        env.storage()
            .persistent()
            .get(&DataKey::PlatformFee)
            .unwrap_or_else(|| PlatformFee {
                bps: 0,
                treasury: Self::get_admin_internal(env),
            })
    }

    /// Resolve the effective platform-fee bps for an engagement of the given
    /// `total_amount`. Walks configured fee tiers (highest threshold first)
    /// and returns the first matching tier's bps, or falls back to the base
    /// platform fee if no tier matches.
    pub(crate) fn resolve_platform_fee_bps(env: &Env, base_bps: u32, total_amount: i128) -> u32 {
        let tiers: Option<Vec<FeeTier>> = env.storage().persistent().get(&DataKey::FeeTiers);
        if let Some(tiers) = tiers {
            let len = tiers.len();
            if len > 0 {
                // Walk from highest threshold to lowest.
                let mut i = len;
                while i > 0 {
                    i -= 1;
                    let tier = tiers.get(i).unwrap();
                    if total_amount >= tier.threshold {
                        return tier.bps;
                    }
                }
            }
        }
        base_bps
    }

    /// Tier-resolved platform-fee bps for one engagement: the rate
    /// snapshotted at creation if the engagement opted into
    /// `snapshot_fee_tier` (issue #505), else `resolve_platform_fee_bps`
    /// against the live tiers. Waivers and referral discounts are applied by
    /// the caller on top of this.
    pub(crate) fn engagement_tier_bps(
        env: &Env,
        engagement_id: &String,
        base_bps: u32,
        total_amount: i128,
    ) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey2::FeeTierSnapshot(engagement_id.clone()))
            .unwrap_or_else(|| Self::resolve_platform_fee_bps(env, base_bps, total_amount))
    }

    /// Whether the admin has waived the platform fee for this engagement
    /// (issue #335).
    pub(crate) fn is_fee_waived_internal(env: &Env, engagement_id: &String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::FeeWaived(engagement_id.clone()))
            .unwrap_or(false)
    }

    /// Resolve the effective platform-fee bps for a milestone payout,
    /// collapsing to 0 when the engagement has an active fee waiver
    /// (issue #335). Otherwise defers to `engagement_tier_bps`.
    pub(crate) fn effective_platform_fee_bps(
        env: &Env,
        engagement_id: &String,
        base_bps: u32,
        total_amount: i128,
    ) -> u32 {
        if Self::is_fee_waived_internal(env, engagement_id) {
            return 0;
        }
        Self::engagement_tier_bps(env, engagement_id, base_bps, total_amount)
    }



    /// Base platform-fee bps for an engagement's token: the per-token
    /// override if configured (issue #479), else the global `global_bps`.
    /// Fee tiers and referral discounts are applied on top by the caller.
    pub(crate) fn token_base_bps(env: &Env, token: &Address, global_bps: u32) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey2::TokenPlatformFee(token.clone()))
            .unwrap_or(global_bps)
    }

    /// Platform fee owed on `payment` at `bps` (issue #478): the bps-derived
    /// amount raised to the admin fee floor, but never above `payment`
    /// itself. Waived engagements always pay zero, floor or not.
    pub(crate) fn platform_fee_amount(
        env: &Env,
        engagement_id: &String,
        payment: i128,
        bps: u32,
    ) -> i128 {
        if Self::is_fee_waived_internal(env, engagement_id) {
            return 0;
        }
        let floor: i128 = env
            .storage()
            .persistent()
            .get(&DataKey2::PlatformFeeFloor)
            .unwrap_or(0);
        let fee = (payment * bps as i128) / 10_000;
        fee.max(floor).min(payment.max(0))
    }

    /// Risk score weights, falling back to the documented defaults.
    pub(crate) fn risk_score_weights_internal(env: &Env) -> RiskScoreWeights {
        env.storage()
            .persistent()
            .get(&DataKey2::RiskScoreWeights)
            .unwrap_or(RiskScoreWeights {
                dispute_weight: DEFAULT_RISK_DISPUTE_WEIGHT,
                replacement_weight: DEFAULT_RISK_REPLACEMENT_WEIGHT,
                extension_weight: DEFAULT_RISK_EXTENSION_WEIGHT,
            })
    }

    /// Whether `referrer` is on the admin-configured recognised referral list.
    pub(crate) fn is_recognised_referrer(env: &Env, referrer: &Address) -> bool {
        let referrers: Option<Vec<Address>> = env.storage().persistent().get(&DataKey::Referrers);
        if let Some(list) = referrers {
            for i in 0..list.len() {
                if list.get(i).unwrap() == *referrer {
                    return true;
                }
            }
        }
        false
    }

    /// If the engagement has a recognised referrer, reduce the given bps
    /// by the admin-configured referral discount (never below 0).
    pub(crate) fn apply_referral_discount(env: &Env, bps: u32, referrer: &Option<Address>) -> u32 {
        if let Some(ref_addr) = referrer {
            if Self::is_recognised_referrer(env, ref_addr) {
                let discount: u32 = env
                    .storage()
                    .persistent()
                    .get(&DataKey::Config(ConfigKey::ReferralDiscountBps))
                    .unwrap_or(0u32);
                return bps.saturating_sub(discount);
            }
        }
        bps
    }

    pub(crate) fn get_ledgers_per_day_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::LedgersPerDay))
            .unwrap_or(LEDGERS_PER_DAY)
    }

    pub(crate) fn get_max_proof_hash_length_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxProofHashLength))
            .unwrap_or(MAX_PROOF_HASH_LENGTH)
    }

    pub(crate) fn get_max_replacements_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxReplacements))
            .unwrap_or(DEFAULT_MAX_REPLACEMENTS)
    }




    /// Decrement the per-company active engagement count, saturating at 0.
    /// Called whenever an engagement leaves the active pool (completed,
    /// cancelled, expired, etc).
    pub(crate) fn decrement_company_active_count(env: &Env, company: &Address) {
        let active_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyActiveCount(company.clone()))
            .unwrap_or(0u32);
        env.storage().persistent().set(
            &DataKey::CompanyActiveCount(company.clone()),
            &active_count.saturating_sub(1),
        );
    }


    // ----------------------------------------------------------
    // ISSUE #460 — WEIGHTED ARBITER VOTING
    // ----------------------------------------------------------

    /// Vote weight of the arbiter in slot `slot_index`; 1 when the engagement
    /// has no `arbiter_weights`.
    pub(crate) fn arbiter_weight(engagement: &Engagement, slot_index: u32) -> u32 {
        match &engagement.arbiter_weights {
            Some(weights) => weights.get(slot_index).unwrap(),
            None => 1,
        }
    }

    /// Sum of every arbiter's weight; `arbiters.len()` when unweighted.
    pub(crate) fn total_arbiter_weight(engagement: &Engagement) -> u32 {
        match &engagement.arbiter_weights {
            Some(weights) => {
                let mut total: u32 = 0;
                for i in 0..weights.len() {
                    total += weights.get(i).unwrap();
                }
                total
            }
            None => engagement.arbiters.len(),
        }
    }

    pub(crate) fn empty_vote_record(env: &Env) -> ArbiterVoteRecord {
        ArbiterVoteRecord {
            approve_votes: 0,
            reject_votes: 0,
            voted: Vec::new(env),
            approve_weight: 0,
            reject_weight: 0,
        }
    }

    // ----------------------------------------------------------
    // ISSUE #463 — ARBITER VOTE DELEGATION
    // ----------------------------------------------------------

    /// Resolve the arbiter slot a voting `caller` acts for: the caller's own
    /// slot if they are an arbiter, otherwise the slot of the arbiter who has
    /// named them as vote delegate on this engagement. Returns the slot's
    /// arbiter address and index; panics `"unauthorized"` if neither applies.
    pub(crate) fn resolve_voting_arbiter(
        env: &Env,
        engagement: &Engagement,
        engagement_id: &String,
        caller: &Address,
    ) -> (Address, u32) {
        for i in 0..engagement.arbiters.len() {
            if engagement.arbiters.get(i).unwrap() == *caller {
                return (caller.clone(), i);
            }
        }
        for i in 0..engagement.arbiters.len() {
            let arbiter = engagement.arbiters.get(i).unwrap();
            let delegate: Option<Address> = env.storage().persistent().get(
                &DataKey2::ArbiterVoteDelegate(engagement_id.clone(), arbiter.clone()),
            );
            if delegate.as_ref() == Some(caller) {
                return (arbiter, i);
            }
        }
        panic!("{}", ERR_UNAUTHORIZED);
    }

    // ----------------------------------------------------------
    // ISSUE #461 — MILESTONE PREREQUISITES
    // ----------------------------------------------------------

    /// Panics unless every prerequisite index is in range and the
    /// prerequisite graph has no cycle (a self-reference counts as a cycle).
    pub(crate) fn validate_milestone_prerequisites(milestones: &Vec<Milestone>) {
        let n = milestones.len();
        for i in 0..n {
            let prereqs = milestones.get(i).unwrap().prerequisites;
            for j in 0..prereqs.len() {
                if prereqs.get(j).unwrap() >= n {
                    panic!("InvalidPrerequisiteIndex: milestone {}", i);
                }
            }
        }

        // Kahn's algorithm: repeatedly retire milestones whose prerequisites
        // are all retired. If a pass retires nothing while some remain, the
        // remainder contains a cycle. n ≤ DEFAULT_MAX_MILESTONES, so the
        // fixed-size bitmap and O(n³) worst case are both trivially bounded.
        let mut done = [false; DEFAULT_MAX_MILESTONES as usize];
        let mut remaining = n;
        while remaining > 0 {
            let mut progressed = false;
            for i in 0..n {
                if done[i as usize] {
                    continue;
                }
                let prereqs = milestones.get(i).unwrap().prerequisites;
                if (0..prereqs.len()).all(|j| done[prereqs.get(j).unwrap() as usize]) {
                    done[i as usize] = true;
                    remaining -= 1;
                    progressed = true;
                }
            }
            if !progressed {
                panic!("PrerequisiteCycle");
            }
        }
    }

    /// Panics `"PreviousMilestoneNotComplete"` unless every prerequisite of
    /// `milestone` is `Confirmed` or `Resolved`.
    pub(crate) fn assert_prerequisites_complete(engagement: &Engagement, milestone: &Milestone) {
        for j in 0..milestone.prerequisites.len() {
            let prereq = engagement
                .milestones
                .get(milestone.prerequisites.get(j).unwrap())
                .unwrap();
            if prereq.status != MilestoneStatus::Confirmed
                && prereq.status != MilestoneStatus::Resolved
            {
                panic!("PreviousMilestoneNotComplete");
            }
        }
    }

    // ----------------------------------------------------------
    // ISSUE #462 — SPLIT-VOTE WITHHELD ESCROW
    // ----------------------------------------------------------

    /// Refund to the company any milestone share withheld by split-vote
    /// resolutions. Call whenever an engagement transitions to `Completed`;
    /// a no-op for engagements that never had a split resolution. Cancel and
    /// expiry need no call because they already refund
    /// `total_amount - released_amount`, which includes the withheld amount.
    ///
    /// The refund is added to `released_amount` so `total_amount -
    /// released_amount` keeps reporting the true escrow balance, and is capped
    /// at that balance so it can never over-draw escrow and block completion.
    pub(crate) fn refund_split_withheld(
        env: &Env,
        engagement_id: &String,
        engagement: &mut Engagement,
    ) {
        let key = DataKey2::SplitWithheld(engagement_id.clone());
        let recorded: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if recorded <= 0 {
            return;
        }
        env.storage().persistent().remove(&key);
        let withheld = recorded.min(engagement.total_amount - engagement.released_amount);
        if withheld <= 0 {
            return;
        }
        engagement.released_amount += withheld;
        token::Client::new(env, &engagement.token).transfer(
            &env.current_contract_address(),
            &engagement.company,
            &withheld,
        );
        env.events().publish(
            (
                soroban_sdk::Symbol::new(env, "split_withheld_refunded"),
                engagement_id.clone(),
            ),
            withheld,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #56 — CO-RECRUITER SPLIT PAYOUT
    // ----------------------------------------------------------

    /// Distribute the net payment (after platform / arbiter fee) between the
    /// primary recruiter and the optional co-recruiter.
    ///
    /// When `co_recruiter` is `Some`, the primary receives
    /// `net * split_bps / 10_000` and the co-recruiter receives the remainder.
    /// When `co_recruiter` is `None` the full net amount goes to the recruiter.
    ///
    /// When `apply_payout_preference` is `true`, each payee's share is routed
    /// through `pay_recruiter_share`, swapping it into their preferred payout
    /// token if one is set and a swap adapter is configured (issue #458).
    pub(crate) fn distribute_recruiter_payout(
        env: &Env,
        engagement: &Engagement,
        net_payment: i128,
        token_client: &token::Client,
        apply_payout_preference: bool,
    ) {
        let pay = |recipient: &Address, amount: i128| {
            if apply_payout_preference {
                Self::pay_recruiter_share(env, token_client, recipient, amount);
            } else {
                token_client.transfer(&env.current_contract_address(), recipient, &amount);
            }
        };
        match &engagement.co_recruiter {
            Some(co_recruiter) => {
                let split = engagement.recruiter_split_bps as i128;
                let primary_payment = (net_payment * split) / (FULL_SPLIT_BPS as i128);
                let co_payment = net_payment - primary_payment;
                pay(&engagement.recruiter, primary_payment);
                pay(co_recruiter, co_payment);
            }
            None => pay(&engagement.recruiter, net_payment),
        }
    }
}
