//! Issue #662 — optional per-engagement insurance pool.
//!
//! Premiums are an additive line item on insured payouts. They are never part
//! of the platform-fee base and are credited to a per-token pool held by the
//! contract. Claims are admin-controlled and capped by the premiums an
//! engagement has actually paid, multiplied by the configured multiplier.

use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    /// Configure the premium rate (bps of each payout) and the claim cap as a
    /// multiple of premiums paid (10_000 = 1x).
    pub fn set_insurance_config(
        env: Env,
        admin: Address,
        premium_bps: u32,
        claim_multiplier_bps: u32,
    ) {
        Self::assert_admin(&env, &admin);
        if premium_bps > FULL_SPLIT_BPS || claim_multiplier_bps > 1_000_000 {
            panic!("InvalidInsuranceConfig");
        }
        env.storage()
            .instance()
            .set(&DataKey3::InsurancePremiumBps, &premium_bps);
        env.storage().instance().set(
            &DataKey3::InsuranceClaimMultiplierBps,
            &claim_multiplier_bps,
        );
        env.events().publish(
            (Symbol::new(&env, "insurance_config_set"),),
            (premium_bps, claim_multiplier_bps),
        );
    }

    /// `(premium_bps, claim_multiplier_bps)`, or `(0, 10_000)` by default.
    pub fn get_insurance_config(env: Env) -> (u32, u32) {
        let premium_bps = env
            .storage()
            .instance()
            .get(&DataKey3::InsurancePremiumBps)
            .unwrap_or(0);
        let multiplier = env
            .storage()
            .instance()
            .get(&DataKey3::InsuranceClaimMultiplierBps)
            .unwrap_or(10_000);
        (premium_bps, multiplier)
    }

    /// Insurance pool balance for a token, in that token's smallest unit.
    pub fn get_insurance_pool_balance(env: Env, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey3::InsurancePool(token))
            .unwrap_or(0)
    }

    /// Premiums paid by one engagement in its own token.
    pub fn get_engagement_insurance_premium(env: Env, engagement_id: String) -> i128 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        env.storage()
            .persistent()
            .get(&DataKey3::EngagementInsurancePremiums(
                engagement_id,
                engagement.token,
            ))
            .unwrap_or(0)
    }

    /// Premium that would be charged on `gross_amount` for an engagement.
    /// Additive to fee quotes; it is not included in the platform fee.
    pub fn get_insurance_premium_quote(
        env: Env,
        engagement_id: String,
        gross_amount: i128,
    ) -> i128 {
        if !env
            .storage()
            .persistent()
            .get::<DataKey3, bool>(&DataKey3::EngagementInsured(engagement_id))
            .unwrap_or(false)
            || gross_amount <= 0
        {
            return 0;
        }
        let premium_bps: u32 = env
            .storage()
            .instance()
            .get(&DataKey3::InsurancePremiumBps)
            .unwrap_or(0);
        (gross_amount * premium_bps as i128) / 10_000
    }

    /// Pay a claim from a token's insurance pool. Capped by the premiums paid
    /// on that engagement times the configured multiplier; admin-only.
    pub fn pay_insurance_claim(
        env: Env,
        admin: Address,
        engagement_id: String,
        amount: i128,
        recipient: Address,
    ) {
        Self::assert_admin(&env, &admin);
        if amount <= 0 {
            panic!("InvalidAmount");
        }
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let pool_key = DataKey3::InsurancePool(engagement.token.clone());
        let balance: i128 = env.storage().persistent().get(&pool_key).unwrap_or(0);
        if amount > balance {
            panic!("InsurancePoolInsufficient");
        }
        let premiums_key = DataKey3::EngagementInsurancePremiums(
            engagement_id.clone(),
            engagement.token.clone(),
        );
        let premiums: i128 = env.storage().persistent().get(&premiums_key).unwrap_or(0);
        let multiplier: u32 = env
            .storage()
            .instance()
            .get(&DataKey3::InsuranceClaimMultiplierBps)
            .unwrap_or(10_000);
        let cap = (premiums * multiplier as i128) / 10_000;
        if amount > cap {
            panic!("InsuranceClaimCapExceeded");
        }
        env.storage().persistent().set(&pool_key, &(balance - amount));
        token::Client::new(&env, &engagement.token).transfer(
            &env.current_contract_address(),
            &recipient,
            &amount,
        );
        env.events().publish(
            (Symbol::new(&env, "insurance_claim_paid"), engagement_id),
            (admin, amount, recipient),
        );
    }

    /// Credit a premium on an insured payout. Funds stay in the contract and
    /// are tracked per token; the premium is excluded from platform fee math.
    pub(crate) fn collect_insurance_premium(
        env: &Env,
        engagement: &Engagement,
        engagement_id: &String,
        gross_amount: i128,
    ) -> i128 {
        let premium = Self::get_insurance_premium_quote(
            env.clone(),
            engagement_id.clone(),
            gross_amount,
        );
        if premium <= 0 {
            return 0;
        }
        let pool_key = DataKey3::InsurancePool(engagement.token.clone());
        let balance: i128 = env.storage().persistent().get(&pool_key).unwrap_or(0);
        env.storage().persistent().set(&pool_key, &(balance + premium));
        env.storage()
            .persistent()
            .extend_ttl(&pool_key, 100_000, 6_300_000);

        let premiums_key = DataKey3::EngagementInsurancePremiums(
            engagement_id.clone(),
            engagement.token.clone(),
        );
        let paid: i128 = env.storage().persistent().get(&premiums_key).unwrap_or(0);
        env.storage().persistent().set(&premiums_key, &(paid + premium));
        env.storage()
            .persistent()
            .extend_ttl(&premiums_key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(env, "insurance_premium_collected"),
                engagement_id.clone(),
            ),
            (engagement.token.clone(), premium),
        );
        premium
    }
}
