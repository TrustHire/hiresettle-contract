//! # HireSettle Smart Contract
//!
//! ## Overview
//! The HireSettle smart contract provides escrow and settlement mechanisms for decentralized work
//! engagements. It manages the full lifecycle of milestone-based agreements, security retentions,
//! disputes, and protocol fee distributions.
//!
//! ## Major Subsystems
//! - **Escrow & Funding**: Handles locking user funds in contract storage during active engagements.
//! - **Milestones**: Tracks deliverable checkpoints, approvals, and payout releases.
//! - **Disputes & Arbitration**: Manages escalation paths, resolution voting, super-arbiters, and quorums.
//! - **Amendments**: Facilitates proposed modifications to live contract parameters and agreements.
//! - **Tags**: Enables metadata classification and custom key-value attributes for engagements.
//! - **Fees & Retention**: Calculates protocol commissions, retention holds, and fee distributions.
//!
//! ## Section Navigation
//! - `Data Types & Storage`: Core structs (`Engagement`, `Milestone`, `Dispute`), state keys, and enums.
//! - `Contract Initialization`: Setup administrative defaults, fee structures, and protocol parameters.
//! - `Core Lifecycle Functions`: Initializing engagements, funding escrow, approving, and releasing milestones.
//! - `Dispute Resolution`: Escalating deadlocks, casting arbiter votes, and executing resolutions.
//! - `Admin & Configuration`: Protocol parameter updates, fee withdrawals, and arbiter management.

#![no_std]
// `#[contractimpl]` expands `create_engagement`'s many required fields into a
// flat parameter list on the generated contract, client, and args types;
// bundling them into a struct would break the deployed ABI, so the lint is
// disabled crate-wide for the macro-generated bindings it triggers on.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, BytesN, Env, String, Symbol, Vec,
};

const MAX_PLATFORM_FEE_BPS: u32 = 500;
const MAX_ARBITER_FEE_BPS: u32 = 200;
const FULL_SPLIT_BPS: u32 = 10_000;

// ============================================================
// DATA TYPES
// ============================================================

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

/// A payment and workflow checkpoint belonging to an [`Engagement`]'s `milestones`.
#[contracttype]
#[derive(Clone)]
pub struct Milestone {
    /// Human-readable label shown in the UI (e.g. "30-day retention").
    pub name: String,
    /// Percentage of `total_amount` released when this milestone is confirmed.
    /// All milestone percentages across an engagement must sum to exactly 100.
    pub payment_percent: u32,
    /// Business-logic type of this milestone — see [`MilestoneKind`].
    /// Determines the starting status and whether a time gate applies.
    pub kind: MilestoneKind,
    /// Ledger sequence number at or after which a `Retention` milestone can be unlocked.
    /// Always `0` for `Placement` milestones (no time gate).
    pub valid_after_ledger: u32,
    /// IPFS CID (or any URI) submitted by the recruiter as proof.
    /// Empty string initially; populated by `submit_proof`; cleared back to empty
    /// when a dispute is rejected or a replacement is requested.
    pub proof_hash: String,
    /// Current lifecycle position of the milestone — see [`MilestoneStatus`].
    pub status: MilestoneStatus,
    /// Ledger at which the most recent proof was submitted; 0 if never submitted.
    pub proof_submitted_at: u32,
    /// Set by `request_replacement` to the gross amount already released for this
    /// (Placement) milestone when it is reset to `Pending` for a replacement
    /// candidate; `0` if never paid out. On re-confirmation, only the difference
    /// between the milestone's current share (which may have grown via
    /// `top_up_escrow`) and this already-paid amount is released, so escrow
    /// added after a replacement is still paid out instead of getting stuck in
    /// the contract. See issue #183.
    pub replacement_paid_out: i128,
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

/// A history entry for a milestone amendment
#[contracttype]
#[derive(Clone)]
pub struct AmendmentEntry {
    /// The party who proposed the amendment
    pub proposer: Address,
    /// Previous payment percentage
    pub old_payment_percent: u32,
    /// New payment percentage
    pub new_payment_percent: u32,
    /// Ledger when the amendment was accepted
    pub ledger: u32,
}

/// A pending amendment proposal for a milestone
#[contracttype]
#[derive(Clone)]
pub struct AmendmentProposal {
    /// The party who proposed the amendment
    pub proposer: Address,
    /// New payment percentage being proposed
    pub new_payment_percent: u32,
    /// Ledger when the proposal was made
    pub proposed_at_ledger: u32,
    /// Ledger at which the proposal expires if not accepted
    pub expires_at_ledger: u32,
}

/// Alias for the frontend-facing name used by `get_pending_amendment`.
pub type PendingAmendment = AmendmentProposal;

/// A pending milestone extension proposal, awaiting company approval (issue #247).
/// Recruiter-initiated: proposes adding `additional_ledgers` to a Locked retention
/// milestone's `valid_after_ledger`, pushing its unlock deadline further out.
#[contracttype]
#[derive(Clone)]
pub struct MilestoneExtensionProposal {
    /// The recruiter who proposed the extension.
    pub proposer: Address,
    /// Number of additional ledgers to add to `valid_after_ledger` on acceptance.
    pub additional_ledgers: u32,
    /// Ledger when the proposal was made.
    pub proposed_at_ledger: u32,
    /// Ledger at which the proposal expires if not accepted.
    pub expires_at_ledger: u32,
}

/// The full engagement record stored on-chain — note that `proof_submitted_at` on
/// each milestone is set by `submit_proof` and consumed by `force_confirm_milestone`.
#[contracttype]
#[derive(Clone)]
pub struct Engagement {
    /// Unique string identifier chosen by the caller at `create_engagement` time.
    pub id: String,
    /// The hiring company; only this address can confirm milestones, raise disputes, or cancel.
    pub company: Address,
    /// The recruitment agency or recruiter receiving milestone payments.
    pub recruiter: Address,
    /// Ordered list of arbiters; quorum of these must agree to resolve a dispute.
    pub arbiters: Vec<Address>,
    /// Number of arbiter votes required to resolve a dispute (M of N).
    pub quorum: u32,
    /// SAC address of the token held in escrow (e.g. USDC).
    pub token: Address,
    /// Total fee locked in escrow at creation, in the token's smallest unit.
    pub total_amount: i128,
    /// Cumulative amount already paid out across all confirmed/resolved milestones.
    /// `total_amount - released_amount` equals the remaining escrow balance.
    pub released_amount: i128,
    /// Free-text job title stored on-chain for display purposes.
    pub job_title: String,
    /// Optional IPFS CID linking to full job description / contract terms off-chain.
    pub metadata_hash: Option<String>,
    /// Ledger sequence number at which the engagement was created.
    pub created_at_ledger: u32,
    /// Ledger sequence number of the most recent state-changing call.
    /// Used by `expire_engagement` to detect inactivity.
    pub last_activity_ledger: u32,
    /// Ordered list of milestones; indices are stable and used throughout the contract API.
    pub milestones: Vec<Milestone>,
    /// Current lifecycle state of the engagement; drives all state-machine transitions.
    /// See [`EngagementStatus`] for the full list of variants and their semantics.
    pub status: EngagementStatus,
    /// Optional co-recruiter address for split-fee engagements (issue #56).
    /// When `Some`, the milestone payout is split between `recruiter` and `co_recruiter`
    /// according to `recruiter_split_bps`.
    pub co_recruiter: Option<Address>,
    /// Primary recruiter's share of the net payout in basis points (issue #56).
    /// Default is 10 000 (100 % to recruiter). Must be ≤ 10 000.
    pub recruiter_split_bps: u32,
    /// Optional off-chain attestation hash (e.g. SHA-256 of the contract PDF).
    /// Stored at engagement creation for audit and verification purposes.
    pub contract_pdf_hash: Option<String>,
    /// Optional referrer address set at creation time (issue #251).
    /// If present and recognised by the admin-configured referral list,
    /// a configurable fee discount is applied to every milestone payout.
    pub referrer: Option<Address>,
    /// Optional list of short string tags for categorization (issue #248, #249).
    pub tags: Option<Vec<String>>,
}

/// A lightweight read-only view of an engagement, suitable for list/dashboard APIs.
///
/// For full milestone detail, use `get_engagement`.
#[contracttype]
#[derive(Clone)]
pub struct EngagementSummary {
    /// Unique engagement identifier.
    pub id: String,
    /// Free-text job title stored at creation time.
    pub job_title: String,
    /// The hiring company address.
    pub company: Address,
    /// The recruiter address receiving milestone payments.
    pub recruiter: Address,
    /// Total fee locked in escrow at creation, in the token's smallest unit.
    pub total_amount: i128,
    /// Cumulative amount paid out across all confirmed/resolved milestones so far.
    ///
    /// Remaining escrow = `total_amount - released_amount`.
    pub released_amount: i128,
    /// Current lifecycle status of the engagement.
    pub status: EngagementStatus,
    /// Total number of milestones in the engagement (does not change after creation).
    pub milestone_count: u32,
    /// Ledger sequence number at which the engagement was created.
    pub created_at_ledger: u32,
    /// Optional co-recruiter address for split-fee engagements (issue #56).
    pub co_recruiter: Option<Address>,
    /// Primary recruiter's share of the net payout in basis points (issue #56).
    pub recruiter_split_bps: u32,
    /// Optional off-chain attestation hash (e.g. SHA-256 of the contract PDF).
    pub contract_pdf_hash: Option<String>,
    /// Optional referrer address (issue #251).
    pub referrer: Option<Address>,
    /// Optional list of short string tags for categorization (issue #248, #249).
    pub tags: Option<Vec<String>>,
}

/// Per-dispute, per-milestone vote tally stored on-chain until the dispute resolves.
/// Cleared once the dispute is resolved or rejected.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterVoteRecord {
    /// Number of arbiters who voted to approve payment to the recruiter.
    pub approve_votes: u32,
    /// Number of arbiters who voted to reject payment and return the milestone to `Pending`.
    pub reject_votes: u32,
    /// Addresses that have already cast a vote; prevents double-voting.
    pub voted: Vec<Address>,
}

/// Passed to `create_engagement` to configure the arbitration panel for an engagement.
///
/// Soroban's 10-parameter limit prevents passing these as individual args, so they
/// are bundled into this struct.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterSetup {
    /// Ordered list of arbiter addresses eligible to vote on disputes.
    /// Must contain at least one address. All addresses must be distinct.
    pub arbiters: Vec<Address>,
    /// Number of votes required to resolve a dispute (M-of-N).
    /// Must be ≥ 1 and ≤ `arbiters.len()`.
    pub quorum: u32,
}

/// Returned by `get_arbiter_votes`.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterVoteCounts {
    /// Number of arbiters who voted to approve (release) the milestone payment.
    /// Incremented in `cast_arbiter_vote` when `approve` is `true`.
    pub approve_votes: u32,
    /// Number of arbiters who voted to reject (withhold) the milestone payment.
    /// Incremented in `cast_arbiter_vote` when `approve` is `false`.
    pub reject_votes: u32,
}

/// Stored under `DataKey::PendingArbiter` during succession.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterNomination {
    /// The arbiter initiating the handover. Set when `nominate_arbiter_successor`
    /// is called; used by `claim_arbiter` to locate the correct slot in
    /// `engagement.arbiters` and replace it with the incoming nominee.
    pub current: Address,
    /// The address authorised to complete the succession by calling
    /// `claim_arbiter`. Only this exact address may claim the slot; any other
    /// caller is rejected with `"unauthorized"`.
    pub nominee: Address,
}

/// Pending contract WASM upgrade proposal stored until execution (issue #69).
#[contracttype]
#[derive(Clone)]
pub struct UpgradeProposal {
    /// The new WASM hash to apply on execute_upgrade.
    pub new_wasm_hash: BytesN<32>,
    /// Ledger sequence at or after which execute_upgrade may be called.
    pub execute_after_ledger: u32,
}

/// Platform fee configuration deducted from each milestone payment.
#[contracttype]
#[derive(Clone)]
pub struct PlatformFee {
    /// Fee in basis points (1 bp = 0.01%), capped at 500 (5%).
    pub bps: u32,
    /// Address that receives accumulated platform fees.
    pub treasury: Address,
}

/// A single fee-tier bracket: engagements whose `total_amount` is at or above
/// `threshold` pay `bps` instead of the default platform-fee rate.
/// Configured via `set_fee_tiers` (issue #250).
#[contracttype]
#[derive(Clone)]
pub struct FeeTier {
    /// Minimum engagement `total_amount` (inclusive) required to fall into this
    /// tier. Tiers are evaluated highest-`threshold` first, so an engagement is
    /// charged the `bps` of the first (highest) tier whose `threshold` it meets
    /// or exceeds; if it is below every tier's `threshold`, the contract-wide
    /// default platform fee applies instead.
    pub threshold: i128,
    /// Platform fee, in basis points (1 bp = 0.01%), charged on engagements that
    /// qualify for this tier (i.e. whose `total_amount` is at or above
    /// `threshold`). Replaces the default platform fee for those engagements.
    pub bps: u32,
}

/// Bundled optional configuration passed as the last argument of `create_engagement`.
/// Combines `metadata_hash` with the new co-recruiter split fields (issue #56)
/// to stay within Soroban's 10-parameter limit.
#[contracttype]
#[derive(Clone)]
pub struct EngagementConfig {
    /// Optional IPFS CID linking to full job description / contract terms off-chain.
    pub metadata_hash: Option<String>,
    /// Optional co-recruiter address that shares the milestone payout.
    pub co_recruiter: Option<Address>,
    /// Primary recruiter's share in basis points (10 000 = 100 %).
    /// If `co_recruiter` is `None` this field is ignored and the full payout goes to `recruiter`.
    /// Must be ≤ 10 000.
    pub recruiter_split_bps: u32,
    /// Optional off-chain attestation hash (e.g. SHA-256 of the contract PDF).
    /// Must be non-empty if provided.
    pub contract_pdf_hash: Option<String>,
    /// Optional referrer address (issue #251). If present and in the
    /// admin-configured referral list, the engagement receives a fee discount.
    pub referrer: Option<Address>,
    /// Optional list of short string tags for off-chain categorization (issue #248, #249).
    pub tags: Option<Vec<String>>,
}

/// A single engagement configuration inside a `batch_create_engagements` call.
/// Bundles every `create_engagement` parameter so a caller can submit many
/// engagements — each with its own company, recruiter, terms and milestones —
/// in one contract invocation (issue #260).
#[contracttype]
#[derive(Clone)]
pub struct BatchEngagementConfig {
    /// Unique string identifier for this engagement — see `create_engagement`.
    pub engagement_id: String,
    /// The hiring company for this engagement. Must `require_auth` like any
    /// direct `create_engagement` call — batching does not relax authorization.
    pub company: Address,
    /// The recruiter receiving milestone payments for this engagement.
    pub recruiter: Address,
    /// Arbitration panel configuration — see `ArbiterSetup`.
    pub arbiter_setup: ArbiterSetup,
    /// SAC address of the escrow token for this engagement.
    pub token: Address,
    /// Total fee locked in escrow for this engagement, in the token's smallest unit.
    pub total_amount: i128,
    /// Short job title string for this engagement.
    pub job_title: String,
    /// Ordered milestone list for this engagement.
    pub milestones: Vec<Milestone>,
    /// Retention windows (in days), one per Retention milestone.
    pub retention_days: Vec<u32>,
    /// Bundled optional configuration — see `EngagementConfig`.
    pub config: EngagementConfig,
}

/// Returned by `get_contract_health` for quick off-chain diagnostics (issue #256).
#[contracttype]
#[derive(Clone)]
pub struct ContractHealth {
    /// Whether the pause switch is engaged. While `true`, every entry point
    /// guarded by `assert_not_paused` rejects. Reports `false` when the flag
    /// has never been set.
    pub paused: bool,
    /// Address currently allowed to call the admin-gated setters. Set by
    /// `init` and replaced only when a nominee claims the role, so it never
    /// reports a pending nomination.
    pub admin: Address,
    /// Contract version string, as set by `set_version`. Falls back to
    /// `DEFAULT_VERSION` when no version has been stored, so an absent value
    /// is indistinguishable from one explicitly set to the default.
    pub version: String,
    /// Number of engagements ever created (issue #34). Monotonic: it counts
    /// creations, not live engagements, so cancelled and completed ones stay
    /// included and the value never decreases.
    pub total_engagement_count: u64,
}

/// Running tally of ratings received by a recruiter or company, keyed by
/// address (issue #244). Stored as sum + count rather than a running average
/// to keep the update arithmetic exact and avoid precision drift from
/// repeatedly re-averaging.
#[contracttype]
#[derive(Clone)]
pub struct RatingRecord {
    /// Sum of every rating received. Each rating is 1–5, so this cannot
    /// realistically overflow `u32`.
    pub total_score: u32,
    /// Number of ratings received.
    pub count: u32,
}

/// Read-only reputation summary returned by `get_recruiter_rating` and
/// `get_company_rating` (issue #244).
#[contracttype]
#[derive(Clone)]
pub struct RatingSummary {
    /// Average rating scaled by 100 to keep two decimal places without
    /// floating point — e.g. `425` means 4.25 stars. `0` when `count` is 0.
    pub average_x100: u32,
    /// Number of ratings received. `0` for an address that has never been rated.
    pub count: u32,
    /// Sum of every rating received, so callers can re-derive the average at a
    /// different precision or merge tallies off-chain.
    pub total_score: u32,
}

/// Full point-in-time snapshot of every admin-configurable parameter,
/// returned by `get_config_snapshot` (issue #240) so off-chain callers don't
/// need to call each individual getter (`get_platform_fee`, `get_arbiter_fee`,
/// `get_confirm_window`, …) to reconstruct the current configuration, and can
/// tell the defaults applied when a parameter has never been explicitly set.
#[contracttype]
#[derive(Clone)]
pub struct ConfigSnapshot {
    /// Contract version string — see `get_version`.
    pub version: String,
    /// Current admin address — see `get_admin`.
    pub admin: Address,
    /// `true` once the admin has permanently renounced the role (issue #59).
    /// When set, every admin-gated setter panics with `"NoAdmin"`.
    pub admin_renounced: bool,
    /// Whether the contract is globally paused — see `is_paused`.
    pub paused: bool,
    /// Platform fee in basis points — see `get_platform_fee`.
    pub platform_fee_bps: u32,
    /// Treasury receiving platform fees — see `get_platform_fee`.
    pub platform_fee_treasury: Address,
    /// Arbiter fee in basis points (issue #52) — see `get_arbiter_fee`.
    pub arbiter_fee_bps: u32,
    /// Configured super-arbiter for escalated disputes (issue #246), if any.
    pub super_arbiter: Option<Address>,
    /// Confirm window in ledgers — see `get_confirm_window`.
    pub confirm_window_ledgers: u32,
    /// Dispute window in ledgers — see `get_dispute_window`.
    pub dispute_window_ledgers: u32,
    /// Proof resubmission cooldown in ledgers — see `set_proof_cooldown`.
    pub proof_cooldown_ledgers: u32,
    /// Due-soon notification window in ledgers (issue #241) —
    /// see `get_due_soon_window`.
    pub due_soon_window_ledgers: u32,
    /// Amendment proposal TTL in ledgers — see `get_amendment_ttl`.
    pub amendment_ttl_ledgers: u32,
    /// Milestone extension proposal TTL in ledgers (issue #247) —
    /// see `get_extension_ttl`.
    pub extension_ttl_ledgers: u32,
    /// Inactivity timeout in ledgers (issue #38) —
    /// see `get_inactivity_timeout_ledgers`.
    pub inactivity_timeout_ledgers: u32,
    /// Storage TTL extension target in ledgers (issue #40) —
    /// see `get_storage_ttl_extend_to`.
    pub storage_ttl_extend_to: u32,
    /// Upgrade time-lock duration in ledgers (issue #69) —
    /// see `get_upgrade_lock_duration`.
    pub upgrade_lock_duration_ledgers: u32,
    /// Ledgers-per-day constant (issue #41) — see `get_ledgers_per_day`.
    pub ledgers_per_day: u32,
    /// Maximum milestone count per engagement (issue #21) —
    /// see `get_max_milestones`.
    pub max_milestones: u32,
    /// Maximum retention window in days (issue #18) —
    /// see `get_max_retention_days`.
    pub max_retention_days: u32,
    /// Maximum replacements per engagement (issue #31) —
    /// see `get_max_replacements`.
    pub max_replacements: u32,
    /// Maximum simultaneously active engagements per company —
    /// see `get_max_active_per_company`.
    pub max_active_per_company: u32,
    /// Maximum proof hash length in characters (issue #68) —
    /// see `get_max_proof_hash_length`.
    pub max_proof_hash_length: u32,
    /// Minimum engagement amount in the token's smallest unit (issue #17) —
    /// see `get_min_amount`.
    pub min_engagement_amount: i128,
    /// Whether the token allowlist is being enforced (issue #26).
    pub token_allowlist_enabled: bool,
}

/// Reserved escrow lifecycle checkpoint action for future yield strategy
/// integrations. Emitted only when the admin explicitly enables callback
/// checkpoints and sets a callback target address.
#[contracttype]
#[derive(Clone)]
pub enum EscrowLifecycleAction {
    /// Escrow balance increased when an engagement was initially funded.
    Funded,
    /// Escrow balance increased from a later top-up.
    ToppedUp,
    /// Escrow balance decreased to pay milestone proceeds.
    PayoutReleased,
    /// Escrow balance decreased due to a refund back to company.
    Refunded,
}

// ============================================================
// STORAGE KEYS
// ============================================================

