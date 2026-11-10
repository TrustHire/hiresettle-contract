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
//! | `already voted` | — | `admin::vote_signer_rotation`, `disputes::recuse_arbiter`, `features::cast_emergency_pause_vote`, `features::rotate_emergency_signer` | 4 |
//! | `AlreadyInPool` | — | `arbiter_pool::add_arbiter_pool_member` | 1 |
//! | `AlreadyRated` | — | `ratings::rate_recruiter` | 1 |
//! | `AlreadyVoted` | — | `disputes::super_arbiter_resolve` | 1 |
//! | `amendment_expired` | — | `engagement::accept_split_amendment`, `engagement::reject_split_amendment` | 2 |
//! | `AmendmentCooldownActive` | — | `engagement::propose_split_amendment` | 1 |
//! | `amount must be greater than zero` | — | 5 functions in `engagement`, `features` | 5 |
//! | `AmountBelowMinimum` | — | `engagement::create_engagement_impl` | 1 |
//! | `ArbiterFeeTooHigh` | — | `admin::set_arbiter_fee` | 1 |
//! | `ArbiterNotFound` | — | `transfers::admin_remove_arbiter` | 1 |
//! | `ArbiterPoolTooSmall` | — | `arbiter_pool::draw_arbiter_panel` | 2 |
//! | `ArbiterRecused` | — | `disputes::cast_arbiter_split_vote`, `disputes::cast_arbiter_vote`, `disputes::change_arbiter_vote` | 3 |
//! | `ArbiterWeightOverflow` | — | `engagement::create_engagement_impl` | 1 |
//! | `ArbiterWeightsLengthMismatch` | — | `engagement::create_engagement_impl` | 1 |
//! | `at least one arbiter required` | — | `bonds::create_engagement_bundle`, `engagement::create_engagement_impl`, `transfers::admin_remove_arbiter` | 3 |
//! | `BundleAlreadyExists` | — | `bonds::create_engagement_bundle` | 1 |
//! | `BundleCompanyMismatch` | — | `bonds::load_bundle_for` | 1 |
//! | `BundleNotFound` | — | `bonds::load_bundle_for` | 1 |
//! | `can only dispute a submitted proof` | — | `disputes::assert_milestone_disputable` | 1 |
//! | `cancellation_expired` | — | `engagement::accept_cancellation` | 1 |
//! | `Cannot expire completed engagement` | — | `admin::expire_engagement` | 1 |
//! | `CompanyActiveLimitReached` | — | `engagement::create_engagement_impl` | 1 |
//! | `CompanyArbiterCollision` | — | `bonds::create_engagement_bundle`, `engagement::create_engagement_impl`, `transfers::admin_add_arbiter` | 3 |
//! | `CompanyRecruiterCollision` | — | `engagement::create_engagement_impl` | 1 |
//! | `ConfigChangeCancelled` | — | `features::accept_config_change` | 1 |
//! | `ConfigChangeExpired` | — | `features::accept_config_change` | 1 |
//! | `ConfirmWindowNotElapsed` | — | `disputes::force_confirm_milestone` | 1 |
//! | `ConflictAlreadyDeclared` | — | `arbiter_pool::declare_conflict_of_interest` | 1 |
//! | `ConflictReasonTooLong` | — | `arbiter_pool::declare_conflict_of_interest` | 1 |
//! | `contract not initialised` | — | `admin::set_proof_cooldown` | 1 |
//! | `ContractPaused` | — | `helpers::assert_not_paused` | 1 |
//! | `DelegateAlreadyAssigned` | — | `disputes::set_arbiter_vote_delegate` | 1 |
//! | `discount too high` | — | `admin::set_referral_discount_bps` | 1 |
//! | `dispute already resolvable without escalation` | — | `disputes::escalate_dispute` | 1 |
//! | `dispute has not been escalated` | — | `disputes::resolve_escalation_timeout`, `disputes::super_arbiter_resolve` | 2 |
//! | `DisputeEscalated` | — | `disputes::assert_dispute_settleable` | 1 |
//! | `DisputePending` | — | `engagement::close_engagement_at_end` | 1 |
//! | `DisputeWindowClosed` | — | `disputes::assert_milestone_disputable` | 1 |
//! | `DisputeWindowNotElapsed` | — | `disputes::escalate_dispute` | 1 |
//! | `DisputeWindowProposalPending` | — | `disputes::propose_dispute_window_override` | 1 |
//! | `duplicate vote` | — | `disputes::cast_arbiter_split_vote`, `disputes::cast_arbiter_vote` | 2 |
//! | `DuplicateArbiter` | — | `bonds::create_engagement_bundle`, `transfers::admin_add_arbiter` | 2 |
//! | `DuplicateEmergencySigner` | — | `features::rotate_emergency_signer`, `features::set_emergency_signers` | 2 |
//! | `DuplicateExternalRef` | — | `engagement::create_engagement_impl`, `transfers::transfer_company` | 2 |
//! | `DuplicateMilestoneIndex` | — | `disputes::batch_raise_dispute` | 1 |
//! | `DuplicateMilestoneName` | — | `engagement::create_engagement_impl` | 1 |
//! | `DuplicateProofHash` | — | `milestones::submit_proof_internal` | 1 |
//! | `DuplicateTreasury` | — | `admin::set_fee_treasuries` | 1 |
//! | `emergency signers not configured` | — | `admin::expire_signer_rotation`, `admin::propose_signer_rotation`, `admin::vote_signer_rotation` | 3 |
//! | `EmptyHoldReason` | — | `admin::hold_milestone` | 1 |
//! | `EmptyIndices` | — | `disputes::batch_raise_dispute`, `milestones::batch_confirm_milestones` | 2 |
//! | `EmptyPauseReason` | — | `admin::pause_engagement` | 1 |
//! | `EmptyTreasuryList` | — | `admin::set_fee_treasuries` | 1 |
//! | `EndLedgerNotReached` | — | `engagement::close_engagement_at_end` | 1 |
//! | `engagement already exists` | — | `engagement::create_engagement_impl` | 1 |
//! | `engagement is in a terminal state` | — | 5 functions in `admin`, `disputes`, `transfers` | 5 |
//! | `engagement is not active` | `ERR_ENGAGEMENT_NOT_ACTIVE` | 33 functions in `admin`, `disputes`, `engagement`, `milestones`, `transfers` | 33 |
//! | `engagement not found` | — | `helpers::get_engagement_internal` | 1 |
//! | `EngagementNotCompleted` | — | `ratings::rate_recruiter` | 1 |
//! | `EngagementNotOffered` | — | `engagement::accept_engagement`, `engagement::withdraw_offer` | 2 |
//! | `EngagementNotPaused` | — | `admin::emergency_exit` | 2 |
//! | `EngagementPaused` | `ERR_ENGAGEMENT_PAUSED` | `helpers::assert_engagement_not_paused` | 1 |
//! | `EscapeDisabled` | — | `admin::emergency_exit` | 1 |
//! | `EscapeNotReady` | — | `admin::emergency_exit` | 1 |
//! | `fee tier not found` | — | `admin::remove_fee_tier` | 1 |
//! | `FeeChangeDelayNotElapsed` | — | `admin::apply_scheduled_fee` | 1 |
//! | `FeeTooHigh` | — | 5 functions in `admin`, `features` | 5 |
//! | `FunctionPaused` | — | `helpers::assert_function_not_paused` | 1 |
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
//! | `InvalidCancellationBps` | — | `engagement::propose_cancellation` | 1 |
//! | `InvalidContractPdfHash` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidDelegate` | — | `disputes::set_arbiter_vote_delegate` | 1 |
//! | `InvalidDisputeWindow` | — | `disputes::propose_dispute_window_override` | 1 |
//! | `InvalidEmergencyThreshold` | — | `features::set_emergency_signers` | 1 |
//! | `InvalidEmergencyVoteWindow` | — | `features::set_emergency_vote_window` | 1 |
//! | `InvalidEndLedger` | — | `engagement::create_engagement_impl` | 2 |
//! | `InvalidEngagementId` | — | `engagement::create_engagement_impl` | 2 |
//! | `InvalidExternalRef` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidFeeFloor` | — | `admin::set_platform_fee_floor` | 1 |
//! | `InvalidMaxActivePerCompany` | — | `admin::set_max_active_per_company` | 1 |
//! | `InvalidMetadataHash` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidMinAmount` | — | `admin::set_token_min_amount` | 1 |
//! | `InvalidPrerequisiteIndex` | — | `helpers::validate_milestone_prerequisites` | 1 |
//! | `InvalidProofHash` | — | `milestones::submit_proof` | 1 |
//! | `InvalidQuorumRatio` | — | `admin::set_min_quorum_ratio_bps` | 1 |
//! | `InvalidRating` | — | `ratings::rate_recruiter` | 1 |
//! | `InvalidRecruiterBps` | — | `disputes::propose_dispute_settlement` | 1 |
//! | `InvalidReferrerFeeShareBps` | — | `admin::set_referrer_fee_share_bps` | 1 |
//! | `InvalidReleaseBps` | — | `milestones::propose_partial_confirmation` | 1 |
//! | `InvalidSlippageBps` | — | `payout::set_recruiter_payout_slippage` | 1 |
//! | `InvalidSplitBps` | — | `engagement::create_engagement_impl`, `engagement::propose_split_amendment` | 2 |
//! | `InvalidSplitPercent` | — | `disputes::cast_arbiter_split_vote` | 1 |
//! | `InvalidStreamDuration` | — | `engagement::create_engagement_impl` | 1 |
//! | `InvalidSuperArbiterPanel` | — | `disputes::set_super_arbiter_panel` | 2 |
//! | `InvalidSuperArbiterResponseWindow` | — | `disputes::set_super_arbiter_deadline` | 1 |
//! | `InvalidTreasuryWeight` | — | `admin::set_fee_treasuries` | 1 |
//! | `InvalidUntilLedger` | — | `arbiter_pool::suspend_arbiter_pool_member` | 1 |
//! | `JobTitleEmpty` | — | `engagement::create_engagement_impl` | 1 |
//! | `JobTitleTooLong` | — | `engagement::create_engagement_impl` | 1 |
//! | `MaxDisputeCyclesReached` | — | `disputes::apply_dispute_raised` | 1 |
//! | `milestone is not in disputed status` | — | 9 functions in `disputes` | 9 |
//! | `milestone is not in ProofSubmitted status` | — | `disputes::force_confirm_milestone` | 1 |
//! | `milestone is not locked` | — | `milestones::unlock_milestone` | 1 |
//! | `milestone is not pending` | — | `milestones::submit_proof_internal`, `milestones::trigger_no_show` | 2 |
//! | `milestone percentages must sum to 100` | — | `engagement::create_engagement_impl` | 1 |
//! | `milestone proof not yet submitted` | — | `milestones::accept_partial_confirmation`, `milestones::batch_confirm_milestones`, `milestones::confirm_milestone`, `milestones::propose_partial_confirmation` | 4 |
//! | `MilestoneDisputed` | — | `engagement::assert_negotiated_cancellation_allowed` | 1 |
//! | `MilestoneNameEmpty` | — | `engagement::create_engagement_impl` | 1 |
//! | `MilestoneNameTooLong` | — | `engagement::create_engagement_impl` | 1 |
//! | `MilestoneOnHold` | `ERR_MILESTONE_ON_HOLD` | `helpers::assert_milestone_not_on_hold` | 1 |
//! | `missing treasury` | — | `features::apply_pending_config_change` | 1 |
//! | `MixedVoteModes` | — | `disputes::cast_arbiter_split_vote`, `disputes::cast_arbiter_vote` | 2 |
//! | `new signer already in emergency signer set` | — | `admin::propose_signer_rotation` | 1 |
//! | `no active signer rotation vote` | — | `admin::cancel_signer_rotation`, `admin::expire_signer_rotation`, `admin::vote_signer_rotation` | 3 |
//! | `no co_recruiter` | — | `engagement::assert_split_amendment_counterparty`, `engagement::propose_split_amendment` | 2 |
//! | `no dispute in progress` | — | `disputes::enforce_vote_deadline`, `disputes::escalate_dispute` | 2 |
//! | `no emergency signers` | — | `features::cast_emergency_pause_vote`, `features::rotate_emergency_signer` | 2 |
//! | `no escalation timestamp recorded` | — | `disputes::resolve_escalation_timeout` | 1 |
//! | `no pending admin nomination` | — | `admin::claim_admin` | 1 |
//! | `no pending amendment proposal` | — | `engagement::accept_split_amendment`, `engagement::reject_split_amendment` | 2 |
//! | `no pending arbiter nomination` | — | `transfers::claim_arbiter` | 1 |
//! | `no pending cancellation proposal` | — | `engagement::accept_cancellation` | 1 |
//! | `no pending config change` | — | `features::accept_config_change`, `features::cancel_config_change` | 2 |
//! | `no pending dispute settlement` | — | `disputes::accept_dispute_settlement` | 2 |
//! | `no pending partial confirmation` | — | `milestones::accept_partial_confirmation`, `milestones::reject_partial_confirmation` | 2 |
//! | `no pending recruiter transfer` | — | `transfers::accept_recruiter_transfer` | 1 |
//! | `no pending upgrade` | — | `admin::cancel_upgrade`, `admin::execute_upgrade` | 2 |
//! | `no super arbiter configured` | — | `disputes::escalate_dispute`, `disputes::super_arbiter_resolve` | 2 |
//! | `NoAdmin` | — | `helpers::assert_admin`, `helpers::assert_arbiter_pool_manager`, `helpers::assert_fee_manager`, `helpers::assert_pauser` | 4 |
//! | `NoConflictDeclaration` | — | `arbiter_pool::clear_conflict_declaration` | 2 |
//! | `NoEndLedger` | — | `engagement::close_engagement_at_end` | 1 |
//! | `NoPendingDisputeWindowProposal` | — | `disputes::accept_dispute_window_override`, `disputes::reject_dispute_window_override` | 2 |
//! | `NoPendingVoters` | — | `disputes::enforce_vote_deadline` | 1 |
//! | `NoScheduledFee` | — | `admin::apply_scheduled_fee`, `admin::cancel_scheduled_fee` | 2 |
//! | `NoShowDeadlineNotReached` | — | `milestones::trigger_no_show` | 1 |
//! | `NoShowDisabled` | — | `milestones::trigger_no_show` | 1 |
//! | `NoStreamedPayout` | — | `milestones::claim_streamed_payout` | 1 |
//! | `NotInPool` | — | `arbiter_pool::remove_arbiter_pool_member`, `arbiter_pool::suspend_arbiter_pool_member`, `arbiter_pool::unsuspend_arbiter_pool_member` | 3 |
//! | `NotVoted` | — | `disputes::change_arbiter_vote` | 3 |
//! | `old signer not in emergency signer set` | — | `admin::propose_signer_rotation` | 1 |
//! | `only placement milestones can be forfeited` | — | `milestones::trigger_no_show` | 1 |
//! | `only retention milestones can be unlocked this way` | — | `milestones::unlock_milestone` | 1 |
//! | `PanelChangeDuringDispute` | — | `transfers::get_engagement_for_panel_change` | 1 |
//! | `PartialConfirmationExpired` | — | `milestones::accept_partial_confirmation` | 1 |
//! | `PauseReasonTooLong` | — | `admin::pause_engagement` | 1 |
//! | `placement not yet confirmed — use cancel_engagement instead` | — | `engagement::request_replacement` | 1 |
//! | `PrerequisiteCycle` | — | `helpers::validate_milestone_prerequisites` | 1 |
//! | `PreviousMilestoneNotComplete` | — | `helpers::assert_prerequisites_complete`, `milestones::batch_confirm_milestones` | 2 |
//! | `ProofHashTooLong` | — | `milestones::submit_proof` | 1 |
//! | `proposer not in emergency signer set` | — | `admin::propose_signer_rotation` | 1 |
//! | `QuorumBelowMinRatio` | — | `engagement::create_engagement_impl` | 1 |
//! | `QuorumUnreachable` | — | `transfers::admin_remove_arbiter` | 1 |
//! | `ReasonTooLong` | — | `disputes::batch_raise_dispute`, `disputes::raise_dispute` | 2 |
//! | `RecruiterActiveLimitReached` | — | `engagement::create_engagement_impl` | 1 |
//! | `RecruiterArbiterCollision` | — | `engagement::create_engagement_impl`, `transfers::admin_add_arbiter` | 2 |
//! | `referrer already exists` | — | `admin::add_referrer` | 1 |
//! | `referrer not found` | — | `admin::remove_referrer` | 1 |
//! | `replacement reason too long` | — | `engagement::request_replacement` | 1 |
//! | `ReplacementLimitReached` | — | `engagement::request_replacement` | 1 |
//! | `ResubmitTooSoon` | — | `milestones::submit_proof_internal` | 1 |
//! | `retention window has not elapsed yet` | — | `milestones::unlock_milestone` | 1 |
//! | `retention window has not elapsed — cannot confirm yet` | — | `milestones::accept_partial_confirmation`, `milestones::batch_confirm_milestones`, `milestones::confirm_milestone` | 3 |
//! | `RetentionDaysTooLarge` | — | `engagement::create_engagement_impl` | 1 |
//! | `RetentionDaysZero` | — | `engagement::create_engagement_impl` | 1 |
//! | `signer rotation vote already in progress` | — | `admin::propose_signer_rotation` | 1 |
//! | `SplitVotingDisabled` | — | `disputes::cast_arbiter_split_vote` | 1 |
//! | `SuperArbiterResponseWindowNotElapsed` | — | `disputes::resolve_escalation_timeout` | 1 |
//! | `SwapOutputBelowMinimum` | — | `payout::try_swap_payout` | 1 |
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
//! | `TooManyTreasuries` | — | `admin::set_fee_treasuries` | 1 |
//! | `TreasuryWeightsMustSumTo10000` | — | `admin::set_fee_treasuries` | 1 |
//! | `unauthorized` | `ERR_UNAUTHORIZED` | 45 functions in `admin`, `arbiter_pool`, `disputes`, `engagement`, `features`, `helpers`, `milestones`, `ratings`, `transfers` | 47 |
//! | `unknown config change fn_id` | — | `features::apply_pending_config_change` | 1 |
//! | `UpgradeLockNotElapsed` | — | `admin::execute_upgrade` | 1 |
//! | `VersionTooLong` | — | `admin::set_version` | 1 |
//! | `vote window expired` | — | `admin::vote_signer_rotation` | 1 |
//! | `vote window not yet expired` | — | `admin::expire_signer_rotation` | 1 |
//! | `VoteAlreadyChanged` | — | `disputes::change_arbiter_vote` | 1 |
//! | `VoteDeadlineDisabled` | — | `disputes::enforce_vote_deadline` | 1 |
//! | `VoteDeadlineNotReached` | — | `disputes::enforce_vote_deadline` | 1 |
//! | `voter not in emergency signer set` | — | `admin::vote_signer_rotation` | 1 |
//! | `VoteUnchanged` | — | `disputes::change_arbiter_vote` | 1 |
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
