use soroban_sdk::{contractimpl, token, Address, BytesN, Env, Map, String, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    /// Initializes the HireSettle contract.
    ///
    /// # Caller
    /// Called by the contract deployer or initial administrator (`admin`). Requires authentication from `admin`.
    ///
    /// # Initialized State
    /// Sets up default contract storage values:
    /// - `DataKey::Admin`: Set to `admin`
    /// - `DataKey::Paused`: Set to `false`
    /// - `DataKey::PlatformFee`: Set to 0 bps with treasury `admin`
    /// - `DataKey::Version`: Set to `DEFAULT_VERSION` ("0.2.0")
    /// - `DataKey::Config(ConfigKey::MinEngagementAmount)`: Set to `DEFAULT_MIN_ENGAGEMENT_AMOUNT` (100,000 stroops)
    ///
    /// # One-Time-Only / Calling Twice
    /// Note: No already-initialized guard is currently present. If invoked again, it will overwrite
    /// all initialized storage fields provided `admin.require_auth()` succeeds.
    ///
    /// # Panics
    /// Panics if authentication from `admin` (`admin.require_auth()`) fails.
    pub fn init(env: Env, admin: Address) {
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().persistent().set(&DataKey::Paused, &false);
        env.storage().persistent().set(
            &DataKey::PlatformFee,
            &PlatformFee {
                bps: 0,
                treasury: admin,
            },
        );

        // Issue #16: Initialize contract version
        env.storage()
            .persistent()
            .set(&DataKey::Version, &String::from_str(&env, DEFAULT_VERSION));

        // Issue #17: Initialize minimum engagement amount
        env.storage().persistent().set(
            &DataKey::Config(ConfigKey::MinEngagementAmount),
            &DEFAULT_MIN_ENGAGEMENT_AMOUNT,
        );
    }

    // ----------------------------------------------------------
    // ADMIN CONFIGURATION
    // ----------------------------------------------------------

    /// Set the platform fee in basis points and the treasury that receives it.
    /// `bps` is capped at 500 (5%).
    pub fn set_platform_fee(env: Env, admin: Address, bps: u32, treasury: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        if bps > MAX_PLATFORM_FEE_BPS {
            panic!("FeeTooHigh");
        }

        env.storage().persistent().set(
            &DataKey::PlatformFee,
            &PlatformFee {
                bps,
                treasury: treasury.clone(),
            },
        );

        env.events()
            .publish((Symbol::new(&env, "platform_fee_set"),), (bps, treasury));
    }

    /// Return the current platform fee configuration.
    pub fn get_platform_fee(env: Env) -> (u32, Address) {
        let fee = Self::get_platform_fee_internal(&env);
        (fee.bps, fee.treasury)
    }


    /// Admin waives the platform fee for a single engagement (issue #335),
    /// zeroing it for every future milestone payout on that engagement
    /// regardless of the contract-wide fee, fee tiers, or referral discount.
    /// Idempotent — waiving an already-waived engagement is a no-op.
    ///
    /// Does not retroactively refund fees already collected before the
    /// waiver was granted.
    ///
    /// # Panics
    /// - `"NoAdmin"` — the admin role has been permanently renounced.
    /// - `"unauthorized"` — caller is not the current admin.
    /// - `"engagement not found"` — no engagement with this ID.
    ///
    /// # Events
    /// Emits `("platform_fee_waived", engagement_id)` with `(admin,)`.
    pub fn waive_platform_fee(env: Env, admin: Address, engagement_id: String) {
        Self::assert_admin(&env, &admin);
        // Confirms the engagement exists before recording the waiver.
        Self::get_engagement_internal(&env, &engagement_id);

        let key = DataKey::FeeWaived(engagement_id.clone());
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "platform_fee_waived"), engagement_id),
            (admin,),
        );
    }

    /// Return whether the platform fee has been waived for `engagement_id`
    /// (issue #335).
    pub fn is_fee_waived(env: Env, engagement_id: String) -> bool {
        Self::is_fee_waived_internal(&env, &engagement_id)
    }

    /// Admin adds a referrer address to the recognised referral list (issue #251).
    pub fn add_referrer(env: Env, admin: Address, referrer: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        let mut list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Referrers)
            .unwrap_or_else(|| Vec::new(&env));

        // Prevent duplicates.
        for i in 0..list.len() {
            if list.get(i).unwrap() == referrer {
                panic!("referrer already exists");
            }
        }
        list.push_back(referrer.clone());
        env.storage().persistent().set(&DataKey::Referrers, &list);
        env.events()
            .publish((Symbol::new(&env, "referrer_added"),), referrer);
    }

    /// Admin removes a referrer address from the recognised referral list.
    pub fn remove_referrer(env: Env, admin: Address, referrer: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        let list: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::Referrers)
            .unwrap_or_else(|| Vec::new(&env));

        let mut new_list: Vec<Address> = Vec::new(&env);
        let mut found = false;
        for i in 0..list.len() {
            let addr = list.get(i).unwrap();
            if addr == referrer {
                found = true;
            } else {
                new_list.push_back(addr);
            }
        }
        if !found {
            panic!("referrer not found");
        }
        env.storage()
            .persistent()
            .set(&DataKey::Referrers, &new_list);
        env.events()
            .publish((Symbol::new(&env, "referrer_removed"),), referrer);
    }

    /// Return the list of recognised referrer addresses.
    pub fn get_referrers(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::Referrers)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Admin sets the referral discount in basis points (issue #251).
    /// When a recognised referrer is attached to an engagement, the platform
    /// fee is reduced by this amount (but never below 0).
    /// Maximum 500 bps (same as max platform fee).
    pub fn set_referral_discount_bps(env: Env, admin: Address, bps: u32) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);
        if bps > MAX_PLATFORM_FEE_BPS {
            panic!("discount too high");
        }
        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::ReferralDiscountBps), &bps);
        env.events()
            .publish((Symbol::new(&env, "referral_discount_set"),), bps);
    }

    /// Return the current referral discount in basis points (default 0).
    pub fn get_referral_discount_bps(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::ReferralDiscountBps))
            .unwrap_or(0u32)
    }

    /// Return the discount, in basis points, currently applied for a given
    /// referrer (issue #312): the admin-configured discount if `referrer` is
    /// on the recognised referral list, or 0 if it is not (or the list is
    /// empty). Companion query to `get_referral_discount_bps`, which returns
    /// the configured rate without regard to any specific referrer.
    pub fn get_referrer_discount_bps(env: Env, referrer: Address) -> u32 {
        if !Self::is_recognised_referrer(&env, &referrer) {
            return 0;
        }
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::ReferralDiscountBps))
            .unwrap_or(0u32)
    }

    /// Admin sets fee tiers that scale the platform fee down for larger
    /// engagements (issue #250). Each tier specifies a `threshold`
    /// (minimum `total_amount`) and the `bps` rate that applies. Tiers
    /// must be sorted by ascending threshold, each `bps` must be ≤ the
    /// base platform fee, and at most 10 tiers are allowed.
    ///
    /// At fee-calculation time the contract walks the tiers from highest
    /// threshold to lowest and uses the first matching tier's `bps`.
    /// If no tier matches, the base `platform_fee.bps` applies.
    ///
    /// Pass an empty vector to clear all tiers (flat fee for every size).
    pub fn set_fee_tiers(env: Env, admin: Address, tiers: Vec<FeeTier>) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        if tiers.len() > 10 {
            panic!("too many fee tiers");
        }

        let base_bps = Self::get_platform_fee_internal(&env).bps;
        for i in 0..tiers.len() {
            let t = tiers.get(i).unwrap();
            if t.bps > base_bps {
                panic!("tier bps exceeds base platform fee");
            }
            if t.threshold <= 0 {
                panic!("tier threshold must be positive");
            }
            if i > 0 {
                let prev = tiers.get(i - 1).unwrap();
                if t.threshold <= prev.threshold {
                    panic!("tiers must be sorted by ascending threshold");
                }
            }
        }

        env.storage().persistent().set(&DataKey::FeeTiers, &tiers);
        env.events()
            .publish((Symbol::new(&env, "fee_tiers_set"),), tiers.len());
    }

    /// Return the current fee tiers. Empty vector means no tiering (flat fee).
    pub fn get_fee_tiers(env: Env) -> Vec<FeeTier> {
        env.storage()
            .persistent()
            .get(&DataKey::FeeTiers)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Admin removes a single fee tier by its `threshold` without replacing
    /// the whole list. Panics if no tier with the given threshold exists.
    /// Remaining tiers keep their relative order; the invariant that thresholds
    /// are strictly ascending is preserved automatically.
    pub fn remove_fee_tier(env: Env, admin: Address, threshold: i128) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &admin);

        let tiers: Vec<FeeTier> = env
            .storage()
            .persistent()
            .get(&DataKey::FeeTiers)
            .unwrap_or_else(|| Vec::new(&env));

        let pos = tiers.iter().position(|t| t.threshold == threshold);
        match pos {
            Some(index) => {
                let mut new_tiers = Vec::new(&env);
                for (i, tier) in tiers.iter().enumerate() {
                    if i != index {
                        new_tiers.push_back(tier.clone());
                    }
                }
                env.storage()
                    .persistent()
                    .set(&DataKey::FeeTiers, &new_tiers);
                env.events().publish(
                    (Symbol::new(&env, "fee_tier_removed"),),
                    threshold,
                );
            }
            None => panic!("fee tier not found"),
        }
    }

    /// Admin sets the contract version string (issue #16).
    /// `version` must be ≤ 32 characters.
    /// Panics with "VersionTooLong" if version exceeds 32 chars.
    /// Panics with "unauthorized" if caller is not admin.
    pub fn set_version(env: Env, admin: Address, version: String) {
        Self::assert_admin(&env, &admin);

        if version.len() > MAX_VERSION_LENGTH {
            panic!("VersionTooLong");
        }

        env.storage().persistent().set(&DataKey::Version, &version);
        env.events()
            .publish((Symbol::new(&env, "version_set"),), version);
    }

    /// Admin sets the minimum engagement amount, in the raw smallest unit of
    /// whichever token a given `create_engagement` call uses (issue #17). This
    /// single global floor applies to every allowlisted token regardless of its
    /// `decimals()` — the contract is token-decimals-agnostic (see issue #175).
    /// If the allowlist mixes tokens of very different precision (e.g. a
    /// 7-decimal token alongside an 18-decimal token), the admin is responsible
    /// for picking a value that is a sane floor for all of them, or for keeping
    /// the allowlist restricted to tokens of comparable precision.
    /// Panics with "unauthorized" if caller is not admin.
    pub fn set_min_amount(env: Env, admin: Address, amount: i128) {
        Self::assert_admin(&env, &admin);

        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::MinEngagementAmount), &amount);
        env.events()
            .publish((Symbol::new(&env, "min_amount_set"),), amount);
    }

    /// Admin sets a per-token minimum engagement amount override, in that
    /// token's own smallest unit (issue #366). Takes precedence over the
    /// admin-wide `MinEngagementAmount` for `token` specifically, so an
    /// allowlist mixing tokens of different `decimals()` can give each one a
    /// sane floor instead of sharing one global value (see the gap noted on
    /// `add_allowed_token`, issue #175). Panics with "unauthorized" if caller
    /// is not admin, or `"InvalidMinAmount"` if `amount` is not positive.
    pub fn set_token_min_amount(env: Env, admin: Address, token: Address, amount: i128) {
        Self::assert_admin(&env, &admin);
        if amount <= 0 {
            panic!("InvalidMinAmount");
        }

        let mut overrides: Map<Address, i128> = env
            .storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::TokenMinAmounts))
            .unwrap_or_else(|| Map::new(&env));
        overrides.set(token.clone(), amount);
        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::TokenMinAmounts), &overrides);
        env.events().publish(
            (Symbol::new(&env, "token_min_amount_set"),),
            (token, amount),
        );
    }

    /// Admin removes a token's minimum-amount override (issue #366), so it
    /// falls back to the admin-wide `MinEngagementAmount`. No-op if `token`
    /// had no override set.
    pub fn remove_token_min_amount(env: Env, admin: Address, token: Address) {
        Self::assert_admin(&env, &admin);

        let mut overrides: Map<Address, i128> = env
            .storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::TokenMinAmounts))
            .unwrap_or_else(|| Map::new(&env));
        overrides.remove(token.clone());
        env.storage()
            .persistent()
            .set(&DataKey::Config(ConfigKey::TokenMinAmounts), &overrides);
        env.events()
            .publish((Symbol::new(&env, "token_min_amount_removed"),), token);
    }

    /// Pause state-changing contract operations.
    pub fn pause(env: Env, admin: Address) {
        Self::assert_admin(&env, &admin);
        env.storage().persistent().set(&DataKey::Paused, &true);
        env.events().publish((Symbol::new(&env, "paused"),), admin);
    }

    /// Resume state-changing contract operations.
    pub fn unpause(env: Env, admin: Address) {
        Self::assert_admin(&env, &admin);
        env.storage().persistent().set(&DataKey::Paused, &false);
        env.events()
            .publish((Symbol::new(&env, "unpaused"),), admin);
    }

    /// Return true if the contract is currently paused.
    pub fn is_paused(env: Env) -> bool {
        Self::is_paused_internal(&env)
    }

    // ----------------------------------------------------------
    // ISSUE #239 — PER-ENGAGEMENT PAUSE (QUARANTINE)
    // ----------------------------------------------------------

    /// Quarantine a single engagement, blocking its state-changing operations
    /// without halting the rest of the contract (issue #239).
    ///
    /// # Caller
    /// `admin` — must be the current contract admin.
    ///
    /// # Scope
    /// While quarantined, every engagement-lifecycle call for this ID is
    /// rejected with `"EngagementPaused"`: milestone unlock / proof submission /
    /// confirmation (including batch and force-confirm), disputes and arbiter
    /// voting, escalation, replacements, amendments, milestone extensions,
    /// role transfers, escrow top-ups, early exit, cancellation, and expiry.
    ///
    /// Deliberately still permitted:
    /// - **Read-only queries** — quarantine must not blind indexers or the
    ///   parties to the engagement's state.
    /// - **`admin_replace_arbiter`** — quarantine is frequently *because* the
    ///   arbiter panel is the problem; the admin keeps the tool to fix it.
    /// - **`pause_engagement` / `unpause_engagement`** themselves, so the admin
    ///   can always lift the freeze.
    ///
    /// This is orthogonal to the global [`Self::pause`]: an engagement can be
    /// quarantined while the contract runs normally, and unpausing the contract
    /// does not lift a per-engagement quarantine. Both guards must pass for a
    /// call to proceed.
    ///
    /// Pausing an already-paused engagement is a no-op that still emits the
    /// event, so the admin can re-assert quarantine idempotently.
    ///
    /// Re-pausing an already-paused engagement overwrites the stored reason
    /// with the new one.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the contract admin.
    /// - `"engagement not found"` — no engagement exists with this ID.
    /// - `"EmptyPauseReason"` — `reason` is an empty string.
    /// - `"PauseReasonTooLong"` — `reason` exceeds `MAX_PAUSE_REASON_LEN` characters.
    ///
    /// # Events
    /// Emits `("engagement_paused", engagement_id)` with the acting admin
    /// and the reason.
    pub fn pause_engagement(env: Env, admin: Address, engagement_id: String, reason: String) {
        Self::assert_admin(&env, &admin);

        // Reject unknown IDs so a typo cannot silently create a quarantine
        // record that later blocks a legitimately created engagement.
        let _ = Self::get_engagement_internal(&env, &engagement_id);

        if reason.is_empty() {
            panic!("EmptyPauseReason");
        }
        if reason.len() > MAX_PAUSE_REASON_LEN {
            panic!("PauseReasonTooLong");
        }

        env.storage()
            .persistent()
            .set(&DataKey::EngagementPaused(engagement_id.clone()), &true);
        env.storage().persistent().extend_ttl(
            &DataKey::EngagementPaused(engagement_id.clone()),
            100_000,
            6_300_000,
        );

        env.storage().persistent().set(
            &DataKey::EngagementPauseReason(engagement_id.clone()),
            &reason,
        );
        env.storage().persistent().extend_ttl(
            &DataKey::EngagementPauseReason(engagement_id.clone()),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "engagement_paused"),
                engagement_id.clone(),
            ),
            (admin, reason),
        );
    }

    /// Lift the quarantine on a single engagement (issue #239), returning it to
    /// normal operation. Unpausing an engagement that is not paused is a no-op
    /// that still emits the event.
    ///
    /// Note this has no bearing on the global pause — if the contract itself is
    /// paused, the engagement stays blocked by `"ContractPaused"`.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the contract admin.
    /// - `"engagement not found"` — no engagement exists with this ID.
    ///
    /// # Events
    /// Emits `("engagement_unpaused", engagement_id)` with the acting admin.
    pub fn unpause_engagement(env: Env, admin: Address, engagement_id: String) {
        Self::assert_admin(&env, &admin);

        let _ = Self::get_engagement_internal(&env, &engagement_id);

        env.storage()
            .persistent()
            .remove(&DataKey::EngagementPaused(engagement_id.clone()));

        env.events().publish(
            (
                Symbol::new(&env, "engagement_unpaused"),
                engagement_id.clone(),
            ),
            admin,
        );
    }



    /// Return `true` if this specific engagement is quarantined (issue #239).
    ///
    /// Independent of [`Self::is_paused`] — check both to know whether a
    /// state-changing call will be accepted. Read-only and permissionless;
    /// unknown engagement IDs return `false` rather than panicking, so callers
    /// can probe without a prior existence check.
    pub fn is_engagement_paused(env: Env, engagement_id: String) -> bool {
        Self::is_engagement_paused_internal(&env, &engagement_id)
    }

    /// Nominate a new admin. The nominee must call `claim_admin` to complete rotation.
    pub fn nominate_admin(env: Env, current_admin: Address, new_admin: Address) {
        Self::assert_not_paused(&env);
        Self::assert_admin(&env, &current_admin);

        env.storage()
            .persistent()
            .set(&DataKey::PendingAdmin, &new_admin);
        env.events()
            .publish((Symbol::new(&env, "admin_nominated"),), new_admin);
    }

    /// Claim admin rights after being nominated by the current admin.
    pub fn claim_admin(env: Env, nominee: Address) {
        Self::assert_not_paused(&env);
        nominee.require_auth();

        let pending: Address = env
            .storage()
            .persistent()
            .get(&DataKey::PendingAdmin)
            .unwrap_or_else(|| panic!("no pending admin nomination"));

        if nominee != pending {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        env.storage().instance().set(&DataKey::Admin, &nominee);
        env.storage().persistent().remove(&DataKey::PendingAdmin);
        env.events()
            .publish((Symbol::new(&env, "admin_claimed"),), nominee);
    }

    /// Return the pending admin nominee, if one exists.
    pub fn get_pending_admin(env: Env) -> Option<Address> {
        env.storage().persistent().get(&DataKey::PendingAdmin)
    }

    /// Return the current contract admin.
    pub fn get_admin(env: Env) -> Address {
        Self::get_admin_internal(&env)
    }


    // ----------------------------------------------------------
    // ADMIN CONFIG
    // ----------------------------------------------------------

    /// Set the minimum ledger gap between successive proof submissions on the
    /// same milestone. Only callable by the admin set during `init`.
    pub fn set_proof_cooldown(env: Env, admin: Address, ledgers: u32) {
        admin.require_auth();
        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("contract not initialised"));
        if admin != stored_admin {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::ProofCooldown), &ledgers);
    }

    pub(crate) fn get_proof_cooldown(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ProofCooldown))
            .unwrap_or(DEFAULT_PROOF_COOLDOWN)
    }

    // ----------------------------------------------------------
    // ISSUE #258 — RESERVED ESCROW CALLBACK CHECKPOINT
    // ----------------------------------------------------------





    // ----------------------------------------------------------
    // ISSUES #358, #359 — COMPANY / RECRUITER BLACKLIST
    // ----------------------------------------------------------







    // ----------------------------------------------------------
    // ISSUE #357 — ADMIN ACTION AUDIT LOG
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // ISSUE #26 — TOKEN ALLOWLIST
    // ----------------------------------------------------------

    /// Add a token SAC address to the allowlist. Admin only.
    ///
    /// Note (issue #175): the contract treats `total_amount`, milestone payouts,
    /// and `MinEngagementAmount` as raw integer units of whichever token is used
    /// for a given engagement — it never queries or adjusts for that token's
    /// `decimals()`. Percentage-based math (`amount * payment_percent / 100`) is
    /// exact integer arithmetic regardless of decimals, but a single admin-wide
    /// `MinEngagementAmount` (see [`Self::set_min_amount`]) means the effective
    /// real-world minimum will differ across tokens with different precision.
    /// Only allowlist tokens whose smallest unit is comparable in scale to what
    /// `MinEngagementAmount` assumes, or adjust the minimum accordingly.
    pub fn add_allowed_token(env: Env, admin: Address, token: Address) {
        Self::assert_admin(&env, &admin);
        let mut allowed: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env));
        let already = (0..allowed.len()).any(|i| allowed.get(i).unwrap() == token);
        if !already {
            allowed.push_back(token.clone());
            env.storage()
                .persistent()
                .set(&DataKey::AllowedTokens, &allowed);
        }
        env.events()
            .publish((Symbol::new(&env, "token_allowlisted"),), token);
    }

    /// Remove a token SAC address from the allowlist. Admin only.
    pub fn remove_allowed_token(env: Env, admin: Address, token: Address) {
        Self::assert_admin(&env, &admin);
        let mut allowed: Vec<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env));
        for i in 0..allowed.len() {
            if allowed.get(i).unwrap() == token {
                allowed.remove(i);
                break;
            }
        }
        env.storage()
            .persistent()
            .set(&DataKey::AllowedTokens, &allowed);
        env.events()
            .publish((Symbol::new(&env, "token_removed"),), token);
    }

    /// Enable or disable the token allowlist. Admin only.
    pub fn set_token_allowlist_enabled(env: Env, admin: Address, enabled: bool) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .set(&DataKey::AllowlistEnabled, &enabled);
        env.events()
            .publish((Symbol::new(&env, "allowlist_enabled_set"),), enabled);
    }

    /// Return all currently allowlisted token addresses.
    pub fn get_allowed_tokens(env: Env) -> Vec<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::AllowedTokens)
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ----------------------------------------------------------
    // ISSUE #41 — CONFIGURABLE LEDGERS PER DAY
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // ISSUE #18 — MAX RETENTION DAYS CAP
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // ISSUE #21 — MAX MILESTONES CAP
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // PER-COMPANY ACTIVE ENGAGEMENT CAP
    // ----------------------------------------------------------

    /// Admin sets the maximum number of simultaneously active engagements allowed
    /// per company address. Defaults to 50 when not explicitly configured.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the contract admin.
    /// - `"InvalidMaxActivePerCompany"` — `count` is 0.
    pub fn set_max_active_per_company(env: Env, admin: Address, count: u32) {
        Self::assert_admin(&env, &admin);
        if count == 0 {
            panic!("InvalidMaxActivePerCompany");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxActivePerCompany), &count);
        env.events()
            .publish((Symbol::new(&env, "max_active_per_company_set"),), count);
    }

    /// Return the current per-company active engagement cap.
    /// Returns `DEFAULT_MAX_ACTIVE_PER_COMPANY` (50) when not configured.
    pub fn get_max_active_per_company(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxActivePerCompany))
            .unwrap_or(DEFAULT_MAX_ACTIVE_PER_COMPANY)
    }

    /// Return the current active engagement count for a company.
    pub fn get_company_active_count(env: Env, company: Address) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::CompanyActiveCount(company))
            .unwrap_or(0u32)
    }


    // ----------------------------------------------------------
    // ISSUE #38 — INACTIVITY TIMEOUT
    // ----------------------------------------------------------



    /// Mark an engagement as `Expired` and refund the remaining escrow to the company.
    /// This is a **permissionless keeper function** — any address may call it; no signature
    /// is required. It is designed to be triggered by off-chain bots or backend pollers.
    ///
    /// # Expiry Condition
    /// The engagement is eligible for expiry when:
    /// `env.ledger().sequence() - last_activity_ledger >= inactivity_timeout_ledgers`
    ///
    /// The default `inactivity_timeout_ledgers` is set at contract initialisation and can be
    /// updated by the admin via `set_inactivity_timeout_ledgers`.
    ///
    /// # Behaviour
    /// - The engagement must not be `Completed`.
    /// - Any escrow balance not yet released (`total_amount - released_amount`) is transferred
    ///   back to the company address.
    /// - The engagement status is set to `Expired`.
    ///
    /// # Panics
    /// - `"Cannot expire completed engagement"` — engagement is already `Completed`.
    /// - `"Inactivity timeout not reached"` — the inactivity threshold has not yet been reached.
    ///
    /// # Events
    /// Emits `("engagement_expired", engagement_id)` with the refund amount.
    pub fn expire_engagement(env: Env, engagement_id: String) {
        Self::assert_engagement_not_paused(&env, &engagement_id);
        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status == EngagementStatus::Completed {
            panic!("Cannot expire completed engagement");
        }

        let timeout = DEFAULT_INACTIVITY_TIMEOUT_LEDGERS;
        let current_ledger = env.ledger().sequence();

        if current_ledger <= engagement.last_activity_ledger + timeout {
            panic!("Inactivity timeout not reached");
        }

        let refund = engagement.total_amount - engagement.released_amount;
        if refund > 0 {
            let token_client = token::Client::new(&env, &engagement.token);
            token_client.transfer(
                &env.current_contract_address(),
                &engagement.company,
                &refund,
            );
        }

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Expired;
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
        );

        Self::decrement_company_active_count(&env, &engagement.company);
        Self::settle_recruiter_bond(&env, &engagement);

        env.events().publish(
            (
                Symbol::new(&env, "engagement_expired"),
                engagement_id.clone(),
            ),
            refund,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #40 — STORAGE TTL EXTENSION
    // ----------------------------------------------------------



    /// Helper to extend TTL for engagement storage.
    pub(crate) fn extend_engagement_ttl(env: &Env, engagement_id: &String) {
        let extend_to = DEFAULT_STORAGE_TTL_EXTEND_TO;
        env.storage().persistent().extend_ttl(
            &DataKey::Engagement(engagement_id.clone()),
            100_000,
            extend_to,
        );
    }

    pub(crate) fn emit_milestone_status_changed(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
        old_status: MilestoneStatus,
        new_status: MilestoneStatus,
    ) {
        if old_status != new_status {
            env.events().publish(
                (
                    Symbol::new(env, "milestone_status_changed"),
                    engagement_id.clone(),
                ),
                (milestone_index, old_status, new_status),
            );
        }
    }

    pub(crate) fn emit_engagement_status_changed(
        env: &Env,
        engagement_id: &String,
        old_status: EngagementStatus,
        new_status: EngagementStatus,
    ) {
        if old_status != new_status {
            env.events().publish(
                (Symbol::new(env, "status_changed"), engagement_id.clone()),
                (old_status, new_status),
            );
        }
    }

    // ----------------------------------------------------------
    // ISSUE #69 — CONTRACT UPGRADE MECHANISM
    // ----------------------------------------------------------

    /// Admin proposes a WASM upgrade with a mandatory time-lock (issue #69).
    /// The pending upgrade is stored and becomes executable after `lock_duration` ledgers.
    /// Default time-lock is 17,280 ledgers (~1 day); use `set_upgrade_lock_duration` to change it.
    /// Emits `upgrade_proposed` with `(new_wasm_hash, execute_after_ledger)`.
    ///
    /// # Repeated calls (issue #185)
    /// Calling `propose_upgrade` again while a previous proposal is still pending
    /// **overwrites** it and **resets the timelock countdown** — the new
    /// `execute_after_ledger` is computed from the current ledger, not the
    /// original proposal's. This is intentional: it lets the admin correct or
    /// retract a bad proposal (e.g. wrong wasm hash) without waiting out the
    /// original lock. The tradeoff is that a compromised admin key can grief
    /// legitimate upgrades by indefinitely re-proposing, or delay execution by
    /// repeatedly re-proposing the same hash — this is accepted as inherent to
    /// admin-key trust and is not separately mitigated here.
    pub fn propose_upgrade(env: Env, admin: Address, new_wasm_hash: BytesN<32>) {
        Self::assert_admin(&env, &admin);

        let lock_duration: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::UpgradeLockDuration))
            .unwrap_or(LEDGERS_PER_DAY);

        let execute_after_ledger = env.ledger().sequence() + lock_duration;

        let proposal = UpgradeProposal {
            new_wasm_hash: new_wasm_hash.clone(),
            execute_after_ledger,
        };

        env.storage()
            .instance()
            .set(&DataKey::PendingUpgrade, &proposal);

        env.events().publish(
            (Symbol::new(&env, "upgrade_proposed"),),
            (new_wasm_hash, execute_after_ledger),
        );
    }

    /// Execute a pending upgrade after the time-lock has elapsed (issue #69).
    /// Permissionless — anyone may call this once `execute_after_ledger` is reached.
    /// Panics with `"no pending upgrade"` if no proposal exists.
    /// Panics with `"UpgradeLockNotElapsed"` if called before the time-lock expires.
    /// Emits `upgrade_executed` before applying the WASM swap.
    pub fn execute_upgrade(env: Env) {
        let proposal: UpgradeProposal = env
            .storage()
            .instance()
            .get(&DataKey::PendingUpgrade)
            .unwrap_or_else(|| panic!("no pending upgrade"));

        if env.ledger().sequence() < proposal.execute_after_ledger {
            panic!("UpgradeLockNotElapsed");
        }

        env.storage().instance().remove(&DataKey::PendingUpgrade);

        env.events().publish(
            (Symbol::new(&env, "upgrade_executed"),),
            proposal.new_wasm_hash.clone(),
        );

        env.deployer()
            .update_current_contract_wasm(proposal.new_wasm_hash);
    }

    /// Admin sets the upgrade time-lock duration in ledgers (issue #69).
    pub fn set_upgrade_lock_duration(env: Env, admin: Address, ledgers: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::UpgradeLockDuration), &ledgers);
        env.events()
            .publish((Symbol::new(&env, "upgrade_lock_duration_set"),), ledgers);
    }

    /// Return the current upgrade time-lock duration in ledgers (issue #69).
    /// Defaults to 17,280 (~1 day).
    pub fn get_upgrade_lock_duration(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::UpgradeLockDuration))
            .unwrap_or(LEDGERS_PER_DAY)
    }

    // ----------------------------------------------------------
    // ISSUE #68 — ADMIN-CONFIGURABLE MAX PROOF HASH LENGTH
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // ISSUE #52 — ARBITER FEE
    // ----------------------------------------------------------

    /// Admin sets the arbiter fee in basis points (0–200, max 2%) (issue #52).
    /// Panics with "ArbiterFeeTooHigh" if bps > 200.
    pub fn set_arbiter_fee(env: Env, admin: Address, bps: u32) {
        Self::assert_admin(&env, &admin);
        if bps > MAX_ARBITER_FEE_BPS {
            panic!("ArbiterFeeTooHigh");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::ArbiterFee), &bps);
        env.events()
            .publish((Symbol::new(&env, "arbiter_fee_set"),), bps);
    }

    /// Return the current arbiter fee in basis points (issue #52).
    /// Defaults to 0 when not configured.
    pub fn get_arbiter_fee(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::ArbiterFee))
            .unwrap_or(0u32)
    }

    // ----------------------------------------------------------
    // ISSUE #240 — FULL CONFIG SNAPSHOT
    // ----------------------------------------------------------


    // ----------------------------------------------------------
    // ISSUE #59 — ADMIN ROLE RENOUNCEMENT
    // ----------------------------------------------------------

}
