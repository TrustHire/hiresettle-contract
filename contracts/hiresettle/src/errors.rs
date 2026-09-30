//! Shared panic message constants for the most-repeated error strings
//! (issue #171). Keeping these as constants means a typo can't silently
//! create an inconsistent error surface for off-chain consumers matching
//! on these strings.
//!
//! # Errors reference
//!
//! The contract has no error enum: every failure is a `panic!` with a string
//! message, and off-chain callers match on that string. The table below lists
//! every message raised outside `test.rs`, the shared constant it comes from
//! (if any), and where it is raised. Messages built with `format!`-style
//! arguments (e.g. `TagEmpty: index {}`) are listed by their fixed prefix.
//!
//! The table is generated; do not edit it by hand. After adding, renaming or
//! removing a panic message, regenerate it from `contracts/hiresettle`:
//!
//! ```text
//! python3 scripts/gen_errors_table.py
//! ```
//!
//! <!-- BEGIN GENERATED ERRORS TABLE -->
//!
//! | Message | Constant | Raised in | Sites |
//! |---|---|---|---|
//! | `admin not initialized` | — | `helpers::get_admin_internal` | 1 |
//! | `already recused` | — | `disputes::recuse_arbiter` | 1 |
//! | `already voted` | — | `disputes::recuse_arbiter`, `features::cast_emergency_pause_vote` | 2 |
//! | `AlreadyInPool` | — | `arbiter_pool::add_arbiter_pool_member` | 1 |
//! | `AlreadyRated` | — | `ratings::rate_recruiter` | 1 |
//! | `amendment_expired` | — | `engagement::accept_split_amendment`, `engagement::reject_split_amendment` | 2 |
//! | `AmendmentCooldownActive` | — | `engagement::propose_split_amendment` | 1 |
//! | `amount must be greater than zero` | — | 5 functions in `engagement`, `features` | 5 |
//! | `AmountBelowMinimum` | — | `engagement::create_engagement_impl` | 1 |
//! | `ArbiterFeeTooHigh` | — | `admin::set_arbiter_fee` | 1 |
//! | `ArbiterNotFound` | — | `transfers::admin_remove_arbiter` | 1 |
//! | `ArbiterPoolTooSmall` | — | `arbiter_pool::draw_arbiter_panel` | 2 |
//! | `ArbiterRecused` | — | `disputes::cast_arbiter_vote` | 1 |
//! | `ArbiterWeightOverflow` | — | `engagement::create_engagement_impl` | 1 |
//! | `ArbiterWeightsLengthMismatch` | — | `engagement::create_engagement_impl` | 1 |
//! | `at least one arbiter required` | — | `bonds::create_engagement_bundle`, `engagement::create_engagement_impl`, `transfers::admin_remove_arbiter` | 3 |
//! | `BundleAlreadyExists` | — | `bonds::create_engagement_bundle` | 1 |
//! | `BundleCompanyMismatch` | — | `bonds::load_bundle_for` | 1 |
//! | `BundleNotFound` | — | `bonds::load_bundle_for` | 1 |
//! | `can only dispute a submitted proof` | — | `disputes::assert_milestone_disputable` | 1 |
//! | `Cannot expire completed engagement` | — | `admin::expire_engagement` | 1 |
//! | `CompanyActiveLimitReached` | — | `engagement::create_engagement_impl` | 1 |
//! | `CompanyArbiterCollision` | — | `bonds::create_engagement_bundle`, `engagement::create_engagement_impl`, `transfers::admin_add_arbiter` | 3 |
//! | `CompanyRecruiterCollision` | — | `engagement::create_engagement_impl` | 1 |
//! | `ConfirmWindowNotElapsed` | — | `disputes::force_confirm_milestone` | 1 |
//! | `contract not initialised` | — | `admin::set_proof_cooldown` | 1 |
//! | `ContractPaused` | — | `helpers::assert_not_paused` | 1 |
//! | `DelegateAlreadyAssigned` | — | `disputes::set_arbiter_vote_delegate` | 1 |
//! | `discount too high` | — | `admin::set_referral_discount_bps` | 1 |
//! | `dispute already resolvable without escalation` | — | `disputes::escalate_dispute` | 1 |
//! | `dispute has not been escalated` | — | `disputes::resolve_escalation_timeout`, `disputes::super_arbiter_resolve` | 2 |
//! | `DisputeWindowClosed` | — | `disputes::assert_milestone_disputable` | 1 |
//! | `DisputeWindowNotElapsed` | — | `disputes::escalate_dispute` | 1 |
//! | `DisputeWindowProposalPending` | — | `disputes::propose_dispute_window_override` | 1 |
//! | `duplicate vote` | — | `disputes::cast_arbiter_split_vote`, `disputes::cast_arbiter_vote` | 2 |
//! | `DuplicateArbiter` | — | `bonds::create_engagement_bundle`, `transfers::admin_add_arbiter` | 2 |
//! | `DuplicateEmergencySigner` | — | `features::set_emergency_signers` | 1 |
//! | `DuplicateMilestoneIndex` | — | `disputes::batch_raise_dispute` | 1 |
//! | `DuplicateMilestoneName` | — | `engagement::create_engagement_impl` | 1 |
//! | `DuplicateProofHash` | — | `milestones::submit_proof` | 1 |
//! | `EmptyIndices` | — | `disputes::batch_raise_dispute`, `milestones::batch_confirm_milestones` | 2 |
//! | `EmptyPauseReason` | — | `admin::pause_engagement` | 1 |
//! | `engagement already exists` | — | `engagement::create_engagement_impl` | 1 |
//! | `engagement is in a terminal state` | — | `disputes::set_arbiter_vote_delegate`, `transfers::claim_arbiter`, `transfers::get_engagement_for_panel_change`, `transfers::nominate_arbiter_successor` | 4 |
//! | `engagement is not active` | `ERR_ENGAGEMENT_NOT_ACTIVE` | 24 functions in `disputes`, `engagement`, `milestones`, `transfers` | 24 |
//! | `engagement not found` | — | `helpers::get_engagement_internal` | 1 |
//! | `EngagementNotCompleted` | — | `ratings::rate_recruiter` | 1 |
//! | `EngagementPaused` | `ERR_ENGAGEMENT_PAUSED` | `helpers::assert_engagement_not_paused` | 1 |
//! | `fee tier not found` | — | `admin::remove_fee_tier` | 1 |
//! | `FeeTooHigh` | — | `admin::set_platform_fee`, `features::set_fee_rebate_bps` | 2 |
//! | `HoldReasonTooLong` | — | `admin::hold_milestone` | 1 |
//! | `Inactivity timeout not reached` | — | `admin::expire_engagement` | 1 |
//! | `InsufficientCompanyBalance` | — | `features::debit_company_pool`, `features::withdraw_company_balance` | 2 |
//! | `InsufficientRebateBalance` | — | `features::redeem_company_rebate` | 1 |
//! | `invalid milestone index` | `ERR_INVALID_MILESTONE_INDEX` | `helpers::get_milestone_or_panic` | 1 |
//! | `invalid quorum` | — | `bonds::create_engagement_bundle`, `engagement::create_engagement_impl`, `transfers::assert_panel_quorum_valid` | 3 |
//! | `InvalidAmountRange` | — | `queries::get_engagement_count_by_amount`, `queries::get_engagements_by_amount_range` | 2 |
//! | `InvalidArbiterWeight` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidBondAmount` | — | `bonds::escrow_bond` | 1 |
//! | `InvalidBondForfeitBps` | — | `bonds::set_bond_forfeit_bps` | 1 |
//! | `InvalidBundleId` | — | `bonds::create_engagement_bundle` | 1 |
//! | `InvalidContractPdfHash` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidDelegate` | — | `disputes::set_arbiter_vote_delegate` | 1 |
//! | `InvalidDisputeWindow` | — | `disputes::propose_dispute_window_override` | 1 |
//! | `InvalidEmergencyThreshold` | — | `features::set_emergency_signers` | 1 |
//! | `InvalidEmergencyVoteWindow` | — | `features::set_emergency_vote_window` | 1 |
//! | `InvalidEngagementId` | — | `engagement::create_engagement_impl` | 2 |
//! | `InvalidMaxActivePerCompany` | — | `admin::set_max_active_per_company` | 1 |
//! | `InvalidMetadataHash` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidMinAmount` | — | `admin::set_token_min_amount` | 1 |
//! | `InvalidPrerequisiteIndex` | — | `helpers::validate_milestone_prerequisites` | 1 |
//! | `InvalidProofHash` | — | `milestones::submit_proof` | 1 |
//! | `InvalidQuorumRatio` | — | `admin::set_min_quorum_ratio_bps` | 1 |
//! | `InvalidRating` | — | `ratings::rate_recruiter` | 1 |
//! | `InvalidSplitBps` | — | `engagement::create_engagement_impl`, `engagement::propose_split_amendment` | 2 |
//! | `InvalidSplitPercent` | — | `disputes::cast_arbiter_split_vote` | 1 |
//! | `InvalidStreamDuration` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidSuperArbiterResponseWindow` | — | `disputes::set_super_arbiter_deadline` | 1 |
//! | `JobTitleEmpty` | — | `engagement::create_engagement_impl` | 1 |
//! | `JobTitleTooLong` | — | `engagement::create_engagement_impl` | 1 |
//! | `milestone is not in disputed status` | — | 6 functions in `disputes` | 6 |
//! | `milestone is not in ProofSubmitted status` | — | `disputes::force_confirm_milestone` | 1 |
//! | `milestone is not locked` | — | `milestones::unlock_milestone` | 1 |
//! | `milestone is not pending` | — | `milestones::submit_proof`, `milestones::trigger_no_show` | 2 |
//! | `milestone percentages must sum to 100` | — | `engagement::create_engagement_impl` | 1 |
//! | `milestone proof not yet submitted` | — | `milestones::batch_confirm_milestones`, `milestones::confirm_milestone` | 2 |
//! | `MilestoneNameEmpty` | — | `engagement::create_engagement_impl` | 1 |
//! | `MilestoneNameTooLong` | — | `engagement::create_engagement_impl` | 1 |
//! | `MilestoneOnHold` | `ERR_MILESTONE_ON_HOLD` | `helpers::assert_milestone_not_on_hold` | 1 |
//! | `missing treasury` | — | `features::apply_pending_config_change` | 1 |
//! | `MixedVoteModes` | — | `disputes::cast_arbiter_split_vote`, `disputes::cast_arbiter_vote` | 2 |
//! | `no co_recruiter` | — | `engagement::assert_split_amendment_counterparty`, `engagement::propose_split_amendment` | 2 |
//! | `no config cosigner` | — | `features::accept_config_change` | 1 |
//! | `no dispute in progress` | — | `disputes::escalate_dispute` | 1 |
//! | `no emergency signers` | — | `features::cast_emergency_pause_vote` | 1 |
//! | `no escalation timestamp recorded` | — | `disputes::resolve_escalation_timeout` | 1 |
//! | `no pending admin nomination` | — | `admin::claim_admin` | 1 |
//! | `no pending amendment proposal` | — | `engagement::accept_split_amendment`, `engagement::reject_split_amendment` | 2 |
//! | `no pending arbiter nomination` | — | `transfers::claim_arbiter` | 1 |
//! | `no pending config change` | — | `features::accept_config_change` | 1 |
//! | `no pending recruiter transfer` | — | `transfers::accept_recruiter_transfer` | 1 |
//! | `no pending upgrade` | — | `admin::execute_upgrade` | 1 |
//! | `no super arbiter configured` | — | `disputes::escalate_dispute`, `disputes::super_arbiter_resolve` | 2 |
//! | `NoAdmin` | — | `helpers::assert_admin` | 1 |
//! | `NoPendingDisputeWindowProposal` | — | `disputes::accept_dispute_window_override`, `disputes::reject_dispute_window_override` | 2 |
//! | `NoShowDeadlineNotReached` | — | `milestones::trigger_no_show` | 1 |
//! | `NoShowDisabled` | — | `milestones::trigger_no_show` | 1 |
//! | `NoStreamedPayout` | — | `milestones::claim_streamed_payout` | 1 |
//! | `NotInPool` | — | `arbiter_pool::remove_arbiter_pool_member` | 1 |
//! | `only placement milestones can be forfeited` | — | `milestones::trigger_no_show` | 1 |
//! | `only retention milestones can be unlocked this way` | — | `milestones::unlock_milestone` | 1 |
//! | `PanelChangeDuringDispute` | — | `transfers::get_engagement_for_panel_change` | 1 |
//! | `PauseReasonTooLong` | — | `admin::pause_engagement` | 1 |
//! | `placement not yet confirmed — use cancel_engagement instead` | — | `engagement::request_replacement` | 1 |
//! | `PrerequisiteCycle` | — | `helpers::validate_milestone_prerequisites` | 1 |
//! | `PreviousMilestoneNotComplete` | — | `helpers::assert_prerequisites_complete`, `milestones::batch_confirm_milestones` | 2 |
//! | `ProofHashTooLong` | — | `milestones::submit_proof` | 1 |
//! | `QuorumBelowMinRatio` | — | `engagement::create_engagement_impl` | 1 |
//! | `ReasonTooLong` | — | `disputes::raise_dispute` | 1 |
//! | `RecruiterArbiterCollision` | — | `engagement::create_engagement_impl`, `transfers::admin_add_arbiter` | 2 |
//! | `referrer already exists` | — | `admin::add_referrer` | 1 |
//! | `referrer not found` | — | `admin::remove_referrer` | 1 |
//! | `replacement reason too long` | — | `engagement::request_replacement` | 1 |
//! | `ReplacementLimitReached` | — | `engagement::request_replacement` | 1 |
//! | `ResubmitTooSoon` | — | `milestones::submit_proof` | 1 |
//! | `retention window has not elapsed yet` | — | `milestones::unlock_milestone` | 1 |
//! | `retention window has not elapsed — cannot confirm yet` | — | `milestones::batch_confirm_milestones`, `milestones::confirm_milestone` | 2 |
//! | `RetentionDaysTooLarge` | — | `engagement::create_engagement_impl` | 1 |
//! | `RetentionDaysZero` | — | `engagement::create_engagement_impl` | 1 |
//! | `SplitVotingDisabled` | — | `disputes::cast_arbiter_split_vote` | 1 |
//! | `SuperArbiterResponseWindowNotElapsed` | — | `disputes::resolve_escalation_timeout` | 1 |
//! | `TagEmpty` | — | `engagement::create_engagement_impl` | 1 |
//! | `TagTooLong` | — | `engagement::create_engagement_impl` | 1 |
//! | `tier bps exceeds base platform fee` | — | `admin::set_fee_tiers` | 1 |
//! | `tier threshold must be positive` | — | `admin::set_fee_tiers` | 1 |
//! | `tiers must be sorted by ascending threshold` | — | `admin::set_fee_tiers` | 1 |
//! | `TokenNotAllowed` | — | `engagement::create_engagement_impl` | 1 |
//! | `too many fee tiers` | — | `admin::set_fee_tiers` | 1 |
//! | `too many IDs` | — | `queries::batch_get_engagement_summary` | 1 |
//! | `TooManyMilestones` | — | `engagement::create_engagement_impl` | 1 |
//! | `TooManyTags` | — | `engagement::create_engagement_impl` | 1 |
//! | `unauthorized` | `ERR_UNAUTHORIZED` | 29 functions in `admin`, `disputes`, `engagement`, `features`, `helpers`, `milestones`, `ratings`, `transfers` | 30 |
//! | `unknown config change fn_id` | — | `features::apply_pending_config_change` | 1 |
//! | `UpgradeLockNotElapsed` | — | `admin::execute_upgrade` | 1 |
//! | `VersionTooLong` | — | `admin::set_version` | 1 |
//! | `ZeroMilestones` | — | `engagement::create_engagement_impl` | 1 |
//!
//! <!-- END GENERATED ERRORS TABLE -->

/// Shared panic message constants for the most-repeated error strings
/// (issue #171). Keeping these as constants means a typo can't silently
/// create an inconsistent error surface for off-chain consumers matching
/// on these strings.
pub(crate) const ERR_UNAUTHORIZED: &str = "unauthorized";
pub(crate) const ERR_ENGAGEMENT_NOT_ACTIVE: &str = "engagement is not active";
pub(crate) const ERR_INVALID_MILESTONE_INDEX: &str = "invalid milestone index";
/// Raised when an operation targets an engagement the admin has quarantined
/// via `pause_engagement` (issue #239). Distinct from `"ContractPaused"` so
/// off-chain callers can tell a single-engagement freeze from a global halt.
pub(crate) const ERR_ENGAGEMENT_PAUSED: &str = "EngagementPaused";
/// Raised when an operation targets a single milestone the admin has frozen
/// via `hold_milestone` (issue #492). Distinct from `"EngagementPaused"` so
/// callers can tell a one-milestone compliance hold from an engagement-wide
/// quarantine.
pub(crate) const ERR_MILESTONE_ON_HOLD: &str = "MilestoneOnHold";
