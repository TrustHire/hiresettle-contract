//! Numeric/default configuration constants used throughout the contract.

pub(crate) const MAX_PLATFORM_FEE_BPS: u32 = 500;
pub(crate) const MAX_ARBITER_FEE_BPS: u32 = 200;
pub(crate) const FULL_SPLIT_BPS: u32 = 10_000;

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
/// Lowest star rating a company may give a recruiter (issue #470).
pub(crate) const MIN_RATING_STARS: u32 = 1;
/// Highest star rating a company may give a recruiter (issue #470).
pub(crate) const MAX_RATING_STARS: u32 = 5;
/// Ledgers a dispute window override proposal stays open for acceptance
/// before it expires and is treated as cleared (issue #469, ~3 days).
pub(crate) const DISPUTE_WINDOW_PROPOSAL_TTL_LEDGERS: u32 = 51_840;
