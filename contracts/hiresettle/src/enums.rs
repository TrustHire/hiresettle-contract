//! Contract enums (`#[contracttype]`), including the `DataKey`/`DataKey2`
//! storage key space.

use soroban_sdk::{contracttype, Address, String};

/// Lifecycle state of a single milestone.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum MilestoneStatus {
    /// Retention milestones start here; `unlock_milestone` moves them to `Pending`
    /// once the required ledger window has elapsed.
    Locked,
    /// The milestone is open: the recruiter may submit a proof hash.
    Pending,
    /// The recruiter has submitted a proof hash; the company must now confirm or raise a dispute.
    ProofSubmitted,
    /// The company confirmed the proof; payment has been released to the recruiter.
    Confirmed,
    /// The company raised a dispute; arbiters must vote before the milestone can progress.
    Disputed,
    /// Arbiters voted to approve the milestone payment (dispute resolved in recruiter's favour).
    /// The milestone counts as done for the purposes of completing the engagement.
    Resolved,
}
/// Category of an [`crate::TimelineEntry`] returned by
/// `get_engagement_timeline` (issue #501). When entries share a ledger they
/// are ordered by this declaration order.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum TimelineKind {
    /// An accepted co-recruiter split amendment (`get_split_amendment_log`).
    Amendment,
    /// A replacement request (`get_replacement_record` / `get_replacement_reason`).
    Replacement,
    /// Reserved for milestone deadline extensions. Nothing records extensions
    /// in this contract version, so no entry of this kind is produced yet.
    Extension,
    /// A dispute raised on a milestone (`get_dispute_history`).
    Dispute,
    /// An engagement status transition (`get_status_history`).
    StatusChange,
}
/// Distinguishes the two business-logic types of milestones.
#[contracttype]
#[derive(Clone, PartialEq)]
pub enum MilestoneKind {
    /// Triggered by the recruiter placing a candidate (offer accepted).
    /// Starts `Pending` so the recruiter can submit proof immediately.
    Placement,
    /// Triggered by the candidate remaining employed past a time gate.
    /// Starts `Locked`; `unlock_milestone` moves it to `Pending` once
    /// `env.ledger().sequence() >= valid_after_ledger`.
    Retention,
}
/// Top-level lifecycle state of an engagement.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum EngagementStatus {
    /// The engagement is active and milestones can be submitted, confirmed,
    /// disputed, or otherwise progressed through the normal workflow.
    Active,

    /// All milestones have reached either `Confirmed` or `Resolved` and the
    /// full engagement fee has been released to the recruiter.
    ///
    /// Terminal state — no further state transitions are possible.
    Completed,

    /// The engagement was mutually cancelled by the company and recruiter
    /// before completion. Any unreleased escrow has been refunded to the
    /// company.
    ///
    /// Terminal state.
    Cancelled,

    /// The company requested a replacement candidate after a previously
    /// accepted placement. The placement milestone is reset to `Pending`
    /// while the recruiter searches for a replacement. Once new placement
    /// proof is submitted, the engagement returns to `Active`.
    ReplacementRequested,

    /// The recruiter has requested an early exit from the engagement.
    /// The company must either accept the request (which cancels the
    /// engagement and refunds any unreleased escrow) or reject it,
    /// returning the engagement to `Active`.
    ExitRequested,

    /// The engagement exceeded the configured inactivity timeout and was
    /// closed through `expire_engagement`. Any unreleased escrow was
    /// refunded to the company.
    ///
    /// Terminal state.
    Expired,
}
/// Discriminator for a single admin-configurable scalar setting, nested inside
/// `DataKey::Config` so each setting is still its own distinct storage key.
/// Grouped into one wrapper variant to keep `DataKey` itself under the
/// contract-spec union case limit of 50 (soroban_sdk's `ScSpecUdtUnionV0`).
#[contracttype]
#[derive(Clone)]
pub enum ConfigKey {
    /// Admin-configurable proof resubmission cooldown in ledgers (default 2 880).
    ProofCooldown,
    /// Admin-configurable ledgers-per-day constant (issue #41).
    LedgersPerDay,
    /// Admin-configurable confirm window in ledgers (default 86_400 — ~5 days).
    ConfirmWindow,
    /// Admin-configurable dispute window in ledgers (default 51_840 — ~3 days).
    DisputeWindow,
    /// Minimum engagement amount in stroops to prevent dust engagements (issue #17).
    MinEngagementAmount,
    /// Admin-configurable upgrade time-lock duration in ledgers (issue #69, default 17_280).
    UpgradeLockDuration,
    /// Admin-configurable max proof hash length in characters (issue #68, default 200).
    MaxProofHashLength,
    /// Arbiter fee in basis points (0–200, max 2%) deducted from payout on dispute approval (issue #52).
    ArbiterFee,
    /// Admin-configurable maximum simultaneous active engagements per company (default 50).
    MaxActivePerCompany,
    /// Admin-configurable maximum number of replacements allowed per engagement (issue #31, default 3).
    MaxReplacements,
    /// Admin-configured referral discount in basis points (issue #251).
    ReferralDiscountBps,
    /// Admin-configurable deadline, in ledgers, for the super-arbiter to
    /// resolve an escalated dispute before it auto-resolves in the
    /// recruiter's favor (issue #318, default `DEFAULT_SUPER_ARBITER_RESPONSE_WINDOW_LEDGERS`).
    SuperArbiterResponseWindow,
    /// Per-token minimum engagement amount overrides (issue #366), stored as a
    /// single `Map<Address, i128>` blob keyed by token SAC address. A token
    /// with no entry here falls back to the admin-wide `MinEngagementAmount`.
    /// Addresses this decimals-agnostic gap without a new `DataKey` variant
    /// per token (see the note on `add_allowed_token`, issue #175).
    TokenMinAmounts,
    /// Admin-configurable recruiter no-show deadline in ledgers (issue #465).
    /// `0` (the default) disables `trigger_no_show` entirely.
    NoShowDeadline,
    /// Trusted swap-adapter contract address for recruiter payout token
    /// conversion (issue #458).
    SwapAdapter,
    /// Rating-based proof cooldown discount curve (issue #470).
    ProofCooldownDiscount,
    /// Fraction of a recruiter bond forfeited on unresolved rejections,
    /// in basis points (issue #459). Default 10_000 (100 %).
    BondForfeitBps,
    /// TTL in ledgers for amendment / split-amendment proposals
    /// (default 17_280 ≈ 1 day).
    AmendmentTTL,
    /// Portion of each platform fee credited as company rebate (issue #475).
    FeeRebateBps,
    /// Emergency pause vote window in ledgers (issue #474).
    EmergencyVoteWindow,
    /// Minimum quorum-to-panel-size ratio, in basis points, enforced by
    /// `create_engagement` (issue #502). Default 0 (no minimum).
    MinQuorumRatioBps,
    /// Grace period in ledgers before the same proposer may re-propose on the
    /// same milestone after a rejection (issue #496, default 0 for no cooldown).
    AmendmentReproposalCooldown,
    /// Maximum dispute cycles per milestone (issue #481). Absent ⇒ unlimited.
    MaxDisputeCycles,
    /// Admin-wide cap on a recruiter's active engagements (issue #482).
    /// Absent or `0` ⇒ unlimited.
    MaxActivePerRecruiter,
}
/// Contract storage key space. Instance keys reset between transactions;
/// persistent keys survive across ledgers.
#[contracttype]
pub enum DataKey {
    /// Full engagement record stored by engagement_id (persistent).
    Engagement(String),
    /// Current admin address (instance).
    Admin,
    /// Pending arbiter succession nomination for an engagement.
    PendingArbiter(String),
    /// Platform fee configuration — basis points and treasury address (persistent).
    PlatformFee,
    /// Whether the contract is currently paused (persistent).
    Paused,
    /// Pending admin transfer nomination address (persistent).
    PendingAdmin,
    /// Proposed new recruiter address awaiting company acceptance (issue #44).
    ProposedRecruiterTransfer(String),
    /// Ledger at which the last proof was submitted for (engagement_id, milestone_index).
    LastProofAt(String, u32),
    /// Running vote tally for a disputed (engagement_id, milestone_index).
    ArbiterVotes(String, u32),
    /// Total number of engagements ever created (issue #34).
    EngagementCount,
    /// Per-company ordered list of engagement IDs (issue #35).
    CompanyEngagements(Address),
    /// Per-recruiter ordered list of engagement IDs (issue #36).
    RecruiterEngagements(Address),
    /// Allowlist of accepted token SAC addresses (issue #26).
    AllowedTokens,
    /// Whether the token allowlist is enabled (issue #26).
    AllowlistEnabled,
    /// Dispute reason string stored per (engagement_id, milestone_index) (issue #50).
    DisputeReason(String, u32),
    /// Structured reason code stored per (engagement_id, replacement_index)
    /// when a company calls `request_replacement` (issue #51). Indexed from 0.
    ReplacementReason(String, u32),
    /// Number of replacements ever requested for an engagement (issue #51).
    /// Acts as the next replacement_index when incremented.
    ReplacementCount(String),
    /// Contract version string (e.g. "0.2.0") for deployment verification (issue #16).
    Version,
    /// Pending contract WASM upgrade proposal (issue #69).
    PendingUpgrade,
    /// Set to true once admin has permanently renounced their role (issue #59).
    AdminRenounced,
    /// Per-company count of currently active (non-terminal) engagements.
    CompanyActiveCount(Address),
    /// Optional co-signer address authorized to perform company-gated actions (issue #254).
    CompanyCosigner(Address),
    /// Optional co-signer address authorized to perform recruiter-gated actions
    /// (issue #257, recruiter mirror of issue #254).
    RecruiterCosigner(Address),
    /// Fee tiers for tiered platform fee (issue #250).
    FeeTiers,
    /// Admin-configurable super-arbiter address for tie-breaking escalated
    /// disputes (issue #246).
    SuperArbiter,
    /// Ledger at which a dispute was raised for (engagement_id, milestone_index),
    /// used to determine when the dispute window has elapsed (issue #246).
    DisputeRaisedAt(String, u32),
    /// Whether a disputed (engagement_id, milestone_index) has been auto-escalated
    /// to the super arbiter (issue #246).
    EscalatedDispute(String, u32),
    /// Per-tag index mapping tag string to list of engagement IDs (issue #248, #249).
    TagEngagements(String),
    /// Set once a `milestone_due_soon` event has been emitted for
    /// (engagement_id, milestone_index), so the notification fires at most once
    /// per unlock deadline (issue #241).
    DueSoonNotified(String, u32),
    /// Whether a single engagement is quarantined by the admin (issue #239).
    /// Independent of the global `Paused` flag.
    EngagementPaused(String),
    /// Reason string supplied by the admin when quarantining an engagement
    /// via `pause_engagement` (issue #327). Overwritten on each re-pause;
    /// not cleared by `unpause_engagement`, so the last reason stays
    /// queryable as an audit trail. Absent for engagements never paused.
    EngagementPauseReason(String),
    /// Global ordered list of every engagement ID ever created (issue #237).
    /// Backs `get_engagement_ids_by_status`.
    AllEngagements,
    /// Admin-configured list of recognised referrer addresses (issue #251).
    Referrers,
    /// Wraps a `ConfigKey` so every admin-tunable scalar setting shares one
    /// `DataKey` variant instead of each needing its own. See `ConfigKey`.
    Config(ConfigKey),
    /// Ledger at which a disputed (engagement_id, milestone_index) was
    /// escalated to the super arbiter, used to measure the response
    /// deadline before it auto-resolves in the recruiter's favor (issue #318).
    EscalatedAt(String, u32),
    /// Total number of disputes concluded via the super-arbiter escalation
    /// path — either by an explicit `super_arbiter_resolve` call or by
    /// `resolve_escalation_timeout` (issue #317).
    SuperArbiterResolutionCount,
    /// Whether the platform fee has been waived (zeroed) for a specific
    /// engagement by the admin (issue #335). When `true`, every milestone
    /// payout on this engagement skips platform-fee collection entirely.
    FeeWaived(String),
    /// Ledger at which a Placement milestone last (re-)entered `Pending` for
    /// (engagement_id, milestone_index) after creation — set when a
    /// replacement resets it or a dispute rejects its proof (issue #465).
    /// Absent means it has been `Pending` since `created_at_ledger`.
    MilestonePendingSince(String, u32),
    /// Running total of milestone shares forfeited by `trigger_no_show` on an
    /// engagement and not yet refunded to the company (issue #465).
    NoShowForfeited(String),
    /// Vesting record for a streamed milestone payout on
    /// (engagement_id, milestone_index) (issue #466).
    StreamedPayout(String, u32),
    /// Admin-curated pool of arbiters that
    /// `create_engagement_with_random_arbiters` draws panels from (issue #467).
    ArbiterPool,
    /// Historical dispute-response record for an arbiter address (issue #468).
    ArbiterStats(Address),
    /// Nested storage keys that would otherwise push `DataKey` past the
    /// Soroban union case limit of 50. See [`ExtKey`].
    Ext(ExtKey),
}

