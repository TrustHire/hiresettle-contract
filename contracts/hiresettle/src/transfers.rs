use soroban_sdk::{contractimpl, Address, Env, String, Symbol};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #43 — COMPANY TRANSFER

    // ----------------------------------------------------------
    // ISSUE #254 — COMPANY MULTI-SIGNER SUPPORT
    // ----------------------------------------------------------

    /// Register a co-signer address that is also authorized to perform
    /// company-gated actions (confirm, dispute, cancel, etc.) on behalf
    /// of this company. Only the company address itself can set the cosigner.
    pub fn set_company_cosigner(env: Env, company: Address, cosigner: Address) {
        company.require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::CompanyCosigner(company.clone()), &cosigner);
        env.events().publish(
            (Symbol::new(&env, "company_cosigner_set"),),
            (company, cosigner),
        );
    }

    /// Return the registered co-signer for a company, or `None` if none set.
    pub fn get_company_cosigner(env: Env, company: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::CompanyCosigner(company))
    }

    /// Register a co-signer address that is also authorized to perform
    /// recruiter-gated actions (submit proof, request exit, etc.) on behalf
    /// of this recruiter. Only the recruiter address itself can set the
    /// co-signer.
    pub fn set_recruiter_cosigner(env: Env, recruiter: Address, cosigner: Address) {
        recruiter.require_auth();
        env.storage()
            .persistent()
            .set(&DataKey::RecruiterCosigner(recruiter.clone()), &cosigner);
        env.events().publish(
            (Symbol::new(&env, "recruiter_cosigner_set"),),
            (recruiter, cosigner),
        );
    }

    /// Return the registered co-signer for a recruiter, or `None` if none set.
    pub fn get_recruiter_cosigner(env: Env, recruiter: Address) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey::RecruiterCosigner(recruiter))
    }

    /// Internal helper: check if `caller` is either the engagement's company
    /// or the company's registered co-signer.
    pub(crate) fn is_authorized_company(env: &Env, caller: &Address, engagement_company: &Address) -> bool {
        if caller == engagement_company {
            return true;
        }
        let cosigner: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::CompanyCosigner(engagement_company.clone()));
        match cosigner {
            Some(c) => caller == &c,
            None => false,
        }
    }

    // ----------------------------------------------------------
    // ISSUE #575 — COMPANY TEAM MEMBERS
    // ----------------------------------------------------------

    /// Register a team member scoped to `company` with a role that grants a
    /// subset of company-gated actions on every engagement of that company:
    /// `Confirmer` may confirm milestones, `Disputer` may raise disputes.
    /// Cancellation, top-ups, transfers and amendments stay company-only.
    /// Only the company address itself may manage its members.
    ///
    /// # Events
    /// `("company_member_added", company)` with `(member, role)`.
    pub fn add_company_member(
        env: Env,
        company: Address,
        member: Address,
        role: CompanyMemberRole,
    ) {
        company.require_auth();
        let key = DataKey::Config(ConfigKey::CompanyMember(company.clone(), member.clone()));
        env.storage().persistent().set(&key, &role);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
        env.events().publish(
            (Symbol::new(&env, "company_member_added"), company),
            (member, role),
        );
    }

    /// Remove a team member effective immediately: the address loses its
    /// delegated rights on every engagement of the company at once. Removing
    /// an address that is not a member is a no-op that still emits the event.
    ///
    /// # Events
    /// `("company_member_removed", company)` with `member`.
    pub fn remove_company_member(env: Env, company: Address, member: Address) {
        company.require_auth();
        env.storage().persistent().remove(&DataKey::Config(
            ConfigKey::CompanyMember(company.clone(), member.clone()),
        ));
        env.events().publish(
            (Symbol::new(&env, "company_member_removed"), company),
            member,
        );
    }

    /// Return the team role of `member` under `company`, or `None` if the
    /// address is not a member.
    pub fn get_company_member_role(
        env: Env,
        company: Address,
        member: Address,
    ) -> Option<CompanyMemberRole> {
        Self::company_member_role(&env, &company, &member)
    }

    /// Internal helper: the team role of `member` under `company`, if any.
    pub(crate) fn company_member_role(
        env: &Env,
        company: &Address,
        member: &Address,
    ) -> Option<CompanyMemberRole> {
        env.storage().persistent().get(&DataKey::Config(
            ConfigKey::CompanyMember(company.clone(), member.clone()),
        ))
    }

    /// Whether `caller` may confirm milestones: the company itself, its
    /// co-signer (full rights), or a team member with the `Confirmer` role.
    pub(crate) fn is_company_confirmer(
        env: &Env,
        caller: &Address,
        engagement_company: &Address,
    ) -> bool {
        if Self::is_authorized_company(env, caller, engagement_company) {
            return true;
        }
        matches!(
            Self::company_member_role(env, engagement_company, caller),
            Some(CompanyMemberRole::Confirmer)
        )
    }

    /// Whether `caller` may dispute milestones: the company itself, its
    /// co-signer (full rights), or a team member with the `Disputer` role.
    pub(crate) fn is_company_disputer(
        env: &Env,
        caller: &Address,
        engagement_company: &Address,
    ) -> bool {
        if Self::is_authorized_company(env, caller, engagement_company) {
            return true;
        }
        matches!(
            Self::company_member_role(env, engagement_company, caller),
            Some(CompanyMemberRole::Disputer)
        )
    }

    /// Internal helper: check if `caller` is either the engagement's recruiter
    /// or the recruiter's registered co-signer.
    pub(crate) fn is_authorized_recruiter(
        env: &Env,
        caller: &Address,
        engagement_recruiter: &Address,
    ) -> bool {
        if caller == engagement_recruiter {
            return true;
        }
        let cosigner: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey::RecruiterCosigner(engagement_recruiter.clone()));
        match cosigner {
            Some(c) => caller == &c,
            None => false,
        }
    }

    // ----------------------------------------------------------
    // ISSUE #43 — COMPANY TRANSFER
    // ----------------------------------------------------------

    /// Transfer the company role on an engagement to a new address, effective
    /// immediately (e.g. the company was acquired or restructured).
    ///
    /// # Caller
    /// `current_company` — must match `engagement.company` and sign the transaction.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's current company.
    /// - `"engagement is not active"` — engagement status is not `Active` or
    ///   `ReplacementRequested`.
    ///
    /// # Events
    /// - `("company_transferred", engagement_id)` with `(old_company, new_company)`.
    pub fn transfer_company(
        env: Env,
        current_company: Address,
        engagement_id: String,
        new_company: Address,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        current_company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if current_company != engagement.company {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let old_company = engagement.company.clone();
        engagement.company = new_company.clone();
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (
                Symbol::new(&env, "company_transferred"),
                engagement_id.clone(),
            ),
            (old_company, new_company),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #44 — RECRUITER TRANSFER
    // ----------------------------------------------------------

    /// Propose a transfer of the recruiter role to a new address.
    ///
    /// The current recruiter initiates the transfer by specifying the
    /// `new_recruiter` address. The company must then call
    /// [`Self::accept_recruiter_transfer`] for the change to take effect.
    /// Until accepted, all payouts continue to go to the original recruiter.
    ///
    /// # Caller
    /// `recruiter` — must match `engagement.recruiter` and sign the transaction.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's current recruiter.
    /// - `"engagement is not active"` — engagement status is not `Active` or
    ///   `ReplacementRequested`.
    pub fn propose_recruiter_transfer(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        new_recruiter: Address,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        env.storage().persistent().set(
            &DataKey::ProposedRecruiterTransfer(engagement_id.clone()),
            &new_recruiter,
        );

        let extend_to = DEFAULT_STORAGE_TTL_EXTEND_TO;
        env.storage().persistent().extend_ttl(
            &DataKey::ProposedRecruiterTransfer(engagement_id),
            100_000,
            extend_to,
        );
    }

    /// Accept a pending recruiter transfer and update the engagement's recruiter.
    ///
    /// The company finalises the transfer that was proposed by the current
    /// recruiter. After this call, all future payouts go to the new recruiter
    /// (or the new recruiter's own payout destination, issue #548 — the old
    /// recruiter's destination is not carried over).
    ///
    /// # Caller
    /// `company` — must match `engagement.company` and sign the transaction.
    ///
    /// # Panics
    /// - `"unauthorized"` — caller is not the engagement's company.
    /// - `"engagement is not active"` — engagement status is not `Active` or
    ///   `ReplacementRequested`.
    /// - `"no pending recruiter transfer"` — there is no active proposal.
    ///
    /// # Events
    /// - `("recruiter_transferred", engagement_id)` with `(old_recruiter, new_recruiter)`.
    pub fn accept_recruiter_transfer(env: Env, company: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if !Self::is_authorized_company(&env, &company, &engagement.company) {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let new_recruiter: Address = env
            .storage()
            .persistent()
            .get(&DataKey::ProposedRecruiterTransfer(engagement_id.clone()))
            .unwrap_or_else(|| panic!("no pending recruiter transfer"));

        let old_recruiter = engagement.recruiter.clone();
        engagement.recruiter = new_recruiter.clone();
        engagement.last_activity_ledger = env.ledger().sequence();

        env.storage()
            .persistent()
            .remove(&DataKey::ProposedRecruiterTransfer(engagement_id.clone()));

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (
                Symbol::new(&env, "recruiter_transferred"),
                engagement_id.clone(),
            ),
            (old_recruiter, new_recruiter),
        );
    }

    // ----------------------------------------------------------
    // ARBITER SUCCESSION
    // ----------------------------------------------------------

    /// Current arbiter nominates a successor. The successor must call `claim_arbiter`.
    /// Any arbiter in the engagement's arbiter list may initiate succession for their slot.
    ///
    /// # Panics
    /// - `"engagement is in a terminal state"` — the engagement is `Completed`,
    ///   `Cancelled`, or `Expired`. Arbiter succession has no practical function
    ///   once an engagement can no longer be disputed.
    pub fn nominate_arbiter_successor(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        successor: Address,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        arbiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if Self::is_terminal_status(&engagement.status) {
            panic!("engagement is in a terminal state");
        }

        let is_arbiter =
            (0..engagement.arbiters.len()).any(|i| engagement.arbiters.get(i).unwrap() == arbiter);
        if !is_arbiter {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let nomination = ArbiterNomination {
            current: arbiter.clone(),
            nominee: successor.clone(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::PendingArbiter(engagement_id.clone()), &nomination);

        env.storage().persistent().extend_ttl(
            &DataKey::PendingArbiter(engagement_id.clone()),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "arbiter_nominated"),
                engagement_id.clone(),
            ),
            successor,
        );
    }

    /// Nominated successor claims the arbiter slot, replacing the nominating arbiter.
    ///
    /// # Panics
    /// - `"engagement is in a terminal state"` — the engagement reached `Completed`,
    ///   `Cancelled`, or `Expired` after the nomination was made; the seat can no
    ///   longer be claimed.
    pub fn claim_arbiter(env: Env, nominee: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        nominee.require_auth();

        let nomination: ArbiterNomination = env
            .storage()
            .persistent()
            .get(&DataKey::PendingArbiter(engagement_id.clone()))
            .unwrap_or_else(|| panic!("no pending arbiter nomination"));

        if nominee != nomination.nominee {
            panic!("{}", ERR_UNAUTHORIZED);
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if Self::is_terminal_status(&engagement.status) {
            panic!("engagement is in a terminal state");
        }

        // Replace the nominating arbiter's slot with the nominee.
        for i in 0..engagement.arbiters.len() {
            if engagement.arbiters.get(i).unwrap() == nomination.current {
                engagement.arbiters.set(i, nominee.clone());
                break;
            }
        }

        // Migrate the seat's vote identity on any dispute currently in progress
        // (issue #178). Without this, the old arbiter's cast vote no longer
        // matches any address in `engagement.arbiters`, but the successor's
        // address also isn't in `voted`, so `cast_arbiter_vote`'s duplicate-vote
        // check would let the successor cast a second vote for the same seat.
        for i in 0..engagement.milestones.len() {
            if engagement.milestones.get(i).unwrap().status == MilestoneStatus::Disputed {
                let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), i);
                if let Some(mut record) = env
                    .storage()
                    .persistent()
                    .get::<DataKey, ArbiterVoteRecord>(&vote_key)
                {
                    for j in 0..record.voted.len() {
                        if record.voted.get(j).unwrap() == nomination.current {
                            record.voted.set(j, nominee.clone());
                        }
                    }
                    // Issue #559: the seat keeps its used vote change too.
                    for j in 0..record.changed.len() {
                        if record.changed.get(j).unwrap() == nomination.current {
                            record.changed.set(j, nominee.clone());
                        }
                    }
                    env.storage().persistent().set(&vote_key, &record);
                }
                // Same seat migration for split votes (issue #462).
                let split_key = DataKey2::ArbiterSplitVotes(engagement_id.clone(), i);
                if let Some(mut record) = env
                    .storage()
                    .persistent()
                    .get::<DataKey2, ArbiterSplitVoteRecord>(&split_key)
                {
                    for j in 0..record.voters.len() {
                        if record.voters.get(j).unwrap() == nomination.current {
                            record.voters.set(j, nominee.clone());
                        }
                    }
                    env.storage().persistent().set(&split_key, &record);
                }
            }
        }

        // Issue #463: delegation belongs to the outgoing arbiter, not the
        // slot, so the new arbiter starts without one. Also drop any other
        // arbiter's delegation to the nominee — as an arbiter now, its votes
        // resolve to its own slot, so that delegation could never be used.
        env.storage()
            .persistent()
            .remove(&DataKey2::ArbiterVoteDelegate(
                engagement_id.clone(),
                nomination.current.clone(),
            ));
        for i in 0..engagement.arbiters.len() {
            let other = engagement.arbiters.get(i).unwrap();
            let key = DataKey2::ArbiterVoteDelegate(engagement_id.clone(), other);
            let delegate: Option<Address> = env.storage().persistent().get(&key);
            if delegate.as_ref() == Some(&nominee) {
                env.storage().persistent().remove(&key);
            }
        }

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.storage()
            .persistent()
            .remove(&DataKey::PendingArbiter(engagement_id.clone()));

        env.events().publish(
            (Symbol::new(&env, "arbiter_claimed"), engagement_id.clone()),
            nominee,
        );
    }

    // ----------------------------------------------------------
    // ADMIN ARBITER REPLACEMENT (issue #245)
    // ----------------------------------------------------------

    // ----------------------------------------------------------
    // ISSUE #507 — ADMIN ARBITER PANEL RESIZE
    // ----------------------------------------------------------

    /// Admin grows an engagement's arbiter panel by appending `new_arbiter`
    /// as a new slot. Quorum stays as it is unless `new_quorum` is given.
    /// On a weighted panel (issue #460) the new slot has weight 1, and quorum
    /// is measured in weight as usual.
    ///
    /// Allowed while the engagement is quarantined, like other admin panel
    /// repairs, but never while any milestone is `Disputed`, so a panel
    /// cannot change mid-vote.
    ///
    /// # Panics
    /// - `"unauthorized"` / `"NoAdmin"` — caller is not the admin.
    /// - `"engagement not found"` / `"engagement is in a terminal state"`.
    /// - `"PanelChangeDuringDispute"` — a milestone is currently `Disputed`.
    /// - `"DuplicateArbiter"` — `new_arbiter` is already on the panel.
    /// - `"CompanyArbiterCollision"` / `"RecruiterArbiterCollision"` —
    ///   `new_arbiter` is the company or the recruiter.
    /// - `"invalid quorum"` — `new_quorum` is 0 or exceeds the new panel's
    ///   total weight.
    ///
    /// # Events
    /// Emits `("arbiter_added", engagement_id)` with `(new_arbiter, quorum)`.
    pub fn admin_add_arbiter(
        env: Env,
        admin: Address,
        engagement_id: String,
        new_arbiter: Address,
        new_quorum: Option<u32>,
    ) {
        Self::assert_admin(&env, &admin);
        let mut engagement = Self::get_engagement_for_panel_change(&env, &engagement_id);

        if engagement.arbiters.contains(&new_arbiter) {
            panic!("DuplicateArbiter");
        }
        if new_arbiter == engagement.company {
            panic!("CompanyArbiterCollision");
        }
        if new_arbiter == engagement.recruiter {
            panic!("RecruiterArbiterCollision");
        }

        engagement.arbiters.push_back(new_arbiter.clone());
        if let Some(weights) = engagement.arbiter_weights.as_mut() {
            weights.push_back(1);
        }
        if let Some(q) = new_quorum {
            Self::assert_panel_quorum_valid(&engagement, q);
            engagement.quorum = q;
        }

        // As in `claim_arbiter`: a delegation to an address that is now an
        // arbiter could never be used, since its votes resolve to its own slot.
        for i in 0..engagement.arbiters.len() {
            let key = DataKey2::ArbiterVoteDelegate(
                engagement_id.clone(),
                engagement.arbiters.get(i).unwrap(),
            );
            let delegate: Option<Address> = env.storage().persistent().get(&key);
            if delegate.as_ref() == Some(&new_arbiter) {
                env.storage().persistent().remove(&key);
            }
        }

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (Symbol::new(&env, "arbiter_added"), engagement_id),
            (new_arbiter, engagement.quorum),
        );
    }

    /// Admin shrinks an engagement's arbiter panel by removing `arbiter`'s
    /// slot (and its weight, on a weighted panel). If the current quorum
    /// would exceed what the remaining panel can reach, the call panics
    /// unless `new_quorum` supplies a reachable one; `new_quorum` may also
    /// lower quorum when it would still be reachable.
    ///
    /// Clears the removed arbiter's vote delegation and any pending
    /// succession nomination it made. Same quarantine and dispute rules as
    /// [`Self::admin_add_arbiter`].
    ///
    /// # Panics
    /// - `"unauthorized"` / `"NoAdmin"` — caller is not the admin.
    /// - `"engagement not found"` / `"engagement is in a terminal state"`.
    /// - `"PanelChangeDuringDispute"` — a milestone is currently `Disputed`.
    /// - `"ArbiterNotFound"` — `arbiter` is not on the panel.
    /// - `"at least one arbiter required"` — `arbiter` is the last one.
    /// - `"QuorumUnreachable"` — no `new_quorum` given and the current quorum
    ///   exceeds the remaining panel's total weight.
    /// - `"invalid quorum"` — `new_quorum` is 0 or exceeds the remaining
    ///   panel's total weight.
    ///
    /// # Events
    /// Emits `("arbiter_removed", engagement_id)` with `(arbiter, quorum)`.
    pub fn admin_remove_arbiter(
        env: Env,
        admin: Address,
        engagement_id: String,
        arbiter: Address,
        new_quorum: Option<u32>,
    ) {
        Self::assert_admin(&env, &admin);
        let mut engagement = Self::get_engagement_for_panel_change(&env, &engagement_id);

        let slot = engagement
            .arbiters
            .first_index_of(&arbiter)
            .unwrap_or_else(|| panic!("ArbiterNotFound"));
        if engagement.arbiters.len() == 1 {
            panic!("at least one arbiter required");
        }

        engagement.arbiters.remove(slot);
        if let Some(weights) = engagement.arbiter_weights.as_mut() {
            weights.remove(slot);
        }
        match new_quorum {
            Some(q) => {
                Self::assert_panel_quorum_valid(&engagement, q);
                engagement.quorum = q;
            }
            None => {
                if engagement.quorum > Self::total_arbiter_weight(&engagement) {
                    panic!("QuorumUnreachable");
                }
            }
        }

        env.storage()
            .persistent()
            .remove(&DataKey2::ArbiterVoteDelegate(engagement_id.clone(), arbiter.clone()));
        let nomination_key = DataKey::PendingArbiter(engagement_id.clone());
        let nomination: Option<ArbiterNomination> = env.storage().persistent().get(&nomination_key);
        if nomination.is_some_and(|n| n.current == arbiter) {
            env.storage().persistent().remove(&nomination_key);
        }

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
        Self::extend_engagement_ttl(&env, &engagement_id);

        env.events().publish(
            (Symbol::new(&env, "arbiter_removed"), engagement_id),
            (arbiter, engagement.quorum),
        );
    }

    /// Load an engagement whose panel is about to be resized, rejecting
    /// terminal engagements and ones with a dispute in progress.
    fn get_engagement_for_panel_change(env: &Env, engagement_id: &String) -> Engagement {
        let engagement = Self::get_engagement_internal(env, engagement_id);
        if Self::is_terminal_status(&engagement.status) {
            panic!("engagement is in a terminal state");
        }
        let disputed = (0..engagement.milestones.len())
            .any(|i| engagement.milestones.get(i).unwrap().status == MilestoneStatus::Disputed);
        if disputed {
            panic!("PanelChangeDuringDispute");
        }
        engagement
    }

    /// Panics `"invalid quorum"` unless `quorum` is reachable by the panel:
    /// at least 1 and at most its total weight (its size when unweighted).
    fn assert_panel_quorum_valid(engagement: &Engagement, quorum: u32) {
        if quorum == 0 || quorum > Self::total_arbiter_weight(engagement) {
            panic!("invalid quorum");
        }
    }
}
