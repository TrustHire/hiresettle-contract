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
}
