use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // UNLOCK RETENTION MILESTONE
    // ----------------------------------------------------------

    /// Unlock a locked retention milestone once its ledger window has elapsed.
    ///
    /// # Caller
    /// Anyone — this function is permissionless.
    ///
    /// # Behaviour
    /// - The engagement must be `Active`.
    /// - The target milestone must be `Locked` and of kind `Retention`.
    /// - The current ledger sequence must be at least `valid_after_ledger`; otherwise
    ///   the function panics and the milestone remains locked.
    /// - On success, the milestone transitions to `Pending`, the engagement's
    ///   `last_activity_ledger` is updated, and a `milestone_unlocked` event is
    ///   emitted with the milestone index, the original `valid_after_ledger`, and
    ///   the ledger where the unlock occurred.
    ///
    /// # Panics
    /// - `"engagement is not active"` — the engagement is not active.
    /// - `"milestone is not locked"` — the milestone is not currently locked.
    /// - `"only retention milestones can be unlocked this way"` — the milestone is
    ///   not a retention milestone.
    /// - `"retention window has not elapsed yet"` — the current ledger is still
    ///   before `valid_after_ledger`.
    pub fn unlock_milestone(env: Env, engagement_id: String, milestone_index: u32) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Locked {
            panic!("milestone is not locked");
        }

        if milestone.kind != MilestoneKind::Retention {
            panic!("only retention milestones can be unlocked this way");
        }

        let current_ledger = env.ledger().sequence();
        if current_ledger < milestone.valid_after_ledger {
            panic!("retention window has not elapsed yet");
        }

        // Capture for the event before mutating the milestone.
        let valid_after_ledger = milestone.valid_after_ledger;
        let unlocked_at_ledger = current_ledger;

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Pending;
        engagement.milestones.set(milestone_index, milestone);
        engagement.last_activity_ledger = unlocked_at_ledger;

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        // The due-soon flag only guards against duplicate notifications for a
        // pending deadline; once unlocked that deadline is spent, so drop the
        // entry rather than leave it occupying storage. See issue #241.

        Self::emit_milestone_status_changed(
            &env,
            &engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Pending,
        );

        // Event body carries the time-gate evidence so off-chain consumers can
        // confirm the unlock was legitimate without a follow-up `get_milestone`
        // query. See issue #54.
        env.events().publish(
            (
                Symbol::new(&env, "milestone_unlocked"),
                engagement_id.clone(),
            ),
            (milestone_index, valid_after_ledger, unlocked_at_ledger),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #348 — MILESTONE REORDER BEFORE ACTIVATION
    // ----------------------------------------------------------


    // ----------------------------------------------------------
    // ISSUE #241 — MILESTONE DUE-SOON NOTIFICATION
    // ----------------------------------------------------------






    // ----------------------------------------------------------
    // SUBMIT PROOF
    // ----------------------------------------------------------

    /// Submit an IPFS CID (or any URI) as proof that a milestone deliverable has been met.
    ///
    /// # Caller
    /// `recruiter` — must match the engagement's recruiter address and sign the transaction.
    ///
    /// # Behaviour
    /// - The engagement must be `Active` or `ReplacementRequested`.
    /// - The target milestone must be in `Pending` status (i.e. already unlocked).
    /// - If a proof was previously submitted and rejected, the caller must wait
    ///   `proof_cooldown` ledgers (default 2 880 ≈ 4 hours) before resubmitting.
    /// - After successful submission the milestone moves to `ProofSubmitted`.
    /// - If the engagement was `ReplacementRequested` and this is the placement milestone,
    ///   the engagement reverts to `Active`.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"` — contract or engagement is paused.
    /// - `"InvalidProofHash"` — empty string passed as `proof_hash`.
    /// - `"ProofHashTooLong"` — `proof_hash` exceeds the configured max proof hash length.
    /// - `"engagement is not active"` — engagement is not `Active` or `ReplacementRequested`.
    /// - `"unauthorized"` — caller is not the engagement's recruiter.
    /// - `"milestone is not pending"` — milestone is not in `Pending` status.
    /// - `"ResubmitTooSoon"` — resubmitting before the proof cooldown has elapsed.
    /// - `"DuplicateProofHash"` — the proof hash is already used by another milestone
    ///   in this engagement.
    ///
    /// # Events
    /// Emits `("proof_submitted", engagement_id)` with `(milestone_index, proof_hash)`.
    pub fn submit_proof(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
        proof_hash: String,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        // Issue #20: Proof hash format validation (before require_auth for fail-fast)
        if proof_hash.is_empty() {
            panic!("InvalidProofHash");
        }

        if proof_hash.len() > Self::get_max_proof_hash_length_internal(&env) {
            panic!("ProofHashTooLong");
        }

        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Pending {
            panic!("milestone is not pending");
        }

        // Rate-limit resubmissions — first submission (no stored ledger) is always allowed.
        let last_key = DataKey::LastProofAt(engagement_id.clone(), milestone_index);
        let current_ledger = env.ledger().sequence();
        if let Some(last_at) = env.storage().persistent().get::<DataKey, u32>(&last_key) {
            let cooldown = Self::get_proof_cooldown(&env);
            if current_ledger < last_at + cooldown {
                panic!("ResubmitTooSoon");
            }
        }

        // A proof hash identifies the evidence for a milestone and must not be
        // reused by another milestone in the same engagement. Exclude the
        // target milestone so a valid resubmission can replace its own proof.
        for i in 0..engagement.milestones.len() {
            if i != milestone_index {
                let existing_milestone = engagement.milestones.get(i).unwrap();
                if !existing_milestone.proof_hash.is_empty()
                    && existing_milestone.proof_hash == proof_hash
                {
                    panic!("DuplicateProofHash");
                }
            }
        }

        // Record this submission ledger for future cooldown checks.
        env.storage().persistent().set(&last_key, &current_ledger);
        env.storage()
            .persistent()
            .extend_ttl(&last_key, 100_000, 6_300_000);

        let is_resubmission = !milestone.proof_hash.is_empty();
        let old_hash = milestone.proof_hash.clone();

        milestone.proof_hash = proof_hash.clone();
        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::ProofSubmitted;
        milestone.proof_submitted_at = current_ledger;
        engagement.milestones.set(milestone_index, milestone);

        let old_engagement_status = engagement.status.clone();
        if engagement.status == EngagementStatus::ReplacementRequested {
            engagement.status = EngagementStatus::Active;
        }
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        Self::emit_milestone_status_changed(
            &env,
            &engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::ProofSubmitted,
        );
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );

        if is_resubmission {
            env.events().publish(
                (
                    Symbol::new(&env, "proof_resubmitted"),
                    engagement_id.clone(),
                ),
                (milestone_index, old_hash, proof_hash),
            );
        } else {
            env.events().publish(
                (Symbol::new(&env, "proof_submitted"), engagement_id.clone()),
                milestone_index,
            );
        }
    }

    // ----------------------------------------------------------
    // MILESTONE ATTACHMENTS (issue #361)
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // CONFIRM MILESTONE
    // ----------------------------------------------------------

    /// Confirm a milestone after the recruiter has submitted proof, releasing the
    /// milestone's payment share to the recruiter (minus platform fee).
    ///
    /// # Caller
    /// `company` — must match the engagement's company (or its registered
    /// co-signer) and sign the transaction.
    ///
    /// # Preconditions
    /// - Engagement status is `Active`.
    /// - Milestone status is `ProofSubmitted`.
    /// - **Sequential confirmation (Issue #67)**: All prior milestones (indices
    ///   `< milestone_index`) must already be `Confirmed` or `Resolved`.
    /// - For `Retention` milestones: `current_ledger >= valid_after_ledger`
    ///   (the retention window must have elapsed).
    ///
    /// # Payment Calculation
    /// The gross payment is `total_amount * payment_percent / 100`. If the milestone
    /// was previously paid out before a replacement reset (issue #183), only the
    /// difference between the current share and `replacement_paid_out` is released.
    /// Platform fee is deducted from the payment before transfer to the recruiter.
    ///
    /// # Panics
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"unauthorized"` — caller is not the engagement's company or co-signer.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"milestone proof not yet submitted"` — milestone is not in `ProofSubmitted` status.
    /// - `"PreviousMilestoneNotComplete"` — a prior milestone is not yet `Confirmed` or `Resolved`.
    /// - `"retention window has not elapsed — cannot confirm yet"` — for `Retention` milestones
    ///   confirmed before their `valid_after_ledger`.
    ///
    /// # Events
    /// - `("milestone_confirmed", engagement_id)` with `(milestone_index, payment)`.
    /// - `("platform_fee_collected", engagement_id)` with `(milestone_index, fee_amount, treasury)` — when fee > 0.
    /// - `("status_changed", engagement_id)` — when the milestone status changes.
    /// - `("engagement_completed", engagement_id)` — if all milestones are now done.
    pub fn confirm_milestone(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        // Issue #67: enforce sequential confirmation — all prior milestones must be done.
        for i in 0..milestone_index {
            let prev = engagement.milestones.get(i).unwrap();
            if prev.status != MilestoneStatus::Confirmed && prev.status != MilestoneStatus::Resolved
            {
                panic!("PreviousMilestoneNotComplete");
            }
        }

        if milestone.kind == MilestoneKind::Retention {
            let current_ledger = env.ledger().sequence();
            if current_ledger < milestone.valid_after_ledger {
                panic!("retention window has not elapsed — cannot confirm yet");
            }
        }

        // Issue #183: if this milestone was already paid out before a replacement
        // reset it to Pending, only release the difference between its current
        // share (which may have grown via top_up_escrow) and what was already
        // paid — this ensures escrow added after a replacement still reaches
        // the recruiter instead of getting stuck in the contract.
        let full_share = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        let payment = full_share - milestone.replacement_paid_out;
        if payment > 0 {
            let platform_fee = Self::get_platform_fee_internal(&env);
            let effective_bps = if Self::is_fee_waived_internal(&env, &engagement_id) {
                0
            } else {
                let tiered_bps =
                    Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
                Self::apply_referral_discount(&env, tiered_bps, &engagement.referrer)
            };
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
        }

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Confirmed;
        engagement
            .milestones
            .set(milestone_index, milestone.clone());
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

        Self::emit_milestone_status_changed(
            &env,
            &engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Confirmed,
        );
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );

        env.events().publish(
            (
                Symbol::new(&env, "milestone_confirmed"),
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
    // ISSUE #352 — ESCROW RELEASE SCHEDULING
    // ----------------------------------------------------------





    // ----------------------------------------------------------
    // MILESTONE EXTENSION REQUEST (issue #247)
    // ----------------------------------------------------------










    // ----------------------------------------------------------
    // ISSUE #39 — BATCH CONFIRM MILESTONES
    // ----------------------------------------------------------


    // ----------------------------------------------------------
    // CONFIRM WINDOW — AUTO-CONFIRM AFTER INACTION
    // ----------------------------------------------------------

    /// Admin sets the confirm window in ledgers.
    /// Default is 86_400 (~5 days at 5 s/ledger).
    pub fn set_confirm_window(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::ConfirmWindow), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "confirm_window_set"),), ledgers);
    }

    /// Return the current confirm window in ledgers.
    pub fn get_confirm_window(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ConfirmWindow))
            .unwrap_or(DEFAULT_CONFIRM_WINDOW_LEDGERS)
    }
}
