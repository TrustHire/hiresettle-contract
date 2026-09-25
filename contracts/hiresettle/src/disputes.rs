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
    ///   (default 51 840 ledgers ≈ 3 days) counted from
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
        let dispute_window = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::DisputeWindow))
            .unwrap_or(DEFAULT_DISPUTE_WINDOW_LEDGERS);

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
    ///   - `approve_votes >= quorum`  → payment released, milestone → Resolved
    ///   - `reject_votes > arbiters.len() - quorum`  → proof cleared, milestone → Pending
    ///
    /// Duplicate votes from the same arbiter are rejected.
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

        let is_arbiter =
            (0..engagement.arbiters.len()).any(|i| engagement.arbiters.get(i).unwrap() == arbiter);
        if !is_arbiter {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), milestone_index);
        let mut record: ArbiterVoteRecord =
            env.storage()
                .persistent()
                .get(&vote_key)
                .unwrap_or(ArbiterVoteRecord {
                    approve_votes: 0,
                    reject_votes: 0,
                    voted: Vec::new(&env),
                });

        // Reject duplicate votes.
        for i in 0..record.voted.len() {
            if record.voted.get(i).unwrap() == arbiter {
                panic!("duplicate vote");
            }
        }

        record.voted.push_back(arbiter.clone());
        if approve {
            record.approve_votes += 1;
        } else {
            record.reject_votes += 1;
        }

        let total_arbiters = engagement.arbiters.len();
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
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client);

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
                (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
                (milestone_index, true),
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
        } else if record.reject_votes > total_arbiters - quorum {
            let old_status = milestone.status.clone();
            milestone.status = MilestoneStatus::Pending;
            milestone.proof_hash = String::from_str(&env, "");
            milestone.proof_submitted_at = 0;
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

        let dispute_window = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::DisputeWindow))
            .unwrap_or(DEFAULT_DISPUTE_WINDOW_LEDGERS);

        let current_ledger = env.ledger().sequence();
        if current_ledger <= raised_at + dispute_window {
            panic!("DisputeWindowNotElapsed");
        }

        let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), milestone_index);
        let record: ArbiterVoteRecord =
            env.storage()
                .persistent()
                .get(&vote_key)
                .unwrap_or(ArbiterVoteRecord {
                    approve_votes: 0,
                    reject_votes: 0,
                    voted: Vec::new(&env),
                });

        let total_arbiters = engagement.arbiters.len();
        let quorum = engagement.quorum;
        if record.approve_votes >= quorum || record.reject_votes > total_arbiters - quorum {
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
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client);

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
        Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client);

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

    /// Force-confirm a milestone after the company has taken no action within the
    /// configured confirm window.  Callable by anyone once the window has elapsed.
    ///
    /// Succeeds only when:
    ///   - `current_ledger > proof_submitted_at + confirm_window`
    ///   - milestone status is exactly `ProofSubmitted`
    ///
    /// Releases payment to the recruiter (with platform fee) and emits
    /// `milestone_force_confirmed`.
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
        Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client);

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
            Self::decrement_company_active_count(&env, &engagement.company);
            Self::settle_recruiter_bond(&env, &engagement);
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
