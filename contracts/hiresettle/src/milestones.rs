use soroban_sdk::{contractimpl, token, Address, BytesN, Env, String, Symbol, Vec};
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
        Self::assert_function_not_paused(&env, FN_UNLOCK_MILESTONE);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
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
        Self::bump_engagement_ttl(&env, &engagement_id);

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
        Self::assert_function_not_paused(&env, FN_SUBMIT_PROOF);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);

        // Issue #20: Proof hash format validation (before require_auth for fail-fast)
        if proof_hash.is_empty() {
            panic!("InvalidProofHash");
        }

        if proof_hash.len() > Self::get_max_proof_hash_length_internal(&env) {
            panic!("ProofHashTooLong");
        }

        Self::submit_proof_internal(env, recruiter, engagement_id, milestone_index, proof_hash, None);
    }

    /// Submit a Placement proof together with a salted hash of the candidate's
    /// identity (issue #573).
    ///
    /// The commitment is a `BytesN<32>` hash derived off-chain by the company;
    /// no plaintext identifier is ever stored on-chain. It is indexed per
    /// company so a second placement of the same candidate inside the
    /// admin-configured duplicate window is rejected with `DuplicateCandidate`.
    /// A window of `0` disables the duplicate check but still records the
    /// commitment and its index entry.
    pub fn submit_proof_with_candidate(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
        proof_hash: String,
        candidate_commitment: BytesN<32>,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_function_not_paused(&env, FN_SUBMIT_PROOF);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);

        if proof_hash.is_empty() {
            panic!("InvalidProofHash");
        }
        if proof_hash.len() > Self::get_max_proof_hash_length_internal(&env) {
            panic!("ProofHashTooLong");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.kind != MilestoneKind::Placement {
            panic!("CandidateCommitmentRequiresPlacement");
        }

        let current_ledger = env.ledger().sequence();
        let window = Self::get_candidate_duplicate_window_internal(&env);
        let placement_key = DataKey3::CandidatePlacement(
            engagement.company.clone(),
            candidate_commitment.clone(),
        );
        if window > 0 {
            if let Some((_, placed_at)) =
                env.storage().persistent().get::<DataKey3, (String, u32)>(&placement_key)
            {
                if current_ledger.saturating_sub(placed_at) <= window {
                    panic!("DuplicateCandidate");
                }
            }
        }

        Self::submit_proof_internal(
            env.clone(),
            recruiter,
            engagement_id.clone(),
            milestone_index,
            proof_hash,
            None,
        );

        let commitment_key =
            DataKey3::CandidateCommitment(engagement_id.clone(), milestone_index);
        env.storage()
            .persistent()
            .set(&commitment_key, &candidate_commitment);
        env.storage()
            .persistent()
            .extend_ttl(&commitment_key, 100_000, 6_300_000);
        env.storage()
            .persistent()
            .set(&placement_key, &(engagement_id.clone(), current_ledger));
        env.storage()
            .persistent()
            .extend_ttl(&placement_key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "candidate_commitment_recorded"),
                engagement_id,
            ),
            (milestone_index, candidate_commitment, current_ledger),
        );
    }

    /// Co-sign a Placement proof as the candidate configured on the engagement
    /// (issue #654). Only while the milestone is `ProofSubmitted` and only the
    /// configured candidate may attest.
    pub fn attest_placement(
        env: Env,
        candidate: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        candidate.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let configured = engagement
            .candidate
            .clone()
            .unwrap_or_else(|| panic!("NoCandidateConfigured"));
        if candidate != configured {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.kind != MilestoneKind::Placement {
            panic!("OnlyPlacementMilestonesCanBeAttested");
        }
        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        let key = DataKey3::PlacementAttestation(engagement_id.clone(), milestone_index);
        env.storage().persistent().set(&key, &candidate);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "placement_attested"),
                engagement_id,
                milestone_index,
            ),
            candidate,
        );
    }

    /// Admin sets the candidate-duplicate window in ledgers (issue #573).
    /// `0` disables duplicate detection while commitments are still recorded.
    pub fn set_candidate_duplicate_window(env: Env, admin: Address, window_ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey3::CandidateDuplicateWindow, &window_ledgers);
        env.events().publish(
            (Symbol::new(&env, "candidate_duplicate_window_set"),),
            window_ledgers,
        );
    }

    /// Alias of [`Self::set_candidate_duplicate_window`] kept for callers that
    /// use the issue's wording (issue #573).
    pub fn set_duplicate_candidate_window(env: Env, admin: Address, window_ledgers: u32) {
        Self::set_candidate_duplicate_window(env, admin, window_ledgers);
    }

    /// Return the configured candidate-duplicate window; `0` means disabled.
    pub fn get_candidate_duplicate_window(env: Env) -> u32 {
        Self::get_candidate_duplicate_window_internal(&env)
    }

    /// Alias of [`Self::get_candidate_duplicate_window`] (issue #573).
    pub fn get_duplicate_candidate_window(env: Env) -> u32 {
        Self::get_candidate_duplicate_window_internal(&env)
    }

    pub(crate) fn get_candidate_duplicate_window_internal(env: &Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey3::CandidateDuplicateWindow)
            .unwrap_or(0)
    }

    /// Submit a Merkle root over several evidence items as a milestone's proof
    /// (issue #486) — the multi-item alternative to [`Self::submit_proof`].
    ///
    /// The recruiter builds a Merkle tree off-chain whose leaves are the
    /// 32-byte hashes of each evidence item, with every level hashed as
    /// `sha256(min(a, b) || max(a, b))`, and commits only the root. Anyone can
    /// later check that one item was part of the committed set with
    /// [`Self::verify_proof_inclusion`] without the contract storing the items.
    ///
    /// # Caller
    /// `recruiter` — must match the engagement's recruiter address and sign the transaction.
    ///
    /// # Behaviour
    /// Identical to [`Self::submit_proof`]: same engagement/milestone status
    /// checks, same resubmission cooldown, same duplicate check and the same
    /// `Pending → ProofSubmitted` transition. The milestone's `proof_hash` is
    /// set to `"merkle:"` followed by the root in lowercase hex, so every
    /// downstream flow (confirmation, disputes, replacements) treats the root
    /// like a single hash. The root is a fixed 32 bytes, so the configurable
    /// max proof hash length does not apply to it.
    ///
    /// Resubmitting with plain `submit_proof` replaces the root; resubmitting
    /// with another root replaces it too.
    ///
    /// # Panics
    /// Same as [`Self::submit_proof`], except `"InvalidProofHash"` and
    /// `"ProofHashTooLong"`, which cannot occur for a fixed-size root.
    ///
    /// # Events
    /// Emits the same `proof_submitted` / `proof_resubmitted` event as
    /// `submit_proof`, followed by `("proof_root_submitted", engagement_id)`
    /// with `(milestone_index, merkle_root)`.
    pub fn submit_proof_root(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
        merkle_root: BytesN<32>,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        let proof_hash = Self::merkle_root_proof_hash(&env, &merkle_root);
        Self::submit_proof_internal(
            env.clone(),
            recruiter,
            engagement_id.clone(),
            milestone_index,
            proof_hash,
            Some(merkle_root.clone()),
        );

        env.events().publish(
            (
                Symbol::new(&env, "proof_root_submitted"),
                engagement_id,
            ),
            (milestone_index, merkle_root),
        );
    }

    /// Shared body of [`Self::submit_proof`] and [`Self::submit_proof_root`]
    /// (issue #486), run after the caller-specific pause and format checks.
    /// `merkle_root` is `Some` only for a root submission; it is recorded under
    /// `DataKey2::ProofMerkleRoot`, and a plain submission clears any root left
    /// by an earlier root submission on the same milestone.
    fn submit_proof_internal(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
        proof_hash: String,
        merkle_root: Option<BytesN<32>>,
    ) {
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
        Self::bump_engagement_ttl(&env, &engagement_id);

        // Issue #486: keep the committed Merkle root (if any) alongside the
        // milestone; a plain single-hash submission drops a stale root.
        let root_key = DataKey2::ProofMerkleRoot(engagement_id.clone(), milestone_index);
        match merkle_root {
            Some(root) => {
                env.storage().persistent().set(&root_key, &root);
                env.storage()
                    .persistent()
                    .extend_ttl(&root_key, 100_000, 6_300_000);
            }
            None => env.storage().persistent().remove(&root_key),
        }

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
            Some(recruiter.clone()),
        );

        if is_resubmission {
            env.events().publish(
                (
                    Symbol::new(&env, "proof_resubmitted"),
                    engagement_id.clone(),
                ),
                (milestone_index, old_hash, proof_hash.clone()),
            );
        } else {
            env.events().publish(
                (Symbol::new(&env, "proof_submitted"), engagement_id.clone()),
                milestone_index,
            );
        }

        Self::record_proof_history(
            &env,
            &engagement_id,
            milestone_index,
            ProofAction::Submitted,
            proof_hash,
            recruiter,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #666 — WITHDRAW PROOF
    // ----------------------------------------------------------

    /// Recruiter retracts a submitted proof before the company has acted on
    /// it, e.g. after submitting the wrong hash. The milestone moves from
    /// `ProofSubmitted` back to `Pending` (for a Retention milestone that is
    /// its unlocked state) and its `proof_hash` is cleared, along with any
    /// committed Merkle root. The withdrawal is recorded in
    /// [`Self::get_proof_history`].
    ///
    /// Allowed only while the company has not acted: the milestone must still
    /// be `ProofSubmitted` (so it is not confirmed or disputed) and no
    /// unexpired partial confirmation may be pending against this proof.
    ///
    /// The proof resubmission cooldown is not reset or bypassed: it keeps
    /// running from the withdrawn submission, so the next `submit_proof` must
    /// still wait out the cooldown. Likewise the placement no-show clock
    /// (`trigger_no_show`) is not restarted.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"` / `"MilestoneOnHold"` —
    ///   contract, engagement or milestone is frozen.
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"unauthorized"` — caller is not the recruiter or recruiter co-signer.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"NoProofToWithdraw"` — milestone is not `ProofSubmitted` (no proof
    ///   pending review, or the company already confirmed or disputed it).
    /// - `"CompanyAlreadyActed"` — the company has a pending partial
    ///   confirmation proposal on this proof.
    ///
    /// # Events
    /// - `("milestone_status_changed", ...)` for `ProofSubmitted → Pending`.
    /// - `("proof_withdrawn", engagement_id)` with `(milestone_index, proof_hash)`.
    pub fn withdraw_proof(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("NoProofToWithdraw");
        }

        let now = env.ledger().sequence();
        let proposal: Option<PartialConfirmationProposal> = env.storage().persistent().get(
            &DataKey2::PartialConfirmationProposal(engagement_id.clone(), milestone_index),
        );
        if let Some(p) = proposal {
            if p.proof_submitted_at == milestone.proof_submitted_at && now <= p.expires_at_ledger {
                panic!("CompanyAlreadyActed");
            }
        }

        let withdrawn_hash = milestone.proof_hash.clone();
        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Pending;
        milestone.proof_hash = String::from_str(&env, "");
        engagement.milestones.set(milestone_index, milestone);
        engagement.last_activity_ledger = now;

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::bump_engagement_ttl(&env, &engagement_id);
        env.storage()
            .persistent()
            .remove(&DataKey2::ProofMerkleRoot(engagement_id.clone(), milestone_index));

        Self::record_proof_history(
            &env,
            &engagement_id,
            milestone_index,
            ProofAction::Withdrawn,
            withdrawn_hash.clone(),
            recruiter,
        );

        Self::emit_milestone_status_changed(
            &env,
            &engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Pending,
        );
        env.events().publish(
            (Symbol::new(&env, "proof_withdrawn"), engagement_id),
            (milestone_index, withdrawn_hash),
        );
    }

    /// Proof submissions and withdrawals on a milestone, oldest first
    /// (issue #666). Keeps the most recent `MAX_PROOF_HISTORY_ENTRIES`
    /// entries; empty for an unknown engagement or milestone.
    pub fn get_proof_history(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Vec<ProofHistoryEntry> {
        env.storage()
            .persistent()
            .get(&DataKey3::ProofHistory(engagement_id, milestone_index))
            .unwrap_or(Vec::new(&env))
    }

    /// Append an entry to a milestone's FIFO-capped proof history (issue #666).
    fn record_proof_history(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
        action: ProofAction,
        proof_hash: String,
        actor: Address,
    ) {
        let key = DataKey3::ProofHistory(engagement_id.clone(), milestone_index);
        let mut history: Vec<ProofHistoryEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(Vec::new(env));
        history.push_back(ProofHistoryEntry {
            action,
            proof_hash,
            actor,
            ledger: env.ledger().sequence(),
        });
        while history.len() > MAX_PROOF_HISTORY_ENTRIES {
            history.pop_front();
        }
        env.storage().persistent().set(&key, &history);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
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
    /// - `("milestone_payout", engagement_id)` with
    ///   `(milestone_index, payment, Some(confirmer), invoice_ref)` (issue #669);
    ///   the same data is stored as the milestone receipt.
    /// - `("platform_fee_collected", engagement_id)` with `(milestone_index, fee_amount, treasury)` — when fee > 0.
    /// - `("status_changed", engagement_id)` — when the milestone status changes.
    /// - `("engagement_completed", engagement_id)` — if all milestones are now done.
    ///
    /// Writes a [`MilestoneReceipt`] without a note (see
    /// [`Self::confirm_milestone_with_note`] to attach one).
    pub fn confirm_milestone(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::confirm_milestone_internal(env, company, engagement_id, milestone_index, None);
    }

    /// Same as [`Self::confirm_milestone`], but records `note_hash` (e.g. an
    /// IPFS CID of a note explaining why, or with what caveats, the company
    /// confirmed) on the milestone's [`MilestoneReceipt`] for audits
    /// (issue #665). Read it back with [`Self::get_milestone_receipt`].
    ///
    /// Subject to the same per-function pause as `confirm_milestone`.
    /// `batch_confirm_milestones` does not take notes; confirm milestones that
    /// need one individually with this call.
    ///
    /// # Panics
    /// Everything [`Self::confirm_milestone`] raises, plus:
    /// - `"InvalidNoteHash"` — `note_hash` is empty.
    /// - `"NoteHashTooLong"` — `note_hash` exceeds the configured max proof
    ///   hash length (`set_max_proof_hash_length`).
    ///
    /// # Events
    /// Everything [`Self::confirm_milestone`] emits, plus
    /// `("milestone_confirmed_with_note", engagement_id)` with
    /// `(milestone_index, payment, note_hash)` right after `milestone_confirmed`.
    pub fn confirm_milestone_with_note(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
        note_hash: String,
    ) {
        if note_hash.is_empty() {
            panic!("InvalidNoteHash");
        }
        if note_hash.len() > Self::get_max_proof_hash_length_internal(&env) {
            panic!("NoteHashTooLong");
        }
        Self::confirm_milestone_internal(
            env,
            company,
            engagement_id,
            milestone_index,
            Some(note_hash),
        );
    }

    /// Return the confirmation receipt for a milestone (issue #665), or
    /// `None` if it has not been confirmed through `confirm_milestone`,
    /// `confirm_milestone_with_note` or `batch_confirm_milestones`. Milestones
    /// settled another way (force confirmation, partial confirmation, dispute
    /// resolution) have no receipt.
    pub fn get_milestone_receipt(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<MilestoneReceipt> {
        env.storage()
            .persistent()
            .get(&DataKey3::MilestoneReceipt(engagement_id, milestone_index))
    }

    /// Store the receipt for a company confirmation (issue #665).
    fn record_milestone_receipt(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
        confirmed_by: &Address,
        payment: i128,
        note_hash: Option<String>,
    ) {
        let key = DataKey3::MilestoneReceipt(engagement_id.clone(), milestone_index);
        let receipt = MilestoneReceipt {
            confirmed_by: confirmed_by.clone(),
            confirmed_at_ledger: env.ledger().sequence(),
            payment,
            note_hash,
        };
        env.storage().persistent().set(&key, &receipt);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Shared body of `confirm_milestone` and `confirm_milestone_with_note`.
    fn confirm_milestone_internal(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
        note_hash: Option<String>,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_function_not_paused(&env, FN_CONFIRM_MILESTONE);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        company.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_company_confirmer(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        Self::confirm_milestone_core(
            &env,
            engagement,
            &engagement_id,
            milestone_index,
            &company,
            false,
        );
    }

    /// Shared body of `confirm_milestone` and `oracle_confirm_milestone`
    /// (issue #667), run after the caller has been authorized: validates the
    /// milestone, releases its payout, records the receipt (issue #669) and
    /// emits the confirmation events. `by_oracle` selects the attribution
    /// event: `milestone_confirmed_by_oracle`, or
    /// `milestone_confirmed_by_member` when a team member confirmed.
    pub(crate) fn confirm_milestone_core(
        env: &Env,
        mut engagement: Engagement,
        engagement_id: &String,
        milestone_index: u32,
        confirmer: &Address,
        by_oracle: bool,
    ) {
        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        // Issue #654: when a candidate is configured, a Placement proof cannot
        // be confirmed until that candidate has co-signed it.
        if milestone.kind == MilestoneKind::Placement {
            if let Some(candidate) = &engagement.candidate {
                let key =
                    DataKey3::PlacementAttestation(engagement_id.clone(), milestone_index);
                let attested: Option<Address> = env.storage().persistent().get(&key);
                if attested.as_ref() != Some(candidate) {
                    panic!("PlacementAttestationRequired");
                }
            }
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
            let effective_bps = Self::payout_fee_bps(env, engagement_id, &engagement);
            let fee_amount =
                Self::platform_fee_amount(env, engagement_id, payment, effective_bps);
            let net_payment = payment - fee_amount;
            engagement.released_amount += payment;
            // Issue #578: payout releases escrow.
            Self::add_token_locked(env, &engagement.token, -payment);

            let token_client = token::Client::new(env, &engagement.token);
            if fee_amount > 0 {
                Self::collect_platform_fee(
                    env,
                    &engagement,
                    engagement_id,
                    milestone_index,
                    fee_amount,
                );
            }
            match engagement.stream_duration_ledgers {
                // Issue #466: keep the net share escrowed and vest it instead.
                Some(duration) => Self::start_streamed_payout(
                    env,
                    engagement_id,
                    milestone_index,
                    net_payment,
                    duration,
                ),
                None => {
                    Self::distribute_recruiter_payout(env, &engagement, net_payment, &token_client, true)
                }
            }
        }

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Confirmed;
        engagement
            .milestones
            .set(milestone_index, milestone.clone());
        Self::bond_clear_rejection(env, engagement_id, milestone_index);

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::settle_bonds(env, &engagement);
            Self::refund_no_show_forfeit(env, engagement_id, &engagement);
            Self::decrement_company_active_count(env, &engagement.company);
            Self::refund_split_withheld(env, engagement_id, &mut engagement);
        }
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::bump_engagement_ttl(&env, &engagement_id);

        Self::emit_milestone_status_changed(
            env,
            engagement_id,
            milestone_index,
            old_status,
            MilestoneStatus::Confirmed,
        );
        Self::emit_engagement_status_changed(
            env,
            engagement_id,
            old_engagement_status,
            engagement.status.clone(),
            Some(confirmer.clone()),
        );

        Self::record_milestone_receipt(
            &env,
            &engagement_id,
            milestone_index,
            &company,
            payment,
            note_hash.clone(),
        );

        env.events().publish(
            (
                Symbol::new(env, "milestone_confirmed"),
                engagement_id.clone(),
            ),
            (milestone_index, payment),
        );
        // Issue #665: carry the company's confirmation note.
        if let Some(note) = note_hash {
            env.events().publish(
                (
                    Symbol::new(&env, "milestone_confirmed_with_note"),
                    engagement_id.clone(),
                ),
                (milestone_index, payment, note),
            );
        }
        // Issue #575: identify the team member when the confirmer is not the
        // company itself.
        if Self::company_member_role(&env, &engagement.company, &company).is_some() {
            env.events().publish(
                (
                    Symbol::new(env, "milestone_confirmed_by_oracle"),
                    engagement_id.clone(),
                ),
                (milestone_index, confirmer.clone()),
            );
        } else if Self::company_member_role(env, &engagement.company, confirmer).is_some() {
            // Issue #575: identify the team member when the confirmer is not
            // the company itself.
            env.events().publish(
                (
                    Symbol::new(env, "milestone_confirmed_by_member"),
                    engagement_id.clone(),
                ),
                (milestone_index, confirmer.clone()),
            );
        }

        if all_done {
            env.events().publish(
                (
                    Symbol::new(env, "engagement_completed"),
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
    // ISSUE #550 — PARTIAL MILESTONE CONFIRMATION
    // ----------------------------------------------------------

    /// Company proposes releasing only `release_bps` of a `ProofSubmitted`
    /// milestone, for partially delivered work, without raising a dispute.
    /// The recruiter accepts with [`Self::accept_partial_confirmation`] or
    /// declines with [`Self::reject_partial_confirmation`]. A new proposal on
    /// the same milestone replaces the pending one.
    ///
    /// The proposal expires after the amendment TTL (`get_amendment_ttl`) and
    /// only applies to the proof it was made against.
    ///
    /// # Panics
    /// - `"InvalidReleaseBps"` — `release_bps` exceeds 10 000.
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"unauthorized"` — caller is not the engagement's company or co-signer.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"milestone proof not yet submitted"` — milestone is not `ProofSubmitted`.
    ///
    /// # Events
    /// `("partial_confirmation_proposed", engagement_id)` with
    /// `(milestone_index, release_bps, expires_at_ledger)`.
    pub fn propose_partial_confirmation(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
        release_bps: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        company.require_auth();

        if release_bps > FULL_SPLIT_BPS {
            panic!("InvalidReleaseBps");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        let now = env.ledger().sequence();
        let proposal = PartialConfirmationProposal {
            proposer: company,
            release_bps,
            proof_submitted_at: milestone.proof_submitted_at,
            proposed_at_ledger: now,
            expires_at_ledger: now.saturating_add(Self::get_amendment_ttl(env.clone())),
        };
        let key = DataKey2::PartialConfirmationProposal(engagement_id.clone(), milestone_index);
        env.storage().persistent().set(&key, &proposal);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "partial_confirmation_proposed"),
                engagement_id,
            ),
            (milestone_index, release_bps, proposal.expires_at_ledger),
        );
    }

    /// Recruiter accepts a pending partial confirmation. `release_bps` of the
    /// milestone amount is paid through the normal fee pipeline (platform fee,
    /// referrer share, streaming, co-recruiter split, pull payouts), the rest
    /// is refunded to the company, and the milestone becomes `Confirmed`.
    ///
    /// The milestone amount is the same `payment` `confirm_milestone` would
    /// release (net of any `replacement_paid_out`); released + refunded always
    /// equal it exactly.
    ///
    /// # Panics
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"unauthorized"` — caller is not the recruiter or recruiter co-signer.
    /// - `"no pending partial confirmation"` — nothing to accept.
    /// - `"PartialConfirmationExpired"` — the proposal outlived the amendment TTL.
    /// - `"milestone proof not yet submitted"` — the milestone left
    ///   `ProofSubmitted`, or the proof changed, since the proposal was made.
    /// - `"PreviousMilestoneNotComplete"` / `"retention window has not elapsed
    ///   — cannot confirm yet"` — same gates as `confirm_milestone`.
    ///
    /// # Events
    /// - `("partial_confirmation_accepted", engagement_id)` with
    ///   `(milestone_index, release_bps, released, refunded)`.
    /// - `("milestone_confirmed", engagement_id)` with `(milestone_index, released)`.
    /// - `("milestone_payout", engagement_id)` with
    ///   `(milestone_index, released, Some(recruiter), invoice_ref)`; the
    ///   receipt records the accepting recruiter as `confirmed_by`.
    /// - The usual status-change / `engagement_completed` events.
    pub fn accept_partial_confirmation(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey2::PartialConfirmationProposal(engagement_id.clone(), milestone_index);
        let proposal: PartialConfirmationProposal = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending partial confirmation"));
        if env.ledger().sequence() > proposal.expires_at_ledger {
            panic!("PartialConfirmationExpired");
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.status != MilestoneStatus::ProofSubmitted
            || milestone.proof_submitted_at != proposal.proof_submitted_at
        {
            panic!("milestone proof not yet submitted");
        }
        Self::assert_prerequisites_complete(&engagement, &milestone);
        if milestone.kind == MilestoneKind::Retention
            && env.ledger().sequence() < milestone.valid_after_ledger
        {
            panic!("retention window has not elapsed — cannot confirm yet");
        }

        env.storage().persistent().remove(&key);

        let full_share = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        let payment = full_share - milestone.replacement_paid_out;
        let mut released: i128 = 0;
        let mut refunded: i128 = 0;
        if payment > 0 {
            released = (payment * proposal.release_bps as i128) / (FULL_SPLIT_BPS as i128);
            refunded = payment - released;
            engagement.released_amount += payment;
            Self::adjust_escrow(&env, &engagement_id, &engagement.token, -payment);

            let token_client = token::Client::new(&env, &engagement.token);
            if released > 0 {
                let effective_bps = Self::payout_fee_bps(&env, &engagement_id, &engagement);
                let fee_amount =
                    Self::platform_fee_amount(&env, &engagement_id, released, effective_bps);
                let net_payment = released - fee_amount;
                if fee_amount > 0 {
                    Self::collect_platform_fee(
                        &env,
                        &engagement,
                        &engagement_id,
                        milestone_index,
                        fee_amount,
                    );
                }
                match engagement.stream_duration_ledgers {
                    Some(duration) => Self::start_streamed_payout(
                        &env,
                        &engagement_id,
                        &engagement.token,
                        milestone_index,
                        net_payment,
                        duration,
                    ),
                    None => Self::distribute_recruiter_payout(
                        &env,
                        &engagement,
                        net_payment,
                        &token_client,
                        true,
                    ),
                }
            }
            if refunded > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &engagement.company,
                    &refunded,
                );
            }
        }

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
            Self::settle_bonds(&env, &engagement);
            Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
            Self::decrement_company_active_count(&env, &engagement.company);
            Self::refund_split_withheld(&env, &engagement_id, &mut engagement);
        }
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::bump_engagement_ttl(&env, &engagement_id);

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
            Some(recruiter.clone()),
        );
        env.events().publish(
            (
                Symbol::new(&env, "partial_confirmation_accepted"),
                engagement_id.clone(),
            ),
            (milestone_index, proposal.release_bps, released, refunded),
        );
        env.events().publish(
            (
                Symbol::new(&env, "milestone_confirmed"),
                engagement_id.clone(),
            ),
            (milestone_index, released),
        );
        Self::record_milestone_receipt(
            &env,
            &engagement_id,
            milestone_index,
            released,
            Some(recruiter.clone()),
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
    // ISSUE #655 — PRO-RATA RETENTION REFUND ON EARLY DEPARTURE
    // ----------------------------------------------------------

    /// Report that a candidate left partway through a retention window. The
    /// company proposes a pro-rata split of the milestone amount; the
    /// recruiter must accept it via [`Self::accept_early_departure`] before
    /// any funds move.
    ///
    /// Only an unpaid `Locked` or `Pending` retention milestone is eligible:
    /// placement milestones and milestones already confirmed, disputed or in
    /// another resolution path cannot use this flow.
    pub fn report_early_departure(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
        departure_ledgers: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        company.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.kind != MilestoneKind::Retention {
            panic!("only retention milestones can be reported early");
        }
        if milestone.status != MilestoneStatus::Locked
            && milestone.status != MilestoneStatus::Pending
        {
            panic!("milestone is not eligible for early departure");
        }

        let retention_started_at: u32 = env
            .storage()
            .persistent()
            .get(&DataKey3::RetentionStartedAt(
                engagement_id.clone(),
                milestone_index,
            ))
            .unwrap_or(engagement.created_at_ledger);
        let retention_window_ledgers = milestone
            .valid_after_ledger
            .saturating_sub(retention_started_at);
        if retention_window_ledgers == 0 {
            panic!("invalid retention window");
        }
        if departure_ledgers == 0 {
            panic!("departure_ledgers must be positive");
        }

        let now = env.ledger().sequence();
        let proposal = EarlyDepartureProposal {
            proposer: company,
            payment: (engagement.total_amount * milestone.payment_percent as i128) / 100
                - milestone.replacement_paid_out,
            retention_started_at,
            retention_window_ledgers,
            valid_after_ledger: milestone.valid_after_ledger,
            departure_ledgers: departure_ledgers.min(retention_window_ledgers),
            expires_at_ledger: now.saturating_add(Self::get_amendment_ttl(env.clone())),
        };
        let key = DataKey3::EarlyDepartureProposal(engagement_id.clone(), milestone_index);
        env.storage().persistent().set(&key, &proposal);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "early_departure_reported"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                proposal.departure_ledgers,
                retention_window_ledgers,
                proposal.expires_at_ledger,
            ),
        );
    }

    /// Accept a pending early-departure proposal. The recruiter receives
    /// `payment * served_ledgers / retention_window_ledgers` through the
    /// normal fee pipeline; the company gets the remainder back. Integer
    /// division rounds down, so any rounding dust stays with the company and
    /// the two amounts always sum to exactly the milestone payment.
    ///
    /// The milestone moves to the terminal `Resolved` state, which counts as
    /// complete for engagement-completion purposes.
    pub fn accept_early_departure(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey3::EarlyDepartureProposal(engagement_id.clone(), milestone_index);
        let proposal: EarlyDepartureProposal = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending early departure"));
        if env.ledger().sequence() > proposal.expires_at_ledger {
            panic!("EarlyDepartureExpired");
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.kind != MilestoneKind::Retention
            || (milestone.status != MilestoneStatus::Locked
                && milestone.status != MilestoneStatus::Pending)
            || milestone.valid_after_ledger != proposal.valid_after_ledger
            || (engagement.total_amount * milestone.payment_percent as i128) / 100
                - milestone.replacement_paid_out
                != proposal.payment
        {
            panic!("early departure proposal no longer valid");
        }

        env.storage().persistent().remove(&key);
        env.storage().persistent().remove(&DataKey3::RetentionStartedAt(
            engagement_id.clone(),
            milestone_index,
        ));

        let payment = proposal.payment.max(0);
        // README "Numeric Precision": non-negative i128 division rounds down;
        // the subtracted side (the company refund) gets the rounding dust.
        let earned = if payment > 0 {
            (payment * proposal.departure_ledgers as i128)
                / proposal.retention_window_ledgers as i128
        } else {
            0
        };
        let refunded = payment - earned;

        if payment > 0 {
            engagement.released_amount += payment;
            Self::add_token_locked(&env, &engagement.token, -payment);
            let token_client = token::Client::new(&env, &engagement.token);
            if earned > 0 {
                let effective_bps = Self::payout_fee_bps(&env, &engagement_id, &engagement);
                let fee_amount =
                    Self::platform_fee_amount(&env, &engagement_id, earned, effective_bps);
                let net_payment = earned - fee_amount;
                if fee_amount > 0 {
                    Self::collect_platform_fee(
                        &env,
                        &engagement,
                        &engagement_id,
                        milestone_index,
                        fee_amount,
                    );
                }
                match engagement.stream_duration_ledgers {
                    Some(duration) => Self::start_streamed_payout(
                        &env,
                        &engagement_id,
                        milestone_index,
                        net_payment,
                        duration,
                    ),
                    None => Self::distribute_recruiter_payout(
                        &env,
                        &engagement,
                        net_payment,
                        &token_client,
                        true,
                    ),
                }
            }
            Self::refund_company_escrow(&env, &engagement, &engagement_id, refunded);
        }

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
            Self::settle_bonds(&env, &engagement);
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
            MilestoneStatus::Resolved,
        );
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
            Some(recruiter.clone()),
        );
        env.events().publish(
            (
                Symbol::new(&env, "retention_prorated"),
                engagement_id.clone(),
            ),
            (milestone_index, earned, refunded),
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

    /// Recruiter declines a pending partial confirmation. The proposal is
    /// discarded and the milestone stays `ProofSubmitted`, so the company can
    /// still confirm in full, raise a dispute, or propose again.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the recruiter or recruiter co-signer.
    /// - `"no pending partial confirmation"` — nothing to reject.
    ///
    /// # Events
    /// `("partial_confirmation_rejected", engagement_id)` with
    /// `(milestone_index, release_bps)`.
    pub fn reject_partial_confirmation(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey2::PartialConfirmationProposal(engagement_id.clone(), milestone_index);
        let proposal: PartialConfirmationProposal = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending partial confirmation"));
        env.storage().persistent().remove(&key);

        env.events().publish(
            (
                Symbol::new(&env, "partial_confirmation_rejected"),
                engagement_id,
            ),
            (milestone_index, proposal.release_bps),
        );
    }

    /// Return the pending partial confirmation for a milestone, or `None` if
    /// there is none or it has expired.
    pub fn get_pending_partial_confirmation(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<PartialConfirmationProposal> {
        let proposal: PartialConfirmationProposal = env
            .storage()
            .persistent()
            .get(&DataKey2::PartialConfirmationProposal(engagement_id, milestone_index))?;
        if env.ledger().sequence() > proposal.expires_at_ledger {
            return None;
        }
        Some(proposal)
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

    /// Confirm several milestones in one transaction with all-or-nothing
    /// semantics. Honours platform-fee waiver the same way as
    /// `confirm_milestone` / `force_confirm_milestone`.
    ///
    /// # Panics
    /// - `"EmptyIndices"` — `milestone_indices` is empty.
    /// - `"engagement is not active"` — engagement is not `Active`.
    /// - `"unauthorized"` — caller is not the company / co-signer.
    /// - `"milestone proof not yet submitted"` — a target is not `ProofSubmitted`.
    /// - `"retention window has not elapsed — cannot confirm yet"` — retention gate.
    /// - `"PreviousMilestoneNotComplete"` — a prerequisite is not done and not
    ///   also present in this batch.
    ///
    /// # Confirmation notes
    /// Batch confirmation does not support per-milestone notes (issue #665).
    /// Each confirmed milestone gets a [`MilestoneReceipt`] with
    /// `note_hash: None`; use [`Self::confirm_milestone_with_note`] for any
    /// milestone that needs a note.
    pub fn batch_confirm_milestones(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_indices: Vec<u32>,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_function_not_paused(&env, FN_BATCH_CONFIRM_MILESTONES);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        // Issue #492: a held index rejects the whole batch.
        for i in 0..milestone_indices.len() {
            let idx = milestone_indices.get(i).unwrap();
            Self::assert_milestone_not_on_hold(&env, &engagement_id, idx);
        }
        company.require_auth();

        if milestone_indices.is_empty() {
            panic!("EmptyIndices");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_company_confirmer(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        // Validate all milestones first (atomic rejection).
        for i in 0..milestone_indices.len() {
            let idx = milestone_indices.get(i).unwrap();
            let m = Self::get_milestone_or_panic(&engagement, idx);
            if m.status != MilestoneStatus::ProofSubmitted {
                panic!("milestone proof not yet submitted");
            }
            if m.kind == MilestoneKind::Retention && env.ledger().sequence() < m.valid_after_ledger
            {
                panic!("retention window has not elapsed — cannot confirm yet");
            }
            // Prerequisites must be done on-chain or included in this batch.
            for j in 0..m.prerequisites.len() {
                let prereq_idx = m.prerequisites.get(j).unwrap();
                let prev = engagement.milestones.get(prereq_idx).unwrap();
                let done_already = prev.status == MilestoneStatus::Confirmed
                    || prev.status == MilestoneStatus::Resolved;
                let done_in_batch = (0..milestone_indices.len())
                    .any(|k| milestone_indices.get(k).unwrap() == prereq_idx);
                if !done_already && !done_in_batch {
                    panic!("PreviousMilestoneNotComplete");
                }
            }
        }

        let effective_bps = Self::payout_fee_bps(&env, &engagement_id, &engagement);
        let token_client = token::Client::new(&env, &engagement.token);

        for i in 0..milestone_indices.len() {
            let idx = milestone_indices.get(i).unwrap();
            let mut m = engagement.milestones.get(idx).unwrap();

            let payment = (engagement.total_amount * m.payment_percent as i128) / 100;
            let fee_amount =
                Self::platform_fee_amount(&env, &engagement_id, payment, effective_bps);
            let net_payment = payment - fee_amount;
            engagement.released_amount += payment;
            // Issue #578: payout releases escrow.
            Self::adjust_escrow(&env, &engagement_id, &engagement.token, -payment);

            if fee_amount > 0 {
                Self::collect_platform_fee(&env, &engagement, &engagement_id, idx, fee_amount);
            }
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client, true);

            let old_status = m.status.clone();
            m.status = MilestoneStatus::Confirmed;
            engagement.milestones.set(idx, m);
            Self::bond_clear_rejection(&env, &engagement_id, idx);

            Self::emit_milestone_status_changed(
                &env,
                &engagement_id,
                idx,
                old_status,
                MilestoneStatus::Confirmed,
            );

            Self::record_milestone_receipt(&env, &engagement_id, idx, &company, payment, None);

            env.events().publish(
                (
                    Symbol::new(&env, "milestone_confirmed"),
                    engagement_id.clone(),
                ),
                (idx, payment),
            );
            Self::record_milestone_receipt(
                &env,
                &engagement_id,
                idx,
                payment,
                Some(company.clone()),
            );
            // Issue #575: identify the team member when the confirmer is not
            // the company itself.
            if Self::company_member_role(&env, &engagement.company, &company).is_some() {
                env.events().publish(
                    (
                        Symbol::new(&env, "milestone_confirmed_by_member"),
                        engagement_id.clone(),
                    ),
                    (idx, company.clone()),
                );
            }
        }

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::settle_bonds(&env, &engagement);
            Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
            Self::decrement_company_active_count(&env, &engagement.company);
            Self::refund_split_withheld(&env, &engagement_id, &mut engagement);
        }
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::bump_engagement_ttl(&env, &engagement_id);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
            Some(company.clone()),
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
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);

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
            Self::settle_bonds(&env, &engagement);
            Self::refund_no_show_forfeit(&env, &engagement_id, &engagement);
            Self::decrement_company_active_count(&env, &engagement.company);
        }
        engagement.last_activity_ledger = current_ledger;

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::bump_engagement_ttl(&env, &engagement_id);

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
            None,
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
            // Issue #671: the forfeit was never added to `released_amount`.
            Self::adjust_escrow(env, engagement_id, &engagement.token, -forfeited);
            Self::record_escrow_refund(env, engagement_id, forfeited);
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
        token: &Address,
        milestone_index: u32,
        net_payment: i128,
        duration_ledgers: u32,
    ) {
        // Issue #672: the net share stays in the contract until claimed.
        Self::add_token_aggregate(env, DataKey3::TokenStreamed(token.clone()), net_payment);
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
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);

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
        Self::add_token_aggregate(&env, DataKey3::TokenStreamed(engagement.token.clone()), -amount);

        let token_client = token::Client::new(&env, &engagement.token);
        Self::distribute_recruiter_payout(&env, &engagement, amount, &token_client, true);

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
