//! Issue #671: read-only book-keeping invariant checks, and issue #672:
//! admin recovery of tokens sent to the contract outside any tracked flow.
//!
//! Every token the contract holds belongs to one of six tracked buckets:
//! engagement escrow, pooled company balances, pull-payout credits,
//! unsettled bonds, redeemable fee rebates, and unclaimed streamed payouts.
//! Each bucket has a per-token running total updated wherever the bucket
//! changes. Anything held above their sum is stray: no record entitles
//! anyone to it.
//!
//! The running totals are forward-only. Balances that existed before a
//! bucket's total was introduced are not counted, so on a contract upgraded
//! from such a version, `recover_stray_tokens` must not be used until
//! `check_token_invariants` reports `totals_consistent`.

use crate::*;
use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol, Vec};

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #671 — ESCROW INVARIANT CHECKS
    // ----------------------------------------------------------

    /// Check one engagement's book-keeping. Read-only and callable while the
    /// contract or the engagement is paused.
    ///
    /// Compares the escrow tracked for the engagement against
    /// `total_amount - released_amount - refunded_amount`, and checks that
    /// the milestone states agree with the engagement status. See
    /// [`InvariantReport`] for the individual checks; `ok` is `true` only if
    /// all of them hold.
    ///
    /// # Panics
    /// - `"engagement not found"`
    pub fn check_escrow_invariants(env: Env, engagement_id: String) -> InvariantReport {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let total = engagement.total_amount;
        let released = engagement.released_amount;
        let refunded = Self::escrow_refunded_internal(&env, &engagement_id);
        let expected_escrow = total - released - refunded;
        let escrow_balance: Option<i128> = env
            .storage()
            .persistent()
            .get(&DataKey3::EscrowBalance(engagement_id.clone()));

        let balance_matches = escrow_balance == Some(expected_escrow);
        let amounts_in_range = released >= 0
            && refunded >= 0
            && released
                .checked_add(refunded)
                .is_some_and(|sum| sum <= total);

        let mut percent_sum: u32 = 0;
        for m in engagement.milestones.iter() {
            percent_sum = percent_sum.saturating_add(m.payment_percent);
        }
        let percents_sum_to_100 = percent_sum == 100;

        let milestone_states_consistent = match engagement.status {
            EngagementStatus::Completed => engagement.milestones.iter().all(|m| {
                m.status == MilestoneStatus::Confirmed || m.status == MilestoneStatus::Resolved
            }),
            EngagementStatus::Offered => {
                released == 0
                    && engagement.milestones.iter().all(|m| match m.kind {
                        MilestoneKind::Placement => m.status == MilestoneStatus::Pending,
                        MilestoneKind::Retention => m.status == MilestoneStatus::Locked,
                    })
            }
            _ => true,
        };

        let terminal_escrow_settled = match engagement.status {
            EngagementStatus::Cancelled | EngagementStatus::Expired => expected_escrow == 0,
            // Each milestone's share is rounded down, so up to one unit per
            // milestone can be left over once every share is paid.
            EngagementStatus::Completed => {
                expected_escrow >= 0 && expected_escrow < engagement.milestones.len() as i128
            }
            _ => true,
        };

        let ok = balance_matches
            && amounts_in_range
            && percents_sum_to_100
            && milestone_states_consistent
            && terminal_escrow_settled;

        InvariantReport {
            engagement_id,
            status: engagement.status,
            total_amount: total,
            released_amount: released,
            refunded_amount: refunded,
            expected_escrow,
            escrow_balance,
            balance_matches,
            amounts_in_range,
            percents_sum_to_100,
            milestone_states_consistent,
            terminal_escrow_settled,
            ok,
        }
    }

    /// Check every balance the contract tracks in `token` against
    /// per-engagement state and against the token balance the contract
    /// actually holds. Read-only and callable while paused.
    ///
    /// Walks the token's engagement index (issue #577), so its cost grows
    /// with the number of engagements ever created in `token`; call it by
    /// simulation rather than in a transaction.
    ///
    /// `ok` requires the summed and running totals to agree and the contract
    /// to be solvent. A positive `surplus` alone does not fail the check:
    /// stray tokens sent to the contract are expected and recoverable with
    /// [`Self::recover_stray_tokens`].
    pub fn check_token_invariants(env: Env, token: Address) -> TokenInvariantReport {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey3::TokenEngagements(token.clone()))
            .unwrap_or_else(|| Vec::new(&env));

        let mut summed_escrow: i128 = 0;
        let mut summed_bonds: i128 = 0;
        let mut summed_streamed: i128 = 0;
        for id in ids.iter() {
            summed_escrow += env
                .storage()
                .persistent()
                .get::<DataKey3, i128>(&DataKey3::EscrowBalance(id.clone()))
                .unwrap_or(0);
            for key in [
                DataKey2::RecruiterBond(id.clone()),
                DataKey2::CoRecruiterBond(id.clone()),
            ] {
                if let Some(bond) = env
                    .storage()
                    .persistent()
                    .get::<DataKey2, RecruiterBond>(&key)
                {
                    if !bond.settled {
                        summed_bonds += bond.amount;
                    }
                }
            }
            let engagement = Self::get_engagement_internal(&env, &id);
            for i in 0..engagement.milestones.len() {
                if let Some(stream) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, StreamedPayout>(&DataKey::StreamedPayout(id.clone(), i))
                {
                    summed_streamed += stream.total - stream.claimed;
                }
            }
        }

        let tracked_escrow = Self::token_aggregate(&env, DataKey3::TokenLocked(token.clone()));
        let tracked_bonds = Self::token_aggregate(&env, DataKey3::TokenBonded(token.clone()));
        let tracked_streamed = Self::token_aggregate(&env, DataKey3::TokenStreamed(token.clone()));
        let pooled = Self::token_aggregate(&env, DataKey3::TokenPooled(token.clone()));
        let claimable = Self::token_aggregate(&env, DataKey3::TokenClaimable(token.clone()));
        let rebates = Self::token_aggregate(&env, DataKey3::TokenRebate(token.clone()));

        let tracked_total = Self::tracked_token_total(&env, &token);
        let contract_balance =
            token::Client::new(&env, &token).balance(&env.current_contract_address());
        let surplus = contract_balance - tracked_total;

        let totals_consistent = summed_escrow == tracked_escrow
            && summed_bonds == tracked_bonds
            && summed_streamed == tracked_streamed;
        let solvent = surplus >= 0;

        TokenInvariantReport {
            token,
            engagement_count: ids.len(),
            summed_escrow,
            tracked_escrow,
            summed_bonds,
            tracked_bonds,
            summed_streamed,
            tracked_streamed,
            pooled,
            claimable,
            rebates,
            tracked_total,
            contract_balance,
            surplus,
            totals_consistent,
            solvent,
            ok: totals_consistent && solvent,
        }
    }

    // ----------------------------------------------------------
    // ISSUE #672 — STRAY TOKEN RECOVERY
    // ----------------------------------------------------------

    /// Amount of `token` the contract holds beyond every tracked balance,
    /// i.e. what [`Self::recover_stray_tokens`] would transfer now. Never
    /// negative. Read-only.
    pub fn get_stray_token_balance(env: Env, token: Address) -> i128 {
        Self::stray_token_amount(&env, &token).max(0)
    }

    /// Admin transfers to `to` the tokens held by the contract beyond every
    /// tracked balance (see the module docs), and returns the amount sent.
    /// Escrow, pooled balances, pull-payout credits, bonds, rebates and
    /// streamed payouts are all excluded, so tracked funds can never be
    /// withdrawn this way.
    ///
    /// When a config cosigner is set and `FN_RECOVER_STRAY_TOKENS` (7) is in
    /// `get_sensitive_functions`, the call only opens a pending config change
    /// and returns `0`; the transfer happens when the cosigner accepts it,
    /// with the stray amount computed at acceptance.
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused.
    /// - `"NoAdmin"` / `"unauthorized"` — `admin` is not the admin.
    /// - `"InvalidRecipient"` — `to` is the contract itself.
    /// - `"NoStrayTokens"` — nothing above the tracked balances to recover.
    ///
    /// # Events
    /// `("stray_tokens_recovered", token)` with `(to, amount)`.
    pub fn recover_stray_tokens(env: Env, admin: Address, token: Address, to: Address) -> i128 {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);
        if to == env.current_contract_address() {
            panic!("InvalidRecipient");
        }
        if Self::defer_if_sensitive_with(
            &env,
            FN_RECOVER_STRAY_TOKENS,
            0,
            0,
            false,
            Some(token.clone()),
            None,
            Some(to.clone()),
        ) {
            return 0;
        }
        Self::execute_stray_token_recovery(&env, &token, &to)
    }

    /// Transfer the current stray amount of `token` to `to`. Shared by the
    /// direct call and the cosigned config change.
    pub(crate) fn execute_stray_token_recovery(env: &Env, token: &Address, to: &Address) -> i128 {
        Self::assert_not_paused(env);
        let stray = Self::stray_token_amount(env, token);
        if stray <= 0 {
            panic!("NoStrayTokens");
        }
        token::Client::new(env, token).transfer(&env.current_contract_address(), to, &stray);
        env.events().publish(
            (Symbol::new(env, "stray_tokens_recovered"), token.clone()),
            (to.clone(), stray),
        );
        stray
    }

    /// Contract balance of `token` minus every tracked balance; negative if
    /// the contract is short.
    fn stray_token_amount(env: &Env, token: &Address) -> i128 {
        let balance = token::Client::new(env, token).balance(&env.current_contract_address());
        balance - Self::tracked_token_total(env, token)
    }

    /// Sum of the running totals of every tracked bucket for `token`.
    fn tracked_token_total(env: &Env, token: &Address) -> i128 {
        [
            DataKey3::TokenLocked(token.clone()),
            DataKey3::TokenPooled(token.clone()),
            DataKey3::TokenClaimable(token.clone()),
            DataKey3::TokenBonded(token.clone()),
            DataKey3::TokenRebate(token.clone()),
            DataKey3::TokenStreamed(token.clone()),
        ]
        .into_iter()
        .map(|key| Self::token_aggregate(env, key))
        .sum()
    }

    fn token_aggregate(env: &Env, key: DataKey3) -> i128 {
        env.storage().persistent().get(&key).unwrap_or(0)
    }
}
