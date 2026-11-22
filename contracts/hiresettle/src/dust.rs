//! Dust sweep to the company on engagement completion (issue #673).

use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    /// Admin sets the escrow-dust threshold in stroops. Remaining escrow at or
    /// below this value is refunded when an engagement completes. `0` disables.
    pub fn set_dust_threshold(env: Env, admin: Address, threshold: i128) {
        Self::assert_admin(&env, &admin);
        if threshold < 0 {
            panic!("InvalidDustThreshold");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::DustThreshold), &threshold);
        env.events()
            .publish((Symbol::new(&env, "dust_threshold_set"),), threshold);
    }

    /// Return the configured dust threshold (default `0`, disabled).
    pub fn get_dust_threshold(env: Env) -> i128 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::DustThreshold))
            .unwrap_or(0)
    }

    /// Escrow remaining for an engagement: `total_amount - released_amount`.
    pub fn get_engagement_escrow_balance(env: Env, engagement_id: String) -> i128 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        (engagement.total_amount - engagement.released_amount).max(0)
    }

    /// Dust refunded on completion, if any. Surfaces leftover balances above
    /// the threshold to indexers and the invariant/health queries.
    pub fn get_escrow_dust_refund(env: Env, engagement_id: String) -> Option<i128> {
        env.storage()
            .persistent()
            .get(&DataKey3::EscrowDustRefund(engagement_id))
    }

    pub(crate) fn sweep_dust_on_completion(env: &Env, engagement: &mut Engagement) {
        let threshold: i128 = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::DustThreshold))
            .unwrap_or(0);
        if threshold <= 0 {
            return;
        }
        let remaining = engagement.total_amount - engagement.released_amount;
        // Above-threshold balances are deliberately left in escrow.
        if remaining <= 0 || remaining > threshold {
            return;
        }
        engagement.released_amount += remaining;
        Self::add_token_locked(env, &engagement.token, -remaining);
        token::Client::new(env, &engagement.token).transfer(
            &env.current_contract_address(),
            &engagement.company,
            &remaining,
        );
        let key = DataKey3::EscrowDustRefund(engagement.id.clone());
        env.storage().persistent().set(&key, &remaining);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
        env.events().publish(
            (Symbol::new(env, "escrow_dust_refunded"), engagement.id.clone()),
            (engagement.company.clone(), remaining),
        );
    }
}
