//! Numeric/default configuration constants used throughout the contract.

pub(crate) const MAX_PLATFORM_FEE_BPS: u32 = 500;
pub(crate) const MAX_ARBITER_FEE_BPS: u32 = 200;
pub(crate) const FULL_SPLIT_BPS: u32 = 10_000;
/// Upper bound for `set_min_quorum_ratio_bps` (issue #502): 10 000 bps
/// (100 %) forces a unanimous panel.
pub(crate) const MAX_MIN_QUORUM_RATIO_BPS: u32 = 10_000;

pub(crate) const LEDGERS_PER_DAY: u32 = 17_280; // 86 400s ÷ 5s per ledger
pub(crate) const DEFAULT_PROOF_COOLDOWN: u32 = 2_880; // ~4 hours
pub(crate) const DEFAULT_MAX_RETENTION_DAYS: u32 = 365;
pub(crate) const DEFAULT_MAX_MILESTONES: u32 = 10;
pub(crate) const DEFAULT_INACTIVITY_TIMEOUT_LEDGERS: u32 = 1_036_800; // ~60 days
pub(crate) const DEFAULT_STORAGE_TTL_EXTEND_TO: u32 = 1_036_800; // ~60 days
pub(crate) const DEFAULT_VERSION: &str = "0.2.0";
/// Maximum length (in characters) of a `request_replacement` reason string
/// (issue #51). Mirrors the dispute-reason cap to keep storage bounded.
pub(crate) const MAX_REPLACEMENT_REASON_LEN: u32 = 128;
/// Maximum length (in characters) of a `pause_engagement` reason string (issue #327).
pub(crate) const MAX_PAUSE_REASON_LEN: u32 = 128;
/// Maximum length (in characters) of a `hold_milestone` reason string (issue #492).
pub(crate) const MAX_HOLD_REASON_LEN: u32 = 128;
pub(crate) const DEFAULT_MIN_ENGAGEMENT_AMOUNT: i128 = 100_000; // 0.01 USDC
pub(crate) const DEFAULT_CONFIRM_WINDOW_LEDGERS: u32 = 86_400; // ~5 days
pub(crate) const DEFAULT_DISPUTE_WINDOW_LEDGERS: u32 = 51_840; // ~3 days
pub(crate) const MAX_VERSION_LENGTH: u32 = 32;
pub(crate) const MAX_PROOF_HASH_LENGTH: u32 = 200;
pub(crate) const MAX_ENGAGEMENT_ID_LENGTH: u32 = 64;
pub(crate) const DEFAULT_MAX_ACTIVE_PER_COMPANY: u32 = 50;
/// Default maximum number of replacements allowed per engagement (issue #31).
pub(crate) const DEFAULT_MAX_REPLACEMENTS: u32 = 3;
/// Default deadline, in ledgers, for the super-arbiter to resolve an
/// escalated dispute before `resolve_escalation_timeout` can auto-favor the
/// recruiter (issue #318). Mirrors the default dispute window (~3 days).
pub(crate) const DEFAULT_SUPER_ARBITER_RESPONSE_WINDOW_LEDGERS: u32 = 51_840;
/// Maximum number of tags stored on an engagement (issue #248).
pub(crate) const MAX_TAGS: u32 = 10;
/// Maximum length, in characters, of a single engagement tag (issue #248).
pub(crate) const MAX_TAG_LENGTH: u32 = 32;
/// Selection weight given to arbiter-pool members with no dispute history yet
/// (issue #468): the midpoint of the 1–100 weight range, so newcomers are
/// neither favoured nor excluded.
pub(crate) const DEFAULT_ARBITER_SELECTION_WEIGHT: u32 = 50;
/// Average response time, in ledgers, at which an arbiter's speed score is
/// halved in `get_arbiter_selection_weight` (issue #468). ~1 day.
pub(crate) const ARBITER_RESPONSE_REFERENCE_LEDGERS: u64 = 17_280;
/// TTL for a pending dispute-window override proposal (issue #469). ~1 day.
pub(crate) const DISPUTE_WINDOW_PROPOSAL_TTL_LEDGERS: u32 = 17_280;
/// Default TTL for amendment / split-amendment proposals (~1 day).
pub(crate) const DEFAULT_AMENDMENT_TTL_LEDGERS: u32 = 17_280;
/// FIFO cap on per-engagement split amendment log entries (issue #471).
pub(crate) const MAX_AMENDMENT_LOG_ENTRIES: u32 = 20;
/// FIFO cap on per-engagement dispute history entries (issue #501).
pub(crate) const MAX_DISPUTE_HISTORY_ENTRIES: u32 = 50;
/// FIFO cap on per-engagement status history entries (issue #501).
pub(crate) const MAX_STATUS_HISTORY_ENTRIES: u32 = 50;

/// Minimum stars accepted by `rate_recruiter` (issue #470).
pub(crate) const MIN_RATING_STARS: u32 = 1;
/// Maximum stars accepted by `rate_recruiter` (issue #470).
pub(crate) const MAX_RATING_STARS: u32 = 5;
/// Default window for emergency pause votes to accumulate (issue #474).
pub(crate) const DEFAULT_EMERGENCY_VOTE_WINDOW_LEDGERS: u32 = 1_728;
/// Sensitive-setter function ids for cosigner gating (issue #473).
pub(crate) const FN_SET_PLATFORM_FEE: u32 = 1;
pub(crate) const FN_SET_TOKEN_ALLOWLIST_ENABLED: u32 = 2;
pub(crate) const FN_SET_REFERRAL_DISCOUNT_BPS: u32 = 3;
pub(crate) const FN_SET_ARBITER_FEE: u32 = 4;
pub(crate) const FN_SET_MIN_AMOUNT: u32 = 5;
pub(crate) const FN_SET_FEE_REBATE_BPS: u32 = 6;

/// Default risk-score points per active dispute (issue #480).
pub(crate) const DEFAULT_RISK_DISPUTE_WEIGHT: u32 = 30;
/// Default risk-score points per recruiter replacement (issue #480).
pub(crate) const DEFAULT_RISK_REPLACEMENT_WEIGHT: u32 = 20;
/// Default risk-score points per granted milestone extension (issue #480).
pub(crate) const DEFAULT_RISK_EXTENSION_WEIGHT: u32 = 10;