/// Discriminator for a single admin-configurable scalar setting, nested inside
/// `DataKey::Config` so each setting is still its own distinct storage key.
/// Grouped into one wrapper variant to keep `DataKey` itself under the
/// contract-spec union case limit of 50 (soroban_sdk's `ScSpecUdtUnionV0`).
#[contracttype]
#[derive(Clone)]
pub enum ConfigKey {
    /// Admin-configurable proof resubmission cooldown in ledgers (default 2 880).
    ProofCooldown,
    /// Admin-configurable TTL extension for amendment proposals (persistent).
    AmendmentTTL,
    /// Admin-configurable ledgers-per-day constant (issue #41).
    LedgersPerDay,
    /// Admin-configurable max retention days cap (issue #18).
    MaxRetentionDays,
    /// Admin-configurable inactivity timeout in ledgers (issue #38).
    InactivityTimeoutLedgers,
    /// Admin-configurable storage TTL extension in ledgers (issue #40).
    StorageTtlExtendTo,
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
    /// Admin-configurable max milestone count (issue #21, default 10).
    MaxMilestones,
    /// Arbiter fee in basis points (0–200, max 2%) deducted from payout on dispute approval (issue #52).
    ArbiterFee,
    /// Admin-configurable maximum simultaneous active engagements per company (default 50).
    MaxActivePerCompany,
    /// Admin-configurable maximum number of replacements allowed per engagement (issue #31, default 3).
    MaxReplacements,
    /// Admin-configurable TTL extension for milestone extension proposals (issue #247).
    MilestoneExtensionTTL,
    /// Admin-configurable due-soon notification window in ledgers (issue #241,
    /// default 17_280 — ~1 day).
    DueSoonWindow,
    /// Admin-configured referral discount in basis points (issue #251).
    ReferralDiscountBps,
    /// Admin-configurable deadline, in ledgers, for the super-arbiter to
    /// resolve an escalated dispute before it auto-resolves in the
    /// recruiter's favor (issue #318, default `DEFAULT_SUPER_ARBITER_RESPONSE_WINDOW_LEDGERS`).
    SuperArbiterResponseWindow,
    /// Admin-configurable maximum number of extensions grantable per milestone
    /// (issue #323, default `DEFAULT_MAX_MILESTONE_EXTENSIONS`).
    MaxMilestoneExtensions,
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
    /// Active amendment proposal for an engagement milestone (persistent).
    AmendmentProposal(String, u32),
    /// Amendment log entries for an engagement milestone (persistent).
    AmendmentLog(String, u32),
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
    /// Legacy tag-index variant. Engagements are now indexed under
    /// `TagEngagements`; this variant is retained only so the `DataKey`
    /// discriminant layout is unchanged for already-deployed storage.
    #[allow(dead_code)]
    EngagementTag(String),
    /// Optional co-signer address authorized to perform company-gated actions (issue #254).
    CompanyCosigner(Address),
    /// Optional co-signer address authorized to perform recruiter-gated actions
    /// (issue #257, recruiter mirror of issue #254).
    RecruiterCosigner(Address),
    /// Admin-gated switch for escrow lifecycle callback checkpoints.
    /// Defaults to `false` (no-op).
    EscrowCallbackEnabled,
    /// Reserved callback target address for future yield-strategy integration.
    EscrowCallbackTarget,
    /// Fee tiers for tiered platform fee (issue #250).
    FeeTiers,
    /// Whether a recruiter has been rated for this engagement (issue #242).
    RecruiterRated(String),
    /// Whether a company has been rated for this engagement (issue #243).
    CompanyRated(String),
    /// Running rating tally for a recruiter address (issue #244).
    RecruiterRating(Address),
    /// Running rating tally for a company address (issue #244).
    CompanyRating(Address),
    /// Active milestone extension proposal for a Locked retention milestone,
    /// awaiting company approval (issue #247).
    MilestoneExtensionProposal(String, u32),
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
    /// Ledger sequence at which a milestone's auto-confirm becomes executable
    /// via `execute_scheduled_auto_confirm`, keyed by (engagement_id, milestone_index).
    ScheduledAutoConfirm(String, u32),
    /// Number of extensions already granted for (engagement_id, milestone_index)
    /// (issue #323), capped by `ConfigKey::MaxMilestoneExtensions`.
    MilestoneExtensionCount(String, u32),
    /// Global registry of every distinct tag ever used across engagements
    /// (issue #321). Backs tag discovery queries.
    AllTags,
}

// ============================================================
// CONTRACT
// ============================================================

/// Milestone-based recruiter fee escrow contract.
#[contract]
pub struct HireSettleContract;

const LEDGERS_PER_DAY: u32 = 17_280; // 86 400s ÷ 5s per ledger
const DEFAULT_PROOF_COOLDOWN: u32 = 2_880; // ~4 hours
const DEFAULT_MAX_RETENTION_DAYS: u32 = 365;
const DEFAULT_MAX_MILESTONES: u32 = 10;
const DEFAULT_INACTIVITY_TIMEOUT_LEDGERS: u32 = 1_036_800; // ~60 days
const DEFAULT_STORAGE_TTL_EXTEND_TO: u32 = 1_036_800; // ~60 days
const DEFAULT_VERSION: &str = "0.2.0";
/// Maximum length (in characters) of a `request_replacement` reason string
/// (issue #51). Mirrors the dispute-reason cap to keep storage bounded.
const MAX_REPLACEMENT_REASON_LEN: u32 = 128;
const DEFAULT_MIN_ENGAGEMENT_AMOUNT: i128 = 100_000; // 0.01 USDC
const DEFAULT_CONFIRM_WINDOW_LEDGERS: u32 = 86_400; // ~5 days
const DEFAULT_DISPUTE_WINDOW_LEDGERS: u32 = 51_840; // ~3 days
const MAX_VERSION_LENGTH: u32 = 32;
const MAX_PROOF_HASH_LENGTH: u32 = 200;
const MAX_ENGAGEMENT_ID_LENGTH: u32 = 64;
const DEFAULT_MAX_ACTIVE_PER_COMPANY: u32 = 50;
/// Default maximum number of replacements allowed per engagement (issue #31).
const DEFAULT_MAX_REPLACEMENTS: u32 = 3;
/// Default TTL, in ledgers, for a pending milestone extension proposal (issue #247).
/// Mirrors `AmendmentTTL`'s default.
const DEFAULT_EXTENSION_TTL: u32 = 17_280;
/// Default due-soon notification window in ledgers (issue #241): a Retention
/// milestone becomes "due soon" once it is within this many ledgers of its
/// `valid_after_ledger`. ~1 day at 5 s/ledger.
const DEFAULT_DUE_SOON_WINDOW_LEDGERS: u32 = 17_280;
/// Maximum number of engagement configs accepted by a single
/// `batch_create_engagements` call (issue #34 batch variant).
const MAX_BATCH_CREATE_ENGAGEMENTS: u32 = 20;
/// Default deadline, in ledgers, for the super-arbiter to resolve an
/// escalated dispute before `resolve_escalation_timeout` can auto-favor the
/// recruiter (issue #318). Mirrors the default dispute window (~3 days).
const DEFAULT_SUPER_ARBITER_RESPONSE_WINDOW_LEDGERS: u32 = 51_840;
/// Maximum number of tags stored on an engagement (issue #248).
const MAX_TAGS: u32 = 10;
/// Maximum length, in characters, of a single engagement tag (issue #248).
const MAX_TAG_LENGTH: u32 = 32;
/// Default maximum number of milestone extensions allowed per milestone
/// (issue #323).
const DEFAULT_MAX_MILESTONE_EXTENSIONS: u32 = 3;