/// Secondary storage-key space nested under [`DataKey::Ext`] so the primary
/// `DataKey` enum stays within Soroban's 50-variant union limit.
#[contracttype]
#[derive(Clone)]
pub enum ExtKey {
    /// Accumulated star ratings received by a recruiter (issue #470).
    RecruiterRating(Address),
    /// Set once a completed engagement has been rated (issue #470).
    EngagementRated(String),
    /// Pending dispute-window override proposal (issue #469).
    DisputeWindowProposal(String),
    /// Accepted per-engagement dispute-window override (issue #469).
    DisputeWindowOverride(String),
    /// Recruiter preferred payout token (issue #458).
    RecruiterPayoutToken(Address),
    /// Escrowed recruiter collateral bond (issue #459).
    RecruiterBond(String),
    /// Shared arbiter panel for an engagement bundle (issue #464).
    Bundle(String),
    /// Engagement IDs belonging to a bundle (issue #464).
    BundleEngagements(String),
    /// Split-vote tally for a disputed milestone (issue #462).
    ArbiterSplitVotes(String, u32),
    /// Whether split voting is enabled for an engagement (issue #462).
    SplitVotingEnabled(String),
    /// Milestone share withheld by split-vote resolutions (issue #462).
    SplitWithheld(String),
    /// Standing vote delegate for (engagement_id, arbiter) (issue #463).
    ArbiterVoteDelegate(String, Address),
}

