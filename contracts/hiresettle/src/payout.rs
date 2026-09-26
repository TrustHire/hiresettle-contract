use soroban_sdk::{contractclient, contractimpl, token, Address, Env, Symbol};
use crate::*;

/// Interface a trusted swap-adapter contract must implement (issue #458).
///
/// Before calling `swap`, HireSettle transfers `amount_in` of `from_token` to
/// the adapter. The adapter must deliver the swapped `to_token` amount to
/// `recipient` and return it, or panic — a panic reverts the whole payout.
#[contractclient(name = "SwapAdapterClient")]
pub trait SwapAdapter {
    fn swap(
        env: Env,
        from_token: Address,
        to_token: Address,
        amount_in: i128,
        recipient: Address,
    ) -> i128;
}

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #458 — RECRUITER PAYOUT TOKEN PREFERENCE
    // ----------------------------------------------------------

    /// Recruiter registers the token they want their net milestone payouts
    /// delivered in, across all of their engagements. Only applied when the
    /// admin has configured a swap adapter.
    pub fn set_recruiter_payout_token(env: Env, recruiter: Address, token: Address) {
        recruiter.require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::RecruiterPayoutToken(recruiter.clone()), &token);
        env.storage().persistent().extend_ttl(
            &DataKey::RecruiterPayoutToken(recruiter.clone()),
            100_000,
            6_300_000,
        );
        env.events().publish(
            (Symbol::new(&env, "payout_token_set"), recruiter),
            token,
        );
    }

    /// Recruiter clears their payout token preference; payouts revert to the
    /// engagement's escrow token.
    pub fn clear_recruiter_payout_token(env: Env, recruiter: Address) {
        recruiter.require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey::RecruiterPayoutToken(recruiter.clone()));
        env.events()
            .publish((Symbol::new(&env, "payout_token_cleared"), recruiter), ());
    }

    /// Return the recruiter's preferred payout token, if any.
    pub fn get_recruiter_payout_token(env: Env, recruiter: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::RecruiterPayoutToken(recruiter))
    }

    /// Admin registers the trusted swap-adapter contract used to convert
    /// recruiter payouts into their preferred token. Replaces any existing one.
    pub fn set_swap_adapter(env: Env, admin: Address, adapter: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::SwapAdapter), &adapter);
        env.events()
            .publish((Symbol::new(&env, "swap_adapter_set"),), adapter);
    }

    /// Admin removes the swap adapter; payouts fall back to the escrow token.
    pub fn clear_swap_adapter(env: Env, admin: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .remove(&DataKey::Config(ConfigKey::SwapAdapter));
        env.events()
            .publish((Symbol::new(&env, "swap_adapter_cleared"),), ());
    }

    /// Return the configured swap adapter, if any.
    pub fn get_swap_adapter(env: Env) -> Option<Address> {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::SwapAdapter))
    }

    /// Pay `amount` of the escrow token owed to `recipient`, swapping it into
    /// the recipient's preferred payout token first when both a preference and
    /// a swap adapter are configured. An adapter failure panics, reverting the
    /// whole call (fee transfers included) rather than paying out partially.
    pub(crate) fn pay_recruiter_share(
        env: &Env,
        token_client: &token::Client,
        recipient: &Address,
        amount: i128,
    ) {
        let preferred: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterPayoutToken(recipient.clone()));
        let adapter: Option<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::SwapAdapter));

        match (preferred, adapter) {
            (Some(to_token), Some(adapter))
                if amount > 0 && to_token != token_client.address =>
            {
                token_client.transfer(&env.current_contract_address(), &adapter, &amount);
                let amount_out = SwapAdapterClient::new(env, &adapter).swap(
                    &token_client.address,
                    &to_token,
                    &amount,
                    recipient,
                );
                env.events().publish(
                    (Symbol::new(env, "payout_swapped"), recipient.clone()),
                    (token_client.address.clone(), to_token, amount, amount_out),
                );
            }
            _ => {
                token_client.transfer(&env.current_contract_address(), recipient, &amount);
            }
        }
    }
}
