use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // RAISE DISPUTE
    // ----------------------------------------------------------

    /// Raise a dispute on a milestone whose proof has been submitted by the
    /// recruiter, moving it into `Disputed` status so that the contract's
    /// arbiters can vote on the outcome.
    ///
    /// # Caller
    /// `company`: must be the engagement's company address or its registered
    /// company co-signer, and must sign the transaction.
    ///
    /// # Behaviour
    /// - The engagement must be `Active`.
    /// - The target milestone must be in `ProofSubmitted` status.
    /// - The dispute must be raised within the dispute window
    ///   (default 51 840 ledgers ≈ 3 days, or the engagement's agreed
    ///   override — see [`Self::get_engagement_dispute_window`]) counted from
    ///   `proof_submitted_at`.
    /// - The milestone transitions to `Disputed` and the supplied `reason`
    ///   (max 128 bytes) is stored for arbiter review.
    /// - After this call the arbiter-vote flow can begin:
    ///   see [`Self::cast_arbiter_vote`].
    ///
    /// # Panics
    /// - `"EngagementPaused"`: the engagement has been paused by the admin.
    /// - Authentication fails for `company` when `company.require_auth()` is
    ///   evaluated.
    /// - `"ReasonTooLong"`: `reason` is longer than 128 bytes.
    /// - `"engagement not found"`: no engagement exists for `engagement_id`.
    /// - `"engagement is not active"`: the engagement is not in `Active` status.
    /// - `"unauthorized"`: the authenticated caller is neither the engagement's
    ///   company nor its registered company co-signer.
    /// - `"invalid milestone index"`: `milestone_index` does not identify a
    ///   milestone in the engagement.
    /// - `"can only dispute a submitted proof"`: the milestone is not in
    ///   `ProofSubmitted` status.
    /// - `"DisputeWindowClosed"`: the current ledger is after the dispute
    ///   window calculated from `proof_submitted_at`.
    ///
    /// # Events
    /// Emits `("dispute_raised", engagement_id)` with
    /// `(milestone_index, reason)`.
    pub fn raise_dispute(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
        reason: String,
    ) {
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        if reason.len() > 128 {
            panic!("ReasonTooLong");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("can only dispute a submitted proof");
        }

        let current_ledger = env.ledger().sequence();
        let dispute_window = Self::engagement_dispute_window_internal(&env, &engagement_id);

        if current_ledger > milestone.proof_submitted_at + dispute_window {
            panic!("DisputeWindowClosed");
        }

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Disputed;
        engagement.milestones.set(milestone_index, milestone);
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage().persistent().set(
            &DataKey::DisputeReason(engagement_id.clone(), milestone_index),
            &reason.clone(),
        );

        // Issue #468: every panel member is now on the hook for a vote.
        Self::record_arbiter_assignments(&env, &engagement.arbiters);

        // Issue #246: record when the dispute was raised so `escalate_dispute`
        // can measure elapsed time against the dispute window.
        env.storage().persistent().set(
            &DataKey::DisputeRaisedAt(engagement_id.clone(), milestone_index),
            &current_ledger,
        );

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        Self::emit_milestone_status_changed(
            &env,
            &engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Disputed,
        );

        env.events().publish(
            (Symbol::new(&env, "dispute_raised"), engagement_id.clone()),
            (milestone_index, reason),
        );
    }

    // ----------------------------------------------------------
    // CAST ARBITER VOTE  (#10 multi-arbiter quorum)
    // ----------------------------------------------------------

    /// Each arbiter calls this to cast their vote on a Disputed milestone.
    /// The dispute resolves automatically once either:
    ///   - `approve_weight >= quorum`  → payment released, milestone → Resolved
    ///   - `reject_weight > total_weight - quorum`  → proof cleared, milestone → Pending
    ///
    /// Without `arbiter_weights` every arbiter weighs 1, so these reduce to
    /// `approve_votes >= quorum` and `reject_votes > arbiters.len() - quorum`
    /// (issue #460).
    ///
    /// `arbiter` may be the arbiter themself or the vote delegate they set via
    /// [`Self::set_arbiter_vote_delegate`] (issue #463); either way the vote is
    /// recorded against the arbiter's slot.
    ///
    /// Duplicate votes for the same arbiter slot are rejected.
    ///
    /// # Panics
    /// - `"MixedVoteModes"` — split votes have already been cast on this dispute
    ///   (issue #462).
    pub fn cast_arbiter_vote(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        milestone_index: u32,
        approve: bool,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        arbiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let caller = arbiter;
        let (arbiter, slot_index) =
            Self::resolve_voting_arbiter(&env, &engagement, &engagement_id, &caller);

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        // Issue #462: a dispute is decided by binary or split votes, never both.
        if env
            .storage()
            .persistent()
            .has(&DataKey::ArbiterSplitVotes(engagement_id.clone(), milestone_index))
        {
            panic!("MixedVoteModes");
        }

        let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), milestone_index);
        let mut record: ArbiterVoteRecord = env
            .storage()
            .persistent()
            .get(&vote_key)
            .unwrap_or_else(|| Self::empty_vote_record(&env));

        // Reject duplicate votes.
        for i in 0..record.voted.len() {
            if record.voted.get(i).unwrap() == arbiter {
                panic!("duplicate vote");
            }
        }

        let weight = Self::arbiter_weight(&engagement, slot_index);
        record.voted.push_back(arbiter.clone());
        Self::record_arbiter_vote(&env, &arbiter, &engagement_id, milestone_index);
        if approve {
            record.approve_votes += 1;
            record.approve_weight += weight;
        } else {
            record.reject_votes += 1;
            record.reject_weight += weight;
        }

        let total_weight = Self::total_arbiter_weight(&engagement);
        let quorum = engagement.quorum;

        env.events().publish(
            (Symbol::new(&env, "arbiter_voted"), engagement_id.clone()),
            (milestone_index, approve),
        );

        if record.approve_votes >= quorum {
            let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
            engagement.released_amount += payment;

            let platform_fee = Self::get_platform_fee_internal(&env);
            let effective_bps = if Self::is_fee_waived_internal(&env, &engagement_id) {
                0
            } else {
                let tiered_bps =
                    Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
                Self::apply_referral_discount(&env, tiered_bps, &engagement.referrer)
            };
            let platform_fee_amount = (payment * effective_bps as i128) / 10_000;
            let after_platform_fee = payment - platform_fee_amount;

            let arbiter_fee_bps: u32 = env
                .storage()
                .instance()
                .get(&DataKey::Config(ConfigKey::ArbiterFee))
                .unwrap_or(0u32);
            let arbiter_fee_amount = (after_platform_fee * arbiter_fee_bps as i128) / 10_000;
            let net_payment = after_platform_fee - arbiter_fee_amount;

            let token_client = token::Client::new(&env, &engagement.token);
            if platform_fee_amount > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &platform_fee.treasury,
                    &platform_fee_amount,
                );
                env.events().publish(
                    (
                        Symbol::new(&env, "platform_fee_collected"),
                        engagement_id.clone(),
                    ),
                    (milestone_index, platform_fee_amount, platform_fee.treasury),
                );
            }
            if arbiter_fee_amount > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &arbiter,
                    &arbiter_fee_amount,
                );
            }
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client, false);

            let old_status = milestone.status.clone();
            milestone.status = MilestoneStatus::Resolved;
            engagement.milestones.set(milestone_index, milestone);
            Self::bond_clear_rejection(&env, &engagement_id, milestone_index);

            let all_done = (0..engagement.milestones.len()).all(|i| {
                let s = engagement.milestones.get(i).unwrap().status;
                s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
            });
            let old_engagement_status = engagement.status.clone();
            if all_done {
                engagement.status = EngagementStatus::Completed;
                Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
                Self::decrement_company_active_count(&env, &engagement.company);
                Self::settle_recruiter_bond(&env, &engagement);
            }

            env.storage().persistent().remove(&vote_key);
            env.storage().persistent().remove(&DataKey::DisputeReason(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&DataKey::DisputeRaisedAt(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage()
                .persistent()
                .remove(&DataKey::EscalatedDispute(
                    engagement_id.clone(),
                    milestone_index,
                ));

            env.events().publish(
                (
                    Symbol::new(&env, "arbiter_vote_delegated"),
                    engagement_id.clone(),
                ),
                (milestone_index, arbiter.clone(), caller.clone()),
            );
        }

        if record.approve_weight >= quorum {
            let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
            Self::release_dispute_payout(
                &env,
                &mut engagement,
                &engagement_id,
                milestone_index,
                payment,
                &arbiter,
            );
            Self::finish_dispute_resolved(&env, &mut engagement, &engagement_id, milestone_index);
        } else if record.reject_weight > total_weight - quorum {
            let mut milestone = milestone;
            let old_status = milestone.status.clone();
            milestone.status = MilestoneStatus::Pending;
            milestone.proof_hash = String::from_str(&env, "");
            milestone.proof_submitted_at = 0;
            // Issue #465: a rejected proof reopens the milestone, so the
            // recruiter's no-show clock restarts from here.
            if milestone.kind == MilestoneKind::Placement {
                Self::mark_milestone_pending_since(&env, &engagement_id, milestone_index);
            }
            engagement.milestones.set(milestone_index, milestone);
            Self::bond_record_rejection(&env, &engagement_id, milestone_index);

            env.storage().persistent().remove(&vote_key);
            // A rejected proof starts a new submission round, so do not make
            // the recruiter wait for the cooldown before replacing it.
            env.storage().persistent().remove(&DataKey::LastProofAt(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&DataKey::DisputeReason(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&DataKey::DisputeRaisedAt(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage()
                .persistent()
                .remove(&DataKey::EscalatedDispute(
                    engagement_id.clone(),
                    milestone_index,
                ));

            env.events().publish(
                (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
                (milestone_index, false),
            );
            Self::emit_milestone_status_changed(
                &env,
                &engagement_id,
                milestone_index,
                old_status,
                MilestoneStatus::Pending,
            );
        } else {
            env.storage().persistent().set(&vote_key, &record);
            env.storage()
                .persistent()
                .extend_ttl(&vote_key, 100_000, 6_300_000);
        }

        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
    }

    /// Pay `payment` of escrow out on a dispute resolved by arbiter vote:
    /// platform fee to treasury, arbiter fee to `arbiter`, remainder to the
    /// recruiter(s). Adds `payment` to `released_amount`.
    pub(crate) fn release_dispute_payout(
        env: &Env,
        engagement: &mut Engagement,
        engagement_id: &String,
        milestone_index: u32,
        payment: i128,
        arbiter: &Address,
    ) {
        engagement.released_amount += payment;

        let platform_fee = Self::get_platform_fee_internal(env);
        let effective_bps = if Self::is_fee_waived_internal(env, engagement_id) {
            0
        } else {
            Self::apply_referral_discount(env, platform_fee.bps, &engagement.referrer)
        };
        Self::resolve_platform_fee_bps(env, platform_fee.bps, engagement.total_amount);
        let platform_fee_amount = (payment * effective_bps as i128) / 10_000;
        let after_platform_fee = payment - platform_fee_amount;

        let arbiter_fee_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ArbiterFee))
            .unwrap_or(0u32);
        let arbiter_fee_amount = (after_platform_fee * arbiter_fee_bps as i128) / 10_000;
        let net_payment = after_platform_fee - arbiter_fee_amount;

        let token_client = token::Client::new(env, &engagement.token);
        if platform_fee_amount > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &platform_fee.treasury,
                &platform_fee_amount,
            );
            env.events().publish(
                (
                    Symbol::new(env, "platform_fee_collected"),
                    engagement_id.clone(),
                ),
                (milestone_index, platform_fee_amount, platform_fee.treasury),
            );
        }
        if arbiter_fee_amount > 0 {
            token_client.transfer(&env.current_contract_address(), arbiter, &arbiter_fee_amount);
        }
        Self::distribute_recruiter_payout(env, engagement, net_payment, &token_client);
    }

    /// Move a disputed milestone to `Resolved` after an arbiter-vote payout,
    /// complete the engagement if every milestone is now done, clear the
    /// dispute's storage, and emit the resolution events. The caller persists
    /// `engagement`.
    pub(crate) fn finish_dispute_resolved(
        env: &Env,
        engagement: &mut Engagement,
        engagement_id: &String,
        milestone_index: u32,
    ) {
        let mut milestone = engagement.milestones.get(milestone_index).unwrap();
        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Resolved;
        engagement.milestones.set(milestone_index, milestone);

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });
        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::decrement_company_active_count(env, &engagement.company);
            Self::refund_split_withheld(env, engagement_id, engagement);
        }

        env.storage().persistent().remove(&DataKey::ArbiterVotes(
            engagement_id.clone(),
            milestone_index,
        ));
        env.storage().persistent().remove(&DataKey::ArbiterSplitVotes(
            engagement_id.clone(),
            milestone_index,
        ));
        env.storage().persistent().remove(&DataKey::DisputeReason(
            engagement_id.clone(),
            milestone_index,
        ));
        env.storage().persistent().remove(&DataKey::DisputeRaisedAt(
            engagement_id.clone(),
            milestone_index,
        ));
        env.storage()
            .persistent()
            .remove(&DataKey::EscalatedDispute(
                engagement_id.clone(),
                milestone_index,
            ));

        env.events().publish(
            (Symbol::new(env, "dispute_resolved"), engagement_id.clone()),
            (milestone_index, true),
        );
        Self::emit_milestone_status_changed(
            env,
            engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Resolved,
        );
        Self::emit_engagement_status_changed(
            env,
            engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );
    }

    // ----------------------------------------------------------
    // SPLIT-VOTE DISPUTE RESOLUTION (issue #462)
    // ----------------------------------------------------------

    /// Admin enables or disables percentage-split dispute voting for an
    /// engagement. Disabled by default, so binary `cast_arbiter_vote` is the
    /// only way to decide a dispute unless this is turned on.
    pub fn set_split_voting_enabled(env: Env, admin: Address, engagement_id: String, enabled: bool) {
        Self::assert_admin(&env, &admin);
        Self::get_engagement_internal(&env, &engagement_id);
        let key = DataKey::SplitVotingEnabled(engagement_id.clone());
        if enabled {
            env.storage().persistent().set(&key, &true);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
        } else {
            env.storage().persistent().remove(&key);
        }
        env.events().publish(
            (Symbol::new(&env, "split_voting_set"), engagement_id),
            enabled,
        );
    }

    /// Whether split dispute voting is enabled for an engagement.
    pub fn is_split_voting_enabled(env: Env, engagement_id: String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::SplitVotingEnabled(engagement_id))
            .unwrap_or(false)
    }

    /// An arbiter (or their vote delegate, issue #463) votes for the
    /// percentage (0-100) of a disputed milestone's share that should be paid
    /// to the recruiter — an alternative to the binary `cast_arbiter_vote`.
    ///
    /// Once the cumulative weight of split voters reaches `quorum`, the
    /// dispute resolves at the **lower weighted median** of the submitted
    /// percentages: sort the votes ascending and take the first percentage at
    /// which the cumulative voter weight reaches at least half the total
    /// weight cast. Unweighted, that is the middle vote for an odd count and
    /// the lower of the two middle votes for an even count (e.g. votes of 30
    /// and 70 settle at 30).
    ///
    /// On resolution, `share * split / 100` (where `share = total_amount *
    /// payment_percent / 100`) is paid out exactly like an approved dispute —
    /// platform fee, arbiter fee to the arbiter whose vote reached quorum, net
    /// to the recruiter(s). The remainder is not released: it stays in escrow,
    /// is included in any cancel/expiry refund, and is refunded to the company
    /// when the engagement completes. The milestone moves to `Resolved`.
    ///
    /// # Panics
    /// - `"SplitVotingDisabled"` — split voting is not enabled for this engagement.
    /// - `"InvalidSplitPercent"` — `split_percent` is greater than 100.
    /// - `"engagement is not active"` / `"milestone is not in disputed status"`.
    /// - `"unauthorized"` — caller is neither an arbiter nor an arbiter's delegate.
    /// - `"MixedVoteModes"` — binary votes have already been cast on this dispute.
    /// - `"duplicate vote"` — this arbiter slot has already voted.
    ///
    /// # Events
    /// - `("arbiter_split_voted", engagement_id)` with `(milestone_index, split_percent)`.
    /// - On resolution, `("dispute_split_resolved", engagement_id)` with
    ///   `(milestone_index, split_percent, released, withheld)`, followed by the
    ///   usual `dispute_resolved` / status-change events.
    pub fn cast_arbiter_split_vote(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        milestone_index: u32,
        split_percent: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        arbiter.require_auth();

        if !Self::is_split_voting_enabled(env.clone(), engagement_id.clone()) {
            panic!("SplitVotingDisabled");
        }
        if split_percent > 100 {
            panic!("InvalidSplitPercent");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let caller = arbiter;
        let (arbiter, slot_index) =
            Self::resolve_voting_arbiter(&env, &engagement, &engagement_id, &caller);

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::ArbiterVotes(engagement_id.clone(), milestone_index))
        {
            panic!("MixedVoteModes");
        }

        let split_key = DataKey::ArbiterSplitVotes(engagement_id.clone(), milestone_index);
        let mut record: ArbiterSplitVoteRecord =
            env.storage()
                .persistent()
                .get(&split_key)
                .unwrap_or(ArbiterSplitVoteRecord {
                    voters: Vec::new(&env),
                    splits: Vec::new(&env),
                    cast_weight: 0,
                });

        for i in 0..record.voters.len() {
            if record.voters.get(i).unwrap() == arbiter {
                panic!("duplicate vote");
            }
        }

        record.voters.push_back(arbiter.clone());
        record.splits.push_back(split_percent);
        record.cast_weight += Self::arbiter_weight(&engagement, slot_index);

        env.events().publish(
            (
                Symbol::new(&env, "arbiter_split_voted"),
                engagement_id.clone(),
            ),
            (milestone_index, split_percent),
        );
        if caller != arbiter {
            env.events().publish(
                (
                    Symbol::new(&env, "arbiter_vote_delegated"),
                    engagement_id.clone(),
                ),
                (milestone_index, arbiter.clone(), caller.clone()),
            );
        }

        if record.cast_weight >= engagement.quorum {
            let settled = Self::weighted_median_split(&engagement, &record);
            let share = (engagement.total_amount * milestone.payment_percent as i128) / 100;
            let released = (share * settled as i128) / 100;
            let withheld = share - released;

            Self::release_dispute_payout(
                &env,
                &mut engagement,
                &engagement_id,
                milestone_index,
                released,
                &arbiter,
            );
            if withheld > 0 {
                let withheld_key = DataKey::SplitWithheld(engagement_id.clone());
                let prior: i128 = env.storage().persistent().get(&withheld_key).unwrap_or(0);
                env.storage()
                    .persistent()
                    .set(&withheld_key, &(prior + withheld));
                env.storage()
                    .persistent()
                    .extend_ttl(&withheld_key, 100_000, 6_300_000);
            }
            env.events().publish(
                (
                    Symbol::new(&env, "dispute_split_resolved"),
                    engagement_id.clone(),
                ),
                (milestone_index, settled, released, withheld),
            );
            Self::finish_dispute_resolved(&env, &mut engagement, &engagement_id, milestone_index);
        } else {
            env.storage().persistent().set(&split_key, &record);
            env.storage()
                .persistent()
                .extend_ttl(&split_key, 100_000, 6_300_000);
        }

        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
    }

    /// Lower weighted median of the submitted split percentages — see
    /// `cast_arbiter_split_vote` for the exact definition.
    pub(crate) fn weighted_median_split(
        engagement: &Engagement,
        record: &ArbiterSplitVoteRecord,
    ) -> u32 {
        let n = record.voters.len();
        let weight_of = |i: u32| -> u64 {
            let voter = record.voters.get(i).unwrap();
            let slot = (0..engagement.arbiters.len())
                .find(|&j| engagement.arbiters.get(j).unwrap() == voter)
                .unwrap_or_else(|| panic!("{}", ERR_UNAUTHORIZED));
            Self::arbiter_weight(engagement, slot) as u64
        };
        let total: u64 = (0..n).map(weight_of).sum();

        // The answer is the smallest submitted percentage whose at-or-below
        // cumulative weight reaches half the total. n is the arbiter count,
        // so the quadratic scan is cheap and avoids a sort buffer.
        let mut best: Option<u32> = None;
        for i in 0..n {
            let candidate = record.splits.get(i).unwrap();
            if best.is_some_and(|b| candidate >= b) {
                continue;
            }
            let at_or_below: u64 = (0..n)
                .filter(|&j| record.splits.get(j).unwrap() <= candidate)
                .map(weight_of)
                .sum();
            if at_or_below * 2 >= total {
                best = Some(candidate);
            }
        }
        best.unwrap()
    }

    /// Individual split percentages submitted so far on a disputed milestone,
    /// in submission order, before aggregation (issue #462). Empty once the
    /// dispute resolves or if no split votes have been cast.
    pub fn get_dispute_split_votes(env: Env, engagement_id: String, milestone_index: u32) -> Vec<u32> {
        env.storage()
            .persistent()
            .get::<DataKey, ArbiterSplitVoteRecord>(&DataKey::ArbiterSplitVotes(
                engagement_id,
                milestone_index,
            ))
            .map(|record| record.splits)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ----------------------------------------------------------
    // ARBITER VOTE DELEGATION (issue #463)
    // ----------------------------------------------------------

    /// An arbiter authorizes `delegate` to cast votes (binary or split) on
    /// their behalf for this engagement only, or revokes delegation with
    /// `None`. Delegated votes count against the arbiter's own slot, so they
    /// share its weight and duplicate-vote protection. Delegation is cleared
    /// when the slot changes hands via `claim_arbiter`.
    ///
    /// # Panics
    /// - `"engagement is in a terminal state"` — engagement is completed, cancelled, or expired.
    /// - `"unauthorized"` — `arbiter` is not an arbiter on this engagement.
    /// - `"InvalidDelegate"` — `delegate` is the arbiter, another arbiter, the
    ///   company, or the recruiter on this engagement.
    /// - `"DelegateAlreadyAssigned"` — `delegate` already acts for another
    ///   arbiter on this engagement (a delegate must map to exactly one slot).
    ///
    /// # Events
    /// Emits `("arbiter_delegate_set", engagement_id)` with `(arbiter, delegate)`.
    pub fn set_arbiter_vote_delegate(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        delegate: Option<Address>,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        arbiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if Self::is_terminal_status(&engagement.status) {
            panic!("engagement is in a terminal state");
        }

        let is_arbiter =
            (0..engagement.arbiters.len()).any(|i| engagement.arbiters.get(i).unwrap() == arbiter);
        if !is_arbiter {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey::ArbiterVoteDelegate(engagement_id.clone(), arbiter.clone());
        match &delegate {
            Some(d) => {
                let is_party = *d == engagement.company
                    || *d == engagement.recruiter
                    || (0..engagement.arbiters.len())
                        .any(|i| engagement.arbiters.get(i).unwrap() == *d);
                if is_party {
                    panic!("InvalidDelegate");
                }
                for i in 0..engagement.arbiters.len() {
                    let other = engagement.arbiters.get(i).unwrap();
                    if other == arbiter {
                        continue;
                    }
                    let existing: Option<Address> = env.storage().persistent().get(
                        &DataKey::ArbiterVoteDelegate(engagement_id.clone(), other),
                    );
                    if existing.as_ref() == Some(d) {
                        panic!("DelegateAlreadyAssigned");
                    }
                }
                env.storage().persistent().set(&key, d);
                env.storage()
                    .persistent()
                    .extend_ttl(&key, 100_000, 6_300_000);
            }
            None => env.storage().persistent().remove(&key),
        }

        env.events().publish(
            (
                Symbol::new(&env, "arbiter_delegate_set"),
                engagement_id,
            ),
            (arbiter, delegate),
        );
    }

    /// The vote delegate currently set by `arbiter` on this engagement, if any.
    pub fn get_arbiter_vote_delegate(
        env: Env,
        engagement_id: String,
        arbiter: Address,
    ) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::ArbiterVoteDelegate(engagement_id, arbiter))
    }

    // ----------------------------------------------------------
    // DISPUTE AUTO-ESCALATION TO SUPER ARBITER (issue #246)
    // ----------------------------------------------------------

    /// Admin sets (or replaces) the super-arbiter address used to break ties
    /// on escalated disputes.
    pub fn set_super_arbiter(env: Env, admin: Address, super_arbiter: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::SuperArbiter, &super_arbiter);
        env.events()
            .publish((Symbol::new(&env, "super_arbiter_set"),), super_arbiter);
    }

    /// Return the currently configured super-arbiter address, if any.
    pub fn get_super_arbiter(env: Env) -> Option<Address> {
        env.storage().instance().get(&DataKey::SuperArbiter)
    }

    /// Return whether a disputed milestone has been auto-escalated to the
    /// super arbiter.
    pub fn is_dispute_escalated(env: Env, engagement_id: String, milestone_index: u32) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::EscalatedDispute(engagement_id, milestone_index))
            .unwrap_or(false)
    }

    /// Permissionlessly escalate a disputed milestone to the configured
    /// super arbiter once arbiter votes remain split (neither quorum nor the
    /// rejection threshold reached) past the dispute window, measured from
    /// when the dispute was raised. Mirrors the permissionless shape of
    /// `unlock_milestone` — anyone can trigger it once the condition holds.
    ///
    /// # Panics
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"milestone is not in disputed status"` — the milestone isn't `Disputed`.
    /// - `"DisputeWindowNotElapsed"` — the dispute window has not yet elapsed
    ///   since the dispute was raised.
    /// - `"dispute already resolvable without escalation"` — votes already
    ///   satisfy the quorum or rejection threshold; call `cast_arbiter_vote`
    ///   (any further vote) to resolve normally instead.
    /// - `"no super arbiter configured"` — the admin has not set a super arbiter.
    pub fn escalate_dispute(env: Env, engagement_id: String, milestone_index: u32) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        let raised_at: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::DisputeRaisedAt(
                engagement_id.clone(),
                milestone_index,
            ))
            .unwrap_or_else(|| panic!("no dispute in progress"));

        let dispute_window = Self::engagement_dispute_window_internal(&env, &engagement_id);

        let current_ledger = env.ledger().sequence();
        if current_ledger <= raised_at + dispute_window {
            panic!("DisputeWindowNotElapsed");
        }

        let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), milestone_index);
        let record: ArbiterVoteRecord = env
            .storage()
            .persistent()
            .get(&vote_key)
            .unwrap_or_else(|| Self::empty_vote_record(&env));

        let total_weight = Self::total_arbiter_weight(&engagement);
        let quorum = engagement.quorum;
        if record.approve_weight >= quorum || record.reject_weight > total_weight - quorum {
            panic!("dispute already resolvable without escalation");
        }

        let super_arbiter: Address = env
            .storage()
            .instance()
            .get(&DataKey::SuperArbiter)
            .unwrap_or_else(|| panic!("no super arbiter configured"));

        env.storage().persistent().set(
            &DataKey::EscalatedDispute(engagement_id.clone(), milestone_index),
            &true,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::EscalatedDispute(engagement_id.clone(), milestone_index),
            100_000,
            6_300_000,
        );

        // Issue #318: record the escalation ledger so `resolve_escalation_timeout`
        // can measure the super-arbiter response deadline from this point.
        env.storage().persistent().set(
            &DataKey::EscalatedAt(engagement_id.clone(), milestone_index),
            &current_ledger,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::EscalatedAt(engagement_id.clone(), milestone_index),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "dispute_escalated"),
                engagement_id.clone(),
            ),
            (milestone_index, super_arbiter),
        );
    }

    /// The configured super arbiter casts a tie-breaking resolution on an
    /// escalated dispute. `approve` mirrors `cast_arbiter_vote`'s semantics:
    /// `true` releases payment to the recruiter (milestone → `Resolved`);
    /// `false` clears the proof and returns the milestone to `Pending`.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the configured super arbiter.
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"milestone is not in disputed status"` — the milestone isn't `Disputed`.
    /// - `"dispute has not been escalated"` — `escalate_dispute` has not been
    ///   called for this milestone yet.
    pub fn super_arbiter_resolve(
        env: Env,
        super_arbiter: Address,
        engagement_id: String,
        milestone_index: u32,
        approve: bool,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        super_arbiter.require_auth();

        let configured: Address = env
            .storage()
            .instance()
            .get(&DataKey::SuperArbiter)
            .unwrap_or_else(|| panic!("no super arbiter configured"));
        if super_arbiter != configured {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        let escalated_key = DataKey::EscalatedDispute(engagement_id.clone(), milestone_index);
        let escalated: bool = env
            .storage()
            .persistent()
            .get(&escalated_key)
            .unwrap_or(false);
        if !escalated {
            panic!("dispute has not been escalated");
        }

        // Issue #317: this call always concludes an escalated dispute (either
        // branch below), so it always counts toward the resolution total.
        Self::increment_super_arbiter_resolution_count(&env);

        let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), milestone_index);
        let escalated_at_key = DataKey::EscalatedAt(engagement_id.clone(), milestone_index);
        env.storage()
            .persistent()
            .remove(&DataKey::ArbiterSplitVotes(engagement_id.clone(), milestone_index));

        if approve {
            let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
            engagement.released_amount += payment;

            let platform_fee = Self::get_platform_fee_internal(&env);
            let effective_bps = if Self::is_fee_waived_internal(&env, &engagement_id) {
                0
            } else {
                let tiered_bps =
                    Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
                Self::apply_referral_discount(&env, tiered_bps, &engagement.referrer)
            };
            let platform_fee_amount = (payment * effective_bps as i128) / 10_000;
            let after_platform_fee = payment - platform_fee_amount;

            let arbiter_fee_bps: u32 = env
                .storage()
                .instance()
                .get(&DataKey::Config(ConfigKey::ArbiterFee))
                .unwrap_or(0u32);
            let arbiter_fee_amount = (after_platform_fee * arbiter_fee_bps as i128) / 10_000;
            let net_payment = after_platform_fee - arbiter_fee_amount;

            let token_client = token::Client::new(&env, &engagement.token);
            if platform_fee_amount > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &platform_fee.treasury,
                    &platform_fee_amount,
                );
                env.events().publish(
                    (
                        Symbol::new(&env, "platform_fee_collected"),
                        engagement_id.clone(),
                    ),
                    (milestone_index, platform_fee_amount, platform_fee.treasury),
                );
            }
            if arbiter_fee_amount > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &super_arbiter,
                    &arbiter_fee_amount,
                );
            }
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client, false);

            let old_status = milestone.status.clone();
            milestone.status = MilestoneStatus::Resolved;
            engagement.milestones.set(milestone_index, milestone);
            Self::bond_clear_rejection(&env, &engagement_id, milestone_index);

            let all_done = (0..engagement.milestones.len()).all(|i| {
                let s = engagement.milestones.get(i).unwrap().status;
                s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
            });
            let old_engagement_status = engagement.status.clone();
            if all_done {
                engagement.status = EngagementStatus::Completed;
                Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
                Self::decrement_company_active_count(&env, &engagement.company);
                Self::refund_split_withheld(&env, &engagement_id, &mut engagement);
            }

            env.storage().persistent().remove(&vote_key);
            env.storage().persistent().remove(&DataKey::DisputeReason(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&DataKey::DisputeRaisedAt(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&escalated_key);
            env.storage().persistent().remove(&escalated_at_key);

            env.events().publish(
                (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
                (milestone_index, true),
            );
            env.events().publish(
                (
                    Symbol::new(&env, "dispute_escalation_resolved"),
                    engagement_id.clone(),
                ),
                (milestone_index, super_arbiter.clone(), true),
            );
            Self::emit_milestone_status_changed(
                &env,
                &engagement_id,
                milestone_index,
                old_status,
                MilestoneStatus::Resolved,
            );
            Self::emit_engagement_status_changed(
                &env,
                &engagement_id,
                old_engagement_status,
                engagement.status.clone(),
            );
        } else {
            let old_status = milestone.status.clone();
            milestone.status = MilestoneStatus::Pending;
            milestone.proof_hash = String::from_str(&env, "");
            milestone.proof_submitted_at = 0;
            // Issue #465: a rejected proof reopens the milestone, so the
            // recruiter's no-show clock restarts from here.
            if milestone.kind == MilestoneKind::Placement {
                Self::mark_milestone_pending_since(&env, &engagement_id, milestone_index);
            }
            engagement.milestones.set(milestone_index, milestone);
            Self::bond_record_rejection(&env, &engagement_id, milestone_index);

            env.storage().persistent().remove(&vote_key);
            env.storage().persistent().remove(&DataKey::LastProofAt(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&DataKey::DisputeReason(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&DataKey::DisputeRaisedAt(
                engagement_id.clone(),
                milestone_index,
            ));
            env.storage().persistent().remove(&escalated_key);
            env.storage().persistent().remove(&escalated_at_key);

            env.events().publish(
                (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
                (milestone_index, false),
            );
            env.events().publish(
                (
                    Symbol::new(&env, "dispute_escalation_resolved"),
                    engagement_id.clone(),
                ),
                (milestone_index, super_arbiter.clone(), false),
            );
            Self::emit_milestone_status_changed(
                &env,
                &engagement_id,
                milestone_index,
                old_status,
                MilestoneStatus::Pending,
            );
        }

        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
    }

    /// Admin sets how many ledgers the super-arbiter has to resolve an
    /// escalated dispute (via `super_arbiter_resolve`) before
    /// `resolve_escalation_timeout` can be called to auto-favor the recruiter
    /// (issue #318). Must be at least 1.
    pub fn set_super_arbiter_deadline(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        if ledgers == 0 {
            panic!("InvalidSuperArbiterResponseWindow");
        }
        env.storage().instance().set(
            &DataKey::Config(ConfigKey::SuperArbiterResponseWindow),
            &ledgers,
        );
        env.events()
            .publish((Symbol::new(&env, "super_arbiter_deadline_set"),), ledgers);
    }

    /// Return the currently configured super-arbiter response window in
    /// ledgers (default `DEFAULT_SUPER_ARBITER_RESPONSE_WINDOW_LEDGERS`).
    pub fn get_super_arbiter_deadline(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::SuperArbiterResponseWindow))
            .unwrap_or(DEFAULT_SUPER_ARBITER_RESPONSE_WINDOW_LEDGERS)
    }

    /// Permissionlessly conclude an escalated dispute in the recruiter's
    /// favor once the super arbiter has failed to call `super_arbiter_resolve`
    /// within the configured response window (issue #318). Mirrors the
    /// `approve = true` outcome of `super_arbiter_resolve`, except no arbiter
    /// fee is deducted — the super arbiter did not act, so it earns no fee.
    ///
    /// # Panics
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"milestone is not in disputed status"` — the milestone isn't `Disputed`.
    /// - `"dispute has not been escalated"` — `escalate_dispute` has not been
    ///   called for this milestone yet.
    /// - `"SuperArbiterResponseWindowNotElapsed"` — the super arbiter still
    ///   has time left to resolve the dispute.
    pub fn resolve_escalation_timeout(env: Env, engagement_id: String, milestone_index: u32) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        let escalated_key = DataKey::EscalatedDispute(engagement_id.clone(), milestone_index);
        let escalated: bool = env
            .storage()
            .persistent()
            .get(&escalated_key)
            .unwrap_or(false);
        if !escalated {
            panic!("dispute has not been escalated");
        }

        let escalated_at_key = DataKey::EscalatedAt(engagement_id.clone(), milestone_index);
        let escalated_at: u32 = env
            .storage()
            .persistent()
            .get(&escalated_at_key)
            .unwrap_or_else(|| panic!("no escalation timestamp recorded"));

        let response_window = Self::get_super_arbiter_deadline(env.clone());
        let current_ledger = env.ledger().sequence();
        if current_ledger <= escalated_at + response_window {
            panic!("SuperArbiterResponseWindowNotElapsed");
        }

        Self::increment_super_arbiter_resolution_count(&env);

        let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), milestone_index);
        env.storage()
            .persistent()
            .remove(&DataKey::ArbiterSplitVotes(engagement_id.clone(), milestone_index));

        let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        engagement.released_amount += payment;

        let platform_fee = Self::get_platform_fee_internal(&env);
        let effective_bps = if Self::is_fee_waived_internal(&env, &engagement_id) {
            0
        } else {
            let tiered_bps =
                Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
            Self::apply_referral_discount(&env, tiered_bps, &engagement.referrer)
        };
        let platform_fee_amount = (payment * effective_bps as i128) / 10_000;
        let net_payment = payment - platform_fee_amount;

        let token_client = token::Client::new(&env, &engagement.token);
        if platform_fee_amount > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &platform_fee.treasury,
                &platform_fee_amount,
            );
            env.events().publish(
                (
                    Symbol::new(&env, "platform_fee_collected"),
                    engagement_id.clone(),
                ),
                (milestone_index, platform_fee_amount, platform_fee.treasury),
            );
        }
        Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client, false);

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Resolved;
        engagement.milestones.set(milestone_index, milestone);
        Self::bond_clear_rejection(&env, &engagement_id, milestone_index);

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });
        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
            Self::decrement_company_active_count(&env, &engagement.company);
            Self::refund_split_withheld(&env, &engagement_id, &mut engagement);
        }

        env.storage().persistent().remove(&vote_key);
        env.storage().persistent().remove(&DataKey::DisputeReason(
            engagement_id.clone(),
            milestone_index,
        ));
        env.storage().persistent().remove(&DataKey::DisputeRaisedAt(
            engagement_id.clone(),
            milestone_index,
        ));
        env.storage().persistent().remove(&escalated_key);
        env.storage().persistent().remove(&escalated_at_key);

        env.events().publish(
            (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
            (milestone_index, true),
        );
        env.events().publish(
            (
                Symbol::new(&env, "super_arbiter_timeout_resolved"),
                engagement_id.clone(),
            ),
            milestone_index,
        );
        Self::emit_milestone_status_changed(
            &env,
            &engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Resolved,
        );
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );

        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
    }

    /// Increment the counter of disputes concluded via the super-arbiter
    /// escalation path (issue #317).
    pub(crate) fn increment_super_arbiter_resolution_count(env: &Env) {
        let count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::SuperArbiterResolutionCount)
            .unwrap_or(0u64);
        env.storage()
            .instance()
            .set(&DataKey::SuperArbiterResolutionCount, &(count + 1));
    }

    /// Return the total number of disputes resolved via the super-arbiter
    /// escalation path — counting both an explicit `super_arbiter_resolve`
    /// call and a `resolve_escalation_timeout` auto-resolution (issue #317).
    pub fn get_super_arbiter_resolutions(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::SuperArbiterResolutionCount)
            .unwrap_or(0u64)
    }

    // ----------------------------------------------------------
    // ISSUE #50 — DISPUTE REASON QUERY
    // ----------------------------------------------------------

    /// Return the stored dispute reason for a milestone, if any.
    pub fn get_dispute_reason(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<String> {
        env.storage()
            .persistent()
            .get(&DataKey::DisputeReason(engagement_id, milestone_index))
    }

    // ----------------------------------------------------------
    // ISSUE #363 — DISPUTE EVIDENCE
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // DISPUTE WINDOW
    // ----------------------------------------------------------

    /// Admin sets the dispute window in ledgers.
    /// Default is 51_840 (~3 days at 5 s/ledger).
    pub fn set_dispute_window(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::DisputeWindow), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "dispute_window_set"),), ledgers);
    }

    /// Return the current dispute window in ledgers.
    pub fn get_dispute_window(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::DisputeWindow))
            .unwrap_or(DEFAULT_DISPUTE_WINDOW_LEDGERS)
    }

    // ----------------------------------------------------------
    // ISSUE #469 — PER-ENGAGEMENT DISPUTE WINDOW OVERRIDE
    // ----------------------------------------------------------

    /// Either party (or its co-signer) proposes a dispute window, in ledgers,
    /// for this engagement only. The counterparty must accept it within
    /// `DISPUTE_WINDOW_PROPOSAL_TTL_LEDGERS` ledgers or it expires and is
    /// treated as cleared. A new proposal can be made once the previous one
    /// has been accepted, rejected, or has expired.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"` — contract or engagement is paused.
    /// - `"InvalidDisputeWindow"` — `ledgers` is 0.
    /// - `"engagement is not active"` — engagement is not `Active`.
    /// - `"unauthorized"` — caller is neither party nor a party's co-signer.
    /// - `"DisputeWindowProposalPending"` — an unexpired proposal already exists.
    ///
    /// # Events
    /// Emits `("dispute_window_proposed", engagement_id)` with
    /// `(proposer, ledgers, expires_at_ledger)`.
    pub fn propose_dispute_window_override(
        env: Env,
        proposer: Address,
        engagement_id: String,
        ledgers: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        proposer.require_auth();

        if ledgers == 0 {
            panic!("InvalidDisputeWindow");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let proposed_by_company =
            Self::is_authorized_company(&env, &proposer, &engagement.company);
        if !proposed_by_company
            && !Self::is_authorized_recruiter(&env, &proposer, &engagement.recruiter)
        {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if Self::pending_dispute_window_proposal(&env, &engagement_id).is_some() {
            panic!("DisputeWindowProposalPending");
        }

        let expires_at_ledger = env
            .ledger()
            .sequence()
            .saturating_add(DISPUTE_WINDOW_PROPOSAL_TTL_LEDGERS);
        let key = DataKey::DisputeWindowProposal(engagement_id.clone());
        env.storage().persistent().set(
            &key,
            &DisputeWindowProposal {
                proposer: proposer.clone(),
                proposed_by_company,
                ledgers,
                expires_at_ledger,
            },
        );
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "dispute_window_proposed"), engagement_id),
            (proposer, ledgers, expires_at_ledger),
        );
    }

    /// The counterparty of the proposer (or its co-signer) accepts a pending
    /// dispute window proposal. From then on, dispute-eligibility checks for
    /// this engagement use the agreed window instead of the global default.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"` — contract or engagement is paused.
    /// - `"engagement is not active"` — engagement is not `Active`.
    /// - `"NoPendingDisputeWindowProposal"` — no proposal, or it has expired.
    /// - `"unauthorized"` — caller is not the proposer's counterparty.
    ///
    /// # Events
    /// Emits `("dispute_window_accepted", engagement_id)` with `(acceptor, ledgers)`.
    pub fn accept_dispute_window_override(env: Env, acceptor: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        acceptor.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let proposal = Self::pending_dispute_window_proposal(&env, &engagement_id)
            .unwrap_or_else(|| panic!("NoPendingDisputeWindowProposal"));
        Self::assert_dispute_window_counterparty(&env, &acceptor, &engagement, &proposal);

        let override_key = DataKey::DisputeWindowOverride(engagement_id.clone());
        env.storage()
            .persistent()
            .set(&override_key, &proposal.ledgers);
        env.storage()
            .persistent()
            .extend_ttl(&override_key, 100_000, 6_300_000);
        env.storage()
            .persistent()
            .remove(&DataKey::DisputeWindowProposal(engagement_id.clone()));

        env.events().publish(
            (Symbol::new(&env, "dispute_window_accepted"), engagement_id),
            (acceptor, proposal.ledgers),
        );
    }

    /// The counterparty of the proposer (or its co-signer) rejects a pending
    /// dispute window proposal, clearing it. Any previously accepted override
    /// stays in effect.
    ///
    /// # Panics
    /// - `"NoPendingDisputeWindowProposal"` — no proposal, or it has expired.
    /// - `"unauthorized"` — caller is not the proposer's counterparty.
    ///
    /// # Events
    /// Emits `("dispute_window_rejected", engagement_id)` with `(acceptor, ledgers)`.
    pub fn reject_dispute_window_override(env: Env, acceptor: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        acceptor.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        let proposal = Self::pending_dispute_window_proposal(&env, &engagement_id)
            .unwrap_or_else(|| panic!("NoPendingDisputeWindowProposal"));
        Self::assert_dispute_window_counterparty(&env, &acceptor, &engagement, &proposal);

        env.storage()
            .persistent()
            .remove(&DataKey::DisputeWindowProposal(engagement_id.clone()));

        env.events().publish(
            (Symbol::new(&env, "dispute_window_rejected"), engagement_id),
            (acceptor, proposal.ledgers),
        );
    }

    /// Return the pending dispute window proposal, or `None` if there is none
    /// or it has expired.
    pub fn get_dispute_window_proposal(
        env: Env,
        engagement_id: String,
    ) -> Option<DisputeWindowProposal> {
        Self::pending_dispute_window_proposal(&env, &engagement_id)
    }

    /// Return the dispute window that applies to this engagement: the
    /// mutually agreed override if one was accepted, else `get_dispute_window()`.
    pub fn get_engagement_dispute_window(env: Env, engagement_id: String) -> u32 {
        Self::engagement_dispute_window_internal(&env, &engagement_id)
    }

    pub(crate) fn engagement_dispute_window_internal(env: &Env, engagement_id: &String) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::DisputeWindowOverride(engagement_id.clone()))
            .unwrap_or_else(|| Self::get_dispute_window(env.clone()))
    }

    fn pending_dispute_window_proposal(
        env: &Env,
        engagement_id: &String,
    ) -> Option<DisputeWindowProposal> {
        let proposal: DisputeWindowProposal = env
            .storage()
            .persistent()
            .get(&DataKey::DisputeWindowProposal(engagement_id.clone()))?;
        if env.ledger().sequence() > proposal.expires_at_ledger {
            return None;
        }
        Some(proposal)
    }

    fn assert_dispute_window_counterparty(
        env: &Env,
        caller: &Address,
        engagement: &Engagement,
        proposal: &DisputeWindowProposal,
    ) {
        let is_counterparty = if proposal.proposed_by_company {
            Self::is_authorized_recruiter(env, caller, &engagement.recruiter)
        } else {
            Self::is_authorized_company(env, caller, &engagement.company)
        };
        if !is_counterparty {
            panic!("{}", ERR_UNAUTHORIZED);
        }
    }

    /// Force-confirm a milestone after the company has taken no action within the
    /// configured confirm window.  Callable by anyone once the window has elapsed.
    ///
    /// Succeeds only when:
    ///   - `current_ledger > proof_submitted_at + confirm_window`
    ///   - milestone status is exactly `ProofSubmitted`
    ///   - every milestone in its `prerequisites` is `Confirmed` or `Resolved`
    ///     (issue #461)
    ///
    /// Releases payment to the recruiter (with platform fee) and emits
    /// `milestone_force_confirmed`. The recruiter's net share honours their
    /// payout token preference (issue #458).
    pub fn force_confirm_milestone(
        env: Env,
        caller: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        caller.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone is not in ProofSubmitted status");
        }

        let current_ledger = env.ledger().sequence();
        let window = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ConfirmWindow))
            .unwrap_or(DEFAULT_CONFIRM_WINDOW_LEDGERS);

        if current_ledger <= milestone.proof_submitted_at + window {
            panic!("ConfirmWindowNotElapsed");
        }

        // Issue #461: an inactive company cannot be used to bypass the
        // prerequisite graph.
        Self::assert_prerequisites_complete(&engagement, &milestone);

        // Release payment identically to confirm_milestone.
        let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        let platform_fee = Self::get_platform_fee_internal(&env);
        let effective_bps = Self::effective_platform_fee_bps(
            &env,
            &engagement_id,
            platform_fee.bps,
            engagement.total_amount,
        );
        let fee_amount = (payment * effective_bps as i128) / 10_000;
        let net_payment = payment - fee_amount;
        engagement.released_amount += payment;

        let token_client = token::Client::new(&env, &engagement.token);
        if fee_amount > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &platform_fee.treasury,
                &fee_amount,
            );
            env.events().publish(
                (
                    Symbol::new(&env, "platform_fee_collected"),
                    engagement_id.clone(),
                ),
                (milestone_index, fee_amount, platform_fee.treasury),
            );
        }
        Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client, true);

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Confirmed;
        engagement.milestones.set(milestone_index, milestone);
        Self::bond_clear_rejection(&env, &engagement_id, milestone_index);

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
            Self::decrement_company_active_count(&env, &engagement.company);
            Self::refund_split_withheld(&env, &engagement_id, &mut engagement);
        }
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );

        Self::emit_milestone_status_changed(
            &env,
            &engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Confirmed,
        );

        env.events().publish(
            (
                Symbol::new(&env, "milestone_force_confirmed"),
                engagement_id.clone(),
            ),
            (milestone_index, payment),
        );

        if all_done {
            env.events().publish(
                (
                    Symbol::new(&env, "engagement_completed"),
                    engagement_id.clone(),
                ),
                (
                    engagement_id.clone(),
                    engagement.released_amount,
                    env.ledger().sequence(),
                ),
            );
        }
    }

    // ----------------------------------------------------------
    // ISSUE #70 — ACTIVE DISPUTE COUNT
    // ----------------------------------------------------------

}