/// Overflow storage key space kept under the Soroban 50-variant union limit
/// on [`DataKey`]. Newer features land here.
#[contracttype]
pub enum DataKey2 {
    /// Split-vote tally for a disputed (engagement_id, milestone_index) (issue #462).
    ArbiterSplitVotes(String, u32),
    /// Whether split voting is enabled for an engagement (issue #462).
    SplitVotingEnabled(String),
    /// Milestone share withheld by split-vote resolutions, pending refund (issue #462).
    SplitWithheld(String),
    /// Vote delegate for an arbiter slot on an engagement (issue #463).
    ArbiterVoteDelegate(String, Address),
    /// Recruiter's preferred payout token across engagements (issue #458).
    RecruiterPayoutToken(Address),
    /// Pending per-engagement dispute window override proposal (issue #469).
    DisputeWindowProposal(String),
    /// Accepted per-engagement dispute window override in ledgers (issue #469).
    DisputeWindowOverride(String),
    /// Set once a company has rated the recruiter on an engagement (issue #470).
    EngagementRated(String),
    /// Aggregated star ratings for a recruiter (issue #470).
    RecruiterRating(Address),
    /// Recruiter collateral bond for an engagement (issue #459).
    RecruiterBond(String),
    /// Shared arbiter panel registered under a bundle id (issue #464).
    Bundle(String),
    /// Engagement IDs created under a bundle, in creation order (issue #464).
    BundleEngagements(String),
    /// Admin-set recruiter verification flag (issue #476). Absent ⇒ false.
    RecruiterVerified(Address),
    /// Pending co-recruiter split amendment proposal (issue #471).
    SplitAmendmentProposal(String),
    /// Accepted co-recruiter split amendment history (issue #471).
    SplitAmendmentLog(String),
    /// Arbiters who have self-recused from a specific dispute (issue #477).
    RecusedArbiters(String, u32),
    /// Per-company, per-token pooled escrow balance (issue #472).
    CompanyBalance(Address, Address),
    /// Whether an engagement was funded from the company pool (issue #472).
    PoolFunded(String),
    /// Optional second cosigner for sensitive admin setters (issue #473).
    ConfigCosigner,
    /// Admin-selected sensitive setter function ids (issue #473).
    SensitiveFunctions,
    /// Pending cosigner-gated config change by change_id (issue #473).
    PendingConfigChange(u64),
    /// Monotonic counter for pending config change ids (issue #473).
    NextConfigChangeId,
    /// Emergency M-of-N signer set and threshold (issue #474).
    EmergencySigners,
    /// Emergency pause vote tally; empty string = global pause (issue #474).
    EmergencyVotes(String),
    /// Per-company, per-token redeemable fee rebate balance (issue #475).
    CompanyRebate(Address, Address),
    /// Who requested replacement `replacement_index` and at which ledger
    /// (issue #501); written alongside `DataKey::ReplacementReason`.
    ReplacementRecord(String, u32),
    /// Durable, FIFO-capped history of disputes raised on an engagement
    /// (issue #501). Unlike `DataKey::DisputeReason`, it is not cleared when
    /// the dispute resolves.
    DisputeHistory(String),
    /// FIFO-capped history of engagement status transitions (issue #501).
    StatusHistory(String),
    /// Platform-fee bps resolved from the fee tiers at creation, present only
    /// for engagements created with `snapshot_fee_tier` (issue #505).
    FeeTierSnapshot(String),
    /// Co-recruiter collateral bond for an engagement (issue #506).
    CoRecruiterBond(String),
    /// Number of disputes raised on (engagement_id, milestone_index) (issue #481).
    DisputeCycles(String, u32),
    /// Recruiter's self-imposed active engagement cap (issue #482).
    RecruiterActiveCap(Address),
    /// Super arbiter panel `(members, quorum)` (issue #483). Mutually
    /// exclusive with `DataKey::SuperArbiter`.
    SuperArbiterPanel,
    /// Panel votes on an escalated dispute: `(approvers, rejecters)` (issue #483).
    SuperArbiterVotes(String, u32),
    /// Merkle root committed by `submit_proof_root` for an
    /// (engagement_id, milestone_index) (issue #486). Present only while the
    /// milestone's current proof is a root; a plain `submit_proof` clears it.
    ProofMerkleRoot(String, u32),
}

