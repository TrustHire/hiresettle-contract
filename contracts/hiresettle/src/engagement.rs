use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // CREATE ENGAGEMENT
    // ----------------------------------------------------------

    /// Create a new recruitment engagement and lock funds in escrow.
    ///
    /// # Arguments
    /// - `engagement_id`   — unique string ID for this engagement
    /// - `company`         — company address (must sign this tx)
    /// - `recruiter`       — recruiter address (receives payments)
    /// - `arbiter_setup`   — arbiter addresses, quorum (M of N), and optional per-arbiter
    ///   weights; with weights, quorum is measured in cumulative weight (issue #460)
    /// - `token`           — SAC address of the escrow token (USDC or any allowlisted token,
    ///   see [`Self::add_allowed_token`]). `total_amount` and all amount-based math below are
    ///   raw integer units in this token's smallest denomination (e.g. stroops for a 7-decimal
    ///   token) — the contract does not read or adjust for the token's `decimals()`. Callers
    ///   integrating non-USDC-like tokens are responsible for choosing a `total_amount` (and,
    ///   for admins, a [`Self::set_min_amount`]) that makes sense for that token's precision.
    /// - `total_amount`    — total recruiter fee in the token's smallest unit
    /// - `job_title`       — short job title string
    /// - `milestones`      — ordered milestone list
    /// - `retention_days`  — Vec of retention windows in days (one per Retention milestone)
    /// - `config`          — bundled optional config: metadata_hash, co_recruiter, recruiter_split_bps
    ///
    /// # Panics
    /// - `"CompanyRecruiterCollision"` — `company` and `recruiter` are the same address.
    /// - `"CompanyArbiterCollision"` — `company` also appears in the arbiter set.
    /// - `"RecruiterArbiterCollision"` — `recruiter` also appears in the arbiter set.
    /// - `"AlreadyInitialized"` — engagement ID already exists.
    /// - `"AmountTooLow"` — `total_amount` is lower than minimum configured threshold.
    /// - `"InvalidMilestones"` — milestone vector is empty or exceeds maximum milestone limit.
    /// - `"InvalidNameLength"` — `job_title` string length is invalid or empty.
    /// - `"TokenNotAllowed"` — payment token is not present in the allowed token list.
    /// - `"ArbiterWeightsLengthMismatch"` / `"InvalidArbiterWeight"` — `arbiter_setup.weights`
    ///   is not parallel to `arbiters` or contains a zero weight (issue #460).
    /// - `"invalid quorum"` — quorum is 0 or exceeds the arbiter count (total weight when
    ///   weights are given).
    /// - `"QuorumBelowMinRatio"` — quorum is below `ceil(N * min_quorum_ratio_bps / 10_000)`
    ///   where `N` is the arbiter count (total weight when weights are given); see
    ///   [`Self::set_min_quorum_ratio_bps`] (issue #502). Never raised at the default ratio of 0.
    /// - `"InvalidPrerequisiteIndex"` / `"PrerequisiteCycle"` — a milestone's `prerequisites`
    ///   reference an out-of-range index or form a cycle (issue #461).
    ///
    /// These checks exist so a company cannot name itself (or a colluding address)
    /// as arbiter and vote on its own disputes, or name itself as recruiter to
    /// self-confirm milestones. See issue #174.
    pub fn create_engagement(
        env: Env,
        engagement_id: String,
        company: Address,
        recruiter: Address,
        arbiter_setup: ArbiterSetup,
        token: Address,
        total_amount: i128,
        job_title: String,
        milestones: Vec<Milestone>,
        retention_days: Vec<u32>,
        config: EngagementConfig,
    ) -> String {
        Self::assert_not_paused(&env);
        company.require_auth();
        Self::create_engagement_impl(
            env,
            engagement_id,
            company,
            recruiter,
            arbiter_setup,
            token,
            total_amount,
            job_title,
            milestones,
            retention_days,
            config,
        )
    }

    /// Shared engagement-creation logic behind `create_engagement`, factored
    /// out so `batch_create_engagements` (issue #260) can authorize each
    /// distinct company address once up front and then create every one of
    /// its engagements without re-`require_auth`-ing that address — calling
    /// `require_auth()` twice for the same address within one call frame
    /// (as opposed to across separate cross-contract invocations) is rejected
    /// by the host as a duplicate authorization.
    ///
    /// Callers are responsible for `assert_not_paused` and
    /// `company.require_auth()` before invoking this.
    /// Recruiter sets their own active engagement cap (issue #482).
    /// `0` removes the self-imposed limit.
    pub fn set_recruiter_active_cap(env: Env, recruiter: Address, cap: u32) {
        recruiter.require_auth();
        let key = DataKey2::RecruiterActiveCap(recruiter.clone());
        if cap == 0 {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &cap);
        }
        env.events().publish(
            (Symbol::new(&env, "recruiter_active_cap_set"), recruiter),
            cap,
        );
    }

    /// Recruiter's self-imposed active engagement cap, if any (issue #482).
    pub fn get_recruiter_active_cap(env: Env, recruiter: Address) -> Option<u32> {
        env.storage()
            .persistent()
            .get(&DataKey2::RecruiterActiveCap(recruiter))
    }

    /// Admin sets a contract-wide cap on each recruiter's active engagements
    /// (issue #482). `0` means unlimited.
    pub fn set_max_active_per_recruiter(env: Env, admin: Address, count: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxActivePerRecruiter), &count);
        env.events()
            .publish((Symbol::new(&env, "max_active_per_recruiter_set"),), count);
    }

    /// Admin-wide recruiter active engagement cap; `0` when unlimited (issue #482).
    pub fn get_max_active_per_recruiter(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxActivePerRecruiter))
            .unwrap_or(0)
    }

    /// Count the recruiter's engagements currently in `Active` status, plus
    /// pending `Offered` ones (issue #569), which already hold a cap slot.
    fn recruiter_active_count(env: &Env, recruiter: &Address) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterEngagements(recruiter.clone()))
            .unwrap_or(Vec::new(env));
        let mut count = 0u32;
        for id in ids.iter() {
            let engagement: Option<Engagement> =
                env.storage().persistent().get(&DataKey::Engagement(id));
            if matches!(
                engagement,
                Some(e) if e.status == EngagementStatus::Active
                    || e.status == EngagementStatus::Offered
            ) {
                count += 1;
            }
        }
        count
    }

    pub(crate) fn create_engagement_impl(
        env: Env,
        engagement_id: String,
        company: Address,
        recruiter: Address,
        arbiter_setup: ArbiterSetup,
        token: Address,
        total_amount: i128,
        job_title: String,
        milestones: Vec<Milestone>,
        retention_days: Vec<u32>,
        config: EngagementConfig,
    ) -> String {
        Self::assert_function_not_paused(&env, FN_CREATE_ENGAGEMENT);
        // Validate engagement_id format: non-empty, ≤ 64 chars, [A-Za-z0-9-] only.
        if engagement_id.is_empty() || engagement_id.len() > MAX_ENGAGEMENT_ID_LENGTH {
            panic!("InvalidEngagementId");
        }
        let id_len = engagement_id.len() as usize;
        let mut id_buf = [0u8; MAX_ENGAGEMENT_ID_LENGTH as usize];
        engagement_id.copy_into_slice(&mut id_buf[..id_len]);
        for &b in &id_buf[..id_len] {
            let valid =
                b.is_ascii_uppercase() || b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-';
            if !valid {
                panic!("InvalidEngagementId");
            }
        }

        // Issue #24: Job title validation
        if job_title.is_empty() {
            panic!("JobTitleEmpty");
        }
        if job_title.len() > 64 {
            panic!("JobTitleTooLong");
        }

        // Issue #21: Max milestone count cap validation
        if milestones.is_empty() {
            panic!("ZeroMilestones");
        }
        let max_milestones = DEFAULT_MAX_MILESTONES;
        if milestones.len() > max_milestones {
            panic!("TooManyMilestones");
        }

        // Issue #22: Milestone name max length validation
        for i in 0..milestones.len() {
            let m = milestones.get(i).unwrap();
            if m.name.is_empty() {
                panic!("MilestoneNameEmpty: index {}", i);
            }
            if m.name.len() > 64 {
                panic!("MilestoneNameTooLong: index {}", i);
            }
        }

        // Issue #23: Milestone name uniqueness validation
        for i in 0..milestones.len() {
            let m_i = milestones.get(i).unwrap();
            for j in (i + 1)..milestones.len() {
                let m_j = milestones.get(j).unwrap();
                if m_i.name == m_j.name {
                    let mut name_buf = [0u8; 64];
                    let name_len = m_i.name.len() as usize;
                    m_i.name.copy_into_slice(&mut name_buf[..name_len]);
                    let name_str = core::str::from_utf8(&name_buf[..name_len]).unwrap_or("");
                    panic!("DuplicateMilestoneName: {}", name_str);
                }
            }
        }

        // Issue #248: engagement tags validation — bounded count and length so
        // storage stays predictable regardless of caller input.
        if let Some(ref tags) = config.tags {
            if tags.len() > MAX_TAGS {
                panic!("TooManyTags");
            }
            for i in 0..tags.len() {
                let tag = tags.get(i).unwrap();
                if tag.is_empty() {
                    panic!("TagEmpty: index {}", i);
                }
                if tag.len() > MAX_TAG_LENGTH {
                    panic!("TagTooLong: index {}", i);
                }
            }
        }

        if total_amount <= 0 {
            panic!("amount must be greater than zero");
        }

        // Issue #17 / #366: minimum amount validation, using token's
        // per-token override if the admin has set one, else the
        // admin-wide default.
        let min_amount = Self::get_effective_min_amount(env.clone(), token.clone());
        if total_amount < min_amount {
            panic!("AmountBelowMinimum");
        }

        // Issue #26: reject token if allowlist is active and token not in it.
        let allowlist_enabled: bool = env
            .storage()
            .persistent()
            .get(&DataKey::AllowlistEnabled)
            .unwrap_or(false);
        if allowlist_enabled {
            let allowed: Vec<Address> = env
                .storage()
                .persistent()
                .get(&DataKey::AllowedTokens)
                .unwrap_or_else(|| Vec::new(&env));
            let is_allowed = (0..allowed.len()).any(|i| allowed.get(i).unwrap() == token);
            if !is_allowed {
                panic!("TokenNotAllowed");
            }
        }

        let arbiters = arbiter_setup.arbiters;
        let quorum = arbiter_setup.quorum;
        let arbiter_weights = arbiter_setup.weights;

        if arbiters.is_empty() {
            panic!("at least one arbiter required");
        }

        // Issue #460: with weights, quorum is measured in cumulative weight
        // rather than headcount, so it is bounded by the total weight.
        let total_weight: u32 = match &arbiter_weights {
            Some(weights) => {
                if weights.len() != arbiters.len() {
                    panic!("ArbiterWeightsLengthMismatch");
                }
                let mut total: u32 = 0;
                for i in 0..weights.len() {
                    let w = weights.get(i).unwrap();
                    if w == 0 {
                        panic!("InvalidArbiterWeight: index {}", i);
                    }
                    total = total
                        .checked_add(w)
                        .unwrap_or_else(|| panic!("ArbiterWeightOverflow"));
                }
                total
            }
            None => arbiters.len(),
        };

        if quorum == 0 || quorum > total_weight {
            panic!("invalid quorum");
        }

        // Issue #502: enforce the admin-configured minimum quorum ratio.
        // `quorum * 10_000 < total_weight * bps` is the exact integer form of
        // `quorum < ceil(total_weight * bps / 10_000)`, so the boundary rounds
        // up (e.g. 50 % of a 5-arbiter panel requires quorum 3). Widened to
        // u64 so large weighted panels cannot overflow.
        let min_ratio_bps = Self::get_min_quorum_ratio_bps(env.clone());
        if (quorum as u64) * (MAX_MIN_QUORUM_RATIO_BPS as u64)
            < (total_weight as u64) * (min_ratio_bps as u64)
        {
            panic!("QuorumBelowMinRatio");
        }

        // Issue #174: reject overlapping company/recruiter/arbiter addresses so a
        // company cannot name itself (or a colluding address) as arbiter and vote
        // on its own disputes, or name itself as recruiter to self-confirm milestones.
        if company == recruiter {
            panic!("CompanyRecruiterCollision");
        }
        for i in 0..arbiters.len() {
            let a = arbiters.get(i).unwrap();
            if a == company {
                panic!("CompanyArbiterCollision");
            }
            if a == recruiter {
                panic!("RecruiterArbiterCollision");
            }
        }

        // Reject empty metadata hash — caller must either omit or provide a real CID.
        if let Some(ref hash) = config.metadata_hash {
            if hash.is_empty() {
                panic!("InvalidMetadataHash");
            }
        }

        // Reject empty contract_pdf_hash — caller must either omit or provide a real hash.
        if let Some(ref hash) = config.contract_pdf_hash {
            if hash.is_empty() {
                panic!("InvalidContractPdfHash");
            }
        }

        // Issue #56: validate co-recruiter split basis points.
        if config.recruiter_split_bps > FULL_SPLIT_BPS {
            panic!("InvalidSplitBps");
        }

        // Issue #466: a zero-length stream would divide by zero when vesting.
        if config.stream_duration_ledgers == Some(0) {
            panic!("InvalidStreamDuration");
        }

        let mut total_percent: u32 = 0;
        for i in 0..milestones.len() {
            total_percent += milestones.get(i).unwrap().payment_percent;
        }
        if total_percent != 100 {
            panic!("milestone percentages must sum to 100");
        }

        // Issue #461: prerequisite indices must be in range and acyclic.
        Self::validate_milestone_prerequisites(&milestones);

        if env
            .storage()
            .persistent()
            .has(&DataKey::Engagement(engagement_id.clone()))
        {
            panic!("engagement already exists");
        }

        // Cap check: reject if the company is already at or over the active engagement limit.
        let active_count: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyActiveCount(company.clone()))
            .unwrap_or(0u32);
        let max_active: u32 = env
            .storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::MaxActivePerCompany))
            .unwrap_or(DEFAULT_MAX_ACTIVE_PER_COMPANY);
        if active_count >= max_active {
            panic!("CompanyActiveLimitReached");
        }

        // Issue #482: enforce the tighter of the recruiter's own cap and the
        // admin-wide recruiter cap (0 / unset means no limit).
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
        let recruiter_cap = match (personal_cap, admin_cap) {
            (0, 0) => None,
            (0, c) | (c, 0) => Some(c),
            (p, a) => Some(p.min(a)),
        };
        if let Some(cap) = recruiter_cap {
            if Self::recruiter_active_count(&env, &recruiter) >= cap {
                panic!("RecruiterActiveLimitReached");
            }
        }

        let current_ledger = env.ledger().sequence();
        let lpd = Self::get_ledgers_per_day_internal(&env);
        let max_retention_days = DEFAULT_MAX_RETENTION_DAYS;
        let mut retention_index: u32 = 0;
        let mut resolved_milestones: Vec<Milestone> = Vec::new(&env);

        for i in 0..milestones.len() {
            let mut m = milestones.get(i).unwrap();
            match m.kind {
                MilestoneKind::Placement => {
                    m.valid_after_ledger = 0;
                    m.status = MilestoneStatus::Pending;
                }
                MilestoneKind::Retention => {
                    let days = retention_days.get(retention_index).unwrap_or(30);
                    retention_index += 1;

                    // Issue #19: Zero retention days validation
                    if days == 0 {
                        panic!("RetentionDaysZero");
                    }

                    if days > max_retention_days {
                        panic!("RetentionDaysTooLarge");
                    }
                    m.valid_after_ledger = current_ledger + (days * lpd);
                    m.status = MilestoneStatus::Locked;
                }
            }
            resolved_milestones.push_back(m);
        }

        // Issue #571: an absolute deadline must lie in the future and after
        // the last retention milestone's earliest unlock.
        if let Some(end) = config.end_ledger {
            if end <= current_ledger {
                panic!("InvalidEndLedger");
            }
            for i in 0..resolved_milestones.len() {
                if end <= resolved_milestones.get(i).unwrap().valid_after_ledger {
                    panic!("InvalidEndLedger");
                }
            }
        }

        // Issue #472: draw from the company pool when requested; otherwise
        // perform a fresh token transfer as before.
        if config.fund_from_pool {
            Self::debit_company_pool(&env, &company, &token, total_amount);
            Self::mark_pool_funded(&env, &engagement_id);
        } else {
            let token_client = token::Client::new(&env, &token);
            token_client.transfer(&company, &env.current_contract_address(), &total_amount);
        }
        // Issue #578: new engagement escrow locked (pool funds were never in TVL).
        Self::add_token_locked(&env, &token, total_amount);

        // Issue #459: escrow the optional recruiter bond alongside the company's funding.
        if let Some(bond_amount) = config.recruiter_bond_amount {
            Self::escrow_recruiter_bond(&env, &engagement_id, &recruiter, &token, bond_amount);
        }
        // Issue #506: the co-recruiter's bond, only when there is a co-recruiter.
        if let (Some(co_recruiter), Some(bond_amount)) =
            (&config.co_recruiter, config.co_recruiter_bond_amount)
        {
            Self::escrow_co_recruiter_bond(&env, &engagement_id, co_recruiter, &token, bond_amount);
        }

        let engagement = Engagement {
            id: engagement_id.clone(),
            company: company.clone(),
            recruiter: recruiter.clone(),
            arbiters,
            quorum,
            arbiter_weights,
            token,
            total_amount,
            released_amount: 0,
            job_title,
            metadata_hash: config.metadata_hash,
            created_at_ledger: current_ledger,
            last_activity_ledger: current_ledger,
            milestones: resolved_milestones,
            // Issue #569: wait for the recruiter's consent when requested.
            status: if config.require_recruiter_acceptance {
                EngagementStatus::Offered
            } else {
                EngagementStatus::Active
            },
            co_recruiter: config.co_recruiter,
            recruiter_split_bps: config.recruiter_split_bps,
            contract_pdf_hash: config.contract_pdf_hash,
            referrer: config.referrer,
            tags: config.tags.clone(),
            is_public: config.is_public,
            stream_duration_ledgers: config.stream_duration_ledgers,
            end_ledger: config.end_ledger,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        Self::extend_engagement_ttl(&env, &engagement_id);

        // Issue #547: credit payouts instead of pushing them.
        if config.pull_payouts {
            Self::mark_pull_payouts(&env, &engagement_id);
        }

        // Issue #505: freeze the tier-resolved rate (or the base rate when no
        // tier matches) so later `set_fee_tiers` calls cannot change it.
        if config.snapshot_fee_tier {
            let base_bps = Self::get_platform_fee_internal(&env).bps;
            let bps = Self::resolve_platform_fee_bps(&env, base_bps, total_amount);
            let key = DataKey2::FeeTierSnapshot(engagement_id.clone());
            env.storage().persistent().set(&key, &bps);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
        }

        // Increment per-company active engagement count.
        let new_active = active_count + 1;
        env.storage()
            .persistent()
            .set(&DataKey::CompanyActiveCount(company.clone()), &new_active);
        env.storage().persistent().extend_ttl(
            &DataKey::CompanyActiveCount(company.clone()),
            100_000,
            6_300_000,
        );

        // Issue #34: increment global engagement counter.
        let count: u64 = env
            .storage()
            .instance()
            .get(&DataKey::EngagementCount)
            .unwrap_or(0u64);
        env.storage()
            .instance()
            .set(&DataKey::EngagementCount, &(count + 1));

        // Issue #237: append engagement_id to the global index that backs
        // `get_engagement_ids_by_status`. Kept separate from `EngagementCount`
        // because that counter is a scalar and cannot be enumerated.
        let mut all_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));
        all_ids.push_back(engagement_id.clone());
        env.storage()
            .persistent()
            .set(&DataKey::AllEngagements, &all_ids);
        env.storage()
            .persistent()
            .extend_ttl(&DataKey::AllEngagements, 100_000, 6_300_000);

        // Issue #35: append engagement_id to the per-company index.
        let mut company_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyEngagements(company.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        company_ids.push_back(engagement_id.clone());
        env.storage()
            .persistent()
            .set(&DataKey::CompanyEngagements(company.clone()), &company_ids);
        env.storage().persistent().extend_ttl(
            &DataKey::CompanyEngagements(company.clone()),
            100_000,
            6_300_000,
        );

        // Issue #36: append engagement_id to the per-recruiter index.
        let mut recruiter_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterEngagements(recruiter.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        recruiter_ids.push_back(engagement_id.clone());
        env.storage().persistent().set(
            &DataKey::RecruiterEngagements(recruiter.clone()),
            &recruiter_ids,
        );

        // Issue #248 & #249: append engagement_id to per-tag indices.
        // Count/length were already validated above; here we only need to
        // de-duplicate so a repeated tag doesn't list the engagement twice
        // in the same per-tag index.
        if let Some(ref tags) = config.tags {
            let mut seen_tags = Vec::new(&env);
            for i in 0..tags.len() {
                let t = tags.get(i).unwrap();
                if !seen_tags.contains(&t) {
                    seen_tags.push_back(t.clone());
                    let mut tag_ids: Vec<String> = env
                        .storage()
                        .persistent()
                        .get(&DataKey::TagEngagements(t.clone()))
                        .unwrap_or_else(|| Vec::new(&env));
                    tag_ids.push_back(engagement_id.clone());
                    env.storage()
                        .persistent()
                        .set(&DataKey::TagEngagements(t.clone()), &tag_ids);
                    env.storage().persistent().extend_ttl(
                        &DataKey::TagEngagements(t.clone()),
                        100_000,
                        6_300_000,
                    );
                }
            }
        }

        env.storage().persistent().extend_ttl(
            &DataKey::RecruiterEngagements(recruiter.clone()),
            100_000,
            6_300_000,
        );

        // Issue #577: append engagement_id to the per-token index.
        // Forward-only: engagements created before this deployment are not
        // backfilled and never appear in this index.
        let mut token_ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey3::TokenEngagements(engagement.token.clone()))
            .unwrap_or_else(|| Vec::new(&env));
        token_ids.push_back(engagement_id.clone());
        env.storage().persistent().set(
            &DataKey3::TokenEngagements(engagement.token.clone()),
            &token_ids,
        );
        env.storage().persistent().extend_ttl(
            &DataKey3::TokenEngagements(engagement.token.clone()),
            100_000,
            6_300_000,
        );

        if let Some(ref bundle_id) = config.bundle_id {
            Self::add_bundle_member(&env, bundle_id, &engagement_id);
        }

        env.events().publish(
            (
                Symbol::new(&env, "engagement_created"),
                engagement_id.clone(),
            ),
            engagement_id.clone(),
        );
        if config.require_recruiter_acceptance {
            env.events().publish(
                (
                    Symbol::new(&env, "engagement_offered"),
                    engagement_id.clone(),
                ),
                (company, recruiter, total_amount),
            );
        }

        engagement_id
    }

    // ----------------------------------------------------------
    // ISSUE #569 — RECRUITER ACCEPTANCE STEP
    // ----------------------------------------------------------

    /// Recruiter (or their co-signer) accepts an `Offered` engagement, moving
    /// it to `Active`. Placement milestones' no-show clocks start from this
    /// ledger rather than from creation. Retention time gates are unchanged:
    /// they were fixed relative to creation.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"EngagementNotOffered"` — the engagement is not in `Offered` status.
    /// - `"unauthorized"` — caller is not the recruiter or their co-signer.
    ///
    /// # Events
    /// `("engagement_accepted", engagement_id)` with the recruiter address,
    /// plus the `Offered → Active` status-change event.
    pub fn accept_engagement(env: Env, recruiter: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Offered {
            panic!("EngagementNotOffered");
        }
        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        for i in 0..engagement.milestones.len() {
            let m = engagement.milestones.get(i).unwrap();
            if m.kind == MilestoneKind::Placement && m.status == MilestoneStatus::Pending {
                Self::mark_milestone_pending_since(&env, &engagement_id, i);
            }
        }

        engagement.status = EngagementStatus::Active;
        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            EngagementStatus::Offered,
            EngagementStatus::Active,
            Some(recruiter.clone()),
        );
        env.events().publish(
            (
                Symbol::new(&env, "engagement_accepted"),
                engagement_id,
            ),
            recruiter,
        );
    }

    /// Company (or its co-signer) withdraws an `Offered` engagement before
    /// the recruiter accepts it. The full escrow is refunded (to the pool for
    /// pool-funded engagements), any bonds are returned, and the engagement
    /// moves to `Cancelled`, freeing its active-engagement cap slots.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"EngagementNotOffered"` — the engagement is not in `Offered` status.
    /// - `"unauthorized"` — caller is not the company or its co-signer.
    ///
    /// # Events
    /// `("offer_withdrawn", engagement_id)` with the refund amount, plus the
    /// `Offered → Cancelled` status-change event.
    pub fn withdraw_offer(env: Env, company: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Offered {
            panic!("EngagementNotOffered");
        }
        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let refund = engagement.total_amount - engagement.released_amount;
        Self::refund_company_escrow(&env, &engagement, &engagement_id, refund);

        engagement.status = EngagementStatus::Cancelled;
        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            EngagementStatus::Offered,
            EngagementStatus::Cancelled,
            Some(company.clone()),
        );

        Self::decrement_company_active_count(&env, &engagement.company);
        Self::settle_bonds(&env, &engagement);

        env.events().publish(
            (
                Symbol::new(&env, "offer_withdrawn"),
                engagement_id,
            ),
            refund,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #260 — BATCH CREATE ENGAGEMENTS
    // ----------------------------------------------------------


    // ----------------------------------------------------------
    // REQUEST REPLACEMENT
    // ----------------------------------------------------------

    /// Company requests a replacement candidate after the placement milestone
    /// was already confirmed. Unconfirmed milestones are reset (Placement →
    /// `Pending`, Retention → `Locked` with its timer restarted).
    ///
    /// A Retention milestone that is currently `Disputed` (a dispute was raised
    /// but arbiters have not yet reached quorum) is included in this reset: it is
    /// forced back to `Locked` and its in-flight vote tally and dispute reason
    /// are cleared, so a future dispute on the same milestone index starts from
    /// a clean vote count instead of inheriting stale votes. See issue #177.
    pub fn request_replacement(env: Env, company: Address, engagement_id: String, reason: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        // Bound reason length so a single engagement cannot accumulate
        // unbounded replacement metadata. See issue #51.
        if reason.len() > MAX_REPLACEMENT_REASON_LEN {
            panic!("replacement reason too long");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if company != engagement.company {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let placement_confirmed = {
            let m0 = engagement.milestones.get(0).unwrap();
            m0.status == MilestoneStatus::Confirmed || m0.status == MilestoneStatus::Resolved
        };

        if !placement_confirmed {
            panic!("placement not yet confirmed — use cancel_engagement instead");
        }

        let replacement_index: u32 = env
            .storage()
            .persistent()
            .get(&DataKey::ReplacementCount(engagement_id.clone()))
            .unwrap_or(0u32);

        if replacement_index >= Self::get_max_replacements_internal(&env) {
            panic!("ReplacementLimitReached");
        }

        let current_ledger = env.ledger().sequence();

        for i in 0..engagement.milestones.len() {
            let mut m = engagement.milestones.get(i).unwrap();
            match m.kind {
                MilestoneKind::Placement => {
                    if m.status == MilestoneStatus::Confirmed
                        || m.status == MilestoneStatus::Resolved
                    {
                        let old_status = m.status.clone();
                        m.status = MilestoneStatus::Pending;
                        m.proof_hash = String::from_str(&env, "");
                        Self::emit_milestone_status_changed(
                            &env,
                            &engagement_id,
                            i,
                            old_status,
                            MilestoneStatus::Pending,
                        );
                        // Clear cooldown so the replacement candidate can submit immediately.
                        env.storage()
                            .persistent()
                            .remove(&DataKey::LastProofAt(engagement_id.clone(), i));
                        // Issue #465: restart the no-show clock for the replacement.
                        Self::mark_milestone_pending_since(&env, &engagement_id, i);
                    }
                }
                MilestoneKind::Retention => {
                    if m.status != MilestoneStatus::Confirmed
                        && m.status != MilestoneStatus::Resolved
                    {
                        // Issue #177: a Retention milestone can be Disputed (dispute
                        // raised, arbiters not yet at quorum) when a replacement is
                        // requested. Resetting it to Locked here must also clear any
                        // in-flight vote tally and dispute reason — otherwise a stale
                        // ArbiterVoteRecord (partial votes, or arbiters who already
                        // "voted") would silently carry over and be read by
                        // `cast_arbiter_vote` the next time this same milestone index
                        // is disputed again, corrupting the new dispute's vote count.
                        let was_disputed = m.status == MilestoneStatus::Disputed;
                        let original_days = (m.valid_after_ledger - engagement.created_at_ledger)
                            / Self::get_ledgers_per_day_internal(&env);
                        m.valid_after_ledger = current_ledger
                            + (original_days * Self::get_ledgers_per_day_internal(&env));
                        let old_status = m.status.clone();
                        m.status = MilestoneStatus::Locked;
                        m.proof_hash = String::from_str(&env, "");
                        if old_status != MilestoneStatus::Locked {
                            Self::emit_milestone_status_changed(
                                &env,
                                &engagement_id,
                                i,
                                old_status,
                                MilestoneStatus::Locked,
                            );
                        }
                        env.storage()
                            .persistent()
                            .remove(&DataKey::LastProofAt(engagement_id.clone(), i));

                        // The retention timer just restarted, so a due-soon
                        // notification emitted against the old deadline must not
                        // suppress one for the new deadline. See issue #241.

                        if was_disputed {
                            env.storage()
                                .persistent()
                                .remove(&DataKey::ArbiterVotes(engagement_id.clone(), i));
                            env.storage()
                                .persistent()
                                .remove(&DataKey2::ArbiterSplitVotes(engagement_id.clone(), i));
                            env.storage()
                                .persistent()
                                .remove(&DataKey::DisputeReason(engagement_id.clone(), i));
                            env.storage()
                                .persistent()
                                .remove(&DataKey::DisputeRaisedAt(engagement_id.clone(), i));
                            env.storage()
                                .persistent()
                                .remove(&DataKey::EscalatedDispute(engagement_id.clone(), i));
                        }
                    }
                }
            }
            engagement.milestones.set(i, m);
        }

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::ReplacementRequested;
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        // Record the reason under a monotonic per-engagement index so the
        // full replacement history is auditable. See issue #51.
        env.storage().persistent().set(
            &DataKey::ReplacementReason(engagement_id.clone(), replacement_index),
            &reason,
        );
        env.storage().persistent().set(
            &DataKey::ReplacementCount(engagement_id.clone()),
            &(replacement_index + 1),
        );
        // Issue #501: who asked and when, for the engagement timeline.
        Self::record_replacement(&env, &engagement_id, replacement_index, &company);

        Self::extend_engagement_ttl(&env, &engagement_id);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
            Some(company.clone()),
        );

        env.events().publish(
            (
                Symbol::new(&env, "replacement_requested"),
                engagement_id.clone(),
            ),
            (replacement_index, reason),
        );
    }

    /// Returns the structured replacement reason recorded for an engagement at
    /// a given replacement index, or `None` if no such replacement exists.
    /// See issue #51.
    pub fn get_replacement_reason(
        env: Env,
        engagement_id: String,
        replacement_index: u32,
    ) -> Option<String> {
        env.storage().persistent().get(&DataKey::ReplacementReason(
            engagement_id,
            replacement_index,
        ))
    }

    /// Returns the number of replacements ever requested for an engagement.
    /// Useful for paging through `get_replacement_reason`. See issue #51.
    pub fn get_replacement_count(env: Env, engagement_id: String) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::ReplacementCount(engagement_id))
            .unwrap_or(0u32)
    }

    // ----------------------------------------------------------
    // ISSUE #31 — REPLACEMENT COUNT LIMIT
    // ----------------------------------------------------------

    /// Admin sets the maximum number of replacements allowed per engagement.
    /// Defaults to 3 when not explicitly configured.
    pub fn set_max_replacements(env: Env, admin: Address, count: u32) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::MaxReplacements), &count);
        env.events()
            .publish((Symbol::new(&env, "max_replacements_set"),), count);
    }

    /// Return the current maximum replacement count cap.
    /// Returns `DEFAULT_MAX_REPLACEMENTS` (3) when not configured.
    pub fn get_max_replacements(env: Env) -> u32 {
        Self::get_max_replacements_internal(&env)
    }

    // ----------------------------------------------------------
    // CANCEL ENGAGEMENT
    // ----------------------------------------------------------

    /// Cancel an engagement and refund any unreleased escrow to the company.
    ///
    /// # Caller
    /// Requires authentication from both `company` and `recruiter` — each
    /// address is validated against `engagement.company` / `engagement.recruiter`
    /// and must call `require_auth`. The two addresses together must agree to
    /// the cancellation; neither party can cancel unilaterally.
    ///
    /// # Precondition
    /// Intended for cancellation **before any milestones have been confirmed**,
    /// the point at which `released_amount == 0` and the full `total_amount`
    /// is still refundable to the company. Once the placement milestone has
    /// already been `Confirmed` / `Resolved`, prefer
    /// [`Self::request_replacement`] instead — `cancel_engagement` is still
    /// callable on a `ReplacementRequested` engagement because
    /// `request_replacement` may have been invoked already, but it will only
    /// refund the unreleased remainder rather than the full fee.
    ///
    /// Strictly enforced: the engagement must be in `Active` or
    /// `ReplacementRequested` status; any other state (`Completed`,
    /// `Cancelled`, `Expired`, `ExitRequested`) is rejected. The contract
    /// must also not be paused, otherwise the call also fails — see
    /// [`Self::assert_not_paused`].
    ///
    /// # Refund behaviour
    /// Transfers `engagement.total_amount - engagement.released_amount` from
    /// the contract's escrow back to `engagement.company` using the
    /// engagement's escrow token.
    ///
    /// Side effects after the refund:
    /// - Engagement status is set to the terminal `Cancelled` state.
    /// - The per-company active engagement counter is decremented.
    ///
    /// # Panics
    /// - `"ContractPaused"` — the contract is paused (raised by
    ///   [`Self::assert_not_paused`] before authentication).
    /// - `"engagement is not active"` — engagement is not in `Active` or
    ///   `ReplacementRequested` status.
    /// - `"unauthorized"` — `company` does not match `engagement.company`
    ///   or `recruiter` does not match `engagement.recruiter`.
    ///
    /// # Events
    /// Emits `("engagement_cancelled", engagement_id)` with the `refund`
    /// amount as the event body, in addition to the usual
    /// `engagement_status_changed` status transition event.
    pub fn cancel_engagement(
        env: Env,
        company: Address,
        recruiter: Address,
        engagement_id: String,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_function_not_paused(&env, FN_CANCEL_ENGAGEMENT);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let refund = engagement.total_amount - engagement.released_amount;
        // Issue #472: pool-funded engagements refund to the pool by default.
        Self::refund_company_escrow(&env, &engagement, &engagement_id, refund);

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Cancelled;
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
            Some(company.clone()),
        );

        Self::decrement_company_active_count(&env, &engagement.company);
        Self::settle_bonds(&env, &engagement);

        env.events().publish(
            (
                Symbol::new(&env, "engagement_cancelled"),
                engagement_id.clone(),
            ),
            refund,
        );
    }

    // ----------------------------------------------------------
    // ISSUE #571 — HARD ENGAGEMENT END DATE
    // ----------------------------------------------------------

    /// Close an engagement past its absolute deadline
    /// (`EngagementConfig::end_ledger`). Permissionless: anyone may call once
    /// `current_ledger >= end_ledger`. `ProofSubmitted` milestones are
    /// auto-confirmed with the same economics as `confirm_milestone`;
    /// confirmed amounts stay paid and everything else unreleased is refunded
    /// to the company, so `released + refunded == total_amount`.
    ///
    /// Idempotent: calling on a terminal engagement is a no-op. Engagements
    /// without an `end_ledger` keep today's behaviour (`"NoEndLedger"`).
    ///
    /// # Panics
    /// - `"NoEndLedger"` — the engagement has no deadline configured.
    /// - `"EndLedgerNotReached"` — the current ledger is before `end_ledger`.
    /// - `"engagement is not active"` — the engagement is not `Active`.
    /// - `"DisputePending"` — a milestone is `Disputed`; resolve it first.
    ///
    /// # Events
    /// Per auto-confirmed milestone `("milestone_confirmed", engagement_id)`
    /// with `(milestone_index, payment)`, plus
    /// `("engagement_closed_at_end", engagement_id)` with
    /// `(released_amount, refund)`.
    pub fn close_engagement_at_end(env: Env, engagement_id: String) {
        Self::assert_engagement_not_paused(&env, &engagement_id);
        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        // Idempotent: an already-closed engagement reports success silently.
        if Self::is_terminal_status(&engagement.status) {
            return;
        }

        let end = engagement
            .end_ledger
            .unwrap_or_else(|| panic!("NoEndLedger"));
        if env.ledger().sequence() < end {
            panic!("EndLedgerNotReached");
        }

        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        for i in 0..engagement.milestones.len() {
            if engagement.milestones.get(i).unwrap().status == MilestoneStatus::Disputed {
                panic!("DisputePending");
            }
        }

        let token_client = token::Client::new(&env, &engagement.token);
        for i in 0..engagement.milestones.len() {
            let m = engagement.milestones.get(i).unwrap();
            if m.status != MilestoneStatus::ProofSubmitted {
                continue;
            }
            let full_share = (engagement.total_amount * m.payment_percent as i128) / 100;
            let payment = full_share - m.replacement_paid_out;
            if payment > 0 {
                let effective_bps = Self::payout_fee_bps(&env, &engagement_id, &engagement);
                let fee_amount =
                    Self::platform_fee_amount(&env, &engagement_id, payment, effective_bps);
                let net_payment = payment - fee_amount;
                engagement.released_amount += payment;
                Self::add_token_locked(&env, &engagement.token, -payment);
                if fee_amount > 0 {
                    Self::collect_platform_fee(&env, &engagement, &engagement_id, i, fee_amount);
                }
                Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client, true);
            }
            let mut updated = m.clone();
            updated.status = MilestoneStatus::Confirmed;
            engagement.milestones.set(i, updated);
            Self::bond_clear_rejection(&env, &engagement_id, i);
            Self::emit_milestone_status_changed(
                &env,
                &engagement_id,
                i,
                MilestoneStatus::ProofSubmitted,
                MilestoneStatus::Confirmed,
            );
            env.events().publish(
                (
                    Symbol::new(&env, "milestone_confirmed"),
                    engagement_id.clone(),
                ),
                (i, payment),
            );
        }

        let refund = engagement.total_amount - engagement.released_amount;
        // Issue #472: pool-funded engagements refund to the pool by default.
        Self::refund_company_escrow(&env, &engagement, &engagement_id, refund);

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Cancelled;
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
            None,
        );

        Self::decrement_company_active_count(&env, &engagement.company);
        Self::settle_bonds(&env, &engagement);

        env.events().publish(
            (
                Symbol::new(&env, "engagement_closed_at_end"),
                engagement_id.clone(),
            ),
            (engagement.released_amount, refund),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #576 — NEGOTIATED MUTUAL CANCELLATION
    // ----------------------------------------------------------

    /// Either side proposes cancelling the engagement with
    /// `recruiter_bps_of_unreleased` basis points of the unreleased escrow
    /// paid to the recruiter as compensation for partial work; the other side
    /// accepts with [`Self::accept_cancellation`]. A new proposal replaces any
    /// pending one. The proposal expires after the amendment TTL
    /// (`get_amendment_ttl`).
    ///
    /// `cancel_engagement` is unchanged and still refunds everything.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"InvalidCancellationBps"` — `recruiter_bps_of_unreleased > 10_000`
    /// - `"engagement is not active"` — not `Active` / `ReplacementRequested`
    /// - `"unauthorized"` — caller is neither the company side nor the recruiter side
    /// - `"MilestoneDisputed"` — a milestone is currently `Disputed`
    ///
    /// # Events
    /// Emits `("cancellation_proposed", engagement_id)` with
    /// `(proposer, recruiter_bps_of_unreleased, expires_at_ledger)`.
    pub fn propose_cancellation(
        env: Env,
        proposer: Address,
        engagement_id: String,
        recruiter_bps_of_unreleased: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        proposer.require_auth();

        if recruiter_bps_of_unreleased > FULL_SPLIT_BPS {
            panic!("InvalidCancellationBps");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        Self::assert_negotiated_cancellation_allowed(&engagement);

        let proposed_by_company =
            if Self::is_authorized_company(&env, &proposer, &engagement.company) {
                true
            } else if Self::is_authorized_recruiter(&env, &proposer, &engagement.recruiter) {
                false
            } else {
                panic!("{}", ERR_UNAUTHORIZED);
            };

        let ttl = Self::get_amendment_ttl(env.clone());
        let now = env.ledger().sequence();
        let proposal = CancellationProposal {
            proposer: proposer.clone(),
            proposed_by_company,
            recruiter_bps: recruiter_bps_of_unreleased,
            proposed_at_ledger: now,
            expires_at_ledger: now.saturating_add(ttl),
        };
        let key = DataKey3::CancellationProposal(engagement_id.clone());
        env.storage().persistent().set(&key, &proposal);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "cancellation_proposed"),
                engagement_id,
            ),
            (proposer, recruiter_bps_of_unreleased, proposal.expires_at_ledger),
        );
    }

    /// The counterparty of a pending [`Self::propose_cancellation`] accepts it.
    ///
    /// `recruiter_bps` of the unreleased escrow (excluding any share withheld
    /// by a split-vote dispute resolution, which always goes back to the
    /// company) is released to the recruiter side through the regular fee
    /// pipeline — platform fee, then the co-recruiter split and payout
    /// preferences. Everything else is refunded to the company (to its pool
    /// for pool-funded engagements) and the engagement becomes `Cancelled`.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"no pending cancellation proposal"`
    /// - `"cancellation_expired"` — the proposal outlived the amendment TTL
    ///   (it is discarded)
    /// - `"engagement is not active"` / `"MilestoneDisputed"`
    /// - `"unauthorized"` — caller is not on the other side from the proposer
    ///
    /// # Events
    /// Emits `engagement_status_changed`, then
    /// `("cancellation_accepted", engagement_id)` with
    /// `(acceptor, recruiter_bps, recruiter_amount, refund)` and
    /// `("engagement_cancelled", engagement_id)` with `refund`.
    pub fn accept_cancellation(env: Env, acceptor: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        acceptor.require_auth();

        let key = DataKey3::CancellationProposal(engagement_id.clone());
        let proposal: CancellationProposal = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending cancellation proposal"));

        if env.ledger().sequence() > proposal.expires_at_ledger {
            env.storage().persistent().remove(&key);
            panic!("cancellation_expired");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        Self::assert_negotiated_cancellation_allowed(&engagement);

        let is_counterparty = if proposal.proposed_by_company {
            Self::is_authorized_recruiter(&env, &acceptor, &engagement.recruiter)
        } else {
            Self::is_authorized_company(&env, &acceptor, &engagement.company)
        };
        if !is_counterparty {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let unreleased = engagement.total_amount - engagement.released_amount;
        // Issue #462: a split-vote resolution's withheld share was already
        // decided in the company's favour, so it is not up for negotiation.
        let withheld_key = DataKey2::SplitWithheld(engagement_id.clone());
        let withheld: i128 = env
            .storage()
            .persistent()
            .get::<DataKey2, i128>(&withheld_key)
            .unwrap_or(0)
            .max(0)
            .min(unreleased.max(0));
        env.storage().persistent().remove(&withheld_key);

        let recruiter_amount =
            ((unreleased - withheld) * proposal.recruiter_bps as i128) / (FULL_SPLIT_BPS as i128);
        if recruiter_amount > 0 {
            let effective_bps = Self::payout_fee_bps(&env, &engagement_id, &engagement);
            let fee_amount =
                Self::platform_fee_amount(&env, &engagement_id, recruiter_amount, effective_bps);
            let net_payment = recruiter_amount - fee_amount;
            engagement.released_amount += recruiter_amount;

            let token_client = token::Client::new(&env, &engagement.token);
            if fee_amount > 0 {
                Self::collect_platform_fee(
                    &env,
                    &engagement,
                    &engagement_id,
                    CANCELLATION_FEE_MILESTONE_INDEX,
                    fee_amount,
                );
            }
            Self::distribute_recruiter_payout(&env, &engagement, net_payment, &token_client, true);
        }

        let refund = unreleased - recruiter_amount;
        // Issue #472: pool-funded engagements refund to the pool by default.
        Self::refund_company_escrow(&env, &engagement, &engagement_id, refund);

        let old_engagement_status = engagement.status.clone();
        engagement.status = EngagementStatus::Cancelled;
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);
        env.storage().persistent().remove(&key);
        Self::emit_engagement_status_changed(
            &env,
            &engagement_id,
            old_engagement_status,
            engagement.status.clone(),
            Some(acceptor.clone()),
        );

        Self::decrement_company_active_count(&env, &engagement.company);
        Self::settle_bonds(&env, &engagement);

        env.events().publish(
            (
                Symbol::new(&env, "cancellation_accepted"),
                engagement_id.clone(),
            ),
            (acceptor, proposal.recruiter_bps, recruiter_amount, refund),
        );
        env.events().publish(
            (
                Symbol::new(&env, "engagement_cancelled"),
                engagement_id.clone(),
            ),
            refund,
        );
    }

    /// Return the pending negotiated cancellation, or `None` if absent/expired.
    pub fn get_pending_cancellation(
        env: Env,
        engagement_id: String,
    ) -> Option<CancellationProposal> {
        let proposal: CancellationProposal = env
            .storage()
            .persistent()
            .get(&DataKey3::CancellationProposal(engagement_id))?;
        if env.ledger().sequence() > proposal.expires_at_ledger {
            return None;
        }
        Some(proposal)
    }

    /// Panic unless a negotiated cancellation may be proposed or accepted:
    /// the engagement must be `Active` or `ReplacementRequested` (as for
    /// `cancel_engagement`) and no milestone may be `Disputed`.
    fn assert_negotiated_cancellation_allowed(engagement: &Engagement) {
        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        for i in 0..engagement.milestones.len() {
            if engagement.milestones.get(i).unwrap().status == MilestoneStatus::Disputed {
                panic!("MilestoneDisputed");
            }
        }
    }

    // ----------------------------------------------------------
    // ISSUE #33 — ESCROW TOP-UP
    // ----------------------------------------------------------

    /// Company tops up the escrow balance for an active engagement.
    pub fn top_up_escrow(env: Env, company: Address, engagement_id: String, amount: i128) {
        Self::apply_escrow_top_up(&env, &company, &engagement_id, amount, false);
    }

    /// Issue #545: company tops up the escrow balance for an active engagement
    /// by debiting its pooled balance for the engagement's token
    /// (`get_company_balance(company, engagement.token)`) instead of
    /// performing a fresh token transfer.
    ///
    /// # Panics
    /// Same as [`Self::top_up_escrow`], plus `"InsufficientCompanyBalance"`
    /// (without mutating state) if the pool cannot cover `amount`.
    pub fn top_up_escrow_from_pool(
        env: Env,
        company: Address,
        engagement_id: String,
        amount: i128,
    ) {
        Self::apply_escrow_top_up(&env, &company, &engagement_id, amount, true);
    }

    /// Shared body of `top_up_escrow` / `top_up_escrow_from_pool`: validates the
    /// caller and engagement, funds `amount` either from a token transfer or
    /// the company pool, and grows `total_amount` by exactly `amount`.
    pub(crate) fn apply_escrow_top_up(
        env: &Env,
        company: &Address,
        engagement_id: &String,
        amount: i128,
        from_pool: bool,
    ) {
        Self::assert_not_paused(env);
        Self::assert_function_not_paused(env, FN_TOP_UP_ESCROW);
        Self::assert_engagement_not_paused(env, engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(env, engagement_id);

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        if !Self::is_authorized_company(env, company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if amount <= 0 {
            panic!("amount must be greater than zero");
        }

        if from_pool {
            // Panics before any write if the pooled balance is insufficient.
            Self::debit_company_pool(env, company, &engagement.token, amount);
        } else {
            let token_client = token::Client::new(env, &engagement.token);
            token_client.transfer(company, &env.current_contract_address(), &amount);
        }

        engagement.total_amount += amount;
        // Issue #578: top-up locks more escrow.
        Self::add_token_locked(&env, &engagement.token, amount);
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(env, engagement_id);

        env.events().publish(
            (Symbol::new(env, "escrow_topped_up"), engagement_id.clone()),
            (amount, engagement.total_amount, from_pool),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #32 — RECRUITER EARLY-EXIT
    // ----------------------------------------------------------




    // ----------------------------------------------------------
    // ENGAGEMENT VISIBILITY (issue #364)
    // ----------------------------------------------------------

    // ----------------------------------------------------------
    // ISSUE #471 — CO-RECRUITER SPLIT AMENDMENT
    // ----------------------------------------------------------

    /// Propose a change to `engagement.recruiter_split_bps`. Caller must be the
    /// primary recruiter or the co-recruiter. The other of the two must accept.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"engagement is not active"`
    /// - `"no co_recruiter"` — engagement has no co-recruiter
    /// - `"InvalidSplitBps"` — `new_split_bps > 10_000`
    /// - `"unauthorized"` — caller is neither recruiter nor co-recruiter
    /// - `"AmendmentCooldownActive"` — proposer is in cooldown after a recent rejection (issue #496)
    pub fn propose_split_amendment(
        env: Env,
        proposer: Address,
        engagement_id: String,
        new_split_bps: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        proposer.require_auth();

        if new_split_bps > FULL_SPLIT_BPS {
            panic!("InvalidSplitBps");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        let co = engagement
            .co_recruiter
            .clone()
            .unwrap_or_else(|| panic!("no co_recruiter"));

        let proposed_by_recruiter = if proposer == engagement.recruiter {
            true
        } else if proposer == co {
            false
        } else {
            panic!("{}", ERR_UNAUTHORIZED);
        };

        // Issue #496: Check amendment reproposal cooldown
        let cooldown = Self::get_amendment_reprop_cooldown(env.clone());
        if cooldown > 0 {
            let rejection_key = DataKey2::SplitAmendmentRejectedAt(engagement_id.clone(), proposer.clone());
            if let Some(rejected_at) = env.storage().persistent().get::<_, u32>(&rejection_key) {
                let current_ledger = env.ledger().sequence();
                if current_ledger < rejected_at.saturating_add(cooldown) {
                    panic!("AmendmentCooldownActive");
                }
            }
        }

        let ttl = Self::get_amendment_ttl(env.clone());
        let now = env.ledger().sequence();
        let proposal = SplitAmendmentProposal {
            proposer: proposer.clone(),
            proposed_by_recruiter,
            new_split_bps,
            proposed_at_ledger: now,
            expires_at_ledger: now.saturating_add(ttl),
        };
        let key = DataKey2::SplitAmendmentProposal(engagement_id.clone());
        env.storage().persistent().set(&key, &proposal);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "split_amendment_proposed"),
                engagement_id,
            ),
            (proposer, new_split_bps, proposal.expires_at_ledger),
        );
    }

    /// Counterparty accepts a pending split amendment. Updates
    /// `recruiter_split_bps` for all future milestone payouts.
    pub fn accept_split_amendment(env: Env, acceptor: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        acceptor.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let key = DataKey2::SplitAmendmentProposal(engagement_id.clone());
        let proposal: SplitAmendmentProposal = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending amendment proposal"));

        if env.ledger().sequence() > proposal.expires_at_ledger {
            env.storage().persistent().remove(&key);
            env.events().publish(
                (
                    Symbol::new(&env, "split_amendment_rejected"),
                    engagement_id.clone(),
                ),
                (acceptor.clone(), String::from_str(&env, "expired")),
            );
            panic!("amendment_expired");
        }

        Self::assert_split_amendment_counterparty(&engagement, &acceptor, &proposal);

        let old = engagement.recruiter_split_bps;
        engagement.recruiter_split_bps = proposal.new_split_bps;
        engagement.last_activity_ledger = env.ledger().sequence();
        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        let entry = SplitAmendmentEntry {
            proposer: proposal.proposer.clone(),
            old_split_bps: old,
            new_split_bps: proposal.new_split_bps,
            ledger: env.ledger().sequence(),
        };
        Self::append_split_amendment_log(&env, &engagement_id, entry);
        env.storage().persistent().remove(&key);

        env.events().publish(
            (
                Symbol::new(&env, "split_amendment_accepted"),
                engagement_id,
            ),
            (acceptor, old, proposal.new_split_bps),
        );
    }

    /// Counterparty rejects a pending split amendment without changing the split.
    /// Records the rejection time for cooldown tracking (issue #496).
    pub fn reject_split_amendment(env: Env, rejector: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        rejector.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let key = DataKey2::SplitAmendmentProposal(engagement_id.clone());
        let proposal: SplitAmendmentProposal = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| panic!("no pending amendment proposal"));

        if env.ledger().sequence() > proposal.expires_at_ledger {
            env.storage().persistent().remove(&key);
            panic!("amendment_expired");
        }

        Self::assert_split_amendment_counterparty(&engagement, &rejector, &proposal);
        env.storage().persistent().remove(&key);

        // Issue #496: Record the rejection ledger for the proposer on this engagement
        let rejection_key = DataKey2::SplitAmendmentRejectedAt(engagement_id.clone(), proposal.proposer.clone());
        env.storage()
            .persistent()
            .set(&rejection_key, &env.ledger().sequence());
        env.storage()
            .persistent()
            .extend_ttl(&rejection_key, 100_000, 6_300_000);

        env.events().publish(
            (
                Symbol::new(&env, "split_amendment_rejected"),
                engagement_id,
            ),
            (rejector, String::from_str(&env, "declined")),
        );
    }

    /// Return the pending split amendment, or `None` if absent/expired.
    pub fn get_pending_split_amendment(
        env: Env,
        engagement_id: String,
    ) -> Option<SplitAmendmentProposal> {
        let proposal: SplitAmendmentProposal = env
            .storage()
            .persistent()
            .get(&DataKey2::SplitAmendmentProposal(engagement_id))?;
        if env.ledger().sequence() > proposal.expires_at_ledger {
            return None;
        }
        Some(proposal)
    }

    /// Return the accepted split-amendment history for an engagement (oldest first).
    pub fn get_split_amendment_log(
        env: Env,
        engagement_id: String,
    ) -> Vec<SplitAmendmentEntry> {
        env.storage()
            .persistent()
            .get(&DataKey2::SplitAmendmentLog(engagement_id))
            .unwrap_or_else(|| Vec::new(&env))
    }

    fn assert_split_amendment_counterparty(
        engagement: &Engagement,
        caller: &Address,
        proposal: &SplitAmendmentProposal,
    ) {
        let co = engagement
            .co_recruiter
            .as_ref()
            .unwrap_or_else(|| panic!("no co_recruiter"));
        let is_ok = if proposal.proposed_by_recruiter {
            caller == co
        } else {
            caller == &engagement.recruiter
        };
        if !is_ok {
            panic!("{}", ERR_UNAUTHORIZED);
        }
    }

    fn append_split_amendment_log(env: &Env, engagement_id: &String, entry: SplitAmendmentEntry) {
        let key = DataKey2::SplitAmendmentLog(engagement_id.clone());
        let mut log: Vec<SplitAmendmentEntry> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        if log.len() >= MAX_AMENDMENT_LOG_ENTRIES {
            log.remove(0);
        }
        log.push_back(entry);
        env.storage().persistent().set(&key, &log);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

}