/// Shared panic message constants for the most-repeated error strings
/// (issue #171). Keeping these as constants means a typo can't silently
/// create an inconsistent error surface for off-chain consumers matching
/// on these strings.
const ERR_UNAUTHORIZED: &str = "unauthorized";
const ERR_ENGAGEMENT_NOT_ACTIVE: &str = "engagement is not active";
const ERR_INVALID_MILESTONE_INDEX: &str = "invalid milestone index";
/// Raised when an operation targets an engagement the admin has quarantined
/// via `pause_engagement` (issue #239). Distinct from `"ContractPaused"` so
/// off-chain callers can tell a single-engagement freeze from a global halt.
const ERR_ENGAGEMENT_PAUSED: &str = "EngagementPaused";

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    /// Initializes the HireSettle contract.
    ///
    /// # Caller
    /// Called by the contract deployer or initial administrator (`admin`). Requires authentication from `admin`.
    ///
    /// # Initialized State
    /// Sets up default contract storage values:
    /// - `DataKey::Admin`: Set to `admin`
    /// - `DataKey::Paused`: Set to `false`
    /// - `DataKey::PlatformFee`: Set to 0 bps with treasury `admin`
    /// - `DataKey::Version`: Set to `DEFAULT_VERSION` ("0.2.0")
    /// - `DataKey::Config(ConfigKey::MinEngagementAmount)`: Set to `DEFAULT_MIN_ENGAGEMENT_AMOUNT` (100,000 stroops)
    ///
    /// # One-Time-Only / Calling Twice
    /// Note: No already-initialized guard is currently present. If invoked again, it will overwrite
    /// all initialized storage fields provided `admin.require_auth()` succeeds.
    ///
    /// # Panics
    /// Panics if authentication from `admin` (`admin.require_auth()`) fails.
    pub fn init(env: Env, admin: Address) {
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().persistent().set(&DataKey::Paused, &false);
        env.storage().persistent().set(
            &DataKey::PlatformFee,
            &PlatformFee {
                bps: 0,
                treasury: admin,
            },
        );

        // Issue #16: Initialize contract version
        env.storage()
            .persistent()
            .set(&DataKey::Version, &String::from_str(&env, DEFAULT_VERSION));

        // Issue #17: Initialize minimum engagement amount
        env.storage().persistent().set(
            &DataKey::Config(ConfigKey::MinEngagementAmount),
            &DEFAULT_MIN_ENGAGEMENT_AMOUNT,
        );
    }

    // ----------------------------------------------------------
    // ADMIN CONFIGURATION
    // ----------------------------------------------------------

    /// Set the platform fee in basis points and the treasury that receives it.
    /// `bps` is capped at 500 (5%).
    pub fn set_platform_fee(env: Env, admin: Address, bps: u32, treasury: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        if bps > MAX_PLATFORM_FEE_BPS {
            panic!("FeeTooHigh");
        }

        env.storage().persistent().set(
            &DataKey::PlatformFee,
            &PlatformFee {
                bps,
                treasury: treasury.clone(),
            },
        );

        env.events()
            .publish((Symbol::new(&env, "platform_fee_set"),), (bps, treasury));
    }

    /// Return the current platform fee configuration.
    pub fn get_platform_fee(env: Env) -> (u32, Address) {
        let fee = Self::get_platform_fee_internal(&env);
        (fee.bps, fee.treasury)
    }

    /// Admin adds a referrer address to the recognised referral list (issue #251).
    pub fn add_referrer(env: Env, admin: Address, referrer: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        let mut list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Referrers)
            .unwrap_or_else(|| Vec::new(&env));

        // Prevent duplicates.
        for i in 0..list.len() {
            if list.get(i).unwrap() == referrer {
                panic!("referrer already exists");
            }
        }
        list.push_back(referrer.clone());
        env.storage().persistent().set(&DataKey::Referrers, &list);
        env.events()
            .publish((Symbol::new(&env, "referrer_added"),), referrer);
    }

    /// Admin removes a referrer address from the recognised referral list.
    pub fn remove_referrer(env: Env, admin: Address, referrer: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        let list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Referrers)
            .unwrap_or_else(|| Vec::new(&env));

        let mut new_list: Vec<Address> = Vec::new(&env);
        let mut found = false;
        for i in 0..list.len() {
            let addr = list.get(i).unwrap();
            if addr == referrer {
                found = true;
            } else {
                new_list.push_back(addr);
            }
        }
        if !found {
            panic!("referrer not found");
        }
        env.storage()
            .persistent()
            .set(&DataKey::Referrers, &new_list);
        env.events()
            .publish((Symbol::new(&env, "referrer_removed"),), referrer);
    }

    /// Return the list of recognised referrer addresses.
    pub fn get_referrers(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Referrers)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Admin sets the referral discount in basis points (issue #251).
    /// When a recognised referrer is attached to an engagement, the platform
    /// fee is reduced by this amount (but never below 0).
    /// Maximum 500 bps (same as max platform fee).
    pub fn set_referral_discount_bps(env: Env, admin: Address, bps: u32) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);
        if bps > MAX_PLATFORM_FEE_BPS {
            panic!("discount too high");
        }
        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::ReferralDiscountBps), &bps);
        env.events()
            .publish((Symbol::new(&env, "referral_discount_set"),), bps);
    }

    /// Return the current referral discount in basis points (default 0).
    pub fn get_referral_discount_bps(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::ReferralDiscountBps))
            .unwrap_or(0u32)
    }

    /// Return the discount, in basis points, currently applied for a given
    /// referrer (issue #312): the admin-configured discount if `referrer` is
    /// on the recognised referral list, or 0 if it is not (or the list is
    /// empty). Companion query to `get_referral_discount_bps`, which returns
    /// the configured rate without regard to any specific referrer.
    pub fn get_referrer_discount_bps(env: Env, referrer: Address) -> u32 {
        if !Self::is_recognised_referrer(&env, &referrer) {
            return 0;
        }
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::ReferralDiscountBps))
            .unwrap_or(0u32)
    }

    /// Admin sets fee tiers that scale the platform fee down for larger
    /// engagements (issue #250). Each tier specifies a `threshold`
    /// (minimum `total_amount`) and the `bps` rate that applies. Tiers
    /// must be sorted by ascending threshold, each `bps` must be ≤ the
    /// base platform fee, and at most 10 tiers are allowed.
    ///
    /// At fee-calculation time the contract walks the tiers from highest
    /// threshold to lowest and uses the first matching tier's `bps`.
    /// If no tier matches, the base `platform_fee.bps` applies.
    ///
    /// Pass an empty vector to clear all tiers (flat fee for every size).
    pub fn set_fee_tiers(env: Env, admin: Address, tiers: Vec<FeeTier>) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        if tiers.len() > 10 {
            panic!("too many fee tiers");
        }

        let base_bps = Self::get_platform_fee_internal(&env).bps;
        for i in 0..tiers.len() {
            let t = tiers.get(i).unwrap();
            if t.bps > base_bps {
                panic!("tier bps exceeds base platform fee");
            }
            if t.threshold <= 0 {
                panic!("tier threshold must be positive");
            }
            if i > 0 {
                let prev = tiers.get(i - 1).unwrap();
                if t.threshold <= prev.threshold {
                    panic!("tiers must be sorted by ascending threshold");
                }
            }
        }

        env.storage().persistent().set(&DataKey::FeeTiers, &tiers);
        env.events()
            .publish((Symbol::new(&env, "fee_tiers_set"),), tiers.len());
    }

    /// Return the current fee tiers. Empty vector means no tiering (flat fee).
    pub fn get_fee_tiers(env: Env) -> Vec<FeeTier> {
        env.storage()
            .persistent()
            .get(&DataKey::FeeTiers)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Admin sets the contract version string (issue #16).
    /// `version` must be ≤ 32 characters.
    /// Panics with "VersionTooLong" if version exceeds 32 chars.
    /// Panics with "unauthorized" if caller is not admin.
    pub fn set_version(env: Env, admin: Address, version: String) {
        Self::assert_admin(&env, &admin);

        if version.len() > MAX_VERSION_LENGTH {
            panic!("VersionTooLong");
        }

        env.storage().persistent().set(&DataKey::Version, &version);
        env.events()
            .publish((Symbol::new(&env, "version_set"),), version);
    }

    /// Admin sets the minimum engagement amount, in the raw smallest unit of
    /// whichever token a given `create_engagement` call uses (issue #17). This
    /// single global floor applies to every allowlisted token regardless of its
    /// `decimals()` — the contract is token-decimals-agnostic (see issue #175).
    /// If the allowlist mixes tokens of very different precision (e.g. a
    /// 7-decimal token alongside an 18-decimal token), the admin is responsible
    /// for picking a value that is a sane floor for all of them, or for keeping
    /// the allowlist restricted to tokens of comparable precision.
    /// Panics with "unauthorized" if caller is not admin.
    pub fn set_min_amount(env: Env, admin: Address, amount: i128) {
        Self::assert_admin(&env, &admin);

        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::MinEngagementAmount), &amount);
        env.events()
            .publish((Symbol::new(&env, "min_amount_set"),), amount);
    }

    /// Pause state-changing contract operations.
    pub fn pause(env: Env, admin: Address) {
        Self::assert_admin(&env, &admin);
        env.storage().persistent().set(&DataKey::Paused, &true);
        env.events().publish((Symbol::new(&env, "paused"),), admin);
    }

    /// Resume state-changing contract operations.
    pub fn unpause(env: Env, admin: Address) {
        Self::assert_admin(&env, &admin);
        env.storage().persistent().set(&DataKey::Paused, &false);
        env.events()
            .publish((Symbol::new(&env, "unpaused"),), admin);
    }

    /// Return true if the contract is currently paused.
    pub fn is_paused(env: Env) -> bool {
        Self::is_paused_internal(&env)
    }

    // ----------------------------------------------------------
    // ISSUE #239 — PER-ENGAGEMENT PAUSE (QUARANTINE)
    // ----------------------------------------------------------

    /// Quarantine a single engagement, blocking its state-changing operations
    /// without halting the rest of the contract (issue #239).
    ///
    /// # Caller
    /// `admin` — must be the current contract admin.
    ///
    /// # Scope
    /// While quarantined, every engagement-lifecycle call for this ID is
    /// rejected with `"EngagementPaused"`: milestone unlock / proof submission /
    /// confirmation (including batch and force-confirm), disputes and arbiter
    /// voting, escalation, replacements, amendments, milestone extensions,
    /// role transfers, escrow top-ups, early exit, cancellation, and expiry.
    ///
    /// Deliberately still permitted:
    /// - **Read-only queries** — quarantine must not blind indexers or the
    ///   parties to the engagement's state.
    /// - **`admin_replace_arbiter`** — quarantine is frequently *because* the
    ///   arbiter panel is the problem; the admin keeps the tool to fix it.
    /// - **`pause_engagement` / `unpause_engagement`** themselves, so the admin
    ///   can always lift the freeze.
    ///
    /// This is orthogonal to the global [`Self::pause`]: an engagement can be
    /// quarantined while the contract runs normally, and unpausing the contract
    /// does not lift a per-engagement quarantine. Both guards must pass for a
    /// call to proceed.
    ///
    /// Pausing an already-paused engagement is a no-op that still emits the
    /// event, so the admin can re-assert quarantine idempotently.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the contract admin.
    /// - `"engagement not found"` — no engagement exists with this ID.
    ///
    /// # Events
    /// Emits `("engagement_paused", engagement_id)` with the acting admin.
    pub fn pause_engagement(env: Env, admin: Address, engagement_id: String) {
        Self::assert_admin(&env, &admin);

        // Reject unknown IDs so a typo cannot silently create a quarantine
        // record that later blocks a legitimately created engagement.
        let _ = Self::get_engagement_internal(&env, &engagement_id);

        env.storage()
            .persistent()
            .set(&DataKey::EngagementPaused(engagement_id.clone()), &true);
        env.storage().persistent().extend_ttl(
            &DataKey::EngagementPaused(engagement_id.clone()),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "engagement_paused"),
                engagement_id.clone(),
            ),
            admin,
        );
    }

    /// Lift the quarantine on a single engagement (issue #239), returning it to
    /// normal operation. Unpausing an engagement that is not paused is a no-op
    /// that still emits the event.
    ///
    /// Note this has no bearing on the global pause — if the contract itself is
    /// paused, the engagement stays blocked by `"ContractPaused"`.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the contract admin.
    /// - `"engagement not found"` — no engagement exists with this ID.
    ///
    /// # Events
    /// Emits `("engagement_unpaused", engagement_id)` with the acting admin.
    pub fn unpause_engagement(env: Env, admin: Address, engagement_id: String) {
        Self::assert_admin(&env, &admin);

        let _ = Self::get_engagement_internal(&env, &engagement_id);

        env.storage()
            .persistent()
            .remove(&DataKey::EngagementPaused(engagement_id.clone()));

        env.events().publish(
            (
                Symbol::new(&env, "engagement_unpaused"),
                engagement_id.clone(),
            ),
            admin,
        );
    }

    /// Return `true` if this specific engagement is quarantined (issue #239).
    ///
    /// Independent of [`Self::is_paused`] — check both to know whether a
    /// state-changing call will be accepted. Read-only and permissionless;
    /// unknown engagement IDs return `false` rather than panicking, so callers
    /// can probe without a prior existence check.
    pub fn is_engagement_paused(env: Env, engagement_id: String) -> bool {
        Self::is_engagement_paused_internal(&env, &engagement_id)
    }

    /// Nominate a new admin. The nominee must call `claim_admin` to complete rotation.
    pub fn nominate_admin(env: Env, current_admin: Address, new_admin: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &current_admin);

        env.storage()
            .persistent()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.events()
            .publish((Symbol::new(&env, "admin_nominated"),), new_admin);
    }

    /// Claim admin rights after being nominated by the current admin.
    pub fn claim_admin(env: Env, nominee: Address) {
        Self::assert_not_paused(&env);
        nominee.require_auth();

        let pending: Address = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic!("no pending admin nomination"));

        if nominee != pending {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        env.storage().instance().set(&DataKey::Admin, &nominee);
        env.storage().persistent().remove(&DataKey::PendingAdmin);
        env.events()
            .publish((Symbol::new(&env, "admin_claimed"),), nominee);
    }

    /// Return the pending admin nominee, if one exists.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::PendingAdmin)
    }

    /// Return the current contract admin.
    pub fn get_admin(env: Env) -> Address {
        Self::get_admin_internal(&env)
    }

    /// Return a single diagnostic snapshot of the contract's health (issue #256).
    /// Returns paused state, admin, version, and total engagement count in one call.
    ///
    /// # Events
    /// Emits `("contract_health_snapshot",)` with the returned `ContractHealth`
    /// as data, so off-chain keepers polling this view can be observed and
    /// indexed on-chain rather than only inferred from RPC traffic (issue #316).
    pub fn get_contract_health(env: Env) -> ContractHealth {
        let health = ContractHealth {
            paused: Self::is_paused_internal(&env),
            admin: Self::get_admin_internal(&env),
            version: env
                .storage()
                .persistent()
                .get(&DataKey::Version)
                .unwrap_or_else(|| String::from_str(&env, DEFAULT_VERSION)),
            total_engagement_count: env
                .storage()
                .instance()
                .get(&DataKey::EngagementCount)
                .unwrap_or(0u64),
        };

        env.events().publish(
            (Symbol::new(&env, "contract_health_snapshot"),),
            health.clone(),
        );

        health
    }

    // ----------------------------------------------------------
    // ADMIN CONFIG
    // ----------------------------------------------------------

    /// Set the minimum ledger gap between successive proof submissions on the
    /// same milestone. Only callable by the admin set during `init`.
    pub fn set_proof_cooldown(env: Env, admin: Address, ledgers: u32) {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("contract not initialised"));
        if admin != stored_admin {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::ProofCooldown), &ledgers);
    }

    fn get_proof_cooldown(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ProofCooldown))
            .unwrap_or(DEFAULT_PROOF_COOLDOWN)
    }

    // ----------------------------------------------------------
    // CREATE ENGAGEMENT
    // ----------------------------------------------------------

    /// Create a new recruitment engagement and lock funds in escrow.
    ///
    /// # Arguments
    /// - `engagement_id`   — unique string ID for this engagement
    /// - `company`         — company address (must sign this tx)
    /// - `recruiter`       — recruiter address (receives payments)
    /// - `arbiters`        — ordered list of arbiter addresses (min 1)
    /// - `quorum`          — number of arbiter approvals required to release on dispute (M of N)
    /// - `token`           — SAC address of the escrow token (USDC or any allowlisted token,
    ///   see [`Self::add_allowed_token`]). `total_amount` and all amount-based math below are
    ///   raw integer units in this token's smallest denomination (e.g. stroops for a 7-decimal
    ///   token) — the contract does not read or adjust for the token's `decimals()`. Callers
    ///   integrating non-USDC-like tokens are responsible for choosing a `total_amount` (and,
    ///   for admins, a [`Self::set_min_amount`]) that makes sense for that token's precision.
    /// - `total_amount`    — total recruiter fee in the token's smallest unit
    /// - `job_title`       — short job title string
    /// - `milestones`      — ordered milestone list
    /// - `retention_days`  — Vec of retention windows in days (one per Retention milestone)
    /// - `config`          — bundled optional config: metadata_hash, co_recruiter, recruiter_split_bps
    ///
    /// # Panics
    /// - `"CompanyRecruiterCollision"` — `company` and `recruiter` are the same address.
    /// - `"CompanyArbiterCollision"` — `company` also appears in the arbiter set.
    /// - `"RecruiterArbiterCollision"` — `recruiter` also appears in the arbiter set.
    ///
    /// These checks exist so a company cannot name itself (or a colluding address)
    /// as arbiter and vote on its own disputes, or name itself as recruiter to
    /// self-confirm milestones. See issue #174.
    pub fn create_engagement(
        env: Env,
        engagement_id: String,
        company: Address,
        recruiter: Address,
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
        Self::create_engagement_impl(
            env,
            engagement_id,
            company,
            recruiter,
            arbiter_setup,
            token,
            total_amount,
            job_title,
            milestones,
            retention_days,
            config,
        )
    }

    /// Shared engagement-creation logic behind `create_engagement`, factored
    /// out so `batch_create_engagements` (issue #260) can authorize each
    /// distinct company address once up front and then create every one of
    /// its engagements without re-`require_auth`-ing that address — calling
    /// `require_auth()` twice for the same address within one call frame
    /// (as opposed to across separate cross-contract invocations) is rejected
    /// by the host as a duplicate authorization.
    ///
    /// Callers are responsible for `assert_not_paused` and
    /// `company.require_auth()` before invoking this.
    fn create_engagement_impl(
        env: Env,
        engagement_id: String,
        company: Address,
        recruiter: Address,
        arbiter_setup: ArbiterSetup,
        token: Address,
        total_amount: i128,
        job_title: String,
        milestones: Vec<Milestone>,
        retention_days: Vec<u32>,
        config: EngagementConfig,
    ) -> String {
        // Validate engagement_id format: non-empty, ≤ 64 chars, [A-Za-z0-9-] only.
        if engagement_id.is_empty() || engagement_id.len() > MAX_ENGAGEMENT_ID_LENGTH {
            panic!("InvalidEngagementId");
        }
        let id_len = engagement_id.len() as usize;
        let mut id_buf = [0u8; MAX_ENGAGEMENT_ID_LENGTH as usize];
        engagement_id.copy_into_slice(&mut id_buf[..id_len]);
        for &b in &id_buf[..id_len] {
            let valid =
                b.is_ascii_uppercase() || b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
            if !valid {
                panic!("InvalidEngagementId");
            }
        }

        // Issue #24: Job title validation
        if job_title.is_empty() {
            panic!("JobTitleEmpty");
        }
        if job_title.len() > 64 {
            panic!("JobTitleTooLong");
        }

        // Issue #21: Max milestone count cap validation
        if milestones.is_empty() {
            panic!("ZeroMilestones");
        }
        let max_milestones = Self::get_max_milestones(env.clone());
        if milestones.len() > max_milestones {
            panic!("TooManyMilestones");
        }

        // Issue #22: Milestone name max length validation
        for i in 0..milestones.len() {
            let m = milestones.get(i).unwrap();
            if m.name.is_empty() {
                panic!("MilestoneNameEmpty: index {}", i);
            }
            if m.name.len() > 64 {
                panic!("MilestoneNameTooLong: index {}", i);
            }
        }

        // Issue #23: Milestone name uniqueness validation
        for i in 0..milestones.len() {
            let m_i = milestones.get(i).unwrap();
            for j in (i + 1)..milestones.len() {
                let m_j = milestones.get(j).unwrap();
                if m_i.name == m_j.name {
                    let mut name_buf = [0u8; 64];
                    let name_len = m_i.name.len() as usize;
                    m_i.name.copy_into_slice(&mut name_buf[..name_len]);
                    let name_str = core::str::from_utf8(&name_buf[..name_len]).unwrap_or("");
                    panic!("DuplicateMilestoneName: {}", name_str);
                }
            }
        }

        // Issue #248: engagement tags validation — bounded count and length so
        // storage stays predictable regardless of caller input.
        if let Some(ref tags) = config.tags {
            if tags.len() > MAX_TAGS {
                panic!("TooManyTags");
            }
            for i in 0..tags.len() {
                let tag = tags.get(i).unwrap();
                if tag.is_empty() {
                    panic!("TagEmpty: index {}", i);
                }
                if tag.len() > MAX_TAG_LENGTH {
                    panic!("TagTooLong: index {}", i);
                }
            }
        }

        if total_amount <= 0 {
            panic!("amount must be greater than zero");
        }

        // Issue #17: Minimum amount validation
        let min_amount = Self::get_min_amount(env.clone());
        if total_amount < min_amount {
            panic!("AmountBelowMinimum");
        }

        // Issue #26: reject token if allowlist is active and token not in it.
        let allowlist_enabled: bool = env
            .storage()
            .persistent()
            .get(&DataKey::AllowlistEnabled)
            .unwrap_or(false);
        if allowlist_enabled {
            let allowed: Vec<Address> = env
                .storage()
                .persistent()
                .get(&DataKey::AllowedTokens)
                .unwrap_or_else(|| Vec::new(&env));
            let is_allowed = (0..allowed.len()).any(|i| allowed.get(i).unwrap() == token);
            if !is_allowed {
                panic!("TokenNotAllowed");
            }
        }

        let arbiters = arbiter_setup.arbiters;
        let quorum = arbiter_setup.quorum;

        if arbiters.is_empty() {
            panic!("at least one arbiter required");
        }

        if quorum == 0 || quorum > arbiters.len() {
            panic!("invalid quorum");
        }

        // Issue #174: reject overlapping company/recruiter/arbiter addresses so a
        // company cannot name itself (or a colluding address) as arbiter and vote
        // on its own disputes, or name itself as recruiter to self-confirm milestones.
        if company == recruiter {
            panic!("CompanyRecruiterCollision");
        }
        for i in 0..arbiters.len() {
            let a = arbiters.get(i).unwrap();
            if a == company {
                panic!("CompanyArbiterCollision");
            }
            if a == recruiter {
                panic!("RecruiterArbiterCollision");
            }
        }

        // Reject empty metadata hash — caller must either omit or provide a real CID.
        if let Some(ref hash) = config.metadata_hash {
            if hash.is_empty() {
                panic!("InvalidMetadataHash");
            }
        }

        // Reject empty contract_pdf_hash — caller must either omit or provide a real hash.
        if let Some(ref hash) = config.contract_pdf_hash {
            if hash.is_empty() {
                panic!("InvalidContractPdfHash");
            }
        }

        // Issue #56: validate co-recruiter split basis points.
        if config.recruiter_split_bps > FULL_SPLIT_BPS {
            panic!("InvalidSplitBps");
        }

        let mut total_percent: u32 = 0;
        for i in 0..milestones.len() {
            total_percent += milestones.get(i).unwrap().payment_percent;
        }
        if total_percent != 100 {
            panic!("milestone percentages must sum to 100");
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Engagement(engagement_id.clone()))
        {
            panic!("engagement already exists");
        }

        // Cap check: reject if the company is already at or over the active engagement limit.
        let active_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyActiveCount(company.clone()))
            .unwrap_or(0u32);
        let max_active: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxActivePerCompany))
            .unwrap_or(DEFAULT_MAX_ACTIVE_PER_COMPANY);
        if active_count >= max_active {
            panic!("CompanyActiveLimitReached");
        }

        let current_ledger = env.ledger().sequence();
        let lpd = Self::get_ledgers_per_day_internal(&env);
        let max_retention_days = Self::get_max_retention_days(env.clone());
        let mut retention_index: u32 = 0;
        let mut resolved_milestones: Vec<Milestone> = Vec::new(&env);

        for i in 0..milestones.len() {
            let mut m = milestones.get(i).unwrap();
            match m.kind {
                MilestoneKind::Placement => {
                    m.valid_after_ledger = 0;
                    m.status = MilestoneStatus::Pending;
                }
                MilestoneKind::Retention => {
                    let days = retention_days.get(retention_index).unwrap_or(30);
                    retention_index += 1;

                    // Issue #19: Zero retention days validation
                    if days == 0 {
                        panic!("RetentionDaysZero");
                    }

                    if days > max_retention_days {
                        panic!("RetentionDaysTooLarge");
                    }
                    m.valid_after_ledger = current_ledger + (days * lpd);
                    m.status = MilestoneStatus::Locked;
                }
            }
            resolved_milestones.push_back(m);
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&company, &env.current_contract_address(), &total_amount);
        Self::on_escrow_lifecycle_checkpoint(
            &env,
            &engagement_id,
            EscrowLifecycleAction::Funded,
            total_amount,
            &token,
            &company,
            &recruiter,
        );

        let engagement = Engagement {
            id: engagement_id.clone(),
            company: company.clone(),
            recruiter: recruiter.clone(),
            arbiters,
            quorum,
            token,
            total_amount,
            released_amount: 0,
            job_title,
            metadata_hash: config.metadata_hash,
            created_at_ledger: current_ledger,
            last_activity_ledger: current_ledger,
            milestones: resolved_milestones,
            status: EngagementStatus::Active,
            co_recruiter: config.co_recruiter,
            recruiter_split_bps: config.recruiter_split_bps,
            contract_pdf_hash: config.contract_pdf_hash,
            referrer: config.referrer,
            tags: config.tags.clone(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        Self::extend_engagement_ttl(&env, &engagement_id);

        // Increment per-company active engagement count.
        let new_active = active_count + 1;
        env.storage()
            .persistent()
            .set(&DataKey::CompanyActiveCount(company.clone()), &new_active);
        env.storage().persistent().extend_ttl(
            &DataKey::CompanyActiveCount(company.clone()),
            100_000,
            6_300_000,
        );

        // Issue #34: increment global engagement counter.
        let count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::EngagementCount)
            .unwrap_or(0u64);
        env.storage()
            .instance()
            .set(&DataKey::EngagementCount, &(count + 1));

        // Issue #237: append engagement_id to the global index that backs
        // `get_engagement_ids_by_status`. Kept separate from `EngagementCount`
        // because that counter is a scalar and cannot be enumerated.
        let mut all_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));
        all_ids.push_back(engagement_id.clone());
        env.storage()
            .persistent()
            .set(&DataKey::AllEngagements, &all_ids);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::AllEngagements, 100_000, 6_300_000);

        // Issue #35: append engagement_id to the per-company index.
        let mut company_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyEngagements(company.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        company_ids.push_back(engagement_id.clone());
        env.storage()
            .persistent()
            .set(&DataKey::CompanyEngagements(company.clone()), &company_ids);
        env.storage().persistent().extend_ttl(
            &DataKey::CompanyEngagements(company.clone()),
            100_000,
            6_300_000,
        );

        // Issue #36: append engagement_id to the per-recruiter index.
        let mut recruiter_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterEngagements(recruiter.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        recruiter_ids.push_back(engagement_id.clone());
        env.storage().persistent().set(
            &DataKey::RecruiterEngagements(recruiter.clone()),
            &recruiter_ids,
        );

        // Issue #248 & #249: append engagement_id to per-tag indices.
        if let Some(ref tags) = config.tags {
            // Count/length were already validated above; here we only need to
            // de-duplicate so a repeated tag doesn't list the engagement twice
            // in the same per-tag index.
            let mut seen_tags = Vec::new(&env);
            for i in 0..tags.len() {
                let t = tags.get(i).unwrap();
                if !seen_tags.contains(&t) {
                    seen_tags.push_back(t.clone());
                    let mut tag_ids: Vec<String> = env
                        .storage()
                        .persistent()
                        .get(&DataKey::TagEngagements(t.clone()))
                        .unwrap_or_else(|| Vec::new(&env));
                    tag_ids.push_back(engagement_id.clone());
                    env.storage()
                        .persistent()
                        .set(&DataKey::TagEngagements(t.clone()), &tag_ids);
                    env.storage().persistent().extend_ttl(
                        &DataKey::TagEngagements(t.clone()),
                        100_000,
                        6_300_000,
                    );
                }
            }
        }
        env.storage().persistent().extend_ttl(
            &DataKey::RecruiterEngagements(recruiter.clone()),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "engagement_created"),
                engagement_id.clone(),
            ),
            engagement_id.clone(),
        );

        engagement_id
    }

    // ----------------------------------------------------------
    // ISSUE #260 — BATCH CREATE ENGAGEMENTS
    // ----------------------------------------------------------

    /// Create multiple engagements from a list of configs in a single call.
    ///
    /// Each entry in `configs` carries the same fields as `create_engagement`'s
    /// individual parameters, bundled into a `BatchEngagementConfig`. This is a
    /// thin wrapper: every engagement is created via the same `create_engagement`
    /// path (same validation, same `company.require_auth()` per entry, same
    /// events), so batching relaxes nothing — a company creating several
    /// engagements for itself still only needs to sign once, but a batch mixing
    /// engagements for different companies requires every distinct company's
    /// signature to be present in the transaction.
    ///
    /// # Atomicity
    /// A panic on any entry aborts the whole call — Soroban invocations are
    /// all-or-nothing, so no partial batch is ever persisted.
    ///
    /// # Panics
    /// - `"EmptyConfigs"` — `configs` is empty.
    /// - `"TooManyEngagements"` — `configs` has more than `MAX_BATCH_CREATE_ENGAGEMENTS` (20) entries.
    /// - Any panic condition documented on `create_engagement`, raised by whichever entry triggers it.
    ///
    /// # Returns
    /// The list of created `engagement_id`s, in the same order as `configs`.
    pub fn batch_create_engagements(env: Env, configs: Vec<BatchEngagementConfig>) -> Vec<String> {
        Self::assert_not_paused(&env);

        if configs.is_empty() {
            panic!("EmptyConfigs");
        }
        if configs.len() > MAX_BATCH_CREATE_ENGAGEMENTS {
            panic!("TooManyEngagements");
        }

        // Authorize each distinct company address exactly once. `require_auth`
        // ties to the current call frame; since every entry in this batch is
        // created via a plain function call rather than a separate
        // cross-contract invocation, requesting auth twice for the same
        // address here would be treated as a duplicate authorization by the
        // host. A batch is free to mix companies — every distinct one just
        // needs its signature present in the transaction.
        let mut authorized: Vec<Address> = Vec::new(&env);
        for i in 0..configs.len() {
            let company = configs.get(i).unwrap().company;
            if !authorized.contains(&company) {
                company.require_auth();
                authorized.push_back(company);
            }
        }

        let mut ids: Vec<String> = Vec::new(&env);
        for i in 0..configs.len() {
            let cfg = configs.get(i).unwrap();
            let id = Self::create_engagement_impl(
                env.clone(),
                cfg.engagement_id,
                cfg.company,
                cfg.recruiter,
                cfg.arbiter_setup,
                cfg.token,
                cfg.total_amount,
                cfg.job_title,
                cfg.milestones,
                cfg.retention_days,
                cfg.config,
            );
            ids.push_back(id);
        }

        ids
    }

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
        Self::clear_due_soon_flag(&env, &engagement_id, milestone_index);

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

    /// Reorder an engagement's milestones, moving `Locked`/`Pending` ones
    /// (i.e. still pre-`ProofSubmitted`) around while leaving any milestone
    /// that has already progressed — `ProofSubmitted`, `Confirmed`,
    /// `Disputed`, or `Resolved` — pinned to its original index.
    ///
    /// Reordering only the not-yet-activated milestones is safe because
    /// `confirm_milestone` / `batch_confirm_milestones` enforce sequential
    /// confirmation by index (issue #67): changing the order of what's still
    /// ahead simply reprioritises which milestone must be completed next,
    /// while every milestone the recruiter has already submitted proof for
    /// (or that already carries a dispute outcome) keeps the position that
    /// proof, dispute, and payout history refer to.
    ///
    /// # Caller
    /// `company` — must match the engagement's company (or its registered
    /// co-signer) and sign the transaction.
    ///
    /// # Arguments
    /// - `new_order` — a permutation of `0..milestones.len()`; `new_order[i]`
    ///   is the original index of the milestone that should end up at
    ///   position `i`.
    ///
    /// # Panics
    /// - `"engagement is not active"` — the engagement is not `Active`.
    /// - `"unauthorized"` — caller is not the engagement's company or co-signer.
    /// - `"InvalidReorderLength"` — `new_order.len()` does not match the
    ///   engagement's milestone count.
    /// - `"invalid milestone index"` — `new_order` contains an out-of-bounds index.
    /// - `"DuplicateReorderIndex"` — `new_order` is not a valid permutation
    ///   (an index appears more than once).
    /// - `"CannotReorderProgressedMilestone"` — `new_order` would move a
    ///   milestone that is `ProofSubmitted`, `Confirmed`, `Disputed`, or
    ///   `Resolved` away from its original index.
    ///
    /// # Events
    /// - `("milestones_reordered", engagement_id)` with `new_order`.
    pub fn reorder_milestones(
        env: Env,
        company: Address,
        engagement_id: String,
        new_order: Vec<u32>,
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

        let len = engagement.milestones.len();
        if new_order.len() != len {
            panic!("InvalidReorderLength");
        }

        // Validate that new_order is a permutation of 0..len before touching
        // any state, so a malformed input never partially reorders anything.
        let mut seen: Vec<u32> = Vec::new(&env);
        for i in 0..new_order.len() {
            let old_idx = new_order.get(i).unwrap();
            if old_idx >= len {
                panic!("{}", ERR_INVALID_MILESTONE_INDEX);
            }
            if seen.contains(old_idx) {
                panic!("DuplicateReorderIndex");
            }
            seen.push_back(old_idx);
        }

        let mut new_milestones: Vec<Milestone> = Vec::new(&env);
        for i in 0..new_order.len() {
            let old_idx = new_order.get(i).unwrap();
            let m = engagement.milestones.get(old_idx).unwrap();
            let progressed = matches!(
                m.status,
                MilestoneStatus::ProofSubmitted
                    | MilestoneStatus::Confirmed
                    | MilestoneStatus::Disputed
                    | MilestoneStatus::Resolved
            );
            if progressed && i != old_idx {
                panic!("CannotReorderProgressedMilestone");
            }
            new_milestones.push_back(m);
        }

        engagement.milestones = new_milestones;
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (Symbol::new(&env, "milestones_reordered"), engagement_id),
            new_order,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #241 — MILESTONE DUE-SOON NOTIFICATION
    // ----------------------------------------------------------

    /// Admin sets the due-soon notification window in ledgers (issue #241).
    ///
    /// A `Locked` retention milestone becomes "due soon" once it is within this
    /// many ledgers of its `valid_after_ledger`. Defaults to 17 280 (~1 day).
    ///
    /// Changing the window takes effect immediately for every engagement, but
    /// does **not** retract notifications already emitted — a milestone that was
    /// notified under a wider window stays flagged until its deadline moves or
    /// it unlocks.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the contract admin.
    /// - `"InvalidDueSoonWindow"` — `ledgers` is 0, which would leave no lead
    ///   time and make the notification useless.
    pub fn set_due_soon_window(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        if ledgers == 0 {
            panic!("InvalidDueSoonWindow");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::DueSoonWindow), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "due_soon_window_set"),), ledgers);
    }

    /// Return the current due-soon notification window in ledgers (issue #241).
    /// Defaults to 17 280 (~1 day) when not configured.
    pub fn get_due_soon_window(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::DueSoonWindow))
            .unwrap_or(DEFAULT_DUE_SOON_WINDOW_LEDGERS)
    }

    /// Returns `true` when a milestone is inside its due-soon window: it is a
    /// `Locked` `Retention` milestone whose `valid_after_ledger` is still in the
    /// future but no more than `due_soon_window` ledgers away (issue #241).
    ///
    /// Returns `false` for `Placement` milestones, for milestones that already
    /// progressed past `Locked`, and for milestones that are already unlockable
    /// (use `is_milestone_unlockable` for that case).
    ///
    /// Read-only and permissionless.
    pub fn is_milestone_due_soon(env: Env, engagement_id: String, milestone_index: u32) -> bool {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.kind != MilestoneKind::Retention || milestone.status != MilestoneStatus::Locked
        {
            return false;
        }

        let current = env.ledger().sequence();
        if current >= milestone.valid_after_ledger {
            return false;
        }

        milestone.valid_after_ledger - current <= Self::get_due_soon_window(env.clone())
    }

    /// Returns `true` once `notify_milestone_due_soon` has fired for this
    /// milestone's current deadline (issue #241). Keepers can poll this to skip
    /// engagements that have already been announced.
    ///
    /// Resets to `false` whenever the deadline itself moves — an accepted
    /// milestone extension, a replacement restarting the retention timer, or the
    /// milestone unlocking.
    pub fn is_milestone_due_soon_notified(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::DueSoonNotified(engagement_id, milestone_index))
            .unwrap_or(false)
    }

    /// Emit a `milestone_due_soon` event for a retention milestone that is
    /// approaching its unlock deadline (issue #241).
    ///
    /// # Caller
    /// Anyone — this is a permissionless keeper function, mirroring
    /// `unlock_milestone` and `escalate_dispute`. It exists so off-chain
    /// notification services can subscribe to a single event stream instead of
    /// polling `ledgers_until_unlock` for every locked milestone.
    ///
    /// # Behaviour
    /// Fires at most **once per deadline**: the emission is recorded under
    /// `DataKey::DueSoonNotified` and a second call panics. The record is
    /// cleared whenever the deadline moves (extension accepted, replacement
    /// requested) or the milestone unlocks, so a rescheduled milestone is
    /// announced again for its new deadline.
    ///
    /// This call deliberately does **not** touch `last_activity_ledger`: it is a
    /// notification, not engagement activity, and bumping it would let a keeper
    /// postpone `expire_engagement` indefinitely.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"` — contract or engagement is paused.
    /// - `"engagement is not active"` — the engagement is not `Active`.
    /// - `"only retention milestones can be due soon"` — milestone is a `Placement`.
    /// - `"milestone is not locked"` — milestone already progressed past `Locked`.
    /// - `"milestone is already unlockable"` — the deadline has passed; call
    ///   `unlock_milestone` instead.
    /// - `"DueSoonWindowNotReached"` — the milestone is further out than the
    ///   configured window.
    /// - `"DueSoonAlreadyNotified"` — this deadline was already announced.
    ///
    /// # Events
    /// Emits `("milestone_due_soon", engagement_id)` with
    /// `(milestone_index, valid_after_ledger, ledgers_remaining, window)`.
    /// `ledgers_remaining` and `window` are included so a consumer can rank
    /// urgency and reproduce the trigger condition without extra queries.
    pub fn notify_milestone_due_soon(env: Env, engagement_id: String, milestone_index: u32) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.kind != MilestoneKind::Retention {
            panic!("only retention milestones can be due soon");
        }

        if milestone.status != MilestoneStatus::Locked {
            panic!("milestone is not locked");
        }

        let current_ledger = env.ledger().sequence();
        if current_ledger >= milestone.valid_after_ledger {
            panic!("milestone is already unlockable");
        }

        let window = Self::get_due_soon_window(env.clone());
        let ledgers_remaining = milestone.valid_after_ledger - current_ledger;
        if ledgers_remaining > window {
            panic!("DueSoonWindowNotReached");
        }

        let notified_key = DataKey::DueSoonNotified(engagement_id.clone(), milestone_index);
        if env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&notified_key)
            .unwrap_or(false)
        {
            panic!("DueSoonAlreadyNotified");
        }

        env.storage().persistent().set(&notified_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&notified_key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "milestone_due_soon"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                milestone.valid_after_ledger,
                ledgers_remaining,
                window,
            ),
        );
    }

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
    // CONFIRM MILESTONE
    // ----------------------------------------------------------

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
            let effective_bps =
                Self::apply_referral_discount(&env, platform_fee.bps, &engagement.referrer);
            Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
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
            Self::on_escrow_lifecycle_checkpoint(
                &env,
                &engagement_id,
                EscrowLifecycleAction::PayoutReleased,
                -payment,
                &engagement.token,
                &engagement.company,
                &engagement.recruiter,
            );
        }

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Confirmed;
        engagement
            .milestones
            .set(milestone_index, milestone.clone());

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::decrement_company_active_count(&env, &engagement.company);
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

    /// Schedule a future-ledger auto-confirm for a milestone whose proof has
    /// already been submitted, so its payment releases automatically once
    /// `target_ledger` is reached instead of requiring the company to call
    /// `confirm_milestone` in person.
    ///
    /// This does not release funds immediately or bypass the dispute window —
    /// the company can still `raise_dispute` at any point before
    /// `execute_scheduled_auto_confirm` is actually called, which moves the
    /// milestone out of `ProofSubmitted` and makes the scheduled entry a no-op.
    ///
    /// # Caller
    /// `company` — must match the engagement's company (or its registered
    /// co-signer) and sign the transaction.
    ///
    /// # Panics
    /// - `"engagement is not active"` — the engagement is not `Active`.
    /// - `"unauthorized"` — caller is not the engagement's company or co-signer.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"milestone proof not yet submitted"` — the milestone is not in `ProofSubmitted` status.
    /// - `"ScheduledLedgerInPast"` — `target_ledger` is at or before the current ledger.
    ///
    /// # Events
    /// - `("auto_confirm_scheduled", engagement_id)` with `(milestone_index, target_ledger)`.
    pub fn schedule_auto_confirm(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
        target_ledger: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

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

        if target_ledger <= env.ledger().sequence() {
            panic!("ScheduledLedgerInPast");
        }

        let key = DataKey::ScheduledAutoConfirm(engagement_id.clone(), milestone_index);
        env.storage().persistent().set(&key, &target_ledger);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "auto_confirm_scheduled"), engagement_id),
            (milestone_index, target_ledger),
        );
    }

    /// Cancel a previously scheduled auto-confirm for a milestone. No-op if
    /// none is scheduled.
    ///
    /// # Caller
    /// `company` — must match the engagement's company (or its registered
    /// co-signer) and sign the transaction.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's company or co-signer.
    pub fn cancel_scheduled_auto_confirm(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        company.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey::ScheduledAutoConfirm(engagement_id.clone(), milestone_index);
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
            env.events().publish(
                (Symbol::new(&env, "auto_confirm_cancelled"), engagement_id),
                milestone_index,
            );
        }
    }

    /// Return the ledger sequence at which a milestone's auto-confirm is
    /// scheduled to become executable, or `None` if none is scheduled.
    /// Read-only and permissionless.
    pub fn get_scheduled_auto_confirm(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<u32> {
        env.storage()
            .persistent()
            .get(&DataKey::ScheduledAutoConfirm(
                engagement_id,
                milestone_index,
            ))
    }

    /// Execute a milestone's scheduled auto-confirm once `target_ledger` has
    /// been reached, releasing payment exactly as `confirm_milestone` would.
    ///
    /// # Caller
    /// Anyone — this function is permissionless. The company already
    /// authorised the release when it called `schedule_auto_confirm`; this
    /// call only carries out a commitment already made on-chain.
    ///
    /// # Panics
    /// - `"engagement is not active"` — the engagement is not `Active`.
    /// - `"NoScheduledAutoConfirm"` — no auto-confirm is scheduled for this milestone.
    /// - `"ScheduledLedgerNotReached"` — the current ledger is still before the scheduled ledger.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"milestone proof not yet submitted"` — the milestone left `ProofSubmitted`
    ///   in the meantime (e.g. a dispute was raised), so the schedule no longer applies.
    /// - `"PreviousMilestoneNotComplete"` — an earlier milestone (by index) is not
    ///   yet `Confirmed` or `Resolved` (issue #67 sequential-confirmation rule).
    ///
    /// # Events
    /// - `("platform_fee_collected", engagement_id)` with `(milestone_index, fee_amount, treasury)` — when fee > 0.
    /// - `("milestone_status_changed", engagement_id)` with `(milestone_index, old_status, new_status)`.
    /// - `("milestone_auto_confirmed", engagement_id)` with `(milestone_index, payment)`.
    /// - `("status_changed", engagement_id)` — if the engagement completes.
    /// - `("engagement_completed", engagement_id)` — if all milestones are now done.
    pub fn execute_scheduled_auto_confirm(env: Env, engagement_id: String, milestone_index: u32) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let key = DataKey::ScheduledAutoConfirm(engagement_id.clone(), milestone_index);
        let target_ledger: u32 = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("NoScheduledAutoConfirm"));

        if env.ledger().sequence() < target_ledger {
            panic!("ScheduledLedgerNotReached");
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        // Issue #67: enforce the same sequential-confirmation rule as confirm_milestone.
        for i in 0..milestone_index {
            let prev = engagement.milestones.get(i).unwrap();
            if prev.status != MilestoneStatus::Confirmed && prev.status != MilestoneStatus::Resolved
            {
                panic!("PreviousMilestoneNotComplete");
            }
        }

        // Release payment identically to confirm_milestone, including the
        // issue #183 replacement-payout accounting.
        let full_share = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        let payment = full_share - milestone.replacement_paid_out;
        if payment > 0 {
            let platform_fee = Self::get_platform_fee_internal(&env);
            let base_bps =
                Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
            let effective_bps = Self::apply_referral_discount(&env, base_bps, &engagement.referrer);
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
            Self::on_escrow_lifecycle_checkpoint(
                &env,
                &engagement_id,
                EscrowLifecycleAction::PayoutReleased,
                -payment,
                &engagement.token,
                &engagement.company,
                &engagement.recruiter,
            );
        }

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Confirmed;
        engagement.milestones.set(milestone_index, milestone);

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::decrement_company_active_count(&env, &engagement.company);
        }
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        env.storage().persistent().remove(&key);

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
                Symbol::new(&env, "milestone_auto_confirmed"),
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
                    engagement_id,
                    engagement.released_amount,
                    env.ledger().sequence(),
                ),
            );
        }
    }

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

            let arbiter_fee_bps: u32 = env
                .storage()
                .instance()
                .get(&DataKey::Config(ConfigKey::ArbiterFee))
                .unwrap_or(0u32);
            let arbiter_fee_amount = (payment * arbiter_fee_bps as i128) / 10_000;
            let net_payment = payment - arbiter_fee_amount;

            let token_client = token::Client::new(&env, &engagement.token);
            if arbiter_fee_amount > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &arbiter,
                    &arbiter_fee_amount,
                );
            }
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client);
            Self::on_escrow_lifecycle_checkpoint(
                &env,
                &engagement_id,
                EscrowLifecycleAction::PayoutReleased,
                -payment,
                &engagement.token,
                &engagement.company,
                &engagement.recruiter,
            );

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
                Self::decrement_company_active_count(&env, &engagement.company);
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

            let arbiter_fee_bps: u32 = env
                .storage()
                .instance()
                .get(&DataKey::Config(ConfigKey::ArbiterFee))
                .unwrap_or(0u32);
            let arbiter_fee_amount = (payment * arbiter_fee_bps as i128) / 10_000;
            let net_payment = payment - arbiter_fee_amount;

            let token_client = token::Client::new(&env, &engagement.token);
            if arbiter_fee_amount > 0 {
                token_client.transfer(
                    &env.current_contract_address(),
                    &super_arbiter,
                    &arbiter_fee_amount,
                );
            }
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client);
            Self::on_escrow_lifecycle_checkpoint(
                &env,
                &engagement_id,
                EscrowLifecycleAction::PayoutReleased,
                -payment,
                &engagement.token,
                &engagement.company,
                &engagement.recruiter,
            );

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
                Self::decrement_company_active_count(&env, &engagement.company);
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

        let token_client = token::Client::new(&env, &engagement.token);
        Self::distribute_recruiter_payout(&env, &engagement, payment, &token_client);
        Self::on_escrow_lifecycle_checkpoint(
            &env,
            &engagement_id,
            EscrowLifecycleAction::PayoutReleased,
            -payment,
            &engagement.token,
            &engagement.company,
            &engagement.recruiter,
        );

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
            Self::decrement_company_active_count(&env, &engagement.company);
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
    fn increment_super_arbiter_resolution_count(env: &Env) {
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
    // REQUEST REPLACEMENT
    // ----------------------------------------------------------

    /// Company requests a replacement candidate after the placement milestone
    /// was already confirmed. Unconfirmed milestones are reset (Placement →
    /// `Pending`, Retention → `Locked` with its timer restarted).
    ///
    /// A Retention milestone that is currently `Disputed` (a dispute was raised
    /// but arbiters have not yet reached quorum) is included in this reset: it is
    /// forced back to `Locked` and its in-flight vote tally and dispute reason
    /// are cleared, so a future dispute on the same milestone index starts from
    /// a clean vote count instead of inheriting stale votes. See issue #177.
    pub fn request_replacement(env: Env, company: Address, engagement_id: String, reason: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        // Bound reason length so a single engagement cannot accumulate
        // unbounded replacement metadata. See issue #51.
        if reason.len() > MAX_REPLACEMENT_REASON_LEN {
            panic!("replacement reason too long");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if company != engagement.company {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let placement_confirmed = {
            let m0 = engagement.milestones.get(0).unwrap();
            m0.status == MilestoneStatus::Confirmed || m0.status == MilestoneStatus::Resolved
        };

        if !placement_confirmed {
            panic!("placement not yet confirmed — use cancel_engagement instead");
        }

        let replacement_index: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ReplacementCount(engagement_id.clone()))
            .unwrap_or(0u32);

        if replacement_index >= Self::get_max_replacements_internal(&env) {
            panic!("ReplacementLimitReached");
        }

        let current_ledger = env.ledger().sequence();

        for i in 0..engagement.milestones.len() {
            let mut m = engagement.milestones.get(i).unwrap();
            match m.kind {
                MilestoneKind::Placement => {
                    if m.status == MilestoneStatus::Confirmed
                        || m.status == MilestoneStatus::Resolved
                    {
                        let old_status = m.status.clone();
                        m.status = MilestoneStatus::Pending;
                        m.proof_hash = String::from_str(&env, "");
                        Self::emit_milestone_status_changed(
                            &env,
                            &engagement_id,
                            i,
                            old_status,
                            MilestoneStatus::Pending,
                        );
                        // Clear cooldown so the replacement candidate can submit immediately.
                        env.storage()
                            .persistent()
                            .remove(&DataKey::LastProofAt(engagement_id.clone(), i));
                    }
                }
                MilestoneKind::Retention => {
                    if m.status != MilestoneStatus::Confirmed
                        && m.status != MilestoneStatus::Resolved
                    {
                        // Issue #177: a Retention milestone can be Disputed (dispute
                        // raised, arbiters not yet at quorum) when a replacement is
                        // requested. Resetting it to Locked here must also clear any
                        // in-flight vote tally and dispute reason — otherwise a stale
                        // ArbiterVoteRecord (partial votes, or arbiters who already
                        // "voted") would silently carry over and be read by
                        // `cast_arbiter_vote` the next time this same milestone index
                        // is disputed again, corrupting the new dispute's vote count.
                        let was_disputed = m.status == MilestoneStatus::Disputed;
                        let original_days = (m.valid_after_ledger - engagement.created_at_ledger)
                            / Self::get_ledgers_per_day_internal(&env);
                        m.valid_after_ledger = current_ledger
                            + (original_days * Self::get_ledgers_per_day_internal(&env));
                        let old_status = m.status.clone();
                        m.status = MilestoneStatus::Locked;
                        m.proof_hash = String::from_str(&env, "");
                        if old_status != MilestoneStatus::Locked {
                            Self::emit_milestone_status_changed(
                                &env,
                                &engagement_id,
                                i,
                                old_status,
                                MilestoneStatus::Locked,
                            );
                        }
                        env.storage()
                            .persistent()
                            .remove(&DataKey::LastProofAt(engagement_id.clone(), i));

                        // The retention timer just restarted, so a due-soon
                        // notification emitted against the old deadline must not
                        // suppress one for the new deadline. See issue #241.
                        Self::clear_due_soon_flag(&env, &engagement_id, i);

                        if was_disputed {
                            env.storage()
                                .persistent()
                                .remove(&DataKey::ArbiterVotes(engagement_id.clone(), i));
                            env.storage()
                                .persistent()
                                .remove(&DataKey::DisputeReason(engagement_id.clone(), i));
                            env.storage()
                                .persistent()
                                .remove(&DataKey::DisputeRaisedAt(engagement_id.clone(), i));
                            env.storage()
                                .persistent()
                                .remove(&DataKey::EscalatedDispute(engagement_id.clone(), i));
                        }
                    }
                }
            }
            engagement.milestones.set(i, m);
        }

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::ReplacementRequested;
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        // Record the reason under a monotonic per-engagement index so the
        // full replacement history is auditable. See issue #51.
        env.storage().persistent().set(
            &DataKey::ReplacementReason(engagement_id.clone(), replacement_index),
            &reason,
        );
        env.storage().persistent().set(
            &DataKey::ReplacementCount(engagement_id.clone()),
            &(replacement_index + 1),
        );

        Self::extend_engagement_ttl(&env, &engagement_id);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );

        env.events().publish(
            (
                Symbol::new(&env, "replacement_requested"),
                engagement_id.clone(),
            ),
            (replacement_index, reason),
        );
    }

    /// Returns the structured replacement reason recorded for an engagement at
    /// a given replacement index, or `None` if no such replacement exists.
    /// See issue #51.
    pub fn get_replacement_reason(
        env: Env,
        engagement_id: String,
        replacement_index: u32,
    ) -> Option<String> {
        env.storage().persistent().get(&DataKey::ReplacementReason(
            engagement_id,
            replacement_index,
        ))
    }

    /// Returns the number of replacements ever requested for an engagement.
    /// Useful for paging through `get_replacement_reason`. See issue #51.
    pub fn get_replacement_count(env: Env, engagement_id: String) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::ReplacementCount(engagement_id))
            .unwrap_or(0u32)
    }

    // ----------------------------------------------------------
    // ISSUE #31 — REPLACEMENT COUNT LIMIT
    // ----------------------------------------------------------

    /// Admin sets the maximum number of replacements allowed per engagement.
    /// Defaults to 3 when not explicitly configured.
    pub fn set_max_replacements(env: Env, admin: Address, count: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxReplacements), &count);
        env.events()
            .publish((Symbol::new(&env, "max_replacements_set"),), count);
    }

    /// Return the current maximum replacement count cap.
    /// Returns `DEFAULT_MAX_REPLACEMENTS` (3) when not configured.
    pub fn get_max_replacements(env: Env) -> u32 {
        Self::get_max_replacements_internal(&env)
    }

    // ----------------------------------------------------------
    // ISSUE #43 — COMPANY TRANSFER
    // ----------------------------------------------------------
    // ISSUE #254 — COMPANY MULTI-SIGNER SUPPORT
    // ----------------------------------------------------------

    /// Register a co-signer address that is also authorized to perform
    /// company-gated actions (confirm, dispute, cancel, etc.) on behalf
    /// of this company. Only the company address itself can set the cosigner.
    pub fn set_company_cosigner(env: Env, company: Address, cosigner: Address) {
        company.require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::CompanyCosigner(company.clone()), &cosigner);
        env.events().publish(
            (Symbol::new(&env, "company_cosigner_set"),),
            (company, cosigner),
        );
    }

    /// Return the registered co-signer for a company, or `None` if none set.
    pub fn get_company_cosigner(env: Env, company: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::CompanyCosigner(company))
    }

    /// Register a co-signer address that is also authorized to perform
    /// recruiter-gated actions (submit proof, request exit, etc.) on behalf
    /// of this recruiter. Only the recruiter address itself can set the
    /// co-signer.
    pub fn set_recruiter_cosigner(env: Env, recruiter: Address, cosigner: Address) {
        recruiter.require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::RecruiterCosigner(recruiter.clone()), &cosigner);
        env.events().publish(
            (Symbol::new(&env, "recruiter_cosigner_set"),),
            (recruiter, cosigner),
        );
    }

    /// Return the registered co-signer for a recruiter, or `None` if none set.
    pub fn get_recruiter_cosigner(env: Env, recruiter: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::RecruiterCosigner(recruiter))
    }

    /// Internal helper: check if `caller` is either the engagement's company
    /// or the company's registered co-signer.
    fn is_authorized_company(env: &Env, caller: &Address, engagement_company: &Address) -> bool {
        if caller == engagement_company {
            return true;
        }
        let cosigner: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyCosigner(engagement_company.clone()));
        match cosigner {
            Some(c) => caller == &c,
            None => false,
        }
    }

    /// Internal helper: check if `caller` is either the engagement's recruiter
    /// or the recruiter's registered co-signer.
    fn is_authorized_recruiter(
        env: &Env,
        caller: &Address,
        engagement_recruiter: &Address,
    ) -> bool {
        if caller == engagement_recruiter {
            return true;
        }
        let cosigner: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterCosigner(engagement_recruiter.clone()));
        match cosigner {
            Some(c) => caller == &c,
            None => false,
        }
    }

    // ----------------------------------------------------------
    // ISSUE #258 — RESERVED ESCROW CALLBACK CHECKPOINT
    // ----------------------------------------------------------

    /// Set (or update) the callback target address reserved for future
    /// yield-strategy integrations. Admin only.
    pub fn set_escrow_callback_target(env: Env, admin: Address, target: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::EscrowCallbackTarget, &target);
        env.events()
            .publish((Symbol::new(&env, "escrow_callback_target_set"),), target);
    }

    /// Clear the reserved escrow callback target. Admin only.
    pub fn clear_escrow_callback_target(env: Env, admin: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .remove(&DataKey::EscrowCallbackTarget);
        env.events()
            .publish((Symbol::new(&env, "escrow_callback_target_cleared"),), ());
    }

    /// Enable or disable escrow lifecycle callback checkpoints. Admin only.
    ///
    /// Defaults to `false`, which keeps the checkpoint path as a no-op.
    pub fn set_escrow_callback_enabled(env: Env, admin: Address, enabled: bool) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::EscrowCallbackEnabled, &enabled);
        env.events()
            .publish((Symbol::new(&env, "escrow_callback_enabled_set"),), enabled);
    }

    /// Return `(enabled, target)` for the reserved escrow callback checkpoint.
    pub fn get_escrow_callback_config(env: Env) -> (bool, Option<Address>) {
        let enabled: bool = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowCallbackEnabled)
            .unwrap_or(false);
        let target: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowCallbackTarget);
        (enabled, target)
    }

    // ----------------------------------------------------------
    // ISSUE #43 — COMPANY TRANSFER
    // ----------------------------------------------------------

    /// Transfer the company role on an engagement to a new address, effective
    /// immediately (e.g. the company was acquired or restructured).
    ///
    /// # Caller
    /// `current_company` — must match `engagement.company` and sign the transaction.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's current company.
    /// - `"engagement is not active"` — engagement status is not `Active` or
    ///   `ReplacementRequested`.
    ///
    /// # Events
    /// - `("company_transferred", engagement_id)` with `(old_company, new_company)`.
    pub fn transfer_company(
        env: Env,
        current_company: Address,
        engagement_id: String,
        new_company: Address,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        current_company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if current_company != engagement.company {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let old_company = engagement.company.clone();
        engagement.company = new_company.clone();
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (
                Symbol::new(&env, "company_transferred"),
                engagement_id.clone(),
            ),
            (old_company, new_company),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #44 — RECRUITER TRANSFER
    // ----------------------------------------------------------

    /// Propose a transfer of the recruiter role to a new address.
    ///
    /// The current recruiter initiates the transfer by specifying the
    /// `new_recruiter` address. The company must then call
    /// [`Self::accept_recruiter_transfer`] for the change to take effect.
    /// Until accepted, all payouts continue to go to the original recruiter.
    ///
    /// # Caller
    /// `recruiter` — must match `engagement.recruiter` and sign the transaction.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's current recruiter.
    /// - `"engagement is not active"` — engagement status is not `Active` or
    ///   `ReplacementRequested`.
    pub fn propose_recruiter_transfer(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        new_recruiter: Address,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        env.storage().persistent().set(
            &DataKey::ProposedRecruiterTransfer(engagement_id.clone()),
            &new_recruiter,
        );

        let extend_to = Self::get_storage_ttl_extend_to(env.clone());
        env.storage().persistent().extend_ttl(
            &DataKey::ProposedRecruiterTransfer(engagement_id),
            100_000,
            extend_to,
        );
    }

    /// Accept a pending recruiter transfer and update the engagement's recruiter.
    ///
    /// The company finalises the transfer that was proposed by the current
    /// recruiter. After this call, all future payouts go to the new recruiter.
    ///
    /// # Caller
    /// `company` — must match `engagement.company` and sign the transaction.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's company.
    /// - `"engagement is not active"` — engagement status is not `Active` or
    ///   `ReplacementRequested`.
    /// - `"no pending recruiter transfer"` — there is no active proposal.
    ///
    /// # Events
    /// - `("recruiter_transferred", engagement_id)` with `(old_recruiter, new_recruiter)`.
    pub fn accept_recruiter_transfer(env: Env, company: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let new_recruiter: Address = env
            .storage()
            .persistent()
            .get(&DataKey::ProposedRecruiterTransfer(engagement_id.clone()))
            .unwrap_or_else(|| panic!("no pending recruiter transfer"));

        let old_recruiter = engagement.recruiter.clone();
        engagement.recruiter = new_recruiter.clone();
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .remove(&DataKey::ProposedRecruiterTransfer(engagement_id.clone()));

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (
                Symbol::new(&env, "recruiter_transferred"),
                engagement_id.clone(),
            ),
            (old_recruiter, new_recruiter),
        );
    }

    // ----------------------------------------------------------
    // CANCEL ENGAGEMENT
    // ----------------------------------------------------------

    /// Cancel an engagement and refund any unreleased escrow to the company.
    ///
    /// # Caller
    /// Requires authentication from both `company` and `recruiter` — each
    /// address is validated against `engagement.company` / `engagement.recruiter`
    /// and must call `require_auth`. The two addresses together must agree to
    /// the cancellation; neither party can cancel unilaterally.
    ///
    /// # Precondition
    /// Intended for cancellation **before any milestones have been confirmed**,
    /// the point at which `released_amount == 0` and the full `total_amount`
    /// is still refundable to the company. Once the placement milestone has
    /// already been `Confirmed` / `Resolved`, prefer
    /// [`Self::request_replacement`] instead — `cancel_engagement` is still
    /// callable on a `ReplacementRequested` engagement because
    /// `request_replacement` may have been invoked already, but it will only
    /// refund the unreleased remainder rather than the full fee.
    ///
    /// Strictly enforced: the engagement must be in `Active` or
    /// `ReplacementRequested` status; any other state (`Completed`,
    /// `Cancelled`, `Expired`, `ExitRequested`) is rejected. The contract
    /// must also not be paused, otherwise the call also fails — see
    /// [`Self::assert_not_paused`].
    ///
    /// # Refund behaviour
    /// Transfers `engagement.total_amount - engagement.released_amount` from
    /// the contract's escrow back to `engagement.company` using the
    /// engagement's escrow token.
    ///
    /// Side effects after the refund:
    /// - Engagement status is set to the terminal `Cancelled` state.
    /// - Any pending `AmendmentProposal`s for the engagement's milestones
    ///   are cleared, so `accept_amendment` / `reject_amendment` cannot
    ///   mutate a cancelled engagement (see issue #176).
    /// - The per-company active engagement counter is decremented.
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused (raised by
    ///   [`Self::assert_not_paused`] before authentication).
    /// - `"engagement is not active"` — engagement is not in `Active` or
    ///   `ReplacementRequested` status.
    /// - `"unauthorized"` — `company` does not match `engagement.company`
    ///   or `recruiter` does not match `engagement.recruiter`.
    ///
    /// # Events
    /// Emits `("engagement_cancelled", engagement_id)` with the `refund`
    /// amount as the event body, in addition to the usual
    /// `engagement_status_changed` status transition event.
    pub fn cancel_engagement(
        env: Env,
        company: Address,
        recruiter: Address,
        engagement_id: String,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let refund = engagement.total_amount - engagement.released_amount;
        let token_client = token::Client::new(&env, &engagement.token);
        token_client.transfer(
            &env.current_contract_address(),
            &engagement.company,
            &refund,
        );
        Self::on_escrow_lifecycle_checkpoint(
            &env,
            &engagement_id,
            EscrowLifecycleAction::Refunded,
            -refund,
            &engagement.token,
            &engagement.company,
            &engagement.recruiter,
        );

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Cancelled;
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

        // Issue #176: a pending (unexpired) amendment proposal survives a
        // cancellation unless explicitly cleared here — `accept_amendment` and
        // `reject_amendment` never check `engagement.status`, so without this a
        // proposal could still be accepted/rejected after the engagement is
        // terminal, mutating milestone.payment_percent on a Cancelled engagement.
        // Clearing on cancel also makes `get_pending_amendment` correctly report
        // no pending amendment once the engagement is gone.
        for i in 0..engagement.milestones.len() {
            let key = DataKey::AmendmentProposal(engagement_id.clone(), i);
            if env.storage().persistent().has(&key) {
                env.storage().persistent().remove(&key);
            }
        }

        Self::decrement_company_active_count(&env, &engagement.company);

        env.events().publish(
            (
                Symbol::new(&env, "engagement_cancelled"),
                engagement_id.clone(),
            ),
            refund,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #33 — ESCROW TOP-UP
    // ----------------------------------------------------------

    /// Company tops up the escrow balance for an active engagement.
    pub fn top_up_escrow(env: Env, company: Address, engagement_id: String, amount: i128) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if amount <= 0 {
            panic!("amount must be greater than zero");
        }

        let token_client = token::Client::new(&env, &engagement.token);
        token_client.transfer(&company, &env.current_contract_address(), &amount);
        Self::on_escrow_lifecycle_checkpoint(
            &env,
            &engagement_id,
            EscrowLifecycleAction::ToppedUp,
            amount,
            &engagement.token,
            &engagement.company,
            &engagement.recruiter,
        );

        engagement.total_amount += amount;
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (Symbol::new(&env, "escrow_topped_up"), engagement_id.clone()),
            (amount, engagement.total_amount),
        );
    }

    // ----------------------------------------------------------
    // READ-ONLY QUERIES
    // ----------------------------------------------------------

    /// Get the deployed contract version string (issue #16).
    /// No authentication required.
    pub fn get_version(env: Env) -> String {
        env.storage()
            .persistent()
            .get(&DataKey::Version)
            .unwrap_or_else(|| String::from_str(&env, DEFAULT_VERSION))
    }

    /// Get the current minimum engagement amount in stroops (issue #17).
    /// No authentication required.
    pub fn get_min_amount(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::MinEngagementAmount))
            .unwrap_or(DEFAULT_MIN_ENGAGEMENT_AMOUNT)
    }

    /// Returns the full engagement record for a given engagement ID.
    ///
    /// # Returns
    /// The complete [`Engagement`] struct containing all engagement details.
    ///
    /// # Panics
    /// Panics with `"engagement not found"` if no engagement exists with the given `engagement_id`.
    pub fn get_engagement(env: Env, engagement_id: String) -> Engagement {
        Self::get_engagement_internal(&env, &engagement_id)
    }

    /// Returns a specific milestone from an engagement.
    ///
    /// # Returns
    /// The [`Milestone`] struct at the given index within the engagement's milestone list.
    ///
    /// # Panics
    /// Panics with `"engagement not found"` if no engagement exists with the given `engagement_id`.
    /// Panics with `"invalid milestone index"` if `milestone_index` is out of bounds for the engagement's milestones.
    pub fn get_milestone(env: Env, engagement_id: String, milestone_index: u32) -> Milestone {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        Self::get_milestone_or_panic(&engagement, milestone_index)
    }

    /// Returns the status of every milestone in the engagement, ordered by
    /// milestone index, in a single call (issue #37). Read-only, permissionless.
    pub fn get_all_milestone_statuses(env: Env, engagement_id: String) -> Vec<MilestoneStatus> {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let mut statuses = Vec::new(&env);
        for i in 0..engagement.milestones.len() {
            statuses.push_back(engagement.milestones.get(i).unwrap().status);
        }
        statuses
    }

    /// Returns the current escrow balance for an engagement.
    ///
    /// # Returns
    /// The remaining escrow balance in the token's smallest unit (e.g., stroops).
    /// Calculated as `total_amount - released_amount`.
    ///
    /// # Panics
    /// Panics with `"engagement not found"` if no engagement exists with the given `engagement_id`.
    pub fn get_escrow_balance(env: Env, engagement_id: String) -> i128 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.total_amount - engagement.released_amount
    }

    /// Returns `true` if the milestone is `Locked` and the current ledger
    /// sequence is greater than or equal to its `valid_after_ledger`, meaning
    /// it can currently be unlocked via `unlock_milestone`.
    pub fn is_milestone_unlockable(env: Env, engagement_id: String, milestone_index: u32) -> bool {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        milestone.status == MilestoneStatus::Locked
            && env.ledger().sequence() >= milestone.valid_after_ledger
    }

    /// Returns the number of ledgers remaining until the milestone becomes
    /// unlockable, or `0` if it is already unlockable.
    ///
    /// When the result is `0`, `unlock_milestone` can be called immediately.
    /// Otherwise, the caller must wait at least this many more ledgers before
    /// `env.ledger().sequence() >= milestone.valid_after_ledger` holds.
    pub fn ledgers_until_unlock(env: Env, engagement_id: String, milestone_index: u32) -> u32 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        let current = env.ledger().sequence();
        milestone.valid_after_ledger.saturating_sub(current)
    }

    /// Returns approximate seconds until a Locked retention milestone unlocks.
    /// Returns 0 if the milestone is already unlockable or is a Placement milestone.
    pub fn get_estimated_unlock_seconds(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> u64 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.kind == MilestoneKind::Placement {
            return 0;
        }

        let current = env.ledger().sequence();
        if current >= milestone.valid_after_ledger {
            return 0;
        }

        let ledgers_remaining = (milestone.valid_after_ledger - current) as u64;
        // seconds_per_ledger = 86_400 / ledgers_per_day
        let lpd = Self::get_ledgers_per_day_internal(&env) as u64;
        ledgers_remaining * (86_400u64 / lpd)
    }

    /// Return the IPFS CID stored at engagement creation, or None if not provided.
    pub fn get_metadata_hash(env: Env, engagement_id: String) -> Option<String> {
        Self::get_engagement_internal(&env, &engagement_id).metadata_hash
    }

    /// Return the current approve/reject vote counts for a disputed milestone.
    /// Returns (0, 0) if no votes have been cast yet.
    pub fn get_arbiter_votes(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> ArbiterVoteCounts {
        let vote_key = DataKey::ArbiterVotes(engagement_id, milestone_index);
        let record: ArbiterVoteRecord =
            env.storage()
                .persistent()
                .get(&vote_key)
                .unwrap_or(ArbiterVoteRecord {
                    approve_votes: 0,
                    reject_votes: 0,
                    voted: Vec::new(&env),
                });
        ArbiterVoteCounts {
            approve_votes: record.approve_votes,
            reject_votes: record.reject_votes,
        }
    }

    /// Total amount released for this engagement, represented by
    /// `Engagement.released_amount`.
    ///
    /// This is not the escrow balance. To get remaining contract funds,
    /// use `total_amount - released_amount`; `get_escrow_balance` provides
    /// that derived value.
    pub fn get_total_released(env: Env, engagement_id: String) -> i128 {
        Self::get_engagement_internal(&env, &engagement_id).released_amount
    }

    /// Return a lightweight summary of an engagement, suitable for list/dashboard views.
    ///
    /// Prefer this over `get_engagement` when you only need top-level fields and not
    /// the full milestone list, as it avoids deserializing the milestone vector.
    ///
    /// # Panics
    ///
    /// - `"EngagementNotFound"` — no engagement with the given `engagement_id` exists.
    pub fn get_engagement_summary(env: Env, engagement_id: String) -> EngagementSummary {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        EngagementSummary {
            id: engagement.id,
            job_title: engagement.job_title,
            company: engagement.company,
            recruiter: engagement.recruiter,
            total_amount: engagement.total_amount,
            released_amount: engagement.released_amount,
            status: engagement.status,
            milestone_count: engagement.milestones.len(),
            created_at_ledger: engagement.created_at_ledger,
            co_recruiter: engagement.co_recruiter,
            recruiter_split_bps: engagement.recruiter_split_bps,
            contract_pdf_hash: engagement.contract_pdf_hash,
            referrer: engagement.referrer,
            tags: engagement.tags,
        }
    }

    /// Return lightweight summaries for multiple engagements in a single call,
    /// reducing round-trips for dashboards that need to render many engagements
    /// at once.
    ///
    /// Engagement IDs that do not exist are silently skipped — the returned
    /// vector may be shorter than the input list when some IDs are invalid.
    ///
    /// # Panics
    ///
    /// - `"too many IDs"` — `engagement_ids` contains more than 20 entries.
    pub fn batch_get_engagement_summary(
        env: Env,
        engagement_ids: Vec<String>,
    ) -> Vec<EngagementSummary> {
        if engagement_ids.len() > 20 {
            panic!("too many IDs");
        }
        let mut results: Vec<EngagementSummary> = Vec::new(&env);
        for i in 0..engagement_ids.len() {
            let eid = engagement_ids.get(i).unwrap();
            let maybe: Option<Engagement> = env
                .storage()
                .persistent()
                .get(&DataKey::Engagement(eid.clone()));
            if let Some(engagement) = maybe {
                results.push_back(EngagementSummary {
                    id: engagement.id,
                    job_title: engagement.job_title,
                    company: engagement.company,
                    recruiter: engagement.recruiter,
                    total_amount: engagement.total_amount,
                    released_amount: engagement.released_amount,
                    status: engagement.status,
                    milestone_count: engagement.milestones.len(),
                    created_at_ledger: engagement.created_at_ledger,
                    co_recruiter: engagement.co_recruiter,
                    recruiter_split_bps: engagement.recruiter_split_bps,
                    contract_pdf_hash: engagement.contract_pdf_hash,
                    referrer: engagement.referrer,
                    tags: engagement.tags,
                });
            }
        }
        results
    }

    /// Return the off-chain attestation hash (e.g. SHA-256 of the contract PDF)
    /// stored at engagement creation, or None if not provided.
    /// Read-only and permissionless.
    pub fn get_contract_pdf_hash(env: Env, engagement_id: String) -> Option<String> {
        Self::get_engagement_internal(&env, &engagement_id).contract_pdf_hash
    }

    /// Return the off-chain categorization tags stored at engagement creation
    /// (issue #248). Empty if none were provided. Read-only and permissionless.
    pub fn get_tags(env: Env, engagement_id: String) -> Vec<String> {
        Self::get_engagement_internal(&env, &engagement_id)
            .tags
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ----------------------------------------------------------
    // ARBITER SUCCESSION
    // ----------------------------------------------------------

    /// Current arbiter nominates a successor. The successor must call `claim_arbiter`.
    /// Any arbiter in the engagement's arbiter list may initiate succession for their slot.
    ///
    /// # Panics
    /// - `"engagement is in a terminal state"` — the engagement is `Completed`,
    ///   `Cancelled`, or `Expired`. Arbiter succession has no practical function
    ///   once an engagement can no longer be disputed.
    pub fn nominate_arbiter_successor(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        successor: Address,
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

        let nomination = ArbiterNomination {
            current: arbiter.clone(),
            nominee: successor.clone(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::PendingArbiter(engagement_id.clone()), &nomination);

        env.storage().persistent().extend_ttl(
            &DataKey::PendingArbiter(engagement_id.clone()),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "arbiter_nominated"),
                engagement_id.clone(),
            ),
            successor,
        );
    }

    /// Nominated successor claims the arbiter slot, replacing the nominating arbiter.
    ///
    /// # Panics
    /// - `"engagement is in a terminal state"` — the engagement reached `Completed`,
    ///   `Cancelled`, or `Expired` after the nomination was made; the seat can no
    ///   longer be claimed.
    pub fn claim_arbiter(env: Env, nominee: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        nominee.require_auth();

        let nomination: ArbiterNomination = env
            .storage()
            .persistent()
            .get(&DataKey::PendingArbiter(engagement_id.clone()))
            .unwrap_or_else(|| panic!("no pending arbiter nomination"));

        if nominee != nomination.nominee {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if Self::is_terminal_status(&engagement.status) {
            panic!("engagement is in a terminal state");
        }

        // Replace the nominating arbiter's slot with the nominee.
        for i in 0..engagement.arbiters.len() {
            if engagement.arbiters.get(i).unwrap() == nomination.current {
                engagement.arbiters.set(i, nominee.clone());
                break;
            }
        }

        // Migrate the seat's vote identity on any dispute currently in progress
        // (issue #178). Without this, the old arbiter's cast vote no longer
        // matches any address in `engagement.arbiters`, but the successor's
        // address also isn't in `voted`, so `cast_arbiter_vote`'s duplicate-vote
        // check would let the successor cast a second vote for the same seat.
        for i in 0..engagement.milestones.len() {
            if engagement.milestones.get(i).unwrap().status == MilestoneStatus::Disputed {
                let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), i);
                if let Some(mut record) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, ArbiterVoteRecord>(&vote_key)
                {
                    for j in 0..record.voted.len() {
                        if record.voted.get(j).unwrap() == nomination.current {
                            record.voted.set(j, nominee.clone());
                        }
                    }
                    env.storage().persistent().set(&vote_key, &record);
                }
            }
        }

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.storage()
            .persistent()
            .remove(&DataKey::PendingArbiter(engagement_id.clone()));

        env.events().publish(
            (Symbol::new(&env, "arbiter_claimed"), engagement_id.clone()),
            nominee,
        );
    }

    // ----------------------------------------------------------
    // ADMIN ARBITER REPLACEMENT (issue #245)
    // ----------------------------------------------------------

    /// Emergency, admin-gated replacement of a non-responsive arbiter slot,
    /// bypassing the nominate/claim succession flow. Intended for cases where
    /// an arbiter is unreachable and a dispute is blocked waiting on their vote.
    ///
    /// # Caller
    /// `admin` — must be the current contract admin.
    ///
    /// # Panics
    /// - `"engagement is in a terminal state"` — the engagement is `Completed`,
    ///   `Cancelled`, or `Expired`.
    /// - `"ArbiterNotFound"` — `old_arbiter` is not in the engagement's arbiter list.
    /// - `"CompanyArbiterCollision"` / `"RecruiterArbiterCollision"` — `new_arbiter`
    ///   is the engagement's company or recruiter.
    /// - `"DuplicateArbiter"` — `new_arbiter` is already an arbiter on this engagement.
    pub fn admin_replace_arbiter(
        env: Env,
        admin: Address,
        engagement_id: String,
        old_arbiter: Address,
        new_arbiter: Address,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if Self::is_terminal_status(&engagement.status) {
            panic!("engagement is in a terminal state");
        }

        if new_arbiter == engagement.company {
            panic!("CompanyArbiterCollision");
        }
        if new_arbiter == engagement.recruiter {
            panic!("RecruiterArbiterCollision");
        }
        for i in 0..engagement.arbiters.len() {
            if engagement.arbiters.get(i).unwrap() == new_arbiter {
                panic!("DuplicateArbiter");
            }
        }

        let mut found = false;
        for i in 0..engagement.arbiters.len() {
            if engagement.arbiters.get(i).unwrap() == old_arbiter {
                engagement.arbiters.set(i, new_arbiter.clone());
                found = true;
                break;
            }
        }
        if !found {
            panic!("ArbiterNotFound");
        }

        // Migrate the seat's vote identity on any dispute currently in progress,
        // mirroring `claim_arbiter` (issue #178) — otherwise the replaced
        // arbiter's cast vote no longer matches any address in
        // `engagement.arbiters`, but `new_arbiter` also isn't in `voted`, so
        // the duplicate-vote check would let it cast a second vote for the
        // same seat.
        for i in 0..engagement.milestones.len() {
            if engagement.milestones.get(i).unwrap().status == MilestoneStatus::Disputed {
                let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), i);
                if let Some(mut record) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, ArbiterVoteRecord>(&vote_key)
                {
                    for j in 0..record.voted.len() {
                        if record.voted.get(j).unwrap() == old_arbiter {
                            record.voted.set(j, new_arbiter.clone());
                        }
                    }
                    env.storage().persistent().set(&vote_key, &record);
                }
            }
        }

        // A pending succession nomination for the replaced seat is no longer
        // meaningful once the admin has intervened directly.
        env.storage()
            .persistent()
            .remove(&DataKey::PendingArbiter(engagement_id.clone()));

        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (
                Symbol::new(&env, "arbiter_admin_replaced"),
                engagement_id.clone(),
            ),
            (old_arbiter, new_arbiter),
        );
    }

    // ----------------------------------------------------------
    // AMENDMENT PROPOSAL MANAGEMENT
    // ----------------------------------------------------------

    /// Admin sets the amendment proposal TTL in ledgers.
    /// Default is 17,280 ledgers (~1 day).
    ///
    /// # Arguments
    /// - `admin`   — must be the contract admin
    /// - `ledgers` — number of ledgers before a proposal expires
    pub fn set_amendment_ttl(env: Env, admin: Address, ledgers: u32) {
        admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("admin not set"));

        if admin != stored_admin {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::AmendmentTTL), &ledgers);

        env.storage().persistent().extend_ttl(
            &DataKey::Config(ConfigKey::AmendmentTTL),
            100_000,
            6_300_000,
        );
    }

    /// Get the current amendment proposal TTL in ledgers.
    /// Returns 17,280 if not yet set.
    pub fn get_amendment_ttl(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::AmendmentTTL))
            .unwrap_or(17_280)
    }

    /// Propose a milestone payment-percent amendment.
    /// Either company or recruiter can propose; the other party must accept.
    /// Only one pending proposal may exist per milestone; a new proposal overwrites.
    ///
    /// # Arguments
    /// - `proposer`             — company or recruiter (must sign)
    /// - `engagement_id`        — the engagement
    /// - `milestone_index`      — the milestone to amend
    /// - `new_payment_percent`  — the proposed new payment percent
    pub fn propose_amendment(
        env: Env,
        proposer: Address,
        engagement_id: String,
        milestone_index: u32,
        new_payment_percent: u32,
    ) {
        Self::assert_engagement_not_paused(&env, &engagement_id);
        proposer.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_company(&env, &proposer, &engagement.company)
            && proposer != engagement.recruiter
        {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let _ = Self::get_milestone_or_panic(&engagement, milestone_index);

        if new_payment_percent > 100 {
            panic!("payment percent must be 0-100");
        }

        let current_ledger = env.ledger().sequence();
        let ttl = env
            .storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::AmendmentTTL))
            .unwrap_or(17_280);

        let proposal = AmendmentProposal {
            proposer: proposer.clone(),
            new_payment_percent,
            proposed_at_ledger: current_ledger,
            expires_at_ledger: current_ledger + ttl,
        };

        env.storage().persistent().set(
            &DataKey::AmendmentProposal(engagement_id.clone(), milestone_index),
            &proposal,
        );

        env.storage().persistent().extend_ttl(
            &DataKey::AmendmentProposal(engagement_id.clone(), milestone_index),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "amendment_proposed"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                proposer,
                new_payment_percent,
                current_ledger + ttl,
            ),
        );
    }

    /// Accept a pending amendment proposal, applying the change immediately.
    /// The acceptor must be the other party (not the proposer).
    /// The milestone's payment percent is updated, and an AmendmentEntry is recorded.
    /// If the proposal is expired (current_ledger > expires_at_ledger), reject with "expired".
    ///
    /// # Arguments
    /// - `acceptor`        — company or recruiter (must sign, must NOT be the proposer)
    /// - `engagement_id`   — the engagement
    /// - `milestone_index` — the milestone to accept the amendment for
    pub fn accept_amendment(
        env: Env,
        acceptor: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_engagement_not_paused(&env, &engagement_id);
        acceptor.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_company(&env, &acceptor, &engagement.company)
            && acceptor != engagement.recruiter
        {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let proposal: AmendmentProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AmendmentProposal(
                engagement_id.clone(),
                milestone_index,
            ))
            .unwrap_or_else(|| panic!("no pending amendment proposal"));

        if acceptor == proposal.proposer {
            panic!("proposer cannot accept their own proposal");
        }

        let current_ledger = env.ledger().sequence();

        if current_ledger > proposal.expires_at_ledger {
            env.storage()
                .persistent()
                .remove(&DataKey::AmendmentProposal(
                    engagement_id.clone(),
                    milestone_index,
                ));

            env.events().publish(
                (
                    Symbol::new(&env, "amendment_rejected"),
                    engagement_id.clone(),
                ),
                (milestone_index, acceptor, Symbol::new(&env, "expired")),
            );

            panic!("amendment_expired");
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        let old_payment_percent = milestone.payment_percent;
        milestone.payment_percent = proposal.new_payment_percent;
        engagement.milestones.set(milestone_index, milestone);

        let amendment_entry = AmendmentEntry {
            proposer: proposal.proposer.clone(),
            old_payment_percent,
            new_payment_percent: proposal.new_payment_percent,
            ledger: current_ledger,
        };

        let mut log: Vec<AmendmentEntry> = env
            .storage()
            .persistent()
            .get(&DataKey::AmendmentLog(
                engagement_id.clone(),
                milestone_index,
            ))
            .unwrap_or_else(|| Vec::new(&env));

        log.push_back(amendment_entry);

        if log.len() > 20 {
            log.remove(0);
        }

        env.storage().persistent().set(
            &DataKey::AmendmentLog(engagement_id.clone(), milestone_index),
            &log,
        );

        env.storage().persistent().extend_ttl(
            &DataKey::AmendmentLog(engagement_id.clone(), milestone_index),
            100_000,
            6_300_000,
        );

        env.storage()
            .persistent()
            .remove(&DataKey::AmendmentProposal(
                engagement_id.clone(),
                milestone_index,
            ));

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (
                Symbol::new(&env, "amendment_accepted"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                acceptor,
                old_payment_percent,
                proposal.new_payment_percent,
            ),
        );
    }

    /// Reject a pending amendment proposal.
    /// Either company or recruiter can reject (if they're not the proposer).
    /// On rejection, the proposal is cleared and amendment_rejected event is emitted.
    ///
    /// # Arguments
    /// - `rejector`        — company or recruiter (must sign, must NOT be the proposer)
    /// - `engagement_id`   — the engagement
    /// - `milestone_index` — the milestone with the proposal to reject
    pub fn reject_amendment(
        env: Env,
        rejector: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_engagement_not_paused(&env, &engagement_id);
        rejector.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_company(&env, &rejector, &engagement.company)
            && rejector != engagement.recruiter
        {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let proposal: AmendmentProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AmendmentProposal(
                engagement_id.clone(),
                milestone_index,
            ))
            .unwrap_or_else(|| panic!("no pending amendment proposal"));

        if rejector == proposal.proposer {
            panic!("proposer cannot reject their own proposal");
        }

        env.storage()
            .persistent()
            .remove(&DataKey::AmendmentProposal(
                engagement_id.clone(),
                milestone_index,
            ));

        env.events().publish(
            (
                Symbol::new(&env, "amendment_rejected"),
                engagement_id.clone(),
            ),
            (milestone_index, rejector, Symbol::new(&env, "declined")),
        );
    }

    /// Withdraw a pending amendment proposal (issue #238).
    ///
    /// Lets the original proposer cancel their own proposal before the
    /// counterparty responds, instead of leaving stale terms on the table until
    /// the TTL expires. Complements `accept_amendment` / `reject_amendment`,
    /// which are both restricted to the *other* party.
    ///
    /// # Caller
    /// `proposer` — must be the address recorded on the pending proposal and
    /// sign the transaction. Being the engagement's company or recruiter is not
    /// sufficient; only whoever actually made this proposal may withdraw it.
    ///
    /// # Behaviour
    /// Clears the pending proposal, after which `get_pending_amendment` reports
    /// `None` and a fresh proposal can be made for the same milestone. The
    /// amendment log is untouched — a withdrawn proposal was never applied, so
    /// it is not part of the milestone's amendment history.
    ///
    /// An expired-but-uncleared proposal can still be withdrawn: doing so is
    /// exactly the storage cleanup the caller intends, and refusing would leave
    /// the entry stranded.
    ///
    /// # Panics
    /// - `"no pending amendment proposal"` — no proposal exists for this milestone.
    /// - `"unauthorized"` — caller is not the proposal's original proposer.
    ///
    /// # Events
    /// Emits `("amendment_withdrawn", engagement_id)` with
    /// `(milestone_index, proposer, new_payment_percent)`. The proposed percent
    /// is included so indexers can retire the pending change they were tracking
    /// without a prior read.
    pub fn withdraw_amendment_proposal(
        env: Env,
        proposer: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        proposer.require_auth();

        let proposal_key = DataKey::AmendmentProposal(engagement_id.clone(), milestone_index);
        let proposal: AmendmentProposal = env
            .storage()
            .persistent()
            .get(&proposal_key)
            .unwrap_or_else(|| panic!("no pending amendment proposal"));

        if proposer != proposal.proposer {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        env.storage().persistent().remove(&proposal_key);

        env.events().publish(
            (
                Symbol::new(&env, "amendment_withdrawn"),
                engagement_id.clone(),
            ),
            (milestone_index, proposer, proposal.new_payment_percent),
        );
    }

    // ----------------------------------------------------------
    // ISSUES #242, #243, #244 — FEEDBACK RATINGS
    // ----------------------------------------------------------

    /// Company submits a 1–5 feedback rating for the recruiter after the
    /// engagement completes (issue #242).
    ///
    /// # Caller
    /// `company` — must match the engagement's company and sign the transaction.
    ///
    /// # Behaviour
    /// The rating is folded into a running tally keyed by the **recruiter's
    /// address**, not the engagement, so reputation accumulates across every
    /// engagement that recruiter completes. Query it with
    /// `get_recruiter_rating`.
    ///
    /// Each engagement contributes at most one rating: a second call for the
    /// same engagement is rejected, so a company cannot inflate or bury a
    /// recruiter's score by rating one job repeatedly.
    ///
    /// The rating is credited to whoever is the recruiter at completion time.
    /// If the recruiter role was transferred mid-engagement (see
    /// `accept_recruiter_transfer`), the incoming address receives it, matching
    /// where the milestone payouts went.
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused.
    /// - `"engagement not found"` — no engagement with this ID.
    /// - `"EngagementNotCompleted"` — the engagement has not reached `Completed`.
    /// - `"unauthorized"` — caller is not the engagement's company.
    /// - `"InvalidRating"` — `rating` is outside 1–5.
    /// - `"AlreadyRated"` — this engagement's recruiter rating was already submitted.
    ///
    /// # Events
    /// Emits `("recruiter_rated", engagement_id)` with
    /// `(recruiter, rating, new_count)`.
    pub fn submit_recruiter_rating(env: Env, company: Address, engagement_id: String, rating: u32) {
        Self::assert_not_paused(&env);
        company.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Completed {
            panic!("EngagementNotCompleted");
        }

        if company != engagement.company {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        Self::assert_valid_rating(rating);

        let rated_key = DataKey::RecruiterRated(engagement_id.clone());
        if env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&rated_key)
            .unwrap_or(false)
        {
            panic!("AlreadyRated");
        }

        let new_count = Self::record_rating(
            &env,
            &DataKey::RecruiterRating(engagement.recruiter.clone()),
            rating,
        );

        env.storage().persistent().set(&rated_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&rated_key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "recruiter_rated"), engagement_id.clone()),
            (engagement.recruiter, rating, new_count),
        );
    }

    /// Recruiter submits a 1–5 feedback rating for the company after the
    /// engagement completes (issue #243). Mirror of
    /// [`Self::submit_recruiter_rating`].
    ///
    /// # Caller
    /// `recruiter` — must match the engagement's recruiter and sign the
    /// transaction.
    ///
    /// # Behaviour
    /// The rating is folded into a running tally keyed by the **company's
    /// address**, accumulating across every engagement that company completes.
    /// Query it with `get_company_rating`. Each engagement contributes at most
    /// one company rating, independent of the recruiter rating for the same
    /// engagement — both sides may rate each other exactly once.
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused.
    /// - `"engagement not found"` — no engagement with this ID.
    /// - `"EngagementNotCompleted"` — the engagement has not reached `Completed`.
    /// - `"unauthorized"` — caller is not the engagement's recruiter.
    /// - `"InvalidRating"` — `rating` is outside 1–5.
    /// - `"AlreadyRated"` — this engagement's company rating was already submitted.
    ///
    /// # Events
    /// Emits `("company_rated", engagement_id)` with
    /// `(company, rating, new_count)`.
    pub fn submit_company_rating(env: Env, recruiter: Address, engagement_id: String, rating: u32) {
        Self::assert_not_paused(&env);
        recruiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Completed {
            panic!("EngagementNotCompleted");
        }

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        Self::assert_valid_rating(rating);

        let rated_key = DataKey::CompanyRated(engagement_id.clone());
        if env
            .storage()
            .persistent()
            .get::<DataKey, bool>(&rated_key)
            .unwrap_or(false)
        {
            panic!("AlreadyRated");
        }

        let new_count = Self::record_rating(
            &env,
            &DataKey::CompanyRating(engagement.company.clone()),
            rating,
        );

        env.storage().persistent().set(&rated_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&rated_key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "company_rated"), engagement_id.clone()),
            (engagement.company, rating, new_count),
        );
    }

    /// Return a recruiter's aggregate feedback rating (issue #244).
    ///
    /// An address that has never been rated returns a zeroed summary
    /// (`average_x100: 0, count: 0, total_score: 0`) rather than panicking, so
    /// callers can render new recruiters without a prior existence check.
    /// Always check `count` before presenting `average_x100` — a `0` average
    /// means "no ratings yet", not "rated zero".
    ///
    /// Read-only and permissionless.
    pub fn get_recruiter_rating(env: Env, recruiter: Address) -> RatingSummary {
        Self::rating_summary(&env, &DataKey::RecruiterRating(recruiter))
    }

    /// Return a company's aggregate feedback rating (issue #244).
    /// Mirror of [`Self::get_recruiter_rating`]; the same zeroed-summary and
    /// `count`-before-`average_x100` notes apply.
    ///
    /// Read-only and permissionless.
    pub fn get_company_rating(env: Env, company: Address) -> RatingSummary {
        Self::rating_summary(&env, &DataKey::CompanyRating(company))
    }

    /// Return whether the recruiter has already been rated for this engagement
    /// (issue #242), so a UI can hide the rating prompt instead of surfacing an
    /// `"AlreadyRated"` failure.
    pub fn is_recruiter_rated(env: Env, engagement_id: String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::RecruiterRated(engagement_id))
            .unwrap_or(false)
    }

    /// Return whether the company has already been rated for this engagement
    /// (issue #243). Companion to [`Self::is_recruiter_rated`].
    pub fn is_company_rated(env: Env, engagement_id: String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::CompanyRated(engagement_id))
            .unwrap_or(false)
    }

    // ----------------------------------------------------------
    // AMENDMENT LOG QUERIES
    // ----------------------------------------------------------

    /// Get the amendment history for a milestone.
    /// Returns entries in chronological order (oldest first).
    /// Capped at 20 entries per milestone (FIFO eviction).
    ///
    /// # Arguments
    /// - `engagement_id`   — the engagement
    /// - `milestone_index` — the milestone
    pub fn get_amendment_log(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Vec<AmendmentEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::AmendmentLog(engagement_id, milestone_index))
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Get the current pending amendment proposal for a milestone, if one
    /// exists and has not expired. Returns `None` when there is no active
    /// proposal or the proposal's TTL has elapsed (treated as non-existent).
    pub fn get_pending_amendment(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<PendingAmendment> {
        let key = DataKey::AmendmentProposal(engagement_id, milestone_index);
        match env.storage().persistent().get::<_, AmendmentProposal>(&key) {
            Some(proposal) if env.ledger().sequence() <= proposal.expires_at_ledger => {
                Some(proposal)
            }
            _ => None,
        }
    }

    // ----------------------------------------------------------
    // MILESTONE EXTENSION REQUEST (issue #247)
    // ----------------------------------------------------------

    /// Admin sets the milestone extension proposal TTL in ledgers.
    /// Default is 17,280 ledgers (~1 day).
    pub fn set_extension_ttl(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);

        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::MilestoneExtensionTTL), &ledgers);

        env.storage().persistent().extend_ttl(
            &DataKey::Config(ConfigKey::MilestoneExtensionTTL),
            100_000,
            6_300_000,
        );
    }

    /// Get the current milestone extension proposal TTL in ledgers.
    /// Returns `DEFAULT_EXTENSION_TTL` (17,280) if not yet set.
    pub fn get_extension_ttl(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::MilestoneExtensionTTL))
            .unwrap_or(DEFAULT_EXTENSION_TTL)
    }

    /// Recruiter requests additional ledgers be added to a Locked retention
    /// milestone's unlock deadline (`valid_after_ledger`), subject to company
    /// approval. Mirrors the propose-then-accept shape of `propose_amendment`
    /// / `accept_amendment`, but is one-directional: only the recruiter may
    /// propose, and only the company may accept or reject.
    ///
    /// Only one pending extension proposal may exist per milestone; a new
    /// proposal overwrites any existing one.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's recruiter.
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"only retention milestones can be extended"` — the milestone is a
    ///   `Placement` milestone.
    /// - `"milestone is not locked"` — the milestone has already unlocked
    ///   (or otherwise progressed) and no longer has a meaningful deadline to extend.
    /// - `"additional ledgers must be greater than zero"` — `additional_ledgers` is 0.
    pub fn propose_milestone_extension(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
        additional_ledgers: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.kind != MilestoneKind::Retention {
            panic!("only retention milestones can be extended");
        }

        if milestone.status != MilestoneStatus::Locked {
            panic!("milestone is not locked");
        }

        if additional_ledgers == 0 {
            panic!("additional ledgers must be greater than zero");
        }

        // Issue #323: reject once this milestone has already been granted
        // the configured maximum number of extensions.
        let granted_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::MilestoneExtensionCount(
                engagement_id.clone(),
                milestone_index,
            ))
            .unwrap_or(0);
        if granted_count >= Self::get_max_milestone_extensions_internal(&env) {
            panic!("MilestoneExtensionLimitReached");
        }

        let current_ledger = env.ledger().sequence();
        let ttl = Self::get_extension_ttl(env.clone());

        let proposal = MilestoneExtensionProposal {
            proposer: recruiter.clone(),
            additional_ledgers,
            proposed_at_ledger: current_ledger,
            expires_at_ledger: current_ledger + ttl,
        };

        env.storage().persistent().set(
            &DataKey::MilestoneExtensionProposal(engagement_id.clone(), milestone_index),
            &proposal,
        );

        env.storage().persistent().extend_ttl(
            &DataKey::MilestoneExtensionProposal(engagement_id.clone(), milestone_index),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "milestone_extension_proposed"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                recruiter,
                additional_ledgers,
                current_ledger + ttl,
            ),
        );
    }

    /// Company accepts a pending milestone extension proposal, applying the
    /// extension immediately by adding `additional_ledgers` to the milestone's
    /// `valid_after_ledger`.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's company.
    /// - `"no pending milestone extension proposal"` — no proposal exists for
    ///   this milestone.
    /// - `"milestone_extension_expired"` — the proposal's TTL has elapsed;
    ///   the proposal is cleared and an `milestone_extension_rejected` event
    ///   is emitted before panicking.
    /// - `"milestone is not locked"` — the milestone progressed since the
    ///   proposal was made and no longer has a deadline to extend.
    pub fn accept_milestone_extension(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if company != engagement.company {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let proposal_key =
            DataKey::MilestoneExtensionProposal(engagement_id.clone(), milestone_index);
        let proposal: MilestoneExtensionProposal = env
            .storage()
            .persistent()
            .get(&proposal_key)
            .unwrap_or_else(|| panic!("no pending milestone extension proposal"));

        let current_ledger = env.ledger().sequence();

        if current_ledger > proposal.expires_at_ledger {
            env.storage().persistent().remove(&proposal_key);

            env.events().publish(
                (
                    Symbol::new(&env, "milestone_extension_rejected"),
                    engagement_id.clone(),
                ),
                (milestone_index, company, Symbol::new(&env, "expired")),
            );

            panic!("milestone_extension_expired");
        }

        let mut milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.status != MilestoneStatus::Locked {
            panic!("milestone is not locked");
        }

        let old_valid_after_ledger = milestone.valid_after_ledger;
        milestone.valid_after_ledger += proposal.additional_ledgers;
        let new_valid_after_ledger = milestone.valid_after_ledger;
        engagement.milestones.set(milestone_index, milestone);
        engagement.last_activity_ledger = current_ledger;

        // The deadline just moved out, so any due-soon notification already
        // emitted referred to the old one — reset so the new deadline can be
        // announced in its own right. See issue #241.
        Self::clear_due_soon_flag(&env, &engagement_id, milestone_index);

        env.storage().persistent().remove(&proposal_key);

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        // Issue #322 / #323: track that an extension was granted for this
        // milestone, both for the aggregate query and the per-milestone cap.
        let extension_count_key =
            DataKey::MilestoneExtensionCount(engagement_id.clone(), milestone_index);
        let new_extension_count: u32 = env
            .storage()
            .persistent()
            .get(&extension_count_key)
            .unwrap_or(0)
            + 1;
        env.storage()
            .persistent()
            .set(&extension_count_key, &new_extension_count);
        env.storage()
            .persistent()
            .extend_ttl(&extension_count_key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "milestone_extension_accepted"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                company,
                old_valid_after_ledger,
                new_valid_after_ledger,
            ),
        );
    }

    /// Company rejects a pending milestone extension proposal.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's company.
    /// - `"no pending milestone extension proposal"` — no proposal exists for
    ///   this milestone.
    pub fn reject_milestone_extension(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if company != engagement.company {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let proposal_key =
            DataKey::MilestoneExtensionProposal(engagement_id.clone(), milestone_index);
        let _: MilestoneExtensionProposal = env
            .storage()
            .persistent()
            .get(&proposal_key)
            .unwrap_or_else(|| panic!("no pending milestone extension proposal"));

        env.storage().persistent().remove(&proposal_key);

        env.events().publish(
            (
                Symbol::new(&env, "milestone_extension_rejected"),
                engagement_id.clone(),
            ),
            (milestone_index, company, Symbol::new(&env, "declined")),
        );
    }

    /// Get the current pending milestone extension proposal for a milestone,
    /// if one exists and has not expired. Returns `None` when there is no
    /// active proposal or the proposal's TTL has elapsed (treated as non-existent).
    pub fn get_pending_milestone_extension(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<MilestoneExtensionProposal> {
        let key = DataKey::MilestoneExtensionProposal(engagement_id, milestone_index);
        match env
            .storage()
            .persistent()
            .get::<_, MilestoneExtensionProposal>(&key)
        {
            Some(proposal) if env.ledger().sequence() <= proposal.expires_at_ledger => {
                Some(proposal)
            }
            _ => None,
        }
    }

    /// Return the total number of milestone extensions granted (accepted)
    /// across every milestone of an engagement (issue #322).
    pub fn get_milestone_extension_count(env: Env, engagement_id: String) -> u32 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let mut total: u32 = 0;
        for i in 0..engagement.milestones.len() {
            total += env
                .storage()
                .persistent()
                .get(&DataKey::MilestoneExtensionCount(engagement_id.clone(), i))
                .unwrap_or(0u32);
        }
        total
    }

    /// Admin sets the maximum number of milestone extensions that may be
    /// granted per milestone. Defaults to 3 when not explicitly configured.
    /// See issue #323.
    pub fn set_max_milestone_extensions(env: Env, admin: Address, count: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxMilestoneExtensions), &count);
        env.events()
            .publish((Symbol::new(&env, "max_milestone_extensions_set"),), count);
    }

    /// Return the current maximum milestone extension count cap.
    /// Returns `DEFAULT_MAX_MILESTONE_EXTENSIONS` (3) when not configured.
    pub fn get_max_milestone_extensions(env: Env) -> u32 {
        Self::get_max_milestone_extensions_internal(&env)
    }

    // ----------------------------------------------------------
    // ISSUE #26 — TOKEN ALLOWLIST
    // ----------------------------------------------------------

    /// Add a token SAC address to the allowlist. Admin only.
    ///
    /// Note (issue #175): the contract treats `total_amount`, milestone payouts,
    /// and `MinEngagementAmount` as raw integer units of whichever token is used
    /// for a given engagement — it never queries or adjusts for that token's
    /// `decimals()`. Percentage-based math (`amount * payment_percent / 100`) is
    /// exact integer arithmetic regardless of decimals, but a single admin-wide
    /// `MinEngagementAmount` (see [`Self::set_min_amount`]) means the effective
    /// real-world minimum will differ across tokens with different precision.
    /// Only allowlist tokens whose smallest unit is comparable in scale to what
    /// `MinEngagementAmount` assumes, or adjust the minimum accordingly.
    pub fn add_allowed_token(env: Env, admin: Address, token: Address) {
        Self::assert_admin(&env, &admin);
        let mut allowed: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env));
        let already = (0..allowed.len()).any(|i| allowed.get(i).unwrap() == token);
        if !already {
            allowed.push_back(token.clone());
            env.storage()
                .persistent()
                .set(&DataKey::AllowedTokens, &allowed);
        }
        env.events()
            .publish((Symbol::new(&env, "token_allowlisted"),), token);
    }

    /// Remove a token SAC address from the allowlist. Admin only.
    pub fn remove_allowed_token(env: Env, admin: Address, token: Address) {
        Self::assert_admin(&env, &admin);
        let mut allowed: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env));
        for i in 0..allowed.len() {
            if allowed.get(i).unwrap() == token {
                allowed.remove(i);
                break;
            }
        }
        env.storage()
            .persistent()
            .set(&DataKey::AllowedTokens, &allowed);
        env.events()
            .publish((Symbol::new(&env, "token_removed"),), token);
    }

    /// Enable or disable the token allowlist. Admin only.
    pub fn set_token_allowlist_enabled(env: Env, admin: Address, enabled: bool) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::AllowlistEnabled, &enabled);
        env.events()
            .publish((Symbol::new(&env, "allowlist_enabled_set"),), enabled);
    }

    /// Return all currently allowlisted token addresses.
    pub fn get_allowed_tokens(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ----------------------------------------------------------
    // ISSUE #32 — RECRUITER EARLY-EXIT
    // ----------------------------------------------------------

    /// Signal that the recruiter wishes to exit the engagement early without completing
    /// all remaining milestones.
    ///
    /// This is the first step of the three-step early-exit protocol. After calling this,
    /// the company must respond with [`accept_early_exit`] or [`reject_early_exit`].
    ///
    /// # Caller
    /// `recruiter` — must match the engagement's recruiter and sign the transaction.
    ///
    /// # Behaviour
    /// Sets the engagement status to `ExitRequested`. The company must then call
    /// [`accept_early_exit`] or [`reject_early_exit`].
    ///
    /// # Panics
    /// - `"engagement is not active"` — engagement is not `Active`.
    /// - `"unauthorized"` — caller is not the recruiter.
    ///
    /// # Events
    /// Emits `("early_exit_requested", engagement_id)`.
    pub fn request_early_exit(env: Env, recruiter: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::ExitRequested;
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

        env.events().publish(
            (
                Symbol::new(&env, "early_exit_requested"),
                engagement_id.clone(),
            ),
            recruiter,
        );
    }

    /// Accept the recruiter's early-exit request.
    ///
    /// This is the second step of the early-exit protocol, called by the company in
    /// response to a prior [`request_early_exit`]. See also [`reject_early_exit`] for
    /// the alternative resolution.
    ///
    /// # Caller
    /// `company` — must match the engagement's company and sign the transaction.
    ///
    /// # Behaviour
    /// - Pays the recruiter for every milestone already `Confirmed` or `Resolved`
    ///   (proportional to `released_amount`).
    /// - Refunds the remaining escrow balance (`total_amount - released_amount`) to the company.
    /// - Sets the engagement status to `Cancelled`.
    ///
    /// # Panics
    /// - `"no exit request pending"` — engagement is not in `ExitRequested` status.
    /// - `"unauthorized"` — caller is not the company.
    ///
    /// # Events
    /// Emits `("early_exit_accepted", engagement_id)` with the refund amount.
    pub fn accept_early_exit(env: Env, company: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        Self::assert_exit_request_pending(&env, &company, &engagement);

        let refund = engagement.total_amount - engagement.released_amount;
        if refund > 0 {
            let token_client = token::Client::new(&env, &engagement.token);
            token_client.transfer(
                &env.current_contract_address(),
                &engagement.company,
                &refund,
            );
            Self::on_escrow_lifecycle_checkpoint(
                &env,
                &engagement_id,
                EscrowLifecycleAction::Refunded,
                -refund,
                &engagement.token,
                &engagement.company,
                &engagement.recruiter,
            );
        }

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Cancelled;
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

        Self::decrement_company_active_count(&env, &engagement.company);

        env.events().publish(
            (
                Symbol::new(&env, "early_exit_accepted"),
                engagement_id.clone(),
            ),
            refund,
        );
    }

    /// Reject the recruiter's early-exit request, returning the engagement to `Active`.
    ///
    /// This is the second step of the early-exit protocol, called by the company in
    /// response to a prior [`request_early_exit`]. See also [`accept_early_exit`] for
    /// the alternative resolution.
    ///
    /// # Caller
    /// `company` — must match the engagement's company and sign the transaction.
    ///
    /// # Behaviour
    /// Sets the engagement status back to `Active`. The recruiter must continue working
    /// through the remaining milestones.
    ///
    /// # Panics
    /// - `"no exit request pending"` — engagement is not in `ExitRequested` status.
    /// - `"unauthorized"` — caller is not the company.
    ///
    /// # Events
    /// Emits `("early_exit_rejected", engagement_id)`.
    pub fn reject_early_exit(env: Env, company: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        Self::assert_exit_request_pending(&env, &company, &engagement);

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Active;
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

        env.events().publish(
            (
                Symbol::new(&env, "early_exit_rejected"),
                engagement_id.clone(),
            ),
            company,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #34 — ENGAGEMENT COUNT
    // ----------------------------------------------------------

    /// Return the total number of engagements ever created.
    pub fn get_engagement_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::EngagementCount)
            .unwrap_or(0u64)
    }

    // ----------------------------------------------------------
    // ISSUE #35 — ENGAGEMENT LIST BY COMPANY
    // ----------------------------------------------------------

    /// Return a paginated slice of engagement IDs for a given company.
    /// `page` is 0-indexed; out-of-range pages return an empty vec.
    ///
    /// # Examples
    ///
    /// First page of 10 engagement IDs for a company:
    ///
    /// ```text
    /// get_engagements_by_company(env, company, 0, 10)
    /// ```
    ///
    /// Second page (IDs 10–19) with the same page size:
    ///
    /// ```text
    /// get_engagements_by_company(env, company, 1, 10)
    /// ```
    ///
    /// A `page_size` of `0` or a `page` past the end of the list returns an empty vec.
    pub fn get_engagements_by_company(
        env: Env,
        company: Address,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyEngagements(company))
            .unwrap_or_else(|| Vec::new(&env));

        let total = ids.len();
        if page_size == 0 {
            return Vec::new(&env);
        }
        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps to an out-of-range start (caught below) instead of wrapping
        // around via u32 overflow.
        let start = page.saturating_mul(page_size);
        if start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(ids.get(i).unwrap());
        }
        result
    }
    /// Return the total number of engagements associated with a company.
    pub fn get_company_engagement_count(env: Env, company: Address) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyEngagements(company))
            .unwrap_or_else(|| Vec::new(&env));
        ids.len()
    }

    // ----------------------------------------------------------
    // ISSUE #36 — ENGAGEMENT LIST BY RECRUITER
    // ----------------------------------------------------------

    /// Return a paginated slice of engagement IDs for a given recruiter.
    /// `page` is 0-indexed; out-of-range pages return an empty vec.
    pub fn get_engagements_by_recruiter(
        env: Env,
        recruiter: Address,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterEngagements(recruiter))
            .unwrap_or_else(|| Vec::new(&env));

        let total = ids.len();
        if page_size == 0 {
            return Vec::new(&env);
        }
        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps to an out-of-range start (caught below) instead of wrapping
        // around via u32 overflow.
        let start = page.saturating_mul(page_size);
        if start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(ids.get(i).unwrap());
        }
        result
    }

    /// Return the total number of engagements associated with a recruiter.
    pub fn get_recruiter_engagement_count(env: Env, recruiter: Address) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterEngagements(recruiter))
            .unwrap_or_else(|| Vec::new(&env));
        ids.len()
    }

    // ----------------------------------------------------------
    // ISSUE #249 — ENGAGEMENT TAGS
    // ----------------------------------------------------------

    /// Add a tag to an engagement. Only the engagement's company (or its
    /// registered co-signer) may tag the engagement.
    /// Duplicate tags are silently ignored.
    pub fn add_engagement_tag(env: Env, caller: Address, engagement_id: String, tag: String) {
        caller.require_auth();
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if !Self::is_authorized_company(&env, &caller, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey::TagEngagements(tag.clone());
        let mut ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        for i in 0..ids.len() {
            if ids.get(i).unwrap() == engagement_id {
                return;
            }
        }
        // Issue #321: this is the tag's first engagement, so it just became
        // "in use" — track it in the global distinct-tag registry.
        if ids.is_empty() {
            Self::track_tag_used(&env, &tag);
        }
        ids.push_back(engagement_id.clone());
        env.storage().persistent().set(&key, &ids);
        env.events().publish(
            (Symbol::new(&env, "engagement_tag_added"), tag),
            engagement_id,
        );
    }

    /// Remove a tag from an engagement. Only the engagement's company (or its
    /// registered co-signer) may remove a tag.
    /// No-op if the tag was not present.
    pub fn remove_engagement_tag(env: Env, caller: Address, engagement_id: String, tag: String) {
        caller.require_auth();
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if !Self::is_authorized_company(&env, &caller, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let key = DataKey::TagEngagements(tag.clone());
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env));
        let mut new_ids = Vec::new(&env);
        let mut found = false;
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            if id == engagement_id {
                found = true;
            } else {
                new_ids.push_back(id);
            }
        }
        if found {
            env.storage().persistent().set(&key, &new_ids);
            // Issue #321: no engagements left under this tag — it's no
            // longer "in use", so drop it from the global tag registry.
            if new_ids.is_empty() {
                Self::untrack_tag_used(&env, &tag);
            }
            env.events().publish(
                (Symbol::new(&env, "engagement_tag_removed"), tag),
                engagement_id,
            );
        }
    }

    /// Rename a tag across its entire engagement index (issue #320). Every
    /// engagement currently listed under `old_tag` is moved to `new_tag`,
    /// merging with (and de-duplicating against) any engagements `new_tag`
    /// already lists. Returns the resulting size of the `new_tag` index.
    ///
    /// # Caller
    /// Either the platform admin, or a caller authorized (as company or its
    /// registered co-signer — see `is_authorized_company`) for *every*
    /// engagement currently under `old_tag`. A company cannot rename a tag
    /// that also covers another company's engagements.
    ///
    /// # Panics
    /// - `"TagEmpty"` — `new_tag` is empty.
    /// - `"TagTooLong"` — `new_tag` exceeds `MAX_TAG_LENGTH` characters.
    /// - `"NewTagSameAsOld"` — `old_tag` and `new_tag` are identical.
    /// - `"TagNotFound"` — `old_tag` has no engagements indexed under it.
    /// - `"unauthorized"` — caller is not the admin and is not authorized
    ///   for every engagement currently under `old_tag`.
    pub fn rename_engagement_tag(
        env: Env,
        caller: Address,
        old_tag: String,
        new_tag: String,
    ) -> u32 {
        Self::assert_not_paused(&env);
        caller.require_auth();

        if new_tag.is_empty() {
            panic!("TagEmpty");
        }
        if new_tag.len() > MAX_TAG_LENGTH {
            panic!("TagTooLong");
        }
        if old_tag == new_tag {
            panic!("NewTagSameAsOld");
        }

        let old_key = DataKey::EngagementTag(old_tag.clone());
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&old_key)
            .unwrap_or_else(|| Vec::new(&env));

        if ids.is_empty() {
            panic!("TagNotFound");
        }

        let is_admin = caller == Self::get_admin_internal(&env);
        if !is_admin {
            for i in 0..ids.len() {
                let engagement = Self::get_engagement_internal(&env, &ids.get(i).unwrap());
                if !Self::is_authorized_company(&env, &caller, &engagement.company) {
                    panic!("{}", ERR_UNAUTHORIZED);
                }
            }
        }

        let new_key = DataKey::EngagementTag(new_tag.clone());
        let mut new_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&new_key)
            .unwrap_or_else(|| Vec::new(&env));

        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            let mut already_present = false;
            for j in 0..new_ids.len() {
                if new_ids.get(j).unwrap() == id {
                    already_present = true;
                    break;
                }
            }
            if !already_present {
                new_ids.push_back(id);
            }
        }

        env.storage().persistent().set(&new_key, &new_ids);
        env.storage()
            .persistent()
            .extend_ttl(&new_key, 100_000, 6_300_000);
        env.storage().persistent().remove(&old_key);

        Self::untrack_tag_used(&env, &old_tag);
        Self::track_tag_used(&env, &new_tag);

        let new_count = new_ids.len();
        env.events().publish(
            (Symbol::new(&env, "engagement_tag_renamed"), old_tag),
            (new_tag, new_count),
        );

        new_count
    }

    /// Return the full set of distinct tags currently in use — i.e. every
    /// tag with at least one engagement in its index (issue #321).
    /// Read-only and permissionless. Order is insertion order, not sorted.
    pub fn get_all_tags(env: Env) -> Vec<String> {
        env.storage()
            .persistent()
            .get(&DataKey::AllTags)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Return a paginated slice of engagement IDs currently in a given status
    /// (issue #237). `page` is 0-indexed; out-of-range pages return an empty vec.
    pub fn get_engagement_ids_by_status(
        env: Env,
        status: EngagementStatus,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let mut result = Vec::new(&env);
        if page_size == 0 {
            return result;
        }

        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps instead of wrapping around via u32 overflow.
        let start = page.saturating_mul(page_size);
        let end = start.saturating_add(page_size);

        // Walk the index counting matches, collecting only those whose position
        // within the filtered sequence falls inside the requested page.
        let mut matched: u32 = 0;
        for i in 0..ids.len() {
            if matched >= end {
                break;
            }
            let id = ids.get(i).unwrap();
            let engagement: Engagement = match env
                .storage()
                .persistent()
                .get(&DataKey::Engagement(id.clone()))
            {
                Some(e) => e,
                // An entry whose record has since expired from storage is
                // skipped rather than treated as a match.
                None => continue,
            };
            if engagement.status == status {
                if matched >= start {
                    result.push_back(id);
                }
                matched += 1;
            }
        }

        result
    }

    /// Return the total number of engagements currently in a given status
    /// (issue #237). Companion to `get_engagement_ids_by_status` for sizing
    /// pagination, mirroring `get_company_engagement_count`.
    ///
    /// Carries the same scan cost and index-coverage caveats as
    /// `get_engagement_ids_by_status`.
    pub fn get_engagement_count_by_status(env: Env, status: EngagementStatus) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        let mut count: u32 = 0;
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            if let Some(engagement) = env
                .storage()
                .persistent()
                .get::<DataKey, Engagement>(&DataKey::Engagement(id))
            {
                if engagement.status == status {
                    count += 1;
                }
            }
        }
        count
    }

    // ----------------------------------------------------------
    // ISSUE #351 — ENGAGEMENT LIST BY AMOUNT RANGE
    // ----------------------------------------------------------

    /// Return a paginated slice of engagement IDs whose `total_amount` falls
    /// within `[min_amount, max_amount]` (inclusive on both ends). `page` is
    /// 0-indexed; out-of-range pages return an empty vec.
    ///
    /// Carries the same linear-scan cost and index-coverage caveats as
    /// `get_engagement_ids_by_status` — an entry whose record has expired
    /// from storage is skipped rather than treated as a match.
    ///
    /// # Panics
    /// - `"InvalidAmountRange"` — `min_amount > max_amount`.
    pub fn get_engagements_by_amount_range(
        env: Env,
        min_amount: i128,
        max_amount: i128,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let mut result = Vec::new(&env);
        if page_size == 0 {
            return result;
        }
        if min_amount > max_amount {
            panic!("InvalidAmountRange");
        }

        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps instead of wrapping around via u32 overflow.
        let start = page.saturating_mul(page_size);
        let end = start.saturating_add(page_size);

        // Walk the index counting matches, collecting only those whose position
        // within the filtered sequence falls inside the requested page.
        let mut matched: u32 = 0;
        for i in 0..ids.len() {
            if matched >= end {
                break;
            }
            let id = ids.get(i).unwrap();
            let engagement: Engagement = match env
                .storage()
                .persistent()
                .get(&DataKey::Engagement(id.clone()))
            {
                Some(e) => e,
                None => continue,
            };
            if engagement.total_amount >= min_amount && engagement.total_amount <= max_amount {
                if matched >= start {
                    result.push_back(id);
                }
                matched += 1;
            }
        }

        result
    }

    /// Return the total number of engagements whose `total_amount` falls
    /// within `[min_amount, max_amount]` (inclusive). Companion to
    /// `get_engagements_by_amount_range` for sizing pagination.
    ///
    /// # Panics
    /// - `"InvalidAmountRange"` — `min_amount > max_amount`.
    pub fn get_engagement_count_by_amount(env: Env, min_amount: i128, max_amount: i128) -> u32 {
        if min_amount > max_amount {
            panic!("InvalidAmountRange");
        }

        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        let mut count: u32 = 0;
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            if let Some(engagement) = env
                .storage()
                .persistent()
                .get::<DataKey, Engagement>(&DataKey::Engagement(id))
            {
                if engagement.total_amount >= min_amount && engagement.total_amount <= max_amount {
                    count += 1;
                }
            }
        }
        count
    }

    // ----------------------------------------------------------
    // ISSUE #249 — ENGAGEMENT LIST BY TAG
    // ----------------------------------------------------------

    /// Return a paginated slice of engagement IDs associated with a given tag.
    /// `page` is 0-indexed; out-of-range pages return an empty vec.
    pub fn get_engagements_by_tag(env: Env, tag: String, page: u32, page_size: u32) -> Vec<String> {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::TagEngagements(tag))
            .unwrap_or_else(|| Vec::new(&env));

        let total = ids.len();
        if page_size == 0 {
            return Vec::new(&env);
        }
        let start = page.saturating_mul(page_size);
        if start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(ids.get(i).unwrap());
        }
        result
    }

    /// Return the total number of engagements tagged with a given tag.
    pub fn get_engagement_tag_count(env: Env, tag: String) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::TagEngagements(tag))
            .unwrap_or_else(|| Vec::new(&env));
        ids.len()
    }

    /// Return a paginated slice of engagement IDs matching across a list of
    /// tags (issue #319). When `match_all` is `true`, an engagement must
    /// carry every tag in `tags` (AND); when `false`, matching any one tag
    /// is enough (OR). Reads the same per-tag index that backs
    /// `get_engagements_by_tag`, so results only reflect tags applied via
    /// `add_engagement_tag`. `page` is 0-indexed; out-of-range pages, an
    /// empty `tags` list, or `page_size` of 0 return an empty vec.
    ///
    /// # Panics
    /// - `"TooManyTags"` — `tags` has more entries than `MAX_TAGS`.
    pub fn get_engagements_by_multiple_tags(
        env: Env,
        tags: Vec<String>,
        match_all: bool,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        if page_size == 0 || tags.is_empty() {
            return Vec::new(&env);
        }
        if tags.len() > MAX_TAGS {
            panic!("TooManyTags");
        }

        let mut per_tag_lists: Vec<Vec<String>> = Vec::new(&env);
        for i in 0..tags.len() {
            let ids: Vec<String> = env
                .storage()
                .persistent()
                .get(&DataKey::EngagementTag(tags.get(i).unwrap()))
                .unwrap_or_else(|| Vec::new(&env));
            per_tag_lists.push_back(ids);
        }

        let matched: Vec<String> = if match_all {
            // AND: start from the first tag's list, keep only ids present in
            // every other tag's list.
            let mut acc = per_tag_lists.get(0).unwrap();
            for i in 1..per_tag_lists.len() {
                let other = per_tag_lists.get(i).unwrap();
                let mut next = Vec::new(&env);
                for j in 0..acc.len() {
                    let id = acc.get(j).unwrap();
                    let mut found = false;
                    for k in 0..other.len() {
                        if other.get(k).unwrap() == id {
                            found = true;
                            break;
                        }
                    }
                    if found {
                        next.push_back(id);
                    }
                }
                acc = next;
            }
            acc
        } else {
            // OR: union, de-duplicated, preserving first-seen order across tags.
            let mut union: Vec<String> = Vec::new(&env);
            for i in 0..per_tag_lists.len() {
                let list = per_tag_lists.get(i).unwrap();
                for j in 0..list.len() {
                    let id = list.get(j).unwrap();
                    if !union.contains(&id) {
                        union.push_back(id);
                    }
                }
            }
            union
        };

        let total = matched.len();
        let start = page.saturating_mul(page_size);
        if start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(matched.get(i).unwrap());
        }
        result
    }

    // ----------------------------------------------------------
    // ISSUE #41 — CONFIGURABLE LEDGERS PER DAY
    // ----------------------------------------------------------

    /// Admin sets how many ledgers constitute one day (min 1, max 25_920).
    pub fn set_ledgers_per_day(env: Env, admin: Address, value: u32) {
        Self::assert_admin(&env, &admin);
        if !(1..=25_920).contains(&value) {
            panic!("InvalidLedgersPerDay");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::LedgersPerDay), &value);
        env.events()
            .publish((Symbol::new(&env, "ledgers_per_day_set"),), value);
    }

    /// Return the current ledgers-per-day constant.
    pub fn get_ledgers_per_day(env: Env) -> u32 {
        Self::get_ledgers_per_day_internal(&env)
    }

    // ----------------------------------------------------------
    // ISSUE #18 — MAX RETENTION DAYS CAP
    // ----------------------------------------------------------

    /// Admin sets the maximum retention days cap.
    ///
    /// This cap is enforced only at `create_engagement` time. Lowering it has
    /// no effect on existing engagements whose retention windows already
    /// exceed the new cap — their lifecycle (`unlock_milestone`,
    /// `propose_amendment`, etc.) re-reads stored per-engagement values, not
    /// this config.
    pub fn set_max_retention_days(env: Env, admin: Address, days: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxRetentionDays), &days);
        env.events()
            .publish((Symbol::new(&env, "max_retention_days_set"),), days);
    }

    /// Return the current max retention days cap.
    pub fn get_max_retention_days(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxRetentionDays))
            .unwrap_or(DEFAULT_MAX_RETENTION_DAYS)
    }

    // ----------------------------------------------------------
    // ISSUE #21 — MAX MILESTONES CAP
    // ----------------------------------------------------------

    /// Admin sets the maximum milestone count cap.
    ///
    /// This cap is enforced only at `create_engagement` time. Lowering it has
    /// no effect on existing engagements that already have more milestones
    /// than the new cap — their lifecycle (`unlock_milestone`,
    /// `propose_amendment`, etc.) operates on the stored milestone list, not
    /// this config.
    pub fn set_max_milestones(env: Env, admin: Address, count: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxMilestones), &count);
        env.events()
            .publish((Symbol::new(&env, "max_milestones_set"),), count);
    }

    /// Return the current maximum milestone count cap.
    pub fn get_max_milestones(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxMilestones))
            .unwrap_or(DEFAULT_MAX_MILESTONES)
    }

    // ----------------------------------------------------------
    // PER-COMPANY ACTIVE ENGAGEMENT CAP
    // ----------------------------------------------------------

    /// Admin sets the maximum number of simultaneously active engagements allowed
    /// per company address. Defaults to 50 when not explicitly configured.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the contract admin.
    /// - `"InvalidMaxActivePerCompany"` — `count` is 0.
    pub fn set_max_active_per_company(env: Env, admin: Address, count: u32) {
        Self::assert_admin(&env, &admin);
        if count == 0 {
            panic!("InvalidMaxActivePerCompany");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxActivePerCompany), &count);
        env.events()
            .publish((Symbol::new(&env, "max_active_per_company_set"),), count);
    }

    /// Return the current per-company active engagement cap.
    /// Returns `DEFAULT_MAX_ACTIVE_PER_COMPANY` (50) when not configured.
    pub fn get_max_active_per_company(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxActivePerCompany))
            .unwrap_or(DEFAULT_MAX_ACTIVE_PER_COMPANY)
    }

    /// Return the current active engagement count for a company.
    pub fn get_company_active_count(env: Env, company: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::CompanyActiveCount(company))
            .unwrap_or(0u32)
    }

    // ----------------------------------------------------------
    // ISSUE #38 — INACTIVITY TIMEOUT
    // ----------------------------------------------------------

    /// Admin sets the inactivity timeout in ledgers.
    pub fn set_inactivity_timeout_ledgers(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage().instance().set(
            &DataKey::Config(ConfigKey::InactivityTimeoutLedgers),
            &ledgers,
        );
        env.events()
            .publish((Symbol::new(&env, "inactivity_timeout_set"),), ledgers);
    }

    /// Return the current inactivity timeout in ledgers.
    pub fn get_inactivity_timeout_ledgers(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::InactivityTimeoutLedgers))
            .unwrap_or(DEFAULT_INACTIVITY_TIMEOUT_LEDGERS)
    }

    /// Mark an engagement as `Expired` and refund the remaining escrow to the company.
    /// This is a **permissionless keeper function** — any address may call it; no signature
    /// is required. It is designed to be triggered by off-chain bots or backend pollers.
    ///
    /// # Expiry Condition
    /// The engagement is eligible for expiry when:
    /// `env.ledger().sequence() - last_activity_ledger >= inactivity_timeout_ledgers`
    ///
    /// The default `inactivity_timeout_ledgers` is set at contract initialisation and can be
    /// updated by the admin via `set_inactivity_timeout_ledgers`.
    ///
    /// # Behaviour
    /// - The engagement must not be `Completed`.
    /// - Any escrow balance not yet released (`total_amount - released_amount`) is transferred
    ///   back to the company address.
    /// - The engagement status is set to `Expired`.
    ///
    /// # Panics
    /// - `"Cannot expire completed engagement"` — engagement is already `Completed`.
    /// - `"Inactivity timeout not reached"` — the inactivity threshold has not yet been reached.
    ///
    /// # Events
    /// Emits `("engagement_expired", engagement_id)` with the refund amount.
    pub fn expire_engagement(env: Env, engagement_id: String) {
        Self::assert_engagement_not_paused(&env, &engagement_id);
        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status == EngagementStatus::Completed {
            panic!("Cannot expire completed engagement");
        }

        let timeout = Self::get_inactivity_timeout_ledgers(env.clone());
        let current_ledger = env.ledger().sequence();

        if current_ledger <= engagement.last_activity_ledger + timeout {
            panic!("Inactivity timeout not reached");
        }

        let refund = engagement.total_amount - engagement.released_amount;
        if refund > 0 {
            let token_client = token::Client::new(&env, &engagement.token);
            token_client.transfer(
                &env.current_contract_address(),
                &engagement.company,
                &refund,
            );
            Self::on_escrow_lifecycle_checkpoint(
                &env,
                &engagement_id,
                EscrowLifecycleAction::Refunded,
                -refund,
                &engagement.token,
                &engagement.company,
                &engagement.recruiter,
            );
        }

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Expired;
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );

        Self::decrement_company_active_count(&env, &engagement.company);

        env.events().publish(
            (
                Symbol::new(&env, "engagement_expired"),
                engagement_id.clone(),
            ),
            refund,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #40 — STORAGE TTL EXTENSION
    // ----------------------------------------------------------

    /// Admin sets the storage TTL extension value in ledgers.
    pub fn set_storage_ttl_extend_to(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::StorageTtlExtendTo), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "storage_ttl_extend_to_set"),), ledgers);
    }

    /// Return the current storage TTL extension value in ledgers.
    pub fn get_storage_ttl_extend_to(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::StorageTtlExtendTo))
            .unwrap_or(DEFAULT_STORAGE_TTL_EXTEND_TO)
    }

    /// Helper to extend TTL for engagement storage.
    fn extend_engagement_ttl(env: &Env, engagement_id: &String) {
        let extend_to = Self::get_storage_ttl_extend_to(env.clone());
        env.storage().persistent().extend_ttl(
            &DataKey::Engagement(engagement_id.clone()),
            100_000,
            extend_to,
        );
    }

    fn emit_milestone_status_changed(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
        old_status: MilestoneStatus,
        new_status: MilestoneStatus,
    ) {
        if old_status != new_status {
            env.events().publish(
                (
                    Symbol::new(env, "milestone_status_changed"),
                    engagement_id.clone(),
                ),
                (milestone_index, old_status, new_status),
            );
        }
    }

    fn emit_engagement_status_changed(
        env: &Env,
        engagement_id: &String,
        old_status: EngagementStatus,
        new_status: EngagementStatus,
    ) {
        if old_status != new_status {
            env.events().publish(
                (Symbol::new(env, "status_changed"), engagement_id.clone()),
                (old_status, new_status),
            );
        }
    }

    // ----------------------------------------------------------
    // ISSUE #39 — BATCH CONFIRM MILESTONES
    // ----------------------------------------------------------

    /// Confirm multiple milestones in a single transaction, releasing payment for each.
    ///
    /// # Caller
    /// `company` — must match the engagement's company address and sign the transaction.
    ///
    /// # Atomicity
    /// All milestones in `milestone_indices` are validated **before** any payment is processed.
    /// If any milestone fails validation (e.g. proof not submitted, retention window not elapsed),
    /// the entire call panics and no funds are transferred.
    ///
    /// # Behaviour
    /// - For each index, releases `(total_amount * payment_percent / 100)` to the recruiter,
    ///   deducting the platform fee first.
    /// - Each confirmed milestone emits a `milestone_confirmed` event.
    /// - If all milestones across the engagement are now `Confirmed` or `Resolved`,
    ///   the engagement transitions to `Completed` and an `engagement_completed` event is emitted.
    ///
    /// # Panics
    /// - `"EmptyIndices"` — `milestone_indices` is empty.
    /// - `"engagement is not active"` — engagement status is not `Active`.
    /// - `"unauthorized"` — caller is not the engagement's company.
    /// - `"invalid milestone index"` — an index is out of bounds.
    /// - `"milestone proof not yet submitted"` — milestone is not in `ProofSubmitted` status.
    /// - `"retention window has not elapsed — cannot confirm yet"` — for `Retention` milestones
    ///   confirmed before their `valid_after_ledger`.
    ///
    /// # Events
    /// - `(\"milestone_confirmed\", engagement_id)` with `(index, payment)` — one per milestone.
    /// - `(\"platform_fee_collected\", engagement_id)` with `(index, fee_amount, treasury)` — when fee > 0.
    /// - `(\"engagement_completed\", engagement_id)` — emitted once if all milestones are done.
    pub fn batch_confirm_milestones(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_indices: Vec<u32>,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        if milestone_indices.is_empty() {
            panic!("EmptyIndices");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        // Validate all milestones first (atomic rejection)
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
            // Issue #67/#184: enforce the same sequential-confirmation rule as
            // confirm_milestone — every prior milestone must already be done,
            // either from an earlier call or earlier in this same batch.
            for j in 0..idx {
                let prev = engagement.milestones.get(j).unwrap();
                let done_already = prev.status == MilestoneStatus::Confirmed
                    || prev.status == MilestoneStatus::Resolved;
                let done_in_batch =
                    (0..milestone_indices.len()).any(|k| milestone_indices.get(k).unwrap() == j);
                if !done_already && !done_in_batch {
                    panic!("PreviousMilestoneNotComplete");
                }
            }
        }

        let platform_fee = Self::get_platform_fee_internal(&env);
        let effective_bps =
            Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
        let token_client = token::Client::new(&env, &engagement.token);

        for i in 0..milestone_indices.len() {
            let idx = milestone_indices.get(i).unwrap();
            let mut m = engagement.milestones.get(idx).unwrap();

            let payment = (engagement.total_amount * m.payment_percent as i128) / 100;
            let fee_amount = (payment * effective_bps as i128) / 10_000;
            let net_payment = payment - fee_amount;
            engagement.released_amount += payment;

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
                    (idx, fee_amount, platform_fee.treasury.clone()),
                );
            }
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client);
            Self::on_escrow_lifecycle_checkpoint(
                &env,
                &engagement_id,
                EscrowLifecycleAction::PayoutReleased,
                -payment,
                &engagement.token,
                &engagement.company,
                &engagement.recruiter,
            );

            let old_status = m.status.clone();
            m.status = MilestoneStatus::Confirmed;
            engagement.milestones.set(idx, m);

            Self::emit_milestone_status_changed(
                &env,
                &engagement_id,
                idx,
                old_status,
                MilestoneStatus::Confirmed,
            );

            env.events().publish(
                (
                    Symbol::new(&env, "milestone_confirmed"),
                    engagement_id.clone(),
                ),
                (idx, payment),
            );
        }

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::decrement_company_active_count(&env, &engagement.company);
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
        let effective_bps =
            Self::resolve_platform_fee_bps(&env, platform_fee.bps, engagement.total_amount);
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
        Self::on_escrow_lifecycle_checkpoint(
            &env,
            &engagement_id,
            EscrowLifecycleAction::PayoutReleased,
            -payment,
            &engagement.token,
            &engagement.company,
            &engagement.recruiter,
        );

        let old_status = milestone.status.clone();
        milestone.status = MilestoneStatus::Confirmed;
        engagement.milestones.set(milestone_index, milestone);

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        let old_engagement_status = engagement.status.clone();
        if all_done {
            engagement.status = EngagementStatus::Completed;
            Self::decrement_company_active_count(&env, &engagement.company);
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

    /// Return the number of milestones currently in `Disputed` status.
    /// Read-only and permissionless. Returns 0 when no disputes are active.
    pub fn get_active_dispute_count(env: Env, engagement_id: String) -> u32 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let mut count: u32 = 0;
        for i in 0..engagement.milestones.len() {
            if engagement.milestones.get(i).unwrap().status == MilestoneStatus::Disputed {
                count += 1;
            }
        }
        count
    }

    // ----------------------------------------------------------
    // ISSUE #69 — CONTRACT UPGRADE MECHANISM
    // ----------------------------------------------------------

    /// Admin proposes a WASM upgrade with a mandatory time-lock (issue #69).
    /// The pending upgrade is stored and becomes executable after `lock_duration` ledgers.
    /// Default time-lock is 17,280 ledgers (~1 day); use `set_upgrade_lock_duration` to change it.
    /// Emits `upgrade_proposed` with `(new_wasm_hash, execute_after_ledger)`.
    ///
    /// # Repeated calls (issue #185)
    /// Calling `propose_upgrade` again while a previous proposal is still pending
    /// **overwrites** it and **resets the timelock countdown** — the new
    /// `execute_after_ledger` is computed from the current ledger, not the
    /// original proposal's. This is intentional: it lets the admin correct or
    /// retract a bad proposal (e.g. wrong wasm hash) without waiting out the
    /// original lock. The tradeoff is that a compromised admin key can grief
    /// legitimate upgrades by indefinitely re-proposing, or delay execution by
    /// repeatedly re-proposing the same hash — this is accepted as inherent to
    /// admin-key trust and is not separately mitigated here.
    pub fn propose_upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::assert_admin(&env, &admin);

        let lock_duration: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::UpgradeLockDuration))
            .unwrap_or(LEDGERS_PER_DAY);

        let execute_after_ledger = env.ledger().sequence() + lock_duration;

        let proposal = UpgradeProposal {
            new_wasm_hash: new_wasm_hash.clone(),
            execute_after_ledger,
        };

        env.storage()
            .instance()
            .set(&DataKey::PendingUpgrade, &proposal);

        env.events().publish(
            (Symbol::new(&env, "upgrade_proposed"),),
            (new_wasm_hash, execute_after_ledger),
        );
    }

    /// Execute a pending upgrade after the time-lock has elapsed (issue #69).
    /// Permissionless — anyone may call this once `execute_after_ledger` is reached.
    /// Panics with `"no pending upgrade"` if no proposal exists.
    /// Panics with `"UpgradeLockNotElapsed"` if called before the time-lock expires.
    /// Emits `upgrade_executed` before applying the WASM swap.
    pub fn execute_upgrade(env: Env) {
        let proposal: UpgradeProposal = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .unwrap_or_else(|| panic!("no pending upgrade"));

        if env.ledger().sequence() < proposal.execute_after_ledger {
            panic!("UpgradeLockNotElapsed");
        }

        env.storage().instance().remove(&DataKey::PendingUpgrade);

        env.events().publish(
            (Symbol::new(&env, "upgrade_executed"),),
            proposal.new_wasm_hash.clone(),
        );

        env.deployer()
            .update_current_contract_wasm(proposal.new_wasm_hash);
    }

    /// Admin sets the upgrade time-lock duration in ledgers (issue #69).
    pub fn set_upgrade_lock_duration(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::UpgradeLockDuration), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "upgrade_lock_duration_set"),), ledgers);
    }

    /// Return the current upgrade time-lock duration in ledgers (issue #69).
    /// Defaults to 17,280 (~1 day).
    pub fn get_upgrade_lock_duration(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::UpgradeLockDuration))
            .unwrap_or(LEDGERS_PER_DAY)
    }

    // ----------------------------------------------------------
    // ISSUE #68 — ADMIN-CONFIGURABLE MAX PROOF HASH LENGTH
    // ----------------------------------------------------------

    /// Admin sets the maximum proof hash length in characters (issue #68).
    /// `len` must be in the range 1–500. Panics with `"InvalidMaxProofHashLength"` otherwise.
    pub fn set_max_proof_hash_length(env: Env, admin: Address, len: u32) {
        Self::assert_admin(&env, &admin);
        if !(1..=500).contains(&len) {
            panic!("InvalidMaxProofHashLength");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxProofHashLength), &len);
        env.events()
            .publish((Symbol::new(&env, "max_proof_hash_length_set"),), len);
    }

    /// Return the current max proof hash length (issue #68).
    /// Defaults to 200 when not configured.
    pub fn get_max_proof_hash_length(env: Env) -> u32 {
        Self::get_max_proof_hash_length_internal(&env)
    }

    // ----------------------------------------------------------
    // ISSUE #52 — ARBITER FEE
    // ----------------------------------------------------------

    /// Admin sets the arbiter fee in basis points (0–200, max 2%) (issue #52).
    /// Panics with "ArbiterFeeTooHigh" if bps > 200.
    pub fn set_arbiter_fee(env: Env, admin: Address, bps: u32) {
        Self::assert_admin(&env, &admin);
        if bps > MAX_ARBITER_FEE_BPS {
            panic!("ArbiterFeeTooHigh");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::ArbiterFee), &bps);
        env.events()
            .publish((Symbol::new(&env, "arbiter_fee_set"),), bps);
    }

    /// Return the current arbiter fee in basis points (issue #52).
    /// Defaults to 0 when not configured.
    pub fn get_arbiter_fee(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ArbiterFee))
            .unwrap_or(0u32)
    }

    // ----------------------------------------------------------
    // ISSUE #55 — ENGAGEMENT COMPLETION QUERY
    // ----------------------------------------------------------

    /// Returns true if the engagement status is Completed, false for all other statuses.
    /// Read-only and permissionless (issue #55).
    pub fn get_is_engagement_complete(env: Env, engagement_id: String) -> bool {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.status == EngagementStatus::Completed
    }

    /// Return the fraction of milestones that are unlocked (not in Locked status).
    /// Returns `(unlocked_count, total_count)`.
    /// - `unlocked_count` = number of milestones with status != Locked
    /// - `total_count` = total number of milestones in the engagement
    ///
    /// Read-only and permissionless.
    pub fn get_unlock_progress(env: Env, engagement_id: String) -> (u32, u32) {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let total = engagement.milestones.len();
        let mut unlocked: u32 = 0;
        for i in 0..total {
            let m = engagement.milestones.get(i).unwrap();
            if m.status != MilestoneStatus::Locked {
                unlocked += 1;
            }
        }
        (unlocked, total)
    }

    // ----------------------------------------------------------
    // ISSUE #240 — FULL CONFIG SNAPSHOT
    // ----------------------------------------------------------

    /// Return every admin-configurable contract parameter in a single
    /// read-only call (issue #240).
    ///
    /// Off-chain indexers otherwise need ~20 separate round-trips
    /// (`get_platform_fee`, `get_arbiter_fee`, `get_confirm_window`, …) to
    /// reconstruct the current configuration. Each field is sourced from the
    /// same accessor its dedicated getter uses, so the snapshot is exactly what
    /// those calls would return at this ledger — including defaults for
    /// parameters never explicitly set.
    ///
    /// Because the whole struct is read within one invocation, the values are
    /// mutually consistent: there is no window for an admin transaction to land
    /// between two reads and yield a torn view.
    ///
    /// Read-only and permissionless.
    ///
    /// # Panics
    /// - `"admin not initialized"` — `init` has not been called.
    pub fn get_config_snapshot(env: Env) -> ConfigSnapshot {
        let platform_fee = Self::get_platform_fee_internal(&env);

        ConfigSnapshot {
            version: Self::get_version(env.clone()),
            admin: Self::get_admin_internal(&env),
            admin_renounced: env
                .storage()
                .instance()
                .get(&DataKey::AdminRenounced)
                .unwrap_or(false),
            paused: Self::is_paused_internal(&env),
            platform_fee_bps: platform_fee.bps,
            platform_fee_treasury: platform_fee.treasury,
            arbiter_fee_bps: Self::get_arbiter_fee(env.clone()),
            super_arbiter: env.storage().instance().get(&DataKey::SuperArbiter),
            confirm_window_ledgers: Self::get_confirm_window(env.clone()),
            dispute_window_ledgers: Self::get_dispute_window(env.clone()),
            proof_cooldown_ledgers: Self::get_proof_cooldown(&env),
            due_soon_window_ledgers: Self::get_due_soon_window(env.clone()),
            amendment_ttl_ledgers: Self::get_amendment_ttl(env.clone()),
            extension_ttl_ledgers: Self::get_extension_ttl(env.clone()),
            inactivity_timeout_ledgers: Self::get_inactivity_timeout_ledgers(env.clone()),
            storage_ttl_extend_to: Self::get_storage_ttl_extend_to(env.clone()),
            upgrade_lock_duration_ledgers: Self::get_upgrade_lock_duration(env.clone()),
            ledgers_per_day: Self::get_ledgers_per_day_internal(&env),
            max_milestones: Self::get_max_milestones(env.clone()),
            max_retention_days: Self::get_max_retention_days(env.clone()),
            max_replacements: Self::get_max_replacements_internal(&env),
            max_active_per_company: Self::get_max_active_per_company(env.clone()),
            max_proof_hash_length: Self::get_max_proof_hash_length_internal(&env),
            min_engagement_amount: Self::get_min_amount(env.clone()),
            token_allowlist_enabled: env
                .storage()
                .persistent()
                .get(&DataKey::AllowlistEnabled)
                .unwrap_or(false),
        }
    }

    // ----------------------------------------------------------
    // ISSUE #59 — ADMIN ROLE RENOUNCEMENT
    // ----------------------------------------------------------

    /// Permanently renounce admin privileges (issue #59).
    /// Sets the AdminRenounced flag so all admin-gated functions fail with "NoAdmin".
    /// Irreversible — once renounced, admin cannot be restored.
    /// Emits `admin_renounced` with `final_ledger`.
    pub fn renounce_admin(env: Env, admin: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::AdminRenounced, &true);
        // Clear any pending nomination so a stale nominee cannot claim_admin
        // after the role was supposed to be permanently renounced. See issue #182.
        env.storage().persistent().remove(&DataKey::PendingAdmin);
        let final_ledger = env.ledger().sequence();
        env.events()
            .publish((Symbol::new(&env, "admin_renounced"),), final_ledger);
    }

    // ----------------------------------------------------------
    // INTERNAL HELPERS
    // ----------------------------------------------------------

    fn get_engagement_internal(env: &Env, engagement_id: &String) -> Engagement {
        env.storage()
            .persistent()
            .get(&DataKey::Engagement(engagement_id.clone()))
            .unwrap_or_else(|| panic!("engagement not found"))
    }

    /// Shared "load milestone by index or panic" helper (issue #170) — mirrors
    /// `get_engagement_internal` for the milestone lookup, so every call site
    /// panics with the same `"invalid milestone index"` message instead of a
    /// bare Vec index-out-of-bounds panic.
    fn get_milestone_or_panic(engagement: &Engagement, milestone_index: u32) -> Milestone {
        engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("{}", ERR_INVALID_MILESTONE_INDEX))
    }

    /// Shared precondition for the second step of the early-exit protocol —
    /// the engagement must be `ExitRequested` and `company` must match the
    /// engagement's company. Used by both `accept_early_exit` and
    /// `reject_early_exit` (issue #173).
    fn assert_exit_request_pending(env: &Env, company: &Address, engagement: &Engagement) {
        if engagement.status != EngagementStatus::ExitRequested {
            panic!("no exit request pending");
        }
        if !Self::is_authorized_company(env, company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }
    }

    /// Reserved hook point for future yield-bearing escrow integrations.
    ///
    /// This is intentionally no-op by default: unless admin both enables the
    /// checkpoint and configures a callback target, the helper returns without
    /// any side effects. Today it emits an event only, reserving a stable
    /// lifecycle checkpoint surface without changing escrow behaviour.
    fn on_escrow_lifecycle_checkpoint(
        env: &Env,
        engagement_id: &String,
        action: EscrowLifecycleAction,
        escrow_delta: i128,
        token: &Address,
        company: &Address,
        recruiter: &Address,
    ) {
        let enabled: bool = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowCallbackEnabled)
            .unwrap_or(false);
        if !enabled {
            return;
        }

        let target: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::EscrowCallbackTarget);
        let callback_target = match target {
            Some(addr) => addr,
            None => return,
        };

        env.events().publish(
            (
                Symbol::new(env, "escrow_callback_point"),
                engagement_id.clone(),
            ),
            (
                action,
                escrow_delta,
                token.clone(),
                company.clone(),
                recruiter.clone(),
                callback_target,
            ),
        );
    }

    fn get_admin_internal(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("admin not initialized"))
    }

    fn assert_admin(env: &Env, admin: &Address) {
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

    fn is_paused_internal(env: &Env) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    fn assert_not_paused(env: &Env) {
        if Self::is_paused_internal(env) {
            panic!("ContractPaused");
        }
    }

    fn is_engagement_paused_internal(env: &Env, engagement_id: &String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::EngagementPaused(engagement_id.clone()))
            .unwrap_or(false)
    }

    /// Per-engagement quarantine guard (issue #239). Layered on top of — not a
    /// replacement for — `assert_not_paused`: the global pause halts every
    /// engagement, this one halts a single quarantined ID.
    fn assert_engagement_not_paused(env: &Env, engagement_id: &String) {
        if Self::is_engagement_paused_internal(env, engagement_id) {
            panic!("{}", ERR_ENGAGEMENT_PAUSED);
        }
    }

    /// Clear the due-soon notification flag for a milestone (issue #241).
    ///
    /// Called whenever a milestone's `valid_after_ledger` moves — an accepted
    /// extension pushes it out, a replacement restarts the retention timer.
    /// Without this the flag from the *previous* deadline would suppress the
    /// notification for the new one, silently starving off-chain notifiers.
    fn clear_due_soon_flag(env: &Env, engagement_id: &String, milestone_index: u32) {
        let key = DataKey::DueSoonNotified(engagement_id.clone(), milestone_index);
        if env.storage().persistent().has(&key) {
            env.storage().persistent().remove(&key);
        }
    }

    /// Terminal engagement states — no further state transitions are possible.
    fn is_terminal_status(status: &EngagementStatus) -> bool {
        matches!(
            status,
            EngagementStatus::Completed | EngagementStatus::Cancelled | EngagementStatus::Expired
        )
    }

    /// Shared 1–5 bounds check for both feedback-rating entry points
    /// (issues #242, #243).
    fn assert_valid_rating(rating: u32) {
        if !(1..=5).contains(&rating) {
            panic!("InvalidRating");
        }
    }

    /// Fold one rating into the running tally at `key`, returning the new
    /// rating count. Shared by both rating entry points so recruiter and
    /// company tallies can never diverge in how they accumulate.
    fn record_rating(env: &Env, key: &DataKey, rating: u32) -> u32 {
        let mut record: RatingRecord =
            env.storage().persistent().get(key).unwrap_or(RatingRecord {
                total_score: 0,
                count: 0,
            });

        record.total_score += rating;
        record.count += 1;
        let new_count = record.count;

        env.storage().persistent().set(key, &record);
        env.storage()
            .persistent()
            .extend_ttl(key, 100_000, 6_300_000);

        new_count
    }

    /// Build the read-only summary for a rating tally (issue #244), returning a
    /// zeroed summary for an address that has never been rated.
    fn rating_summary(env: &Env, key: &DataKey) -> RatingSummary {
        let record: RatingRecord = env.storage().persistent().get(key).unwrap_or(RatingRecord {
            total_score: 0,
            count: 0,
        });

        // Scale before dividing so the two decimal places survive integer
        // truncation; an unrated address has no average, so count == 0 falls
        // back to 0 rather than dividing.
        let average_x100 = (record.total_score * 100)
            .checked_div(record.count)
            .unwrap_or(0);

        RatingSummary {
            average_x100,
            count: record.count,
            total_score: record.total_score,
        }
    }

    fn get_platform_fee_internal(env: &Env) -> PlatformFee {
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
    fn resolve_platform_fee_bps(env: &Env, base_bps: u32, total_amount: i128) -> u32 {
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

    /// Whether `referrer` is on the admin-configured recognised referral list.
    fn is_recognised_referrer(env: &Env, referrer: &Address) -> bool {
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
    fn apply_referral_discount(env: &Env, bps: u32, referrer: &Option<Address>) -> u32 {
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

    fn get_ledgers_per_day_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::LedgersPerDay))
            .unwrap_or(LEDGERS_PER_DAY)
    }

    fn get_max_proof_hash_length_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxProofHashLength))
            .unwrap_or(MAX_PROOF_HASH_LENGTH)
    }

    fn get_max_replacements_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxReplacements))
            .unwrap_or(DEFAULT_MAX_REPLACEMENTS)
    }

    fn get_max_milestone_extensions_internal(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxMilestoneExtensions))
            .unwrap_or(DEFAULT_MAX_MILESTONE_EXTENSIONS)
    }

    /// Add `tag` to the global distinct-tag registry if it isn't already
    /// present (issue #321). Idempotent.
    fn track_tag_used(env: &Env, tag: &String) {
        let mut tags: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllTags)
            .unwrap_or_else(|| Vec::new(env));
        for i in 0..tags.len() {
            if tags.get(i).unwrap() == *tag {
                return;
            }
        }
        tags.push_back(tag.clone());
        env.storage().persistent().set(&DataKey::AllTags, &tags);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::AllTags, 100_000, 6_300_000);
    }

    /// Remove `tag` from the global distinct-tag registry if present
    /// (issue #321). No-op if the tag isn't tracked.
    fn untrack_tag_used(env: &Env, tag: &String) {
        let tags: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllTags)
            .unwrap_or_else(|| Vec::new(env));
        let mut new_tags: Vec<String> = Vec::new(env);
        let mut changed = false;
        for i in 0..tags.len() {
            let t = tags.get(i).unwrap();
            if t == *tag {
                changed = true;
            } else {
                new_tags.push_back(t);
            }
        }
        if changed {
            env.storage().persistent().set(&DataKey::AllTags, &new_tags);
        }
    }

    /// Decrement the per-company active engagement count, saturating at 0.
    /// Called whenever an engagement leaves the active pool (completed,
    /// cancelled, expired, etc).
    fn decrement_company_active_count(env: &Env, company: &Address) {
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
    // ISSUE #56 — CO-RECRUITER SPLIT PAYOUT
    // ----------------------------------------------------------

    /// Distribute the net payment (after platform / arbiter fee) between the
    /// primary recruiter and the optional co-recruiter.
    ///
    /// When `co_recruiter` is `Some`, the primary receives
    /// `net * split_bps / 10_000` and the co-recruiter receives the remainder.
    /// When `co_recruiter` is `None` the full net amount goes to the recruiter.
    fn distribute_recruiter_payout(
        env: &Env,
        engagement: &Engagement,
        net_payment: i128,
        token_client: &token::Client,
    ) {
        match &engagement.co_recruiter {
            Some(co_recruiter) => {
                let split = engagement.recruiter_split_bps as i128;
                let primary_payment = (net_payment * split) / (FULL_SPLIT_BPS as i128);
                let co_payment = net_payment - primary_payment;
                token_client.transfer(
                    &env.current_contract_address(),
                    &engagement.recruiter,
                    &primary_payment,
                );
                token_client.transfer(&env.current_contract_address(), co_recruiter, &co_payment);
            }
            None => {
                token_client.transfer(
                    &env.current_contract_address(),
                    &engagement.recruiter,
                    &net_payment,
                );
            }
        }
    }
}

mod test;
