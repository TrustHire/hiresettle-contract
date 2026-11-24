//! Off-chain integration features:
//!
//! - **Placement oracles (issue #667)** — an admin-managed registry of trusted
//!   integrations (e.g. an HRIS) that a company can opt into per engagement
//!   with `EngagementConfig::placement_oracle`, letting the oracle confirm
//!   `Placement` milestones in place of a manual company signature.
//! - **Invoice references (issue #669)** — a recruiter-supplied reference per
//!   milestone, carried on the `milestone_payout` event and the stored
//!   [`MilestoneReceipt`] so finance teams can reconcile payouts.
//! - **External references (issue #572)** — an ATS / job-board identifier
//!   indexed per company so integrations can find an engagement on-chain.
//! - **Attester registry (issue #661)** — admin-registered attesters (KYC
//!   providers, partner job boards) that may issue or clear recruiter and/or
//!   company verification badges without holding the admin key.

use soroban_sdk::{contractimpl, Address, Env, String, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #667 — ORACLE-ATTESTED PLACEMENT CONFIRMATION
    // ----------------------------------------------------------

    /// Admin registers a trusted placement oracle. Engagements can then name
    /// it in `EngagementConfig::placement_oracle`.
    ///
    /// # Events
    /// `("placement_oracle_added",)` with `oracle`.
    pub fn add_placement_oracle(env: Env, admin: Address, oracle: Address) {
        Self::assert_admin(&env, &admin);
        let key = DataKey3::PlacementOracle(oracle.clone());
        env.storage().persistent().set(&key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
        env.events()
            .publish((Symbol::new(&env, "placement_oracle_added"),), oracle);
    }

    /// Admin removes a placement oracle from the registry. Takes effect
    /// immediately: the oracle can no longer confirm on any engagement,
    /// including engagements that already opted into it. Those engagements
    /// keep their manual confirmation path, and re-registering the oracle
    /// restores its rights on them.
    ///
    /// # Events
    /// `("placement_oracle_removed",)` with `oracle`.
    pub fn remove_placement_oracle(env: Env, admin: Address, oracle: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .remove(&DataKey3::PlacementOracle(oracle.clone()));
        env.events()
            .publish((Symbol::new(&env, "placement_oracle_removed"),), oracle);
    }

    /// Whether `oracle` is currently in the placement oracle registry.
    pub fn is_placement_oracle(env: Env, oracle: Address) -> bool {
        Self::is_placement_oracle_internal(&env, &oracle)
    }

    /// The placement oracle an engagement opted into, if any.
    pub fn get_engagement_oracle(env: Env, engagement_id: String) -> Option<Address> {
        env.storage()
            .persistent()
            .get(&DataKey3::EngagementOracle(engagement_id))
    }

    /// The engagement's placement oracle confirms a `Placement` milestone,
    /// with the same preconditions and payout economics as
    /// `confirm_milestone`. The company can still confirm or dispute the
    /// milestone manually; whichever happens first wins.
    ///
    /// Paused together with `confirm_milestone` by
    /// `set_function_paused(FN_CONFIRM_MILESTONE)`.
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"FunctionPaused"` / `"EngagementPaused"` / `"MilestoneOnHold"`
    /// - `"engagement is not active"` — engagement is not `Active`.
    /// - `"unauthorized"` — `oracle` is not the engagement's placement oracle.
    /// - `"OracleNotRegistered"` — the oracle was removed from the registry.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"OracleNotPlacementMilestone"` — the milestone is a `Retention` milestone.
    /// - `"milestone proof not yet submitted"` — milestone is not `ProofSubmitted`.
    /// - `"PreviousMilestoneNotComplete"` — a prerequisite is not done.
    ///
    /// # Events
    /// The `confirm_milestone` events, with the oracle as the status-change
    /// actor and the `milestone_payout` confirmer, plus
    /// `("milestone_confirmed_by_oracle", engagement_id)` with
    /// `(milestone_index, oracle)`.
    pub fn oracle_confirm_milestone(
        env: Env,
        oracle: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_function_not_paused(&env, FN_CONFIRM_MILESTONE);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        Self::assert_milestone_not_on_hold(&env, &engagement_id, milestone_index);
        oracle.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if engagement.status != EngagementStatus::Active {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }

        let configured: Option<Address> = env
            .storage()
            .persistent()
            .get(&DataKey3::EngagementOracle(engagement_id.clone()));
        if configured.as_ref() != Some(&oracle) {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        if !Self::is_placement_oracle_internal(&env, &oracle) {
            panic!("OracleNotRegistered");
        }

        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.kind != MilestoneKind::Placement {
            panic!("OracleNotPlacementMilestone");
        }

        Self::confirm_milestone_core(
            &env,
            engagement,
            &engagement_id,
            milestone_index,
            &oracle,
            true,
        );
    }

    pub(crate) fn is_placement_oracle_internal(env: &Env, oracle: &Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey3::PlacementOracle(oracle.clone()))
    }

    /// Validate `EngagementConfig::placement_oracle` at creation: it must be
    /// registered and must not be the recruiter side, which could otherwise
    /// confirm its own placements.
    pub(crate) fn validate_placement_oracle(
        env: &Env,
        oracle: &Address,
        recruiter: &Address,
        co_recruiter: &Option<Address>,
    ) {
        if !Self::is_placement_oracle_internal(env, oracle) {
            panic!("OracleNotRegistered");
        }
        if oracle == recruiter || co_recruiter.as_ref() == Some(oracle) {
            panic!("OracleRecruiterCollision");
        }
    }

    // ----------------------------------------------------------
    // ISSUE #669 — INVOICE REFERENCE ON MILESTONE PAYOUTS
    // ----------------------------------------------------------

    /// Recruiter (or their co-signer) attaches an invoice reference to a
    /// milestone so the payout can be reconciled off-chain. May be called
    /// repeatedly to correct the reference until the milestone is confirmed;
    /// after that it is locked. (A replacement that resets a confirmed
    /// placement milestone to `Pending` unlocks it again for the new payout.)
    ///
    /// # Panics
    /// - `"ContractPaused"` / `"EngagementPaused"`
    /// - `"InvoiceRefEmpty"` / `"InvoiceRefTooLong"` — empty or longer than 64 characters.
    /// - `"engagement is not active"` — the engagement is in a terminal status.
    /// - `"unauthorized"` — caller is not the recruiter or recruiter co-signer.
    /// - `"invalid milestone index"` — `milestone_index` is out of bounds.
    /// - `"InvoiceRefLocked"` — the milestone is already `Confirmed` or `Resolved`.
    ///
    /// # Events
    /// `("milestone_invoice_ref_set", engagement_id)` with
    /// `(milestone_index, invoice_ref)`.
    pub fn set_milestone_invoice_ref(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
        invoice_ref: String,
    ) {
        Self::assert_not_paused(&env);
        Self::assert_engagement_not_paused(&env, &engagement_id);
        recruiter.require_auth();

        if invoice_ref.is_empty() {
            panic!("InvoiceRefEmpty");
        }
        if invoice_ref.len() > MAX_INVOICE_REF_LEN {
            panic!("InvoiceRefTooLong");
        }

        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        if Self::is_terminal_status(&engagement.status) {
            panic!("{}", ERR_ENGAGEMENT_NOT_ACTIVE);
        }
        if !Self::is_authorized_recruiter(&env, &recruiter, &engagement.recruiter) {
            panic!("{}", ERR_UNAUTHORIZED);
        }
        let milestone = Self::get_milestone_or_panic(&engagement, milestone_index);
        if milestone.status == MilestoneStatus::Confirmed
            || milestone.status == MilestoneStatus::Resolved
        {
            panic!("InvoiceRefLocked");
        }

        let key = DataKey3::MilestoneInvoiceRef(engagement_id.clone(), milestone_index);
        env.storage().persistent().set(&key, &invoice_ref);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "milestone_invoice_ref_set"), engagement_id),
            (milestone_index, invoice_ref),
        );
    }

    /// The invoice reference set on a milestone, if any.
    pub fn get_milestone_invoice_ref(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<String> {
        env.storage()
            .persistent()
            .get(&DataKey3::MilestoneInvoiceRef(engagement_id, milestone_index))
    }

    /// The payout receipt written when the milestone was confirmed, if any.
    ///
    /// Written by `confirm_milestone`, `batch_confirm_milestones`,
    /// `oracle_confirm_milestone`, `accept_partial_confirmation`,
    /// `force_confirm_milestone` and `close_engagement_at_end`. Milestones
    /// that reach `Resolved` through dispute arbitration have no receipt;
    /// their payout is reported by the `dispute_resolved` event. A
    /// re-confirmation after a replacement overwrites the receipt.
    pub fn get_milestone_receipt(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Option<MilestoneReceipt> {
        env.storage()
            .persistent()
            .get(&DataKey3::MilestoneReceipt(engagement_id, milestone_index))
    }

    /// Store the milestone's payout receipt and emit
    /// `("milestone_payout", engagement_id)` with
    /// `(milestone_index, amount, confirmed_by, invoice_ref)`.
    pub(crate) fn record_milestone_receipt(
        env: &Env,
        engagement_id: &String,
        milestone_index: u32,
        amount: i128,
        confirmed_by: Option<Address>,
    ) {
        let invoice_ref: Option<String> = env.storage().persistent().get(
            &DataKey3::MilestoneInvoiceRef(engagement_id.clone(), milestone_index),
        );
        let receipt = MilestoneReceipt {
            milestone_index,
            amount,
            confirmed_by: confirmed_by.clone(),
            confirmed_at_ledger: env.ledger().sequence(),
            invoice_ref: invoice_ref.clone(),
        };
        let key = DataKey3::MilestoneReceipt(engagement_id.clone(), milestone_index);
        env.storage().persistent().set(&key, &receipt);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(env, "milestone_payout"), engagement_id.clone()),
            (milestone_index, amount, confirmed_by, invoice_ref),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #572 — EXTERNAL REFERENCE ID
    // ----------------------------------------------------------

    /// Look up an engagement by the external (ATS / job-board) reference it
    /// was created with under `company`. After `transfer_company` the
    /// reference is found under the new company only. Mappings are kept
    /// after the engagement ends, so a reference stays reserved per company.
    pub fn get_engagement_by_external_ref(
        env: Env,
        company: Address,
        external_ref: String,
    ) -> Option<String> {
        env.storage()
            .persistent()
            .get(&DataKey3::ExternalRef(company, external_ref))
    }

    /// The external reference an engagement was created with, if any.
    pub fn get_engagement_external_ref(env: Env, engagement_id: String) -> Option<String> {
        env.storage()
            .persistent()
            .get(&DataKey3::EngagementExternalRef(engagement_id))
    }

    /// Validate `EngagementConfig::external_ref` at creation.
    ///
    /// # Panics
    /// - `"ExternalRefEmpty"` / `"ExternalRefTooLong"` — empty or over 64 characters.
    /// - `"DuplicateExternalRef"` — `company` already uses this reference.
    pub(crate) fn validate_external_ref(env: &Env, company: &Address, external_ref: &String) {
        if external_ref.is_empty() {
            panic!("ExternalRefEmpty");
        }
        if external_ref.len() > MAX_EXTERNAL_REF_LEN {
            panic!("ExternalRefTooLong");
        }
        if env
            .storage()
            .persistent()
            .has(&DataKey3::ExternalRef(company.clone(), external_ref.clone()))
        {
            panic!("DuplicateExternalRef");
        }
    }

    /// Index `external_ref` under `company` for `engagement_id`.
    pub(crate) fn index_external_ref(
        env: &Env,
        company: &Address,
        external_ref: &String,
        engagement_id: &String,
    ) {
        let key = DataKey3::ExternalRef(company.clone(), external_ref.clone());
        env.storage().persistent().set(&key, engagement_id);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Re-index an engagement's external reference from `old_company` to
    /// `new_company` on `transfer_company`. A no-op for engagements without
    /// one.
    ///
    /// # Panics
    /// - `"DuplicateExternalRef"` — `new_company` already uses this reference
    ///   for a different engagement.
    pub(crate) fn reindex_external_ref(
        env: &Env,
        engagement_id: &String,
        old_company: &Address,
        new_company: &Address,
    ) {
        let external_ref: Option<String> = env
            .storage()
            .persistent()
            .get(&DataKey3::EngagementExternalRef(engagement_id.clone()));
        let external_ref = match external_ref {
            Some(r) => r,
            None => return,
        };
        if old_company == new_company {
            return;
        }
        let new_key = DataKey3::ExternalRef(new_company.clone(), external_ref.clone());
        if env.storage().persistent().has(&new_key) {
            panic!("DuplicateExternalRef");
        }
        env.storage()
            .persistent()
            .remove(&DataKey3::ExternalRef(old_company.clone(), external_ref.clone()));
        Self::index_external_ref(env, new_company, &external_ref, engagement_id);
    }

    // ----------------------------------------------------------
    // ISSUE #661 — ATTESTER REGISTRY
    // ----------------------------------------------------------

    /// Admin registers (or re-scopes) an attester allowed to issue and clear
    /// verification badges within `scope`.
    ///
    /// # Events
    /// `("attester_added",)` with `(attester, scope)`.
    pub fn add_attester(env: Env, admin: Address, attester: Address, scope: AttesterScope) {
        Self::assert_admin(&env, &admin);
        let key = DataKey3::Attester(attester.clone());
        env.storage().persistent().set(&key, &scope);
        env.storage()
            .persistent()
            .extend_ttl(&key, 100_000, 6_300_000);
        env.events()
            .publish((Symbol::new(&env, "attester_added"),), (attester, scope));
    }

    /// Admin removes an attester. The attester immediately loses the ability
    /// to issue or clear badges.
    ///
    /// **Badges it already issued are not revoked automatically**: they stay
    /// valid until cleared individually or revoked in bulk with
    /// [`Self::revoke_attester_badges`].
    ///
    /// # Events
    /// `("attester_removed",)` with `attester`.
    pub fn remove_attester(env: Env, admin: Address, attester: Address) {
        Self::assert_admin(&env, &admin);
        env.storage()
            .persistent()
            .remove(&DataKey3::Attester(attester.clone()));
        env.events()
            .publish((Symbol::new(&env, "attester_removed"),), attester);
    }

    /// The scope of a registered attester, or `None` if not registered.
    pub fn get_attester_scope(env: Env, attester: Address) -> Option<AttesterScope> {
        env.storage()
            .persistent()
            .get(&DataKey3::Attester(attester))
    }

    /// A registered attester with `Recruiter` or `Both` scope sets or clears
    /// a recruiter's verification badge. Clearing works on any badge,
    /// whoever issued it.
    ///
    /// # Panics
    /// - `"AttesterNotAuthorized"` — caller is not a registered attester with
    ///   recruiter scope.
    ///
    /// # Events
    /// `("recruiter_attested", attester)` with `(recruiter, verified)`.
    pub fn attest_recruiter(env: Env, attester: Address, recruiter: Address, verified: bool) {
        attester.require_auth();
        Self::assert_attester_scope(&env, &attester, false);
        Self::write_recruiter_verification(&env, &recruiter, verified, &attester);
        Self::track_attester_badge(&env, &attester, &recruiter, false, verified);
        env.events().publish(
            (Symbol::new(&env, "recruiter_attested"), attester),
            (recruiter, verified),
        );
    }

    /// A registered attester with `Company` or `Both` scope sets or clears a
    /// company's verification badge. Clearing works on any badge, whoever
    /// issued it.
    ///
    /// # Panics
    /// - `"AttesterNotAuthorized"` — caller is not a registered attester with
    ///   company scope.
    ///
    /// # Events
    /// `("company_attested", attester)` with `(company, verified)`.
    pub fn attest_company(env: Env, attester: Address, company: Address, verified: bool) {
        attester.require_auth();
        Self::assert_attester_scope(&env, &attester, true);
        Self::write_company_verification(&env, &company, verified, &attester);
        Self::track_attester_badge(&env, &attester, &company, true, verified);
        env.events().publish(
            (Symbol::new(&env, "company_attested"), attester),
            (company, verified),
        );
    }

    /// Admin sets or clears the verification flag for a company address,
    /// the company counterpart of `set_recruiter_verified`.
    ///
    /// # Events
    /// `("company_verified_set",)` with `(company, verified)`.
    pub fn set_company_verified(env: Env, admin: Address, company: Address, verified: bool) {
        Self::assert_admin(&env, &admin);
        Self::write_company_verification(&env, &company, verified, &admin);
        env.events().publish(
            (Symbol::new(&env, "company_verified_set"),),
            (company, verified),
        );
    }

    /// Permissionless read of the company verification flag.
    pub fn is_company_verified(env: Env, company: Address) -> bool {
        env.storage()
            .persistent()
            .has(&DataKey3::CompanyVerification(company))
    }

    /// Who issued a recruiter's current verification badge. `None` when the
    /// recruiter is not verified, or was verified before issuer records
    /// existed.
    pub fn get_recruiter_verification(env: Env, recruiter: Address) -> Option<VerificationRecord> {
        env.storage()
            .persistent()
            .get(&DataKey3::RecruiterVerification(recruiter))
    }

    /// Who issued a company's current verification badge, or `None`.
    pub fn get_company_verification(env: Env, company: Address) -> Option<VerificationRecord> {
        env.storage()
            .persistent()
            .get(&DataKey3::CompanyVerification(company))
    }

    /// Admin revokes every badge still attributed to `attester`, recruiter
    /// and company alike. Works whether or not the attester is still
    /// registered. Badges another issuer has since re-issued are left alone.
    /// Returns the number of badges revoked.
    ///
    /// # Events
    /// `("attester_badges_revoked",)` with `(attester, revoked_count)`.
    pub fn revoke_attester_badges(env: Env, admin: Address, attester: Address) -> u32 {
        Self::assert_admin(&env, &admin);
        let mut revoked: u32 = 0;

        let recruiters_key = DataKey3::AttesterRecruiterBadges(attester.clone());
        let recruiters: Vec<Address> = env
            .storage()
            .persistent()
            .get(&recruiters_key)
            .unwrap_or_else(|| Vec::new(&env));
        for recruiter in recruiters.iter() {
            let record: Option<VerificationRecord> = env
                .storage()
                .persistent()
                .get(&DataKey3::RecruiterVerification(recruiter.clone()));
            if record.map(|r| r.issuer == attester).unwrap_or(false) {
                Self::write_recruiter_verification(&env, &recruiter, false, &attester);
                revoked += 1;
            }
        }
        env.storage().persistent().remove(&recruiters_key);

        let companies_key = DataKey3::AttesterCompanyBadges(attester.clone());
        let companies: Vec<Address> = env
            .storage()
            .persistent()
            .get(&companies_key)
            .unwrap_or_else(|| Vec::new(&env));
        for company in companies.iter() {
            let record: Option<VerificationRecord> = env
                .storage()
                .persistent()
                .get(&DataKey3::CompanyVerification(company.clone()));
            if record.map(|r| r.issuer == attester).unwrap_or(false) {
                Self::write_company_verification(&env, &company, false, &attester);
                revoked += 1;
            }
        }
        env.storage().persistent().remove(&companies_key);

        env.events().publish(
            (Symbol::new(&env, "attester_badges_revoked"),),
            (attester, revoked),
        );
        revoked
    }

    /// Panic with `"AttesterNotAuthorized"` unless `attester` is registered
    /// with a scope covering companies (`for_company`) or recruiters.
    fn assert_attester_scope(env: &Env, attester: &Address, for_company: bool) {
        let scope: Option<AttesterScope> = env
            .storage()
            .persistent()
            .get(&DataKey3::Attester(attester.clone()));
        let allowed = match scope {
            Some(AttesterScope::Both) => true,
            Some(AttesterScope::Company) => for_company,
            Some(AttesterScope::Recruiter) => !for_company,
            None => false,
        };
        if !allowed {
            panic!("AttesterNotAuthorized");
        }
    }

    /// Set or clear a recruiter's badge: the legacy boolean flag read by
    /// `is_recruiter_verified` plus the issuer record.
    pub(crate) fn write_recruiter_verification(
        env: &Env,
        recruiter: &Address,
        verified: bool,
        issuer: &Address,
    ) {
        let flag_key = DataKey2::RecruiterVerified(recruiter.clone());
        let record_key = DataKey3::RecruiterVerification(recruiter.clone());
        if verified {
            let record = VerificationRecord {
                issuer: issuer.clone(),
                issued_at_ledger: env.ledger().sequence(),
            };
            env.storage().persistent().set(&flag_key, &true);
            env.storage().persistent().set(&record_key, &record);
            env.storage()
                .persistent()
                .extend_ttl(&flag_key, 100_000, 6_300_000);
            env.storage()
                .persistent()
                .extend_ttl(&record_key, 100_000, 6_300_000);
        } else {
            env.storage().persistent().remove(&flag_key);
            env.storage().persistent().remove(&record_key);
        }
    }

    /// Set or clear a company's badge (the issuer record doubles as the flag).
    fn write_company_verification(
        env: &Env,
        company: &Address,
        verified: bool,
        issuer: &Address,
    ) {
        let key = DataKey3::CompanyVerification(company.clone());
        if verified {
            let record = VerificationRecord {
                issuer: issuer.clone(),
                issued_at_ledger: env.ledger().sequence(),
            };
            env.storage().persistent().set(&key, &record);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
        } else {
            env.storage().persistent().remove(&key);
        }
    }

    /// Keep the attester's list of issued badges in step: add `subject` when
    /// verifying, drop it when the attester clears it.
    fn track_attester_badge(
        env: &Env,
        attester: &Address,
        subject: &Address,
        is_company: bool,
        verified: bool,
    ) {
        let key = if is_company {
            DataKey3::AttesterCompanyBadges(attester.clone())
        } else {
            DataKey3::AttesterRecruiterBadges(attester.clone())
        };
        let mut subjects: Vec<Address> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        let position = subjects.first_index_of(subject);
        match (verified, position) {
            (true, None) => subjects.push_back(subject.clone()),
            (false, Some(i)) => {
                subjects.remove(i);
            }
            _ => return,
        }
        if subjects.is_empty() {
            env.storage().persistent().remove(&key);
        } else {
            env.storage().persistent().set(&key, &subjects);
            env.storage()
                .persistent()
                .extend_ttl(&key, 100_000, 6_300_000);
        }
    }
}
