use soroban_sdk::{
    auth::{ContractContext, InvokerContractAuthEntry, SubContractInvocation},
    contractclient, contractimpl, token, vec, Address, Env, IntoVal, String, Symbol,
};
use crate::*;

/// Interface a trusted swap-adapter contract must implement (issues #458, #549).
///
/// HireSettle first calls `quote` for the expected output, derives
/// `min_out = quote * (10_000 - max_slippage_bps) / 10_000` from the
/// recipient's slippage tolerance, then calls `swap`. Inside `swap` the
/// adapter pulls `amount_in` of `from_token` from `payer` (HireSettle
/// pre-authorizes exactly that `transfer(payer, adapter, amount_in)`),
/// delivers the swapped `to_token` amount to `recipient` and returns it.
///
/// If the adapter cannot deliver at least `min_out` it must panic. HireSettle
/// invokes `swap` with `try_`, so the panic rolls back the adapter's pull and
/// the payout falls back to the original token instead of failing.
#[contractclient(name = "SwapAdapterClient")]
pub trait SwapAdapter {
    fn quote(env: Env, from_token: Address, to_token: Address, amount_in: i128) -> i128;

    fn swap(
        env: Env,
        payer: Address,
        from_token: Address,
        to_token: Address,
        amount_in: i128,
        min_out: i128,
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
            .set(&DataKey2::RecruiterPayoutToken(recruiter.clone()), &token);
        env.storage().persistent().extend_ttl(
            &DataKey2::RecruiterPayoutToken(recruiter.clone()),
            100_000,
            6_300_000,
        );
        env.events().publish(
            (Symbol::new(&env, "payout_token_set"), recruiter),
            token,
        );
    }

    /// Recruiter clears their payout token preference; payouts revert to the
    /// engagement's escrow token. Also resets their slippage tolerance to the
    /// default (issue #549).
    pub fn clear_recruiter_payout_token(env: Env, recruiter: Address) {
        recruiter.require_auth();
        env.storage()
            .persistent()
            .remove(&DataKey2::RecruiterPayoutToken(recruiter.clone()));
        env.storage()
            .persistent()
            .remove(&DataKey3::RecruiterPayoutSlippage(recruiter.clone()));
        env.events()
            .publish((Symbol::new(&env, "payout_token_cleared"), recruiter), ());
    }

