//! Withholding split and payout fee quote views (issue #670).

use soroban_sdk::{contractimpl, Env, String};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    /// Show how a gross milestone amount is split for an engagement: platform
    /// fee, withholding, and the net reaching the recruiter side.
    pub fn get_payout_breakdown(env: Env, engagement_id: String, gross: i128) -> PayoutBreakdown {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let platform_bps = Self::payout_fee_bps(&env, &engagement_id, &engagement);
        let platform_fee = Self::platform_fee_amount(&env, &engagement_id, gross, platform_bps);
        let after_platform = gross - platform_fee;
        let withholding = if engagement.withholding_address.is_some() {
            after_platform
                .checked_mul(engagement.withholding_bps as i128)
                .map(|v| v / FULL_SPLIT_BPS as i128)
                .unwrap_or(0)
                .min(after_platform.max(0))
        } else {
            0
        };
        let withholding_bps = engagement.withholding_bps;
        PayoutBreakdown {
            gross,
            platform_fee,
            withholding,
            net: after_platform - withholding,
            total_bps: platform_bps.saturating_add(withholding_bps),
        }
    }

    /// Alias used by fee-quote clients: same breakdown as
    /// `get_payout_breakdown` but for a prospective gross amount.
    pub fn get_fee_quote(env: Env, engagement_id: String, gross: i128) -> PayoutBreakdown {
        Self::get_payout_breakdown(env, engagement_id, gross)
    }

    /// Configured withholding `(address, bps)` for an engagement, if any.
    pub fn get_withholding(env: Env, engagement_id: String) -> Option<(soroban_sdk::Address, u32)> {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement
            .withholding_address
            .map(|address| (address, engagement.withholding_bps))
    }
}
