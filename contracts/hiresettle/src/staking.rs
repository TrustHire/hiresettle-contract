//! Recruiter stake tiers (issue #663): a recruiter posts a stake to raise
//! their active-engagement cap above the admin-wide base cap.

use soroban_sdk::{contractimpl, token, Address, Env, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #663 — RECRUITER STAKE TIERS
    // ----------------------------------------------------------

    /// Admin sets the stake token and the stake tiers.
    ///
    /// A recruiter whose active stake (in `token`) is at least a tier's
    /// `min_stake` gets that tier's `extra_active_slots` on top of the
    /// admin-wide cap (`set_max_active_per_recruiter`); the highest tier met
    /// applies. An empty `tiers` list removes the configuration and disables
    /// new stakes. Existing stakes can always be unstaked and withdrawn.
    ///
    /// Stakes held in a different token than the configured one earn no bonus.
    ///
    /// # Panics
    /// - `"TooManyStakeTiers"` — more than `MAX_RECRUITER_STAKE_TIERS` tiers.
    /// - `"InvalidStakeTier"` — a tier has `min_stake <= 0` or zero
    ///   `extra_active_slots`.
    /// - `"StakeTiersNotAscending"` — `min_stake` is not strictly ascending.
    ///
    /// # Events
    /// `("recruiter_stake_tiers_set",)` with `(token, tiers)`.
    pub fn set_recruiter_stake_tiers(
        env: Env,
        admin: Address,
        token: Address,
        tiers: Vec<RecruiterStakeTier>,
    ) {
        Self::assert_admin(&env, &admin);
        if tiers.len() > MAX_RECRUITER_STAKE_TIERS {
            panic!("TooManyStakeTiers");
        }
        let mut prev: Option<i128> = None;
        for tier in tiers.iter() {
            if tier.min_stake <= 0 || tier.extra_active_slots == 0 {
                panic!("InvalidStakeTier");
            }
            if let Some(p) = prev {
                if tier.min_stake <= p {
                    panic!("StakeTiersNotAscending");
                }
            }
            prev = Some(tier.min_stake);
        }

        let key = DataKey3::RecruiterStakeConfig;
        if tiers.is_empty() {
            env.storage().instance().remove(&key);
        } else {
            env.storage()
                .instance()
                .set(&key, &(token.clone(), tiers.clone()));
        }
        env.events().publish(
            (Symbol::new(&env, "recruiter_stake_tiers_set"),),
            (token, tiers),
        );
    }

    /// Configured stake token and tiers, or `None` when staking is disabled.
    pub fn get_recruiter_stake_tiers(env: Env) -> Option<(Address, Vec<RecruiterStakeTier>)> {
        env.storage().instance().get(&DataKey3::RecruiterStakeConfig)
    }

    /// Admin sets how many ledgers unstaked stake waits before
    /// `withdraw_unbonded_stake` can pay it out. Applies to later
    /// `unstake_recruiter` calls only. `0` makes unstaked stake withdrawable
    /// immediately.
    ///
    /// # Events
    /// `("stake_unbonding_delay_set",)` with `ledgers`.
    pub fn set_stake_unbonding_delay(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey3::StakeUnbondingDelay, &ledgers);
        env.events()
            .publish((Symbol::new(&env, "stake_unbonding_delay_set"),), ledgers);
    }

    /// Unbonding delay in ledgers; defaults to
    /// `DEFAULT_STAKE_UNBONDING_DELAY_LEDGERS` (~7 days).
    pub fn get_stake_unbonding_delay(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey3::StakeUnbondingDelay)
            .unwrap_or(DEFAULT_STAKE_UNBONDING_DELAY_LEDGERS)
    }

    /// Recruiter stakes `amount` of `token` to raise their active-engagement
    /// cap. Adds to any existing active stake.
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused.
    /// - `"amount must be greater than zero"` — `amount <= 0`.
    /// - `"StakingNotConfigured"` — no stake tiers are configured.
    /// - `"StakeTokenMismatch"` — `token` is not the configured stake token,
    ///   or the recruiter still holds (active or unbonding) stake in another
    ///   token.
    ///
    /// # Events
    /// `("recruiter_staked", recruiter)` with `(token, amount, new_stake)`.
    pub fn stake_recruiter(env: Env, recruiter: Address, token: Address, amount: i128) {
        Self::assert_not_paused(&env);
        recruiter.require_auth();
        if amount <= 0 {
            panic!("amount must be greater than zero");
        }
        let (stake_token, _) = Self::get_recruiter_stake_tiers(env.clone())
            .unwrap_or_else(|| panic!("StakingNotConfigured"));
        if token != stake_token {
            panic!("StakeTokenMismatch");
        }

        let key = DataKey3::RecruiterStake(recruiter.clone());
        let mut stake: RecruiterStake =
            env.storage()
                .persistent()
                .get(&key)
                .unwrap_or(RecruiterStake {
                    token: token.clone(),
                    amount: 0,
                    unbonding_amount: 0,
                    unbonding_release_ledger: 0,
                });
        if stake.token != token {
            if stake.amount > 0 || stake.unbonding_amount > 0 {
                panic!("StakeTokenMismatch");
            }
            stake.token = token.clone();
        }

        token::Client::new(&env, &token).transfer(
            &recruiter,
            &env.current_contract_address(),
            &amount,
        );
        stake.amount += amount;
        env.storage().persistent().set(&key, &stake);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "recruiter_staked"), recruiter),
            (token, amount, stake.amount),
        );
    }

    /// Recruiter queues `amount` of their active stake for withdrawal. The
    /// amount stops counting toward the tier bonus immediately and becomes
    /// withdrawable via [`Self::withdraw_unbonded_stake`] after the unbonding
    /// delay. Unstaking again while stake is unbonding adds to it and restarts
    /// the delay.
    ///
    /// Rejected when the lower stake would shrink the recruiter's effective
    /// cap below their current number of active engagements; finish or cancel
    /// engagements first.
    ///
    /// # Panics
    /// - `"amount must be greater than zero"` — `amount <= 0`.
    /// - `"InsufficientStake"` — `amount` exceeds the active stake.
    /// - `"UnstakeBelowActiveCount"` — the new effective cap would be below
    ///   the recruiter's active engagement count.
    ///
    /// # Events
    /// `("recruiter_unstaked", recruiter)` with
    /// `(token, amount, remaining_stake, unbonding_release_ledger)`.
    pub fn unstake_recruiter(env: Env, recruiter: Address, amount: i128) {
        recruiter.require_auth();
        if amount <= 0 {
            panic!("amount must be greater than zero");
        }
        let key = DataKey3::RecruiterStake(recruiter.clone());
        let mut stake: RecruiterStake = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("InsufficientStake"));
        if amount > stake.amount {
            panic!("InsufficientStake");
        }

        let remaining = stake.amount - amount;
        let old_cap = Self::effective_cap_for_stake(&env, &recruiter, &stake.token, stake.amount);
        let new_cap = Self::effective_cap_for_stake(&env, &recruiter, &stake.token, remaining);
        if let Some(cap) = new_cap {
            let shrinks = match old_cap {
                Some(old) => cap < old,
                None => true,
            };
            if shrinks && cap < Self::recruiter_active_count(&env, &recruiter) {
                panic!("UnstakeBelowActiveCount");
            }
        }

        stake.amount = remaining;
        stake.unbonding_amount += amount;
        stake.unbonding_release_ledger = env
            .ledger()
            .sequence()
            .saturating_add(Self::get_stake_unbonding_delay(env.clone()));
        env.storage().persistent().set(&key, &stake);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "recruiter_unstaked"), recruiter),
            (
                stake.token,
                amount,
                stake.amount,
                stake.unbonding_release_ledger,
            ),
        );
    }

    /// Pay out the recruiter's unbonding stake once the unbonding delay has
    /// elapsed. Returns the amount transferred.
    ///
    /// # Panics
    /// - `"NoUnbondingStake"` — nothing is unbonding.
    /// - `"UnbondingNotElapsed"` — the unbonding delay has not elapsed yet.
    ///
    /// # Events
    /// `("recruiter_stake_withdrawn", recruiter)` with `(token, amount)`.
    pub fn withdraw_unbonded_stake(env: Env, recruiter: Address) -> i128 {
        recruiter.require_auth();
        let key = DataKey3::RecruiterStake(recruiter.clone());
        let mut stake: RecruiterStake = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("NoUnbondingStake"));
        if stake.unbonding_amount <= 0 {
            panic!("NoUnbondingStake");
        }
        if env.ledger().sequence() < stake.unbonding_release_ledger {
            panic!("UnbondingNotElapsed");
        }

        let amount = stake.unbonding_amount;
        stake.unbonding_amount = 0;
        stake.unbonding_release_ledger = 0;
        if stake.amount == 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &stake);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
        }

        token::Client::new(&env, &stake.token).transfer(
            &env.current_contract_address(),
            &recruiter,
            &amount,
        );
        env.events().publish(
            (Symbol::new(&env, "recruiter_stake_withdrawn"), recruiter),
            (stake.token, amount),
        );
        amount
    }

    /// The recruiter's stake record, or `None` if they have never staked (or
    /// have fully withdrawn).
    pub fn get_recruiter_stake(env: Env, recruiter: Address) -> Option<RecruiterStake> {
        env.storage()
            .persistent()
            .get(&DataKey3::RecruiterStake(recruiter))
    }

    /// Extra active slots the recruiter's current active stake earns; `0`
    /// with no qualifying stake.
    pub fn get_recruiter_stake_bonus(env: Env, recruiter: Address) -> u32 {
        match Self::get_recruiter_stake(env.clone(), recruiter) {
            Some(stake) => Self::stake_bonus_for(&env, &stake.token, stake.amount),
            None => 0,
        }
    }

    /// The active-engagement cap `create_engagement` enforces for this
    /// recruiter; `0` means unlimited.
    ///
    /// Effective cap = admin base cap (`get_max_active_per_recruiter`) + the
    /// stake tier bonus, further limited by the recruiter's own
    /// `set_recruiter_active_cap` when that is lower. With no admin base cap
    /// the admin side is unlimited and the bonus has no effect.
    pub fn get_recruiter_effective_cap(env: Env, recruiter: Address) -> u32 {
        Self::recruiter_effective_cap_internal(&env, &recruiter).unwrap_or(0)
    }

    /// Effective cap for the recruiter's current stake; `None` ⇒ unlimited.
    pub(crate) fn recruiter_effective_cap_internal(env: &Env, recruiter: &Address) -> Option<u32> {
        let bonus = match Self::get_recruiter_stake(env.clone(), recruiter.clone()) {
            Some(stake) => Self::stake_bonus_for(env, &stake.token, stake.amount),
            None => 0,
        };
        Self::combine_recruiter_caps(env, recruiter, bonus)
    }

    /// Effective cap the recruiter would have with `amount` staked in `token`.
    fn effective_cap_for_stake(
        env: &Env,
        recruiter: &Address,
        token: &Address,
        amount: i128,
    ) -> Option<u32> {
        let bonus = Self::stake_bonus_for(env, token, amount);
        Self::combine_recruiter_caps(env, recruiter, bonus)
    }

    /// `min(personal cap, admin cap + bonus)`, treating `0` / unset as no
    /// limit on either side.
    fn combine_recruiter_caps(env: &Env, recruiter: &Address, bonus: u32) -> Option<u32> {
        let personal_cap: u32 = env
            .storage()
            .persistent()
            .get(&DataKey2::RecruiterActiveCap(recruiter.clone()))
            .unwrap_or(0);
        let admin_cap: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxActivePerRecruiter))
            .unwrap_or(0);
        let admin_cap = if admin_cap == 0 {
            0
        } else {
            admin_cap.saturating_add(bonus)
        };
        match (personal_cap, admin_cap) {
            (0, 0) => None,
            (0, c) | (c, 0) => Some(c),
            (p, a) => Some(p.min(a)),
        }
    }

    /// Extra slots earned by `amount` staked in `token`: the highest tier met,
    /// or `0` when `token` is not the configured stake token.
    fn stake_bonus_for(env: &Env, token: &Address, amount: i128) -> u32 {
        let (stake_token, tiers) = match Self::get_recruiter_stake_tiers(env.clone()) {
            Some(cfg) => cfg,
            None => return 0,
        };
        if *token != stake_token {
            return 0;
        }
        let mut bonus = 0u32;
        for tier in tiers.iter() {
            if amount >= tier.min_stake {
                bonus = tier.extra_active_slots;
            }
        }
        bonus
    }
}
