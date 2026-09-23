//! Shared panic message constants for the most-repeated error strings
//! (issue #171). Keeping these as constants means a typo can't silently
//! create an inconsistent error surface for off-chain consumers matching
//! on these strings.

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
