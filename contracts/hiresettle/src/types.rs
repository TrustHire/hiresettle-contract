//! Contract data structs (`#[contracttype]`).

use soroban_sdk::{contracttype, Address, BytesN, String, Vec};
use crate::{EngagementStatus, MilestoneKind, MilestoneStatus};

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
    /// Indices of the milestones that must be `Confirmed` or `Resolved` before
    /// this one can be confirmed (issue #461). Empty means no prerequisites.
    /// Declaring every lower index — or just the previous one, as a linear
    /// chain — reproduces the old strict sequential rule (issue #67).
    /// Validated at `create_engagement`: every index must be in range and the
    /// graph must be acyclic.
    pub prerequisites: Vec<u32>,
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
    /// When `arbiter_weights` is set, this is measured in cumulative weight
    /// rather than headcount (issue #460).
    pub quorum: u32,
    /// Optional per-arbiter vote weight, parallel to `arbiters` (issue #460).
    /// `None` means every arbiter carries a weight of 1. Weights belong to the
    /// slot, so they carry over when `claim_arbiter` replaces the address.
    pub arbiter_weights: Option<Vec<u32>>,
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
    /// Whether this engagement is listed by `get_public_engagement_ids`
    /// (issue #365). Set at creation time from `EngagementConfig::is_public`.
    pub is_public: bool,
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
    /// Always the arbiter's slot address, even for a vote cast by a delegate
    /// (issue #463).
    pub voted: Vec<Address>,
    /// Cumulative weight of approving arbiters (issue #460). Equals
    /// `approve_votes` when the engagement has no `arbiter_weights`.
    pub approve_weight: u32,
    /// Cumulative weight of rejecting arbiters (issue #460). Equals
    /// `reject_votes` when the engagement has no `arbiter_weights`.
    pub reject_weight: u32,
}
/// Returned by `get_arbiter_vote_weights` (issue #460).
#[contracttype]
#[derive(Clone)]
pub struct ArbiterVoteWeights {
    /// Cumulative weight of arbiters who voted to approve.
    pub approve_weight: u32,
    /// Cumulative weight of arbiters who voted to reject.
    pub reject_weight: u32,
    /// Sum of every arbiter's weight on the panel.
    pub total_weight: u32,
    /// Weight required to approve; rejection resolves once
    /// `reject_weight > total_weight - quorum`.
    pub quorum: u32,
}
/// Per-dispute tally of split votes cast via `cast_arbiter_split_vote`
/// (issue #462). Cleared once the dispute resolves.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterSplitVoteRecord {
    /// Arbiter slot addresses that have voted, in submission order.
    pub voters: Vec<Address>,
    /// Payout percentage (0-100) submitted by each voter; parallel to `voters`.
    pub splits: Vec<u32>,
    /// Cumulative weight of `voters`; the dispute resolves once this reaches
    /// the engagement's `quorum`.
    pub cast_weight: u32,
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
    /// Must be ≥ 1 and ≤ `arbiters.len()`, or ≤ the sum of `weights` when
    /// weights are given.
    pub quorum: u32,
    /// Optional per-arbiter vote weight, parallel to `arbiters` (issue #460).
    /// Must be the same length as `arbiters`, with every weight ≥ 1. `None`
    /// gives every arbiter a weight of 1, i.e. one-address-one-vote.
    pub weights: Option<Vec<u32>>,
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
    /// Whether this engagement should be listed by `get_public_engagement_ids`
    /// (issue #365). Most engagements are private; set `true` to opt in.
    pub is_public: bool,
}
