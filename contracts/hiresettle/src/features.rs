//! Issues #472–#475: pooled company escrow, cosigned config, emergency pause,
//! and platform fee rebate pool.

use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #472 — POOLED COMPANY ESCROW
    // ----------------------------------------------------------

    /// Deposit tokens into the company's per-token pooled escrow balance.
    pub fn deposit_company_balance(env: Env, company: Address, token: Address, amount: i128) {
        Self::assert_not_paused(&env);
        company.require_auth();
        if amount <= 0 {
            panic!("amount must be greater than zero");
        }
        token::Client::new(&env, &token).transfer(
            &company,
            &env.current_contract_address(),
            &amount,
        );
        let key = DataKey2::CompanyBalance(company.clone(), token.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(bal + amount));
        // Issue #578: pooled balance aggregate.
        Self::add_token_pooled(&env, &token, amount);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
        env.events().publish(
            (Symbol::new(&env, "company_balance_deposited"),),
            (company, token, amount),
        );
    }

    /// Withdraw tokens from the company's pooled escrow balance.
    pub fn withdraw_company_balance(env: Env, company: Address, token: Address, amount: i128) {
        Self::assert_not_paused(&env);
        company.require_auth();
        if amount <= 0 {
            panic!("amount must be greater than zero");
        }
        let key = DataKey2::CompanyBalance(company.clone(), token.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > bal {
            panic!("InsufficientCompanyBalance");
        }
        env.storage().persistent().set(&key, &(bal - amount));
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &company,
            &amount,
        );
        // Issue #578: pooled balance aggregate.
        Self::add_token_pooled(&env, &token, -amount);
        env.events().publish(
            (Symbol::new(&env, "company_balance_withdrawn"),),
            (company, token, amount),
        );
    }

    /// Return the company's pooled escrow balance for `token`.
    pub fn get_company_balance(env: Env, company: Address, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey2::CompanyBalance(company, token))
            .unwrap_or(0)
    }

    /// Debit `amount` from the company pool. Panics with
    /// `InsufficientCompanyBalance` without mutating state if the pool cannot
    /// cover it.
    pub(crate) fn debit_company_pool(
        env: &Env,
        company: &Address,
        token: &Address,
        amount: i128,
    ) {
        let key = DataKey2::CompanyBalance(company.clone(), token.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > bal {
            panic!("InsufficientCompanyBalance");
        }
        env.storage().persistent().set(&key, &(bal - amount));
        // Issue #578: pooled balance aggregate.
        Self::add_token_pooled(env, token, -amount);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Credit `amount` back into the company pool (used for pool-funded refunds).
    pub(crate) fn credit_company_pool(
        env: &Env,
        company: &Address,
        token: &Address,
        amount: i128,
    ) {
        if amount <= 0 {
            return;
        }
        let key = DataKey2::CompanyBalance(company.clone(), token.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        env.storage().persistent().set(&key, &(bal + amount));
        // Issue #578: pooled balance aggregate.
        Self::add_token_pooled(env, token, amount);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    pub(crate) fn mark_pool_funded(env: &Env, engagement_id: &String) {
        let key = DataKey2::PoolFunded(engagement_id.clone());
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    pub(crate) fn is_pool_funded(env: &Env, engagement_id: &String) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey2::PoolFunded(engagement_id.clone()))
            .unwrap_or(false)
    }

    /// Refund unreleased escrow: back to the pool for pool-funded engagements,
    /// otherwise an external transfer to the company.
    pub(crate) fn refund_company_escrow(
        env: &Env,
        engagement: &Engagement,
        engagement_id: &String,
        refund: i128,
    ) {
        if refund <= 0 {
            return;
        }
        if Self::is_pool_funded(env, engagement_id) {
            Self::credit_company_pool(env, &engagement.company, &engagement.token, refund);
        } else {
            token::Client::new(env, &engagement.token).transfer(
                &env.current_contract_address(),
                &engagement.company,
                &refund,
            );
        }
    }

    // ----------------------------------------------------------
    // ISSUE #473 — 2-OF-2 CO-SIGNED ADMIN CONFIG
    // ----------------------------------------------------------

    /// Set or clear the config cosigner. `None` disables cosigner gating.
    pub fn set_config_cosigner(env: Env, admin: Address, cosigner: Option<Address>) {
        Self::assert_admin(&env, &admin);
        if let Some(c) = cosigner {
            env.storage()
                .persistent()
                .set(&DataKey2::ConfigCosigner, &c);
            env.events()
                .publish((Symbol::new(&env, "config_cosigner_set"),), c);
        } else {
            env.storage().persistent().remove(&DataKey2::ConfigCosigner);
            env.events()
                .publish((Symbol::new(&env, "config_cosigner_cleared"),), ());
        }
    }

    /// Return the configured config cosigner, if any.
    pub fn get_config_cosigner(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey2::ConfigCosigner)
    }

    /// Admin selects which setter function ids require cosigner acceptance.
    pub fn set_sensitive_functions(env: Env, admin: Address, fn_ids: Vec<u32>) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey2::SensitiveFunctions, &fn_ids);
        env.events().publish(
            (Symbol::new(&env, "sensitive_functions_set"),),
            fn_ids.len(),
        );
    }

    /// Return the list of sensitive setter function ids.
    pub fn get_sensitive_functions(env: Env) -> Vec<u32> {
        env.storage()
            .persistent()
            .get(&DataKey2::SensitiveFunctions)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Admin sets the TTL, in ledgers, for pending config change proposals.
    pub fn set_config_change_ttl(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::ConfigChangeTTL), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "config_change_ttl_set"),), ledgers);
    }

    /// Return the pending config change TTL in ledgers
    /// (default `DEFAULT_CONFIG_CHANGE_TTL`, ~1 day).
    pub fn get_config_change_ttl(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ConfigChangeTTL))
            .unwrap_or(DEFAULT_CONFIG_CHANGE_TTL)
    }

    /// Admin opens a bare config change proposal that applies nothing on
    /// acceptance, and returns its `change_id`. Sensitive setters create
    /// their own proposals via `defer_if_sensitive`.
    pub fn propose_config_change(env: Env, admin: Address) -> u64 {
        Self::assert_admin(&env, &admin);
        Self::store_pending_config_change(
            &env,
            admin,
            FN_CONFIG_CHANGE_NOOP,
            0,
            0,
            false,
            None,
            None,
        )
    }

    /// Accept a pending config change, applying it to live state. When a
    /// config cosigner is configured only the cosigner may accept; otherwise
    /// the proposer may.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller may not accept this proposal.
    /// - `"no pending config change"` — no proposal with this id.
    /// - `"ConfigChangeExpired"` — the TTL elapsed.
    /// - `"ConfigChangeCancelled"` — the proposer cancelled it.
    ///
    /// Expired and cancelled proposals stay stored (the panic rolls back any
    /// write) but can never be accepted.
    pub fn accept_config_change(env: Env, signer: Address, change_id: u64) {
        signer.require_auth();
        let key = DataKey2::PendingConfigChange(change_id);
        let pending: PendingConfigChange = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending config change"));
        let cosigner: Option<Address> = env.storage().persistent().get(&DataKey2::ConfigCosigner);
        let authorized = match cosigner {
            Some(c) => signer == c,
            None => signer == pending.proposer,
        };
        if !authorized {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let now = env.ledger().sequence();
        if now > pending.expires_at_ledger {
            env.events().publish(
                (Symbol::new(&env, "config_change_expired"),),
                (change_id, now),
            );
            panic!("ConfigChangeExpired");
        }
        if pending.cancelled {
            env.events().publish(
                (Symbol::new(&env, "config_change_cancelled"),),
                (change_id, signer),
            );
            panic!("ConfigChangeCancelled");
        }

        Self::apply_pending_config_change(&env, &pending);
        env.storage().persistent().remove(&key);
        env.events().publish(
            (Symbol::new(&env, "config_change_accepted"),),
            (change_id, pending.fn_id),
        );
    }

    /// Proposer cancels a pending config change. The proposal stays readable
    /// with `cancelled = true` and can no longer be accepted.
    pub fn cancel_config_change(env: Env, proposer: Address, change_id: u64) {
        proposer.require_auth();
        let key = DataKey2::PendingConfigChange(change_id);
        let mut pending: PendingConfigChange = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending config change"));
        if proposer != pending.proposer {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        pending.cancelled = true;
        env.storage().persistent().set(&key, &pending);
        env.events().publish(
            (Symbol::new(&env, "config_change_cancelled"),),
            (change_id, proposer),
        );
    }

    /// Return a pending config change by id, if it exists.
    pub fn get_pending_config_change(env: Env, change_id: u64) -> Option<PendingConfigChange> {
        env.storage()
            .persistent()
            .get(&DataKey2::PendingConfigChange(change_id))
    }

    /// If a cosigner is configured and `fn_id` is marked sensitive, store a
    /// pending change and return `true` (caller must not apply). Otherwise
    /// return `false` (caller applies immediately).
    pub(crate) fn defer_if_sensitive(
        env: &Env,
        fn_id: u32,
        u32_val: u32,
        i128_val: i128,
        bool_val: bool,
        address_val: Option<Address>,
        string_val: Option<String>,
    ) -> bool {
        let cosigner: Option<Address> = env.storage().persistent().get(&DataKey2::ConfigCosigner);
        if cosigner.is_none() {
            return false;
        }
        let sensitive: Vec<u32> = env
            .storage()
            .persistent()
            .get(&DataKey2::SensitiveFunctions)
            .unwrap_or_else(|| Vec::new(env));
        let is_sensitive = (0..sensitive.len()).any(|i| sensitive.get(i).unwrap() == fn_id);
        if !is_sensitive {
            return false;
        }
        Self::store_pending_config_change(
            env,
            Self::get_admin_internal(env),
            fn_id,
            u32_val,
            i128_val,
            bool_val,
            address_val,
            string_val,
        );
        true
    }

    /// Store a new pending config change expiring after the configured TTL
    /// and return its id.
    fn store_pending_config_change(
        env: &Env,
        proposer: Address,
        fn_id: u32,
        u32_val: u32,
        i128_val: i128,
        bool_val: bool,
        address_val: Option<Address>,
        string_val: Option<String>,
    ) -> u64 {
        let change_id: u64 = env
            .storage()
            .persistent()
            .get(&DataKey2::NextConfigChangeId)
            .unwrap_or(1u64);
        let now = env.ledger().sequence();
        let pending = PendingConfigChange {
            change_id,
            fn_id,
            u32_val,
            i128_val,
            bool_val,
            address_val,
            string_val,
            proposer,
            proposed_at_ledger: now,
            expires_at_ledger: now.saturating_add(Self::get_config_change_ttl(env.clone())),
            cancelled: false,
        };
        env.storage()
            .persistent()
            .set(&DataKey2::PendingConfigChange(change_id), &pending);
        env.storage()
            .persistent()
            .set(&DataKey2::NextConfigChangeId, &(change_id + 1));
        env.events().publish(
            (Symbol::new(env, "config_change_proposed"),),
            (change_id, fn_id, pending.expires_at_ledger),
        );
        change_id
    }

    pub(crate) fn apply_pending_config_change(env: &Env, pending: &PendingConfigChange) {
        match pending.fn_id {
            FN_CONFIG_CHANGE_NOOP => {}
            FN_SET_PLATFORM_FEE => {
                let treasury = pending
                    .address_val
                    .clone()
                    .unwrap_or_else(|| panic!("missing treasury"));
                // Issue #541: a co-signed change still waits out the fee-change delay.
                Self::set_or_schedule_platform_fee(env, pending.u32_val, treasury);
            }
            FN_SET_TOKEN_ALLOWLIST_ENABLED => {
                env.storage()
                    .persistent()
                    .set(&DataKey::AllowlistEnabled, &pending.bool_val);
                env.events().publish(
                    (Symbol::new(env, "allowlist_enabled_set"),),
                    pending.bool_val,
                );
            }
            FN_SET_REFERRAL_DISCOUNT_BPS => {
                env.storage().persistent().set(
                    &DataKey::Config(ConfigKey::ReferralDiscountBps),
                    &pending.u32_val,
                );
                env.events().publish(
                    (Symbol::new(env, "referral_discount_set"),),
                    pending.u32_val,
                );
            }
            FN_SET_ARBITER_FEE => {
                env.storage()
                    .instance()
                    .set(&DataKey::Config(ConfigKey::ArbiterFee), &pending.u32_val);
                env.events()
                    .publish((Symbol::new(env, "arbiter_fee_set"),), pending.u32_val);
            }
            FN_SET_MIN_AMOUNT => {
                env.storage().persistent().set(
                    &DataKey::Config(ConfigKey::MinEngagementAmount),
                    &pending.i128_val,
                );
                env.events()
                    .publish((Symbol::new(env, "min_amount_set"),), pending.i128_val);
            }
            FN_SET_FEE_REBATE_BPS => {
                env.storage().persistent().set(
                    &DataKey::Config(ConfigKey::FeeRebateBps),
                    &pending.u32_val,
                );
                env.events().publish(
                    (Symbol::new(env, "fee_rebate_bps_set"),),
                    pending.u32_val,
                );
            }
            _ => panic!("unknown config change fn_id"),
        }
    }

    // ----------------------------------------------------------
    // ISSUE #474 — EMERGENCY MULTI-SIG PAUSE
    // ----------------------------------------------------------

    /// Admin configures the M-of-N emergency signer set that can trigger pause.
    pub fn set_emergency_signers(
        env: Env,
        admin: Address,
        signers: Vec<Address>,
        threshold: u32,
    ) {
        Self::assert_admin(&env, &admin);
        if threshold == 0 || threshold > signers.len() {
            panic!("InvalidEmergencyThreshold");
        }
        // Reject duplicates.
        for i in 0..signers.len() {
            let a = signers.get(i).unwrap();
            for j in (i + 1)..signers.len() {
                if signers.get(j).unwrap() == a {
                    panic!("DuplicateEmergencySigner");
                }
            }
        }
        let window: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::EmergencyVoteWindow))
            .unwrap_or(DEFAULT_EMERGENCY_VOTE_WINDOW_LEDGERS);
        let cfg = EmergencySignerConfig {
            signers: signers.clone(),
            threshold,
            vote_window_ledgers: window,
        };
        env.storage()
            .persistent()
            .set(&DataKey::EmergencySigners, &cfg);
        // Clear any in-flight tallies.
        env.storage()
            .persistent()
            .remove(&DataKey2::EmergencyVotes(String::from_str(&env, "")));
        env.events().publish(
            (Symbol::new(&env, "emergency_signers_set"),),
            (signers.len(), threshold),
        );
    }

    /// Return `(signers, threshold)`. Empty vec / 0 when unset.
    pub fn get_emergency_signers(env: Env) -> (Vec<Address>, u32) {
        let cfg: Option<EmergencySignerConfig> =
            env.storage().persistent().get(&DataKey::EmergencySigners);
        match cfg {
            Some(c) => (c.signers, c.threshold),
            None => (Vec::new(&env), 0),
        }
    }

    /// Admin sets the emergency vote accumulation window in ledgers.
    pub fn set_emergency_vote_window(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        if ledgers == 0 {
            panic!("InvalidEmergencyVoteWindow");
        }
        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::EmergencyVoteWindow), &ledgers);
        if let Some(mut cfg) = env
            .storage()
            .persistent()
            .get::<DataKey, EmergencySignerConfig>(&DataKey::EmergencySigners)
        {
            cfg.vote_window_ledgers = ledgers;
            env.storage()
                .persistent()
                .set(&DataKey::EmergencySigners, &cfg);
        }
        env.events().publish(
            (Symbol::new(&env, "emergency_vote_window_set"),),
            ledgers,
        );
    }

    /// Cast an emergency pause vote. Once `threshold` distinct signers have
    /// voted within the window, the contract (or a single engagement) pauses
    /// automatically. `unpause` remains admin-only.
    pub fn cast_emergency_pause_vote(
        env: Env,
        signer: Address,
        engagement_id: Option<String>,
    ) {
        signer.require_auth();
        let cfg: EmergencySignerConfig = env
            .storage()
            .persistent()
            .get(&DataKey::EmergencySigners)
            .unwrap_or_else(|| panic!("no emergency signers"));

        let is_signer = (0..cfg.signers.len()).any(|i| cfg.signers.get(i).unwrap() == signer);
        if !is_signer {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let tally_key_id = match &engagement_id {
            Some(id) => id.clone(),
            None => String::from_str(&env, ""),
        };
        let key = DataKey2::EmergencyVotes(tally_key_id.clone());
        let now = env.ledger().sequence();

        let mut tally: EmergencyVoteTally = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(EmergencyVoteTally {
                voters: Vec::new(&env),
                started_at_ledger: now,
            });

        // Reset expired window.
        if now > tally.started_at_ledger.saturating_add(cfg.vote_window_ledgers)
            && !tally.voters.is_empty()
        {
            tally = EmergencyVoteTally {
                voters: Vec::new(&env),
                started_at_ledger: now,
            };
        }
        if tally.voters.is_empty() {
            tally.started_at_ledger = now;
        }

        // Duplicate-vote rejection (mirrors arbiter voting).
        for i in 0..tally.voters.len() {
            if tally.voters.get(i).unwrap() == signer {
                panic!("already voted");
            }
        }
        tally.voters.push_back(signer.clone());

        if tally.voters.len() >= cfg.threshold {
            // Trigger pause, then reset tally.
            match &engagement_id {
                None => {
                    env.storage().persistent().set(&DataKey::Paused, &true);
                    env.events()
                        .publish((Symbol::new(&env, "paused"),), signer.clone());
                }
                Some(id) => {
                    // Ensure engagement exists.
                    let _ = Self::get_engagement_internal(&env, id);
                    let reason = String::from_str(&env, "emergency");
                    env.storage()
                        .persistent()
                        .set(&DataKey::EngagementPaused(id.clone()), &true);
                    env.storage().persistent().extend_ttl(
                        &DataKey::EngagementPaused(id.clone()),
                        100_000,
                        6_300_000,
                    );
                    env.storage().persistent().set(
                        &DataKey::EngagementPauseReason(id.clone()),
                        &reason,
                    );
                    env.events().publish(
                        (Symbol::new(&env, "engagement_paused"), id.clone()),
                        (signer.clone(), reason),
                    );
                }
            }
            env.storage().persistent().remove(&key);
            env.events().publish(
                (Symbol::new(&env, "emergency_pause_triggered"),),
                (tally_key_id, cfg.threshold),
            );
        } else {
            env.storage().persistent().set(&key, &tally);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
            env.events().publish(
                (Symbol::new(&env, "emergency_pause_vote"),),
                (signer, tally.voters.len(), cfg.threshold),
            );
        }
    }

    // ----------------------------------------------------------
    // ISSUE #475 — PLATFORM FEE REBATE POOL
    // ----------------------------------------------------------

    /// Admin sets the fee rebate rate in basis points (default 0).
    pub fn set_fee_rebate_bps(env: Env, admin: Address, bps: u32) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);
        if bps > MAX_PLATFORM_FEE_BPS {
            panic!("FeeTooHigh");
        }
        if Self::defer_if_sensitive(
            &env,
            FN_SET_FEE_REBATE_BPS,
            bps,
            0,
            false,
            None,
            None,
        ) {
            return;
        }
        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::FeeRebateBps), &bps);
        env.events()
            .publish((Symbol::new(&env, "fee_rebate_bps_set"),), bps);
    }

    /// Return the current fee rebate rate in basis points (default 0).
    pub fn get_fee_rebate_bps(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::FeeRebateBps))
            .unwrap_or(0u32)
    }

    /// Return the company's redeemable rebate balance for `token`.
    pub fn get_company_rebate_balance(env: Env, company: Address, token: Address) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey2::CompanyRebate(company, token))
            .unwrap_or(0)
    }

    /// Company withdraws unused rebate balance directly.
    pub fn redeem_company_rebate(env: Env, company: Address, token: Address, amount: i128) {
        Self::assert_not_paused(&env);
        company.require_auth();
        if amount <= 0 {
            panic!("amount must be greater than zero");
        }
        let key = DataKey2::CompanyRebate(company.clone(), token.clone());
        let bal: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount > bal {
            panic!("InsufficientRebateBalance");
        }
        env.storage().persistent().set(&key, &(bal - amount));
        token::Client::new(&env, &token).transfer(
            &env.current_contract_address(),
            &company,
            &amount,
        );
        env.events().publish(
            (Symbol::new(&env, "company_rebate_redeemed"),),
            (company, token, amount),
        );
    }

    /// Collect a platform fee with rebate offset + rebate credit (issue #475).
    /// When rebate_bps is 0 and no prior rebate balance exists, behaviour is
    /// identical to a direct treasury transfer.
    ///
    /// When weighted fee treasuries are configured (issue #540), the amount
    /// owed to the treasury is split across them instead; see
    /// [`Self::route_fee_to_treasuries`].
    ///
    /// When a referrer fee share is configured (issue #543), that share of the
    /// amount owed to the treasury goes to the engagement's referrer first;
    /// see [`Self::pay_referrer_fee_share`].
    pub(crate) fn collect_platform_fee(
        env: &Env,
        engagement: &Engagement,
        engagement_id: &String,
        milestone_index: u32,
        fee_amount: i128,
    ) {
        if fee_amount <= 0 {
            return;
        }
        let rebate_key =
            DataKey2::CompanyRebate(engagement.company.clone(), engagement.token.clone());
        let mut rebate: i128 = env.storage().persistent().get(&rebate_key).unwrap_or(0);

        let offset = if rebate < fee_amount { rebate } else { fee_amount };
        rebate -= offset;
        let remaining = fee_amount - offset;

        let rebate_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::FeeRebateBps))
            .unwrap_or(0u32);
        let credit = (remaining * rebate_bps as i128) / 10_000;
        let to_treasury = remaining - credit;
        rebate += credit;

        env.storage().persistent().set(&rebate_key, &rebate);
        env.storage()
            .persistent()
            .extend_ttl(&rebate_key, 100_000, 6_300_000);

        // Issue #543: the referrer's cut comes out of what would otherwise
        // reach the treasury, so treasury + referrer always sum to it.
        let to_treasury = to_treasury
            - Self::pay_referrer_fee_share(env, engagement, engagement_id, to_treasury);

        let treasuries: Vec<(Address, u32)> = env
            .storage()
            .persistent()
            .get(&DataKey2::FeeTreasuries)
            .unwrap_or_else(|| Vec::new(env));
        if !treasuries.is_empty() {
            Self::route_fee_to_treasuries(
                env,
                engagement,
                engagement_id,
                milestone_index,
                to_treasury,
                &treasuries,
            );
            return;
        }

        let platform_fee = Self::get_platform_fee_internal(env);
        if to_treasury > 0 {
            token::Client::new(env, &engagement.token).transfer(
                &env.current_contract_address(),
                &platform_fee.treasury,
                &to_treasury,
            );
        }

        env.events().publish(
            (
                Symbol::new(env, "platform_fee_collected"),
                engagement_id.clone(),
            ),
            (milestone_index, fee_amount, platform_fee.treasury),
        );
    }

    /// Pay the admin-configured referrer share (issue #543) of `fee` to the
    /// engagement's referrer and return the amount paid. Pays nothing — and
    /// returns `0` — when the share is `0`, the engagement has no referrer,
    /// or the referrer is no longer on the recognised referral list.
    ///
    /// Emits `("referrer_paid", engagement_id)` with `(referrer, amount)`
    /// when `amount > 0`.
    fn pay_referrer_fee_share(
        env: &Env,
        engagement: &Engagement,
        engagement_id: &String,
        fee: i128,
    ) -> i128 {
        if fee <= 0 {
            return 0;
        }
        let referrer = match &engagement.referrer {
            Some(referrer) => referrer,
            None => return 0,
        };
        let share_bps: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::ReferrerFeeShareBps))
            .unwrap_or(0u32);
        if share_bps == 0 || !Self::is_recognised_referrer(env, referrer) {
            return 0;
        }
        let amount = (fee * share_bps as i128) / (FULL_SPLIT_BPS as i128);
        if amount <= 0 {
            return 0;
        }
        token::Client::new(env, &engagement.token).transfer(
            &env.current_contract_address(),
            referrer,
            &amount,
        );
        env.events().publish(
            (Symbol::new(env, "referrer_paid"), engagement_id.clone()),
            (referrer.clone(), amount),
        );
        amount
    }

    /// Split `amount` across weighted fee treasuries (issue #540). Every
    /// treasury but the first gets `amount * weight / 10_000` rounded down;
    /// the first gets the remainder, so the rounding dust lands there and the
    /// shares always sum to `amount` exactly.
    ///
    /// Emits one `("platform_fee_collected", engagement_id)` event per
    /// treasury, in configured order, with `(milestone_index, share,
    /// treasury)`, where `share` is what that treasury received.
    fn route_fee_to_treasuries(
        env: &Env,
        engagement: &Engagement,
        engagement_id: &String,
        milestone_index: u32,
        amount: i128,
        treasuries: &Vec<(Address, u32)>,
    ) {
        let full = FULL_SPLIT_BPS as i128;
        let mut first_share = amount;
        for i in 1..treasuries.len() {
            let (_, weight) = treasuries.get(i).unwrap();
            first_share -= (amount * weight as i128) / full;
        }

        let token_client = token::Client::new(env, &engagement.token);
        for i in 0..treasuries.len() {
            let (treasury, weight) = treasuries.get(i).unwrap();
            let share = if i == 0 {
                first_share
            } else {
                (amount * weight as i128) / full
            };
            if share > 0 {
                token_client.transfer(&env.current_contract_address(), &treasury, &share);
            }
            env.events().publish(
                (
                    Symbol::new(env, "platform_fee_collected"),
                    engagement_id.clone(),
                ),
                (milestone_index, share, treasury),
            );
        }
    }

    // ----------------------------------------------------------
    // EMERGENCY SIGNER SELF-ROTATION BY M-OF-N VOTE
    // ----------------------------------------------------------

    /// Existing emergency signer nominates a replacement address via M-of-N vote.
    /// When `threshold` signers vote for the same (old, new) pair within the window,
    /// the old signer is replaced with the new one in the signer set.
    ///
    /// # Authorization
    /// Requires auth from `current_signer`, who must already be in the emergency signers set.
    ///
    /// # Voting Window
    /// Votes are cleared if the window elapses before threshold is reached. A fresh
    /// vote batch must then start from zero.
    ///
    /// # Events
    /// - `("emergency_signer_rotation_vote",)` with `(current_signer, new_signer, vote_count, threshold)`
    /// - `("emergency_signer_rotated",)` with `(current_signer, new_signer)` when threshold is reached
    ///
    /// # Panics
    /// - `"no emergency signers"` — no signer set configured.
    /// - `"unauthorized"` — caller is not in the current signer set.
    /// - `"DuplicateEmergencySigner"` — `new_signer` is already in the set.
    /// - `"already voted"` — `current_signer` has already voted for this (old, new) pair.
    pub fn rotate_emergency_signer(
        env: Env,
        current_signer: Address,
        new_signer: Address,
    ) {
        current_signer.require_auth();
        
        let mut cfg: EmergencySignerConfig = env
            .storage()
            .persistent()
            .get(&DataKey::EmergencySigners)
            .unwrap_or_else(|| panic!("no emergency signers"));

        // Verify current_signer is in the set
        let position = (0..cfg.signers.len())
            .find(|&i| cfg.signers.get(i).unwrap() == current_signer);
        if position.is_none() {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        let pos = position.unwrap();

        // Reject if new_signer is already in the set
        for i in 0..cfg.signers.len() {
            if cfg.signers.get(i).unwrap() == new_signer {
                panic!("DuplicateEmergencySigner");
            }
        }

        let key = DataKey3::EmergencyRotationVotes(current_signer.clone(), new_signer.clone());
        let now = env.ledger().sequence();

        let mut tally: EmergencyVoteTally = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or(EmergencyVoteTally {
                voters: Vec::new(&env),
                started_at_ledger: now,
            });

        // Reset if window expired
        if now > tally.started_at_ledger.saturating_add(cfg.vote_window_ledgers)
            && !tally.voters.is_empty()
        {
            tally = EmergencyVoteTally {
                voters: Vec::new(&env),
                started_at_ledger: now,
            };
        }
        if tally.voters.is_empty() {
            tally.started_at_ledger = now;
        }

        // Reject duplicate votes
        for i in 0..tally.voters.len() {
            if tally.voters.get(i).unwrap() == current_signer {
                panic!("already voted");
            }
        }
        tally.voters.push_back(current_signer.clone());

        if tally.voters.len() >= cfg.threshold {
            // Replace current_signer with new_signer
            cfg.signers.set(pos, new_signer.clone());
            env.storage()
                .persistent()
                .set(&DataKey::EmergencySigners, &cfg);
            
            // Clear the vote tally
            env.storage().persistent().remove(&key);
            
            env.events().publish(
                (Symbol::new(&env, "emergency_signer_rotated"),),
                (current_signer, new_signer),
            );
        } else {
            env.storage().persistent().set(&key, &tally);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
            
            env.events().publish(
                (Symbol::new(&env, "emergency_signer_rotation_vote"),),
                (current_signer, new_signer, tally.voters.len(), cfg.threshold),
            );
        }
    }
}
