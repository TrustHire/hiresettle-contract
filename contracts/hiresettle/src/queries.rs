use soroban_sdk::{contractimpl, Address, Env, Map, String, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // READ-ONLY QUERIES
    // ----------------------------------------------------------

    /// Get the deployed contract version string (issue #16).
    /// No authentication required.
    pub fn get_version(env: Env) -> String {
        env.storage()
            .persistent()
            .get(&DataKey::Version)
            .unwrap_or_else(|| String::from_str(&env, DEFAULT_VERSION))
    }

    /// Get the current minimum engagement amount in stroops (issue #17).
    /// No authentication required.
    pub fn get_min_amount(env: Env) -> i128 {
        env.storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::MinEngagementAmount))
            .unwrap_or(DEFAULT_MIN_ENGAGEMENT_AMOUNT)
    }

    /// Return `token`'s minimum-amount override, if the admin has set one
    /// (issue #366). `None` means `token` uses the admin-wide
    /// `MinEngagementAmount` instead — see `get_effective_min_amount` for the
    /// resolved value `create_engagement` actually enforces.
    pub fn get_token_min_amount(env: Env, token: Address) -> Option<i128> {
        let overrides: Map<Address, i128> = env
            .storage()
            .persistent()
            .get(&DataKey::Config(ConfigKey::TokenMinAmounts))
            .unwrap_or_else(|| Map::new(&env));
        overrides.get(token)
    }

    /// Return the minimum engagement amount actually enforced for `token`
    /// (issue #366): its per-token override if one is set, otherwise the
    /// admin-wide `MinEngagementAmount`. This is what `create_engagement`
    /// validates `total_amount` against.
    pub fn get_effective_min_amount(env: Env, token: Address) -> i128 {
        Self::get_token_min_amount(env.clone(), token).unwrap_or(Self::get_min_amount(env))
    }

    /// Returns the full engagement record for a given engagement ID.
    ///
    /// # Returns
    /// The complete [`Engagement`] struct containing all engagement details.
    ///
    /// # Panics
    /// Panics with `"engagement not found"` if no engagement exists with the given `engagement_id`.
    pub fn get_engagement(env: Env, engagement_id: String) -> Engagement {
        Self::get_engagement_internal(&env, &engagement_id)
    }

    /// Returns a specific milestone from an engagement.
    ///
    /// # Returns
    /// The [`Milestone`] struct at the given index within the engagement's milestone list.
    ///
    /// # Panics
    /// Panics with `"engagement not found"` if no engagement exists with the given `engagement_id`.
    /// Panics with `"invalid milestone index"` if `milestone_index` is out of bounds for the engagement's milestones.
    pub fn get_milestone(env: Env, engagement_id: String, milestone_index: u32) -> Milestone {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        Self::get_milestone_or_panic(&engagement, milestone_index)
    }

    /// Returns the status of every milestone in the engagement, ordered by
    /// milestone index, in a single call (issue #37). Read-only, permissionless.
    pub fn get_all_milestone_statuses(env: Env, engagement_id: String) -> Vec<MilestoneStatus> {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let mut statuses = Vec::new(&env);
        for i in 0..engagement.milestones.len() {
            statuses.push_back(engagement.milestones.get(i).unwrap().status);
        }
        statuses
    }

    /// Returns the current escrow balance for an engagement.
    ///
    /// # Returns
    /// The remaining escrow balance in the token's smallest unit (e.g., stroops).
    /// Calculated as `total_amount - released_amount`.
    ///
    /// # Panics
    /// Panics with `"engagement not found"` if no engagement exists with the given `engagement_id`.
    pub fn get_escrow_balance(env: Env, engagement_id: String) -> i128 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.total_amount - engagement.released_amount
    }

    /// Returns `true` if the milestone is `Locked` and the current ledger
    /// sequence is greater than or equal to its `valid_after_ledger`, meaning
    /// it can currently be unlocked via `unlock_milestone`.
    pub fn is_milestone_unlockable(env: Env, engagement_id: String, milestone_index: u32) -> bool {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        milestone.status == MilestoneStatus::Locked
            && env.ledger().sequence() >= milestone.valid_after_ledger
    }

    /// Returns the number of ledgers remaining until the milestone becomes
    /// unlockable, or `0` if it is already unlockable.
    ///
    /// When the result is `0`, `unlock_milestone` can be called immediately.
    /// Otherwise, the caller must wait at least this many more ledgers before
    /// `env.ledger().sequence() >= milestone.valid_after_ledger` holds.
    pub fn ledgers_until_unlock(env: Env, engagement_id: String, milestone_index: u32) -> u32 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        let current = env.ledger().sequence();
        milestone.valid_after_ledger.saturating_sub(current)
    }

    /// Returns approximate seconds until a Locked retention milestone unlocks.
    /// Returns 0 if the milestone is already unlockable or is a Placement milestone.
    pub fn get_estimated_unlock_seconds(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> u64 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);

        if milestone.kind == MilestoneKind::Placement {
            return 0;
        }

        let current = env.ledger().sequence();
        if current >= milestone.valid_after_ledger {
            return 0;
        }

        let ledgers_remaining = (milestone.valid_after_ledger - current) as u64;
        // seconds_per_ledger = 86_400 / ledgers_per_day
        let lpd = Self::get_ledgers_per_day_internal(&env) as u64;
        ledgers_remaining * (86_400u64 / lpd)
    }

    /// Return the IPFS CID stored at engagement creation, or None if not provided.
    pub fn get_metadata_hash(env: Env, engagement_id: String) -> Option<String> {
        Self::get_engagement_internal(&env, &engagement_id).metadata_hash
    }

    /// Return the current approve/reject vote counts for a disputed milestone.
    /// Returns (0, 0) if no votes have been cast yet.
    pub fn get_arbiter_votes(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> ArbiterVoteCounts {
        let vote_key = DataKey::ArbiterVotes(engagement_id, milestone_index);
        let record: ArbiterVoteRecord = env
            .storage()
            .persistent()
            .get(&vote_key)
            .unwrap_or_else(|| Self::empty_vote_record(&env));
        ArbiterVoteCounts {
            approve_votes: record.approve_votes,
            reject_votes: record.reject_votes,
        }
    }

    /// Return the weighted approve/reject tally for a disputed milestone on an
    /// engagement configured with `arbiter_weights` (issue #460), or `None`
    /// for an unweighted engagement (use `get_arbiter_votes` there). Weights
    /// are (0, 0) if no votes have been cast yet.
    pub fn get_arbiter_vote_weights(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<ArbiterVoteWeights> {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.arbiter_weights.as_ref()?;
        let record: ArbiterVoteRecord = env
            .storage()
            .persistent()
            .get(&DataKey::ArbiterVotes(engagement_id, milestone_index))
            .unwrap_or_else(|| Self::empty_vote_record(&env));
        Some(ArbiterVoteWeights {
            approve_weight: record.approve_weight,
            reject_weight: record.reject_weight,
            total_weight: Self::total_arbiter_weight(&engagement),
            quorum: engagement.quorum,
        })
    }

    /// Total amount released for this engagement, represented by
    /// `Engagement.released_amount`.
    ///
    /// This is not the escrow balance. To get remaining contract funds,
    /// use `total_amount - released_amount`; `get_escrow_balance` provides
    /// that derived value.
    pub fn get_total_released(env: Env, engagement_id: String) -> i128 {
        Self::get_engagement_internal(&env, &engagement_id).released_amount
    }

    /// Return a lightweight summary of an engagement, suitable for list/dashboard views.
    ///
    /// Prefer this over `get_engagement` when you only need top-level fields and not
    /// the full milestone list, as it avoids deserializing the milestone vector.
    ///
    /// # Panics
    ///
    /// - `"EngagementNotFound"` — no engagement with the given `engagement_id` exists.
    pub fn get_engagement_summary(env: Env, engagement_id: String) -> EngagementSummary {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        EngagementSummary {
            id: engagement.id,
            job_title: engagement.job_title,
            company: engagement.company,
            recruiter: engagement.recruiter,
            total_amount: engagement.total_amount,
            released_amount: engagement.released_amount,
            status: engagement.status,
            milestone_count: engagement.milestones.len(),
            created_at_ledger: engagement.created_at_ledger,
            co_recruiter: engagement.co_recruiter,
            recruiter_split_bps: engagement.recruiter_split_bps,
            contract_pdf_hash: engagement.contract_pdf_hash,
            referrer: engagement.referrer,
            tags: engagement.tags,
        }
    }

    /// Return lightweight summaries for multiple engagements in a single call,
    /// reducing round-trips for dashboards that need to render many engagements
    /// at once.
    ///
    /// Engagement IDs that do not exist are silently skipped — the returned
    /// vector may be shorter than the input list when some IDs are invalid.
    ///
    /// # Panics
    ///
    /// - `"too many IDs"` — `engagement_ids` contains more than 20 entries.
    pub fn batch_get_engagement_summary(
        env: Env,
        engagement_ids: Vec<String>,
    ) -> Vec<EngagementSummary> {
        if engagement_ids.len() > 20 {
            panic!("too many IDs");
        }
        let mut results: Vec<EngagementSummary> = Vec::new(&env);
        for i in 0..engagement_ids.len() {
            let eid = engagement_ids.get(i).unwrap();
            let maybe: Option<Engagement> = env
                .storage()
                .persistent()
                .get(&DataKey::Engagement(eid.clone()));
            if let Some(engagement) = maybe {
                results.push_back(EngagementSummary {
                    id: engagement.id,
                    job_title: engagement.job_title,
                    company: engagement.company,
                    recruiter: engagement.recruiter,
                    total_amount: engagement.total_amount,
                    released_amount: engagement.released_amount,
                    status: engagement.status,
                    milestone_count: engagement.milestones.len(),
                    created_at_ledger: engagement.created_at_ledger,
                    co_recruiter: engagement.co_recruiter,
                    recruiter_split_bps: engagement.recruiter_split_bps,
                    contract_pdf_hash: engagement.contract_pdf_hash,
                    referrer: engagement.referrer,
                    tags: engagement.tags,
                });
            }
        }
        results
    }

    /// Return the off-chain attestation hash (e.g. SHA-256 of the contract PDF)
    /// stored at engagement creation, or None if not provided.
    /// Read-only and permissionless.
    pub fn get_contract_pdf_hash(env: Env, engagement_id: String) -> Option<String> {
        Self::get_engagement_internal(&env, &engagement_id).contract_pdf_hash
    }

    /// Return the off-chain categorization tags stored at engagement creation
    /// (issue #248). Empty if none were provided. Read-only and permissionless.
    pub fn get_tags(env: Env, engagement_id: String) -> Vec<String> {
        Self::get_engagement_internal(&env, &engagement_id)
            .tags
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ----------------------------------------------------------
    // ISSUE #34 — ENGAGEMENT COUNT
    // ----------------------------------------------------------

    /// Return the total number of engagements ever created.
    pub fn get_engagement_count(env: Env) -> u64 {
        env.storage()
            .instance()
            .get(&DataKey::EngagementCount)
            .unwrap_or(0u64)
    }

    // ----------------------------------------------------------
    // ISSUE #35 — ENGAGEMENT LIST BY COMPANY
    // ----------------------------------------------------------

    /// Return a paginated slice of engagement IDs for a given company.
    /// `page` is 0-indexed; out-of-range pages return an empty vec.
    ///
    /// # Examples
    ///
    /// First page of 10 engagement IDs for a company:
    ///
    /// ```text
    /// get_engagements_by_company(env, company, 0, 10)
    /// ```
    ///
    /// Second page (IDs 10–19) with the same page size:
    ///
    /// ```text
    /// get_engagements_by_company(env, company, 1, 10)
    /// ```
    ///
    /// A `page_size` of `0` or a `page` past the end of the list returns an empty vec.
    pub fn get_engagements_by_company(
        env: Env,
        company: Address,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyEngagements(company))
            .unwrap_or_else(|| Vec::new(&env));

        let total = ids.len();
        if page_size == 0 {
            return Vec::new(&env);
        }
        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps to an out-of-range start (caught below) instead of wrapping
        // around via u32 overflow.
        let start = page.saturating_mul(page_size);
        if start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(ids.get(i).unwrap());
        }
        result
    }
    /// Return the total number of engagements associated with a company.
    pub fn get_company_engagement_count(env: Env, company: Address) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyEngagements(company))
            .unwrap_or_else(|| Vec::new(&env));
        ids.len()
    }

    // ----------------------------------------------------------
    // ISSUE #36 — ENGAGEMENT LIST BY RECRUITER
    // ----------------------------------------------------------

    /// Return a paginated slice of engagement IDs for a given recruiter.
    /// `page` is 0-indexed; out-of-range pages return an empty vec.
    pub fn get_engagements_by_recruiter(
        env: Env,
        recruiter: Address,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterEngagements(recruiter))
            .unwrap_or_else(|| Vec::new(&env));

        let total = ids.len();
        if page_size == 0 {
            return Vec::new(&env);
        }
        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps to an out-of-range start (caught below) instead of wrapping
        // around via u32 overflow.
        let start = page.saturating_mul(page_size);
        if start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(ids.get(i).unwrap());
        }
        result
    }

    /// Return the total number of engagements associated with a recruiter.
    pub fn get_recruiter_engagement_count(env: Env, recruiter: Address) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterEngagements(recruiter))
            .unwrap_or_else(|| Vec::new(&env));
        ids.len()
    }


    // ----------------------------------------------------------
    // ISSUE #249 — ENGAGEMENT TAGS
    // ----------------------------------------------------------





    /// Return a paginated slice of engagement IDs currently in a given status
    /// (issue #237). `page` is 0-indexed; out-of-range pages return an empty vec.
    pub fn get_engagement_ids_by_status(
        env: Env,
        status: EngagementStatus,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let mut result = Vec::new(&env);
        if page_size == 0 {
            return result;
        }

        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps instead of wrapping around via u32 overflow.
        let start = page.saturating_mul(page_size);
        let end = start.saturating_add(page_size);

        // Walk the index counting matches, collecting only those whose position
        // within the filtered sequence falls inside the requested page.
        let mut matched: u32 = 0;
        for i in 0..ids.len() {
            if matched >= end {
                break;
            }
            let id = ids.get(i).unwrap();
            let engagement: Engagement = match env
                .storage()
                .persistent()
                .get(&DataKey::Engagement(id.clone()))
            {
                Some(e) => e,
                // An entry whose record has since expired from storage is
                // skipped rather than treated as a match.
                None => continue,
            };
            if engagement.status == status {
                if matched >= start {
                    result.push_back(id);
                }
                matched += 1;
            }
        }

        result
    }

    /// Return the total number of engagements currently in a given status
    /// (issue #237). Companion to `get_engagement_ids_by_status` for sizing
    /// pagination, mirroring `get_company_engagement_count`.
    ///
    /// Carries the same scan cost and index-coverage caveats as
    /// `get_engagement_ids_by_status`.
    pub fn get_engagement_count_by_status(env: Env, status: EngagementStatus) -> u32 {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        let mut count: u32 = 0;
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            if let Some(engagement) = env
                .storage()
                .persistent()
                .get::<DataKey, Engagement>(&DataKey::Engagement(id))
            {
                if engagement.status == status {
                    count += 1;
                }
            }
        }
        count
    }

    // ----------------------------------------------------------
    // ISSUE #365 — PUBLIC ENGAGEMENT LIST
    // ----------------------------------------------------------

    /// Return a paginated slice of IDs for engagements marked public at
    /// creation time (issue #365), i.e. `EngagementConfig::is_public == true`.
    /// `page` is 0-indexed; out-of-range pages return an empty vec. Carries
    /// the same scan cost and index-coverage caveats as
    /// `get_engagement_ids_by_status`.
    pub fn get_public_engagement_ids(env: Env, page: u32, page_size: u32) -> Vec<String> {
        let mut result = Vec::new(&env);
        if page_size == 0 {
            return result;
        }

        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        let start = page.saturating_mul(page_size);
        let end = start.saturating_add(page_size);

        let mut matched: u32 = 0;
        for i in 0..ids.len() {
            if matched >= end {
                break;
            }
            let id = ids.get(i).unwrap();
            let engagement: Engagement = match env
                .storage()
                .persistent()
                .get(&DataKey::Engagement(id.clone()))
            {
                Some(e) => e,
                None => continue,
            };
            if engagement.is_public {
                if matched >= start {
                    result.push_back(id);
                }
                matched += 1;
            }
        }

        result
    }

    // ----------------------------------------------------------
    // ISSUE #351 — ENGAGEMENT LIST BY AMOUNT RANGE
    // ----------------------------------------------------------

    /// Return a paginated slice of engagement IDs whose `total_amount` falls
    /// within `[min_amount, max_amount]` (inclusive on both ends). `page` is
    /// 0-indexed; out-of-range pages return an empty vec.
    ///
    /// Carries the same linear-scan cost and index-coverage caveats as
    /// `get_engagement_ids_by_status` — an entry whose record has expired
    /// from storage is skipped rather than treated as a match.
    ///
    /// # Panics
    /// - `"InvalidAmountRange"` — `min_amount > max_amount`.
    pub fn get_engagements_by_amount_range(
        env: Env,
        min_amount: i128,
        max_amount: i128,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let mut result = Vec::new(&env);
        if page_size == 0 {
            return result;
        }
        if min_amount > max_amount {
            panic!("InvalidAmountRange");
        }

        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        // Use saturating arithmetic so a huge `page` / `page_size` combination
        // clamps instead of wrapping around via u32 overflow.
        let start = page.saturating_mul(page_size);
        let end = start.saturating_add(page_size);

        // Walk the index counting matches, collecting only those whose position
        // within the filtered sequence falls inside the requested page.
        let mut matched: u32 = 0;
        for i in 0..ids.len() {
            if matched >= end {
                break;
            }
            let id = ids.get(i).unwrap();
            let engagement: Engagement = match env
                .storage()
                .persistent()
                .get(&DataKey::Engagement(id.clone()))
            {
                Some(e) => e,
                None => continue,
            };
            if engagement.total_amount >= min_amount && engagement.total_amount <= max_amount {
                if matched >= start {
                    result.push_back(id);
                }
                matched += 1;
            }
        }

        result
    }

    /// Return the total number of engagements whose `total_amount` falls
    /// within `[min_amount, max_amount]` (inclusive). Companion to
    /// `get_engagements_by_amount_range` for sizing pagination.
    ///
    /// # Panics
    /// - `"InvalidAmountRange"` — `min_amount > max_amount`.
    pub fn get_engagement_count_by_amount(env: Env, min_amount: i128, max_amount: i128) -> u32 {
        if min_amount > max_amount {
            panic!("InvalidAmountRange");
        }

        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey::AllEngagements)
            .unwrap_or_else(|| Vec::new(&env));

        let mut count: u32 = 0;
        for i in 0..ids.len() {
            let id = ids.get(i).unwrap();
            if let Some(engagement) = env
                .storage()
                .persistent()
                .get::<DataKey, Engagement>(&DataKey::Engagement(id))
            {
                if engagement.total_amount >= min_amount && engagement.total_amount <= max_amount {
                    count += 1;
                }
            }
        }
        count
    }

    // ----------------------------------------------------------
    // ISSUE #249 — ENGAGEMENT LIST BY TAG
    // ----------------------------------------------------------



    // ----------------------------------------------------------
    // ISSUE #55 — ENGAGEMENT COMPLETION QUERY
    // ----------------------------------------------------------


    /// Return the fraction of milestones that are unlocked (not in Locked status).
    /// Returns `(unlocked_count, total_count)`.
    /// - `unlocked_count` = number of milestones with status != Locked
    /// - `total_count` = total number of milestones in the engagement
    ///
    /// Read-only and permissionless.
    pub fn get_unlock_progress(env: Env, engagement_id: String) -> (u32, u32) {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let total = engagement.milestones.len();
        let mut unlocked: u32 = 0;
        for i in 0..total {
            let m = engagement.milestones.get(i).unwrap();
            if m.status != MilestoneStatus::Locked {
                unlocked += 1;
            }
        }
        (unlocked, total)
    }
}
