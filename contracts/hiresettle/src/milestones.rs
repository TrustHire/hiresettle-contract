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
    ///   `proof_cooldown` ledgers (default 2 880 ≈ 4 hours) before resubmitting,
    ///   reduced by the recruiter's rating discount (see
    ///   [`Self::get_effective_proof_cooldown`], issue #470).
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
        // Issue #470: well-rated recruiters get a shorter, rating-discounted cooldown.
        let last_key = DataKey::LastProofAt(engagement_id.clone(), milestone_index);
        let current_ledger = env.ledger().sequence();
        if let Some(last_at) = env.storage().persistent().get::<DataKey, u32>(&last_key) {
            let cooldown = Self::effective_proof_cooldown_internal(&env, &engagement.recruiter);
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
    /// - **Prerequisites (issue #461)**: every index in the milestone's
    ///   `prerequisites` must already be `Confirmed` or `Resolved`. A linear
    ///   chain of prerequisites reproduces the old sequential rule (issue #67).
    /// - For `Retention` milestones: `current_ledger >= valid_after_ledger`
    ///   (the retention window must have elapsed).
    ///
    /// # Payment Calculation
    /// The gross payment is `total_amount * payment_percent / 100`. If the milestone
    /// was previously paid out before a replacement reset (issue #183), only the
    /// difference between the current share and `replacement_paid_out` is released.
    /// Platform fee is deducted from the payment before transfer to the recruiter.
    /// If the engagement was created with `stream_duration_ledgers` set
    /// (issue #466), the platform fee is still collected immediately but the net
    /// payment stays in escrow and vests linearly from this ledger; the recruiter
    /// pulls it via [`Self::claim_streamed_payout`].
    ///
    /// # Panics
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"unauthorized"` — caller is not the engagement's company or co-signer.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"milestone proof not yet submitted"` — milestone is not in `ProofSubmitted` status.
    /// - `"PreviousMilestoneNotComplete"` — a prerequisite milestone is not yet `Confirmed` or `Resolved`.
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

        // Issue #461 (replacing #67's flat "all lower indices" rule): every
        // declared prerequisite must be done.
        Self::assert_prerequisites_complete(&engagement, &milestone);

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
            match engagement.stream_duration_ledgers {
                // Issue #466: keep the net share escrowed and vest it instead.
                Some(duration) => Self::start_streamed_payout(
                    &env,
                    &engagement_id,
                    milestone_index,
                    net_payment,
                    duration,
                ),
                None => {
                    Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client)
                }
            }
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
            Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
            Self::decrement_company_active_count(&env, &engagement.company);
            Self::refund_split_withheld(&env, &engagement_id, &mut engagement);
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

    // ----------------------------------------------------------
    // ISSUE #465 — RECRUITER NO-SHOW PENALTY
    // ----------------------------------------------------------

    /// Admin sets how many ledgers a Placement milestone may sit `Pending`
    /// without proof before `trigger_no_show` can forfeit it. `0` (the
    /// default) disables the feature.
    pub fn set_no_show_deadline_ledgers(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::NoShowDeadline), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "no_show_deadline_set"),), ledgers);
    }

    /// Return the configured no-show deadline in ledgers (`0` = disabled).
    pub fn get_no_show_deadline_ledgers(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::NoShowDeadline))
            .unwrap_or(0u32)
    }

    /// Forfeit a Placement milestone whose recruiter never submitted proof.
    ///
    /// # Caller
    /// Anyone — this is a permissionless keeper function.
    ///
    /// # Behaviour
    /// - The engagement must be `Active` or `ReplacementRequested`.
    /// - The milestone must be a `Pending` Placement milestone, and the current
    ///   ledger must be strictly after `unlocked_at + no_show_deadline_ledgers`,
    ///   where `unlocked_at` is the ledger it last became `Pending` (creation,
    ///   a replacement reset, or a rejected dispute).
    /// - The milestone moves to the terminal `Resolved` status (closed, unpaid)
    ///   and its unpaid share is excluded from any recruiter payout; it is not
    ///   added to `released_amount`, so it stays in the company's refund path
    ///   (`cancel_engagement` / `expire_engagement` refund
    ///   `total_amount - released_amount`). If the engagement instead runs to
    ///   `Completed`, the forfeited amount is refunded to the company then.
    ///
    /// # Panics
    /// - `"NoShowDisabled"` — no deadline configured.
    /// - `"engagement is not active"` — engagement is not `Active`/`ReplacementRequested`.
    /// - `"milestone is not pending"` — milestone is not `Pending`.
    /// - `"only placement milestones can be forfeited"` — milestone is a Retention milestone.
    /// - `"NoShowDeadlineNotReached"` — the deadline has not elapsed yet.
    ///
    /// # Events
    /// `("milestone_no_show", engagement_id)` with `(milestone_index, forfeited_amount)`.
    pub fn trigger_no_show(env: Env, engagement_id: String, milestone_index: u32) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        let deadline = Self::get_no_show_deadline_ledgers(env.clone());
        if deadline == 0 {
            panic!("NoShowDisabled");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.status != MilestoneStatus::Pending {
            panic!("milestone is not pending");
        }
        if milestone.kind != MilestoneKind::Placement {
            panic!("only placement milestones can be forfeited");
        }

        let unlocked_at: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::MilestonePendingSince(
                engagement_id.clone(),
                milestone_index,
            ))
            .unwrap_or(engagement.created_at_ledger);
        let current_ledger = env.ledger().sequence();
        if current_ledger <= unlocked_at.saturating_add(deadline) {
            panic!("NoShowDeadlineNotReached");
        }

        // Only the not-yet-paid part of the share is forfeited; anything paid
        // before a replacement reset (issue #183) is already gone.
        let full_share = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        let forfeited = (full_share - milestone.replacement_paid_out).max(0);
        let forfeit_key = DataKey::NoShowForfeited(engagement_id.clone());
        let pending_forfeit: i128 = env.storage().persistent().get(&forfeit_key).unwrap_or(0);
        env.storage()
            .persistent()
            .set(&forfeit_key, &(pending_forfeit + forfeited));
        env.storage()
            .persistent()
            .extend_ttl(&forfeit_key, 100_000, 6_300_000);

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
            Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
            Self::decrement_company_active_count(&env, &engagement.company);
        }
        engagement.last_activity_ledger = current_ledger;

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

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
        env.events().publish(
            (Symbol::new(&env, "milestone_no_show"), engagement_id.clone()),
            (milestone_index, forfeited),
        );
    }

    /// Record that a Placement milestone re-entered `Pending` at the current
    /// ledger, restarting its no-show clock (issue #465).
    pub(crate) fn mark_milestone_pending_since(env: &Env, engagement_id: &String, milestone_index: u32) {
        let key = DataKey::MilestonePendingSince(engagement_id.clone(), milestone_index);
        env.storage().persistent().set(&key, &env.ledger().sequence());
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Refund any no-show-forfeited shares to the company once an engagement
    /// reaches `Completed` (issue #465). Cancelled/expired engagements don't
    /// need this — their `total_amount - released_amount` refund already
    /// covers the forfeited shares.
    pub(crate) fn refund_no_show_forfeit(env: &Env, engagement_id: &String, engagement: &Engagement) {
        let key = DataKey::NoShowForfeited(engagement_id.clone());
        let forfeited: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if forfeited > 0 {
            token::Client::new(env, &engagement.token).transfer(
                &env.current_contract_address(),
                &engagement.company,
                &forfeited,
            );
            env.storage().persistent().remove(&key);
        }
    }

    // ----------------------------------------------------------
    // ISSUE #466 — STREAMING MILESTONE PAYOUT
    // ----------------------------------------------------------

    /// Start (or restart) vesting `net_payment` for a confirmed milestone.
    /// If a previous stream on this milestone still has an unclaimed balance
    /// (e.g. a replacement re-confirmation), that balance is folded into the
    /// new stream so no escrowed funds are stranded.
    pub(crate) fn start_streamed_payout(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
        net_payment: i128,
        duration_ledgers: u32,
    ) {
        let key = DataKey::StreamedPayout(engagement_id.clone(), milestone_index);
        let carried_over = env
            .storage()
            .persistent()
            .get::<DataKey, StreamedPayout>(&key)
            .map(|prev| prev.total - prev.claimed)
            .unwrap_or(0);
        let stream = StreamedPayout {
            total: net_payment + carried_over,
            claimed: 0,
            start_ledger: env.ledger().sequence(),
            duration_ledgers,
        };
        env.storage().persistent().set(&key, &stream);
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Transfer the currently-vested, not-yet-claimed part of a streamed
    /// milestone payout and return the amount transferred.
    ///
    /// # Caller
    /// Anyone may trigger the claim, but funds only ever go to the
    /// engagement's recruiter (split with the co-recruiter, if any, exactly as
    /// a lump-sum payout would be). `recruiter` must match the engagement's
    /// recruiter; no signature is required.
    ///
    /// # Vesting
    /// `vested = total * min(elapsed, duration) / duration` (integer division,
    /// rounding down), where `elapsed = current_ledger - start_ledger`. Rounding
    /// dust is never lost: once `elapsed >= duration`, `vested == total`
    /// exactly. Claiming with nothing newly vested returns `0` without
    /// panicking.
    ///
    /// # Panics
    /// - `"unauthorized"` — `recruiter` is not the engagement's recruiter.
    /// - `"NoStreamedPayout"` — the milestone has no streamed payout.
    ///
    /// # Events
    /// `("streamed_payout_claimed", engagement_id)` with
    /// `(milestone_index, amount, claimed_total)` when `amount > 0`.
    pub fn claim_streamed_payout(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
    ) -> i128 {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if recruiter != engagement.recruiter {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey::StreamedPayout(engagement_id.clone(), milestone_index);
        let mut stream: StreamedPayout = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("NoStreamedPayout"));

        let amount = Self::vested_amount(&env, &stream) - stream.claimed;
        if amount <= 0 {
            return 0;
        }

        stream.claimed += amount;
        env.storage().persistent().set(&key, &stream);
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);

        let token_client = token::Client::new(&env, &engagement.token);
        Self::distribute_recruiter_payout(&env, &engagement, amount, &token_client);

        env.events().publish(
            (
                Symbol::new(&env, "streamed_payout_claimed"),
                engagement_id.clone(),
            ),
            (milestone_index, amount, stream.claimed),
        );
        amount
    }

    /// Return `(claimed, total)` for a streamed milestone payout, or `(0, 0)`
    /// if the milestone has no stream.
    pub fn get_streamed_payout_status(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> (i128, i128) {
        env.storage()
            .persistent()
            .get::<DataKey, StreamedPayout>(&DataKey::StreamedPayout(
                engagement_id,
                milestone_index,
            ))
            .map(|s| (s.claimed, s.total))
            .unwrap_or((0, 0))
    }

    /// Linear vesting: amount of `stream.total` vested at the current ledger.
    pub(crate) fn vested_amount(env: &Env, stream: &StreamedPayout) -> i128 {
        let elapsed = env.ledger().sequence().saturating_sub(stream.start_ledger);
        if elapsed >= stream.duration_ledgers {
            return stream.total;
        }
        stream.total * elapsed as i128 / stream.duration_ledgers as i128
    }
}