    /// Return the recruiter's preferred payout token, if any.
    pub fn get_recruiter_payout_token(env: Env, recruiter: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey2::RecruiterPayoutToken(recruiter))
    }

    // ----------------------------------------------------------
    // ISSUE #548 — RECRUITER PAYOUT DESTINATION
    // ----------------------------------------------------------

    /// Recruiter (primary or co-recruiter) sets the address that receives
    /// their payouts across all engagements — milestone payouts, streamed
    /// claims, swapped payouts, bond refunds and `claim_payouts` withdrawals.
    /// `None` clears it, so payouts go to the recruiter address again.
    ///
    /// The destination belongs to the recruiter address, not to an
    /// engagement, so a recruiter transfer (`accept_recruiter_transfer`)
    /// never carries the old recruiter's destination over to the new one.
    ///
    /// # Events
    /// `("payout_address_set", recruiter)` with `destination`.
    pub fn set_recruiter_payout_address(
        env: Env,
        recruiter: Address,
        destination: Option<Address>,
    ) {
        recruiter.require_auth();
        let key = DataKey2::RecruiterPayoutAddress(recruiter.clone());
        match &destination {
            Some(addr) if *addr != recruiter => {
                env.storage().persistent().set(&key, addr);
                env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);
            }
            _ => env.storage().persistent().remove(&key),
        }
        env.events().publish(
            (Symbol::new(&env, "payout_address_set"), recruiter),
            destination,
        );
    }

    /// Return the recruiter's payout destination, if one is set.
    pub fn get_recruiter_payout_address(env: Env, recruiter: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey2::RecruiterPayoutAddress(recruiter))
    }

    /// Address that actually receives a payout owed to `recruiter`: their
    /// payout destination if set, otherwise `recruiter` itself (issue #548).
    pub(crate) fn recruiter_payout_destination(env: &Env, recruiter: &Address) -> Address {
        env.storage()
            .persistent()
            .get(&DataKey2::RecruiterPayoutAddress(recruiter.clone()))
            .unwrap_or_else(|| recruiter.clone())
    }

    // ----------------------------------------------------------
    // ISSUE #549 — SWAP SLIPPAGE PROTECTION
    // ----------------------------------------------------------

    /// Recruiter sets the maximum slippage, in basis points, they accept when
    /// a payout is swapped into their preferred token. Leaves the token
    /// preference itself untouched. `10_000` accepts any output.
    ///
    /// # Panics
    /// - `"InvalidSlippageBps"` — `max_slippage_bps` exceeds 10 000.
    ///
    /// # Events
    /// `("payout_slippage_set", recruiter)` with `max_slippage_bps`.
    pub fn set_recruiter_payout_slippage(env: Env, recruiter: Address, max_slippage_bps: u32) {
        recruiter.require_auth();
        if max_slippage_bps > FULL_SPLIT_BPS {
            panic!("InvalidSlippageBps");
        }
        let key = DataKey3::RecruiterPayoutSlippage(recruiter.clone());
        env.storage().persistent().set(&key, &max_slippage_bps);
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);
        env.events().publish(
            (Symbol::new(&env, "payout_slippage_set"), recruiter),
            max_slippage_bps,
        );
    }

    /// Return the recruiter's slippage tolerance in basis points, or
    /// `DEFAULT_MAX_SLIPPAGE_BPS` (100 = 1 %) if they never set one.
    pub fn get_recruiter_payout_slippage(env: Env, recruiter: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey3::RecruiterPayoutSlippage(recruiter))
            .unwrap_or(DEFAULT_MAX_SLIPPAGE_BPS)
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
    /// a swap adapter are configured.
    ///
    /// Issue #549: the swap must deliver at least the `min_out` derived from
    /// the adapter's quote and the recipient's slippage tolerance. If the
    /// quote fails or the adapter cannot meet `min_out`, the original token is
    /// paid instead and `payout_swap_fallback` is emitted, so the confirmation
    /// still succeeds. An adapter that reports success below `min_out` breaks
    /// the [`SwapAdapter`] contract and the whole call panics.
    ///
    /// Issue #548: the token preference is looked up for `recipient`, but the
    /// funds (swapped or not) go to `recipient`'s payout destination.
    pub(crate) fn pay_recruiter_share(
        env: &Env,
        token_client: &token::Client,
        recipient: &Address,
        amount: i128,
    ) {
        let preferred: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey2::RecruiterPayoutToken(recipient.clone()));
        let destination = Self::recruiter_payout_destination(env, recipient);
        let adapter: Option<Address> = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::SwapAdapter));

        match (preferred, adapter) {
            (Some(to_token), Some(adapter))
                if amount > 0 && to_token != token_client.address =>
            {
                let min_out = match Self::try_swap_payout(
                    env,
                    token_client,
                    &adapter,
                    &to_token,
                    recipient,
                    &destination,
                    amount,
                ) {
                    Ok(()) => return,
                    Err(min_out) => min_out,
                };
                token_client.transfer(&env.current_contract_address(), &destination, &amount);
                env.events().publish(
                    (Symbol::new(env, "payout_swap_fallback"), recipient.clone()),
                    (token_client.address.clone(), to_token, amount, min_out),
                );
            }
            _ => {
                token_client.transfer(&env.current_contract_address(), &destination, &amount);
            }
        }
    }

    /// Attempt the slippage-bounded swap for one payout. Returns `Err(min_out)`
    /// (`0` when no quote was obtained) when the payout must fall back to the
    /// escrow token; nothing has moved in that case, because the adapter pulls
    /// the funds inside its own `swap` frame and a failed `try_swap` rolls
    /// that pull back.
    ///
    /// The slippage tolerance is `recipient`'s; the swapped tokens are
    /// delivered to `destination`, their payout destination (issue #548).
    fn try_swap_payout(
        env: &Env,
        token_client: &token::Client,
        adapter: &Address,
        to_token: &Address,
        recipient: &Address,
        destination: &Address,
        amount: i128,
    ) -> Result<(), i128> {
        let from_token = token_client.address.clone();
        let adapter_client = SwapAdapterClient::new(env, adapter);

        let quote = match adapter_client.try_quote(&from_token, to_token, &amount) {
            Ok(Ok(q)) if q > 0 => q,
            _ => return Err(0),
        };
        let slippage_bps = Self::get_recruiter_payout_slippage(env.clone(), recipient.clone());
        let keep_bps = (FULL_SPLIT_BPS - slippage_bps) as i128;
        let min_out = quote
            .checked_mul(keep_bps)
            .map(|v| v / FULL_SPLIT_BPS as i128)
            // Only reachable for quotes near i128::MAX; round down instead.
            .unwrap_or((quote / FULL_SPLIT_BPS as i128) * keep_bps);

        let this = env.current_contract_address();
        env.authorize_as_current_contract(vec![
            env,
            InvokerContractAuthEntry::Contract(SubContractInvocation {
                context: ContractContext {
                    contract: from_token.clone(),
                    fn_name: Symbol::new(env, "transfer"),
                    args: (this.clone(), adapter.clone(), amount).into_val(env),
                },
                sub_invocations: vec![env],
            }),
        ]);

        let amount_out = match adapter_client.try_swap(
            &this,
            &from_token,
            to_token,
            &amount,
            &min_out,
            destination,
        ) {
            Ok(Ok(out)) => out,
            _ => return Err(min_out),
        };
        if amount_out < min_out {
            panic!("SwapOutputBelowMinimum");
        }
        env.events().publish(
            (Symbol::new(env, "payout_swapped"), recipient.clone()),
            (from_token, to_token.clone(), amount, amount_out),
        );
        Ok(())
    }

    // ----------------------------------------------------------
    // ISSUE #547 — PULL-BASED PAYOUTS
    // ----------------------------------------------------------

    /// Withdraw everything credited to `recruiter` in `token` by
    /// `pull_payouts` engagements, and return the amount transferred. Works
    /// for primary recruiters and co-recruiters alike, and covers milestone
    /// payouts, streamed-payout claims and bond refunds across every
    /// engagement. Returns `0` without transferring when nothing is owed.
    ///
    /// Credited balances are always in the engagement's escrow token; the
    /// payout token preference (issue #458) is not applied. The withdrawal is
    /// sent to the recruiter's payout destination when one is set (issue #548).
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused.
    ///
    /// # Events
    /// `("payouts_claimed", recruiter)` with `(token, amount)` when `amount > 0`.
    pub fn claim_payouts(env: Env, recruiter: Address, token: Address) -> i128 {
        Self::assert_not_paused(&env);
        recruiter.require_auth();

        let key = DataKey2::RecruiterClaimable(recruiter.clone(), token.clone());
        let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount <= 0 {
            return 0;
        }
        env.storage().persistent().remove(&key);
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &Self::recruiter_payout_destination(&env, &recruiter),
            &amount,
        );
        env.events().publish(
            (Symbol::new(&env, "payouts_claimed"), recruiter),
            (token, amount),
        );
        amount
    }

    /// Amount of `token` credited to `recruiter` and not yet claimed.
    pub fn get_claimable_payout(env: Env, recruiter: Address, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey2::RecruiterClaimable(recruiter, token))
            .unwrap_or(0)
    }

    /// Whether an engagement was created with `pull_payouts`.
    pub fn is_pull_payouts_enabled(env: Env, engagement_id: String) -> bool {
        Self::is_pull_payouts_internal(&env, &engagement_id)
    }

    pub(crate) fn is_pull_payouts_internal(env: &Env, engagement_id: &String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey2::PullPayouts(engagement_id.clone()))
            .unwrap_or(false)
    }

    pub(crate) fn mark_pull_payouts(env: &Env, engagement_id: &String) {
        let key = DataKey2::PullPayouts(engagement_id.clone());
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Add `amount` to `recipient`'s claimable `token` balance. No-op for a
    /// non-positive amount.
    ///
    /// Emits `("payout_credited", recipient)` with `(engagement_id, token, amount)`.
    pub(crate) fn credit_claimable_payout(
        env: &Env,
        engagement_id: &String,
        recipient: &Address,
        token: &Address,
        amount: i128,
    ) {
        if amount <= 0 {
            return;
        }
        let key = DataKey2::RecruiterClaimable(recipient.clone(), token.clone());
        let balance: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(balance + amount));
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
        env.events().publish(
            (Symbol::new(env, "payout_credited"), recipient.clone()),
            (engagement_id.clone(), token.clone(), amount),
        );
    }

    /// Send a recruiter-side amount that is not a milestone payout (e.g. a
    /// bond refund): credited on `pull_payouts` engagements, transferred to
    /// the recipient's payout destination (issue #548) otherwise.
    pub(crate) fn pay_or_credit_recruiter(
        env: &Env,
        engagement: &Engagement,
        token_client: &token::Client,
        recipient: &Address,
        amount: i128,
    ) {
        if Self::is_pull_payouts_internal(env, &engagement.id) {
            Self::credit_claimable_payout(
                env,
                &engagement.id,
                recipient,
                &token_client.address,
                amount,
            );
        } else {
            token_client.transfer(
                &env.current_contract_address(),
                &Self::recruiter_payout_destination(env, recipient),
                &amount,
            );
        }
    }
}
