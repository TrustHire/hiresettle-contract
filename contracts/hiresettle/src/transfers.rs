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
    /// recruiter. After this call, all future payouts go to the new recruiter.
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
                    env.storage().persistent().set(&vote_key, &record);
                }
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

}
