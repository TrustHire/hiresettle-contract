//! Recruiter collateral bonds (issue #459), co-recruiter collateral bonds
//! (issue #506) and engagement bundles (issue #464).

use soroban_sdk::{contractimpl, token, Address, Env, String, Symbol, Vec};
use crate::*;

#[contractimpl]
impl HireSettleContract {
    // ----------------------------------------------------------
    // ISSUE #459 — RECRUITER COLLATERAL BOND
    // ----------------------------------------------------------

    /// Return `(amount, forfeited)` for the engagement's recruiter bond, or
    /// `None` if the engagement was created without one.
    pub fn get_recruiter_bond(env: Env, engagement_id: String) -> Option<(i128, bool)> {
        env.storage()
            .persistent()
            .get::<DataKey2, RecruiterBond>(&DataKey2::RecruiterBond(engagement_id))
            .map(|b| (b.amount, b.forfeited))
    }

    /// Admin sets the fraction of a recruiter bond, in basis points, that is
    /// forfeited to the company when the forfeit condition is met. The rest
    /// is returned to the recruiter. Must be ≤ 10 000; default 10 000 (100 %).
    pub fn set_bond_forfeit_bps(env: Env, admin: Address, bps: u32) {
        Self::assert_admin(&env, &admin);
        if bps > FULL_SPLIT_BPS {
            panic!("InvalidBondForfeitBps");
        }
        env.storage()
            .instance()
            .set(&DataKey::Config(ConfigKey::BondForfeitBps), &bps);
        env.events()
            .publish((Symbol::new(&env, "bond_forfeit_bps_set"),), bps);
    }

    /// Return the configured bond forfeit fraction in basis points.
    pub fn get_bond_forfeit_bps(env: Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::Config(ConfigKey::BondForfeitBps))
            .unwrap_or(FULL_SPLIT_BPS)
    }

    /// Escrow the recruiter's bond at engagement creation. Caller has already
    /// validated the engagement; this only validates the amount, authorizes
    /// the recruiter and pulls the funds.
    pub(crate) fn escrow_recruiter_bond(
        env: &Env,
        engagement_id: &String,
        recruiter: &Address,
        token: &Address,
        amount: i128,
    ) {
        Self::escrow_bond(
            env,
            DataKey2::RecruiterBond(engagement_id.clone()),
            recruiter,
            token,
            amount,
        );
        env.events().publish(
            (Symbol::new(env, "recruiter_bond_posted"), engagement_id.clone()),
            amount,
        );
    }

    /// Validate `amount`, authorize `payer`, pull the funds into escrow and
    /// store a fresh bond record under `key`.
    fn escrow_bond(env: &Env, key: DataKey2, payer: &Address, token: &Address, amount: i128) {
        if amount <= 0 {
            panic!("InvalidBondAmount");
        }
        payer.require_auth();
        token::Client::new(env, token).transfer(payer, &env.current_contract_address(), &amount);
        // Issue #672: held until `settle_bonds`.
        Self::add_token_aggregate(env, DataKey3::TokenBonded(token.clone()), amount);
        env.storage().persistent().set(
            &key,
            &RecruiterBond {
                amount,
                forfeited: false,
                settled: false,
                rejected_milestones: Vec::new(env),
            },
        );
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);
    }

    /// Record that a dispute on `milestone_index` resolved against the
    /// recruiter's proof, on the recruiter bond and the co-recruiter bond
    /// (issue #506) independently. No-op for a bond the engagement lacks.
    pub(crate) fn bond_record_rejection(env: &Env, engagement_id: &String, milestone_index: u32) {
        for key in [
            DataKey2::RecruiterBond(engagement_id.clone()),
            DataKey2::CoRecruiterBond(engagement_id.clone()),
        ] {
            if let Some(mut bond) = env.storage().persistent().get::<DataKey2, RecruiterBond>(&key) {
                if !bond.rejected_milestones.contains(milestone_index) {
                    bond.rejected_milestones.push_back(milestone_index);
                    env.storage().persistent().set(&key, &bond);
                }
            }
        }
    }

    /// Clear the rejection mark on `milestone_index` once it is confirmed or
    /// resolved — the recruiter successfully resubmitted. Applies to both
    /// bonds; no-op for a bond the engagement lacks.
    pub(crate) fn bond_clear_rejection(env: &Env, engagement_id: &String, milestone_index: u32) {
        for key in [
            DataKey2::RecruiterBond(engagement_id.clone()),
            DataKey2::CoRecruiterBond(engagement_id.clone()),
        ] {
            if let Some(mut bond) = env.storage().persistent().get::<DataKey2, RecruiterBond>(&key) {
                if let Some(pos) = bond.rejected_milestones.first_index_of(milestone_index) {
                    bond.rejected_milestones.remove(pos);
                    env.storage().persistent().set(&key, &bond);
                }
            }
        }
    }

    /// Settle every bond on an engagement that just reached a terminal state
    /// (`Completed`, `Cancelled` or `Expired`).
    pub(crate) fn settle_bonds(env: &Env, engagement: &Engagement) {
        Self::settle_recruiter_bond(env, engagement);
        Self::settle_co_recruiter_bond(env, engagement);
    }

    /// Pay out the bond when the engagement reaches a terminal state
    /// (`Completed`, `Cancelled` or `Expired`). If any rejected milestone was
    /// never subsequently confirmed/resolved, the configured forfeit fraction
    /// goes to the company and the remainder to the recruiter; otherwise the
    /// full bond is returned to the recruiter. Settles at most once; no-op
    /// without a bond.
    pub(crate) fn settle_recruiter_bond(env: &Env, engagement: &Engagement) {
        let key = DataKey2::RecruiterBond(engagement.id.clone());
        let mut bond = match env.storage().persistent().get::<DataKey2, RecruiterBond>(&key) {
            Some(b) if !b.settled => b,
            _ => return,
        };

        let forfeit = if bond.rejected_milestones.is_empty() {
            0
        } else {
            let bps = Self::get_bond_forfeit_bps(env.clone()) as i128;
            (bond.amount * bps) / (FULL_SPLIT_BPS as i128)
        };
        let refund = bond.amount - forfeit;

        let token_client = token::Client::new(env, &engagement.token);
        if forfeit > 0 {
            token_client.transfer(&env.current_contract_address(), &engagement.company, &forfeit);
        }
        if refund > 0 {
            // Issue #547: credited rather than pushed on pull-payout engagements.
            Self::pay_or_credit_recruiter(env, engagement, &token_client, &engagement.recruiter, refund);
        }

        bond.forfeited = forfeit > 0;
        bond.settled = true;
        env.storage().persistent().set(&key, &bond);
        Self::add_token_aggregate(env, DataKey3::TokenBonded(engagement.token.clone()), -bond.amount);

        env.events().publish(
            (Symbol::new(env, "recruiter_bond_settled"), engagement.id.clone()),
            (forfeit, refund),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #506 — CO-RECRUITER COLLATERAL BOND
    // ----------------------------------------------------------

    /// Return `(amount, forfeited)` for the engagement's co-recruiter bond, or
    /// `None` if the engagement was created without one. Mirrors
    /// `get_recruiter_bond`.
    ///
    /// # Forfeiture rule
    /// The co-recruiter bond is escrowed from `co_recruiter` at creation when
    /// both `co_recruiter` and `co_recruiter_bond_amount` are set, and settled
    /// once, independently of the recruiter bond, when the engagement reaches
    /// `Completed`, `Cancelled` or `Expired`:
    ///
    /// - **Trigger** — the same one as the recruiter bond, tracked on the
    ///   co-recruiter bond's own record: some milestone had a dispute resolve
    ///   against its submitted proof and was never confirmed or resolved
    ///   afterwards.
    /// - **Amount** — the co-recruiter shares in payouts, so it shares that
    ///   liability pro rata to its payout share. It forfeits
    ///   `amount * bond_forfeit_bps * (10_000 - recruiter_split_bps) / 10_000²`
    ///   to the company, using `recruiter_split_bps` as it stands at
    ///   settlement. The rest returns to the co-recruiter.
    ///
    /// Without the trigger, the full bond returns to the co-recruiter. The
    /// recruiter bond's forfeiture is unaffected by this bond.
    pub fn get_co_recruiter_bond(env: Env, engagement_id: String) -> Option<(i128, bool)> {
        env.storage()
            .persistent()
            .get::<DataKey2, RecruiterBond>(&DataKey2::CoRecruiterBond(engagement_id))
            .map(|b| (b.amount, b.forfeited))
    }

    /// Escrow the co-recruiter's bond at engagement creation (issue #506).
    pub(crate) fn escrow_co_recruiter_bond(
        env: &Env,
        engagement_id: &String,
        co_recruiter: &Address,
        token: &Address,
        amount: i128,
    ) {
        Self::escrow_bond(
            env,
            DataKey2::CoRecruiterBond(engagement_id.clone()),
            co_recruiter,
            token,
            amount,
        );
        env.events().publish(
            (
                Symbol::new(env, "co_recruiter_bond_posted"),
                engagement_id.clone(),
            ),
            amount,
        );
    }

    /// Pay out the co-recruiter bond per the rule on `get_co_recruiter_bond`.
    /// Settles at most once; no-op without a bond.
    pub(crate) fn settle_co_recruiter_bond(env: &Env, engagement: &Engagement) {
        let key = DataKey2::CoRecruiterBond(engagement.id.clone());
        let mut bond = match env.storage().persistent().get::<DataKey2, RecruiterBond>(&key) {
            Some(b) if !b.settled => b,
            _ => return,
        };
        let co_recruiter = match &engagement.co_recruiter {
            Some(c) => c.clone(),
            None => return,
        };

        let forfeit = if bond.rejected_milestones.is_empty() {
            0
        } else {
            let bps = Self::get_bond_forfeit_bps(env.clone()) as i128;
            let co_share_bps = (FULL_SPLIT_BPS - engagement.recruiter_split_bps) as i128;
            let full = FULL_SPLIT_BPS as i128;
            (bond.amount * bps * co_share_bps) / (full * full)
        };
        let refund = bond.amount - forfeit;

        let token_client = token::Client::new(env, &engagement.token);
        if forfeit > 0 {
            token_client.transfer(&env.current_contract_address(), &engagement.company, &forfeit);
        }
        if refund > 0 {
            Self::pay_or_credit_recruiter(env, engagement, &token_client, &co_recruiter, refund);
        }

        bond.forfeited = forfeit > 0;
        bond.settled = true;
        env.storage().persistent().set(&key, &bond);
        Self::add_token_aggregate(env, DataKey3::TokenBonded(engagement.token.clone()), -bond.amount);

        env.events().publish(
            (
                Symbol::new(env, "co_recruiter_bond_settled"),
                engagement.id.clone(),
            ),
            (forfeit, refund),
        );
    }

    // ----------------------------------------------------------
    // ISSUE #464 — ENGAGEMENT BUNDLES
    // ----------------------------------------------------------

    /// Company registers a shared arbiter panel under `bundle_id`. Engagements
    /// created with `EngagementConfig::bundle_id` set to this ID take their
    /// arbiters/quorum from the bundle. Disputes still resolve independently
    /// per engagement and milestone — only the panel configuration is shared.
    ///
    /// # Panics
    /// - `"InvalidBundleId"` — `bundle_id` is empty or longer than 64 chars.
    /// - `"BundleAlreadyExists"` — `bundle_id` is already registered.
    /// - `"at least one arbiter required"` / `"invalid quorum"` — bad panel.
    /// - `"DuplicateArbiter"` — an arbiter address appears more than once.
    /// - `"CompanyArbiterCollision"` — `company` is in the arbiter set.
    pub fn create_engagement_bundle(
        env: Env,
        company: Address,
        bundle_id: String,
        arbiters: Vec<Address>,
        quorum: u32,
    ) {
        Self::assert_not_paused(&env);
        company.require_auth();

        if bundle_id.is_empty() || bundle_id.len() > MAX_ENGAGEMENT_ID_LENGTH {
            panic!("InvalidBundleId");
        }
        let key = DataKey2::Bundle(bundle_id.clone());
        if env.storage().persistent().has(&key) {
            panic!("BundleAlreadyExists");
        }
        if arbiters.is_empty() {
            panic!("at least one arbiter required");
        }
        if quorum == 0 || quorum > arbiters.len() {
            panic!("invalid quorum");
        }
        for i in 0..arbiters.len() {
            let a = arbiters.get(i).unwrap();
            if a == company {
                panic!("CompanyArbiterCollision");
            }
            for j in (i + 1)..arbiters.len() {
                if arbiters.get(j).unwrap() == a {
                    panic!("DuplicateArbiter");
                }
            }
        }

        env.storage().persistent().set(
            &key,
            &EngagementBundle {
                company: company.clone(),
                arbiters,
                quorum,
            },
        );
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "bundle_created"), bundle_id),
            (company, quorum),
        );
    }

    /// Return the bundle's shared panel configuration, or `None` if unregistered.
    pub fn get_bundle(env: Env, bundle_id: String) -> Option<EngagementBundle> {
        env.storage().persistent().get(&DataKey2::Bundle(bundle_id))
    }

    /// Return a page of engagement IDs created under `bundle_id`, in creation
    /// order. `page` is 0-indexed; a `page_size` of 0, a page past the end, or
    /// an unknown bundle returns an empty vec.
    pub fn get_bundle_engagements(
        env: Env,
        bundle_id: String,
        page: u32,
        page_size: u32,
    ) -> Vec<String> {
        let ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&DataKey2::BundleEngagements(bundle_id))
            .unwrap_or_else(|| Vec::new(&env));

        let total = ids.len();
        let start = page.saturating_mul(page_size);
        if page_size == 0 || start >= total {
            return Vec::new(&env);
        }
        let end = start.saturating_add(page_size).min(total);
        let mut result = Vec::new(&env);
        for i in start..end {
            result.push_back(ids.get(i).unwrap());
        }
        result
    }

    /// Resolve the bundle an engagement is joining, checking it exists and
    /// belongs to `company`.
    #[allow(dead_code)]
    pub(crate) fn load_bundle_for(env: &Env, bundle_id: &String, company: &Address) -> EngagementBundle {
        let bundle: EngagementBundle = env
            .storage()
            .persistent()
            .get(&DataKey2::Bundle(bundle_id.clone()))
            .unwrap_or_else(|| panic!("BundleNotFound"));
        if bundle.company != *company {
            panic!("BundleCompanyMismatch");
        }
        bundle
    }

    /// Append a newly created engagement to its bundle's member index.
    pub(crate) fn add_bundle_member(env: &Env, bundle_id: &String, engagement_id: &String) {
        let key = DataKey2::BundleEngagements(bundle_id.clone());
        let mut ids: Vec<String> = env
            .storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(env));
        ids.push_back(engagement_id.clone());
        env.storage().persistent().set(&key, &ids);
        env.storage().persistent().extend_ttl(&key, 100_000, 6_300_000);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{
        testutils::{Address as _, Events, Ledger, MockAuth, MockAuthInvoke},
        token, vec, Address, Env, IntoVal, String, Symbol, Vec,
    };

    fn setup() -> (Env, Address, Address, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();

        env.ledger().set(soroban_sdk::testutils::LedgerInfo {
            timestamp: 0,
            protocol_version: 22,
            sequence_number: 100,
            network_id: Default::default(),
            base_reserve: 5_000_000,
            min_temp_entry_ttl: 16,
            min_persistent_entry_ttl: 6_300_000,
            max_entry_ttl: 6_300_000,
        });

        let contract_id = env.register(HireSettleContract, ());
        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();
        let token_client = token::StellarAssetClient::new(&env, &token_id);
        let company = Address::generate(&env);
        let recruiter = Address::generate(&env);
        let arbiter = Address::generate(&env);

        token_client.mint(&company, &500_000_000_000);
        token_client.mint(&recruiter, &100_000_000);

        let client = HireSettleContractClient::new(&env, &contract_id);
        client.init(&company);

        (env, contract_id, token_id, company, recruiter, arbiter)
    }

    fn build_milestones(env: &Env) -> Vec<Milestone> {
        vec![
            env,
            Milestone {
                name: String::from_str(env, "Placement"),
                payment_percent: 50,
                kind: MilestoneKind::Placement,
                valid_after_ledger: 0,
                proof_hash: String::from_str(env, ""),
                status: MilestoneStatus::Pending,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
                prerequisites: Vec::new(env),
            },
            Milestone {
                name: String::from_str(env, "Retention"),
                payment_percent: 50,
                kind: MilestoneKind::Retention,
                valid_after_ledger: 0,
                proof_hash: String::from_str(env, ""),
                status: MilestoneStatus::Locked,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
                prerequisites: vec![env, 0u32],
            },
        ]
    }

    fn default_config() -> EngagementConfig {
        EngagementConfig {
            metadata_hash: None,
            co_recruiter: None,
            recruiter_split_bps: 10_000,
            contract_pdf_hash: None,
            referrer: None,
            tags: None,
            is_public: false,
            stream_duration_ledgers: None,
            recruiter_bond_amount: None,
            bundle_id: None,
            insured: false,
            fund_from_pool: false,
            snapshot_fee_tier: false,
            co_recruiter_bond_amount: None,
            pull_payouts: false,
            require_recruiter_acceptance: false,
            end_ledger: None,
            placement_oracle: None,
            external_ref: None,
        }
    }

    // ============================================================
    // TEST 1: Recruiter bond escrow and full refund on clean completion
    // ============================================================
    #[test]
    fn test_recruiter_bond_full_refund_on_clean_completion() {
        let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
        let client = HireSettleContractClient::new(&env, &contract_id);

        let bond_amount = 10_000_000i128;
        let mut config = default_config();
        config.recruiter_bond_amount = Some(bond_amount);

        // Create engagement with recruiter bond
        client.create_engagement(
            &String::from_str(&env, "eng-001"),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
                weights: None,
            },
            &token_id,
            &100_000_000,
            &String::from_str(&env, "Engineer"),
            &build_milestones(&env),
            &vec![&env, 30u32],
            &config,
        );

        // Verify bond was escrowed
        let bond = client.get_recruiter_bond(&String::from_str(&env, "eng-001"));
        assert_eq!(bond, Some((bond_amount, false)));

        let token_client = token::Client::new(&env, &token_id);
        let recruiter_balance_before = token_client.balance(&recruiter);

        // Complete the engagement cleanly (submit and confirm milestone)
        client.submit_proof(
            &String::from_str(&env, "eng-001"),
            &0u32,
            &String::from_str(&env, "proof-hash"),
        );
        client.confirm_milestone(&String::from_str(&env, "eng-001"), &0u32, &company);

        // Bond should be fully refunded
        let bond_after = client.get_recruiter_bond(&String::from_str(&env, "eng-001"));
        assert_eq!(bond_after, Some((bond_amount, false)));

        let recruiter_balance_after = token_client.balance(&recruiter);
        assert_eq!(recruiter_balance_after, recruiter_balance_before + bond_amount);
    }

    // ============================================================
    // TEST 2: Recruiter bond partial forfeit with custom forfeit BPS
    // ============================================================
    #[test]
    fn test_recruiter_bond_partial_forfeit_custom_bps() {
        let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
        let client = HireSettleContractClient::new(&env, &contract_id);

        // Set bond forfeit to 60% (6000 bps)
        client.set_bond_forfeit_bps(&company, &6000u32);
        assert_eq!(client.get_bond_forfeit_bps(), 6000u32);

        let bond_amount = 10_000_000i128;
        let mut config = default_config();
        config.recruiter_bond_amount = Some(bond_amount);

        client.create_engagement(
            &String::from_str(&env, "eng-002"),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
                weights: None,
            },
            &token_id,
            &100_000_000,
            &String::from_str(&env, "Engineer"),
            &build_milestones(&env),
            &vec![&env, 30u32],
            &config,
        );

        let token_client = token::Client::new(&env, &token_id);
        let company_balance_before = token_client.balance(&company);
        let recruiter_balance_before = token_client.balance(&recruiter);

        // Submit proof, raise dispute, and resolve against recruiter
        client.submit_proof(
            &String::from_str(&env, "eng-002"),
            &0u32,
            &String::from_str(&env, "bad-proof"),
        );
        client.raise_dispute(
            &String::from_str(&env, "eng-002"),
            &0u32,
            &company,
            &String::from_str(&env, "Invalid proof"),
        );
        client.cast_vote(
            &String::from_str(&env, "eng-002"),
            &0u32,
            &arbiter,
            &false,
        );

        // Cancel engagement to trigger settlement
        client.cancel_engagement(&String::from_str(&env, "eng-002"), &company);

        // Verify forfeit: 60% to company, 40% to recruiter
        let expected_forfeit = (bond_amount * 6000) / 10_000;
        let expected_refund = bond_amount - expected_forfeit;

        let company_balance_after = token_client.balance(&company);
        let recruiter_balance_after = token_client.balance(&recruiter);

        assert_eq!(
            company_balance_after - company_balance_before,
            expected_forfeit
        );
        assert_eq!(
            recruiter_balance_after - recruiter_balance_before,
            expected_refund
        );

        let bond_after = client.get_recruiter_bond(&String::from_str(&env, "eng-002"));
        assert_eq!(bond_after, Some((bond_amount, true)));
    }

    // ============================================================
    // TEST 3: Co-recruiter bond with pro-rata forfeit calculation
    // ============================================================
    #[test]
    fn test_co_recruiter_bond_pro_rata_forfeit() {
        let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
        let client = HireSettleContractClient::new(&env, &contract_id);
        let co_recruiter = Address::generate(&env);
        let token_client = token::Client::new(&env, &token_id);

        token_client.mint(&co_recruiter, &100_000_000);

        let recruiter_bond = 10_000_000i128;
        let co_recruiter_bond = 5_000_000i128;
        let recruiter_split_bps = 7000u32; // Recruiter gets 70%, co-recruiter gets 30%

        let mut config = default_config();
        config.recruiter_bond_amount = Some(recruiter_bond);
        config.co_recruiter = Some(co_recruiter.clone());
        config.co_recruiter_bond_amount = Some(co_recruiter_bond);
        config.recruiter_split_bps = recruiter_split_bps;

        client.create_engagement(
            &String::from_str(&env, "eng-003"),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
                weights: None,
            },
            &token_id,
            &100_000_000,
            &String::from_str(&env, "Engineer"),
            &build_milestones(&env),
            &vec![&env, 30u32],
            &config,
        );

        // Verify both bonds escrowed
        assert_eq!(
            client.get_recruiter_bond(&String::from_str(&env, "eng-003")),
            Some((recruiter_bond, false))
        );
        assert_eq!(
            client.get_co_recruiter_bond(&String::from_str(&env, "eng-003")),
            Some((co_recruiter_bond, false))
        );

        let company_balance_before = token_client.balance(&company);
        let co_recruiter_balance_before = token_client.balance(&co_recruiter);

        // Trigger forfeit condition
        client.submit_proof(
            &String::from_str(&env, "eng-003"),
            &0u32,
            &String::from_str(&env, "bad-proof"),
        );
        client.raise_dispute(
            &String::from_str(&env, "eng-003"),
            &0u32,
            &company,
            &String::from_str(&env, "Invalid"),
        );
        client.cast_vote(&String::from_str(&env, "eng-003"), &0u32, &arbiter, &false);
        client.cancel_engagement(&String::from_str(&env, "eng-003"), &company);

        // Co-recruiter forfeit = bond * forfeit_bps * co_share_bps / (10_000^2)
        // co_share_bps = 10_000 - 7_000 = 3_000
        // forfeit = 5_000_000 * 10_000 * 3_000 / 100_000_000 = 1_500_000
        let co_forfeit = (co_recruiter_bond * 10_000 * 3_000) / 100_000_000;
        let co_refund = co_recruiter_bond - co_forfeit;

        let company_balance_after = token_client.balance(&company);
        let co_recruiter_balance_after = token_client.balance(&co_recruiter);

        // Company receives recruiter forfeit + co-recruiter forfeit
        let total_company_gain = company_balance_after - company_balance_before;
        assert!(total_company_gain >= co_forfeit);

        assert_eq!(
            co_recruiter_balance_after - co_recruiter_balance_before,
            co_refund
        );

        assert_eq!(
            client.get_co_recruiter_bond(&String::from_str(&env, "eng-003")),
            Some((co_recruiter_bond, true))
        );
    }

    // ============================================================
    // TEST 4: Bundle creation and engagement association
    // ============================================================
    #[test]
    fn test_bundle_creation_and_engagement_tracking() {
        let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
        let client = HireSettleContractClient::new(&env, &contract_id);

        let arbiter2 = Address::generate(&env);
        let arbiters = vec![&env, arbiter.clone(), arbiter2.clone()];

        // Create a bundle
        client.create_engagement_bundle(
            &company,
            &String::from_str(&env, "bundle-alpha"),
            &arbiters,
            &2u32,
        );

        // Verify bundle exists
        let bundle = client.get_bundle(&String::from_str(&env, "bundle-alpha"));
        assert!(bundle.is_some());
        let bundle_data = bundle.unwrap();
        assert_eq!(bundle_data.company, company);
        assert_eq!(bundle_data.quorum, 2);
        assert_eq!(bundle_data.arbiters.len(), 2);

        // Create engagements under the bundle
        let mut config = default_config();
        config.bundle_id = Some(String::from_str(&env, "bundle-alpha"));

        for i in 0..3 {
            let eng_id = format!("eng-bundle-{}", i);
            client.create_engagement(
                &String::from_str(&env, &eng_id),
                &company,
                &recruiter,
                &ArbiterSetup {
                    arbiters: arbiters.clone(),
                    quorum: 2,
                    weights: None,
                },
                &token_id,
                &100_000_000,
                &String::from_str(&env, "Engineer"),
                &build_milestones(&env),
                &vec![&env, 30u32],
                &config,
            );
        }

        // Verify bundle tracks all engagements
        let bundle_engagements =
            client.get_bundle_engagements(&String::from_str(&env, "bundle-alpha"), &0u32, &10u32);
        assert_eq!(bundle_engagements.len(), 3);

        // Test pagination
        let page_0 = client.get_bundle_engagements(
            &String::from_str(&env, "bundle-alpha"),
            &0u32,
            &2u32,
        );
        assert_eq!(page_0.len(), 2);

        let page_1 = client.get_bundle_engagements(
            &String::from_str(&env, "bundle-alpha"),
            &1u32,
            &2u32,
        );
        assert_eq!(page_1.len(), 1);
    }

    // ============================================================
    // TEST 5: Bond rejection tracking across multiple milestones
    // ============================================================
    #[test]
    fn test_bond_rejection_tracking_multiple_milestones() {
        let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
        let client = HireSettleContractClient::new(&env, &contract_id);

        let bond_amount = 10_000_000i128;
        let mut config = default_config();
        config.recruiter_bond_amount = Some(bond_amount);

        // Create engagement with multiple milestones
        let milestones = vec![
            &env,
            Milestone {
                name: String::from_str(&env, "M1"),
                payment_percent: 30,
                kind: MilestoneKind::Placement,
                valid_after_ledger: 0,
                proof_hash: String::from_str(&env, ""),
                status: MilestoneStatus::Pending,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
                prerequisites: Vec::new(&env),
            },
            Milestone {
                name: String::from_str(&env, "M2"),
                payment_percent: 40,
                kind: MilestoneKind::Retention,
                valid_after_ledger: 0,
                proof_hash: String::from_str(&env, ""),
                status: MilestoneStatus::Locked,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
                prerequisites: vec![&env, 0u32],
            },
            Milestone {
                name: String::from_str(&env, "M3"),
                payment_percent: 30,
                kind: MilestoneKind::Retention,
                valid_after_ledger: 0,
                proof_hash: String::from_str(&env, ""),
                status: MilestoneStatus::Locked,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
                prerequisites: vec![&env, 1u32],
            },
        ];

        client.create_engagement(
            &String::from_str(&env, "eng-004"),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
                weights: None,
            },
            &token_id,
            &100_000_000,
            &String::from_str(&env, "Engineer"),
            &milestones,
            &vec![&env, 30u32, 60u32],
            &config,
        );

        // M1: Submit, dispute, reject
        client.submit_proof(
            &String::from_str(&env, "eng-004"),
            &0u32,
            &String::from_str(&env, "proof1"),
        );
        client.raise_dispute(
            &String::from_str(&env, "eng-004"),
            &0u32,
            &company,
            &String::from_str(&env, "Bad"),
        );
        client.cast_vote(&String::from_str(&env, "eng-004"), &0u32, &arbiter, &false);

        // M1: Resubmit and confirm (clears rejection)
        client.submit_proof(
            &String::from_str(&env, "eng-004"),
            &0u32,
            &String::from_str(&env, "proof1-fixed"),
        );
        client.confirm_milestone(&String::from_str(&env, "eng-004"), &0u32, &company);

        // M2: Submit, dispute, reject (not cleared)
        client.submit_proof(
            &String::from_str(&env, "eng-004"),
            &1u32,
            &String::from_str(&env, "proof2"),
        );
        client.raise_dispute(
            &String::from_str(&env, "eng-004"),
            &1u32,
            &company,
            &String::from_str(&env, "Bad"),
        );
        client.cast_vote(&String::from_str(&env, "eng-004"), &1u32, &arbiter, &false);

        let token_client = token::Client::new(&env, &token_id);
        let company_balance_before = token_client.balance(&company);

        // Cancel to trigger settlement
        client.cancel_engagement(&String::from_str(&env, "eng-004"), &company);

        // Bond should be forfeited because M2 rejection was never cleared
        let bond_after = client.get_recruiter_bond(&String::from_str(&env, "eng-004"));
        assert_eq!(bond_after, Some((bond_amount, true)));

        let company_balance_after = token_client.balance(&company);
        let company_gain = company_balance_after - company_balance_before;
        
        // With default 100% forfeit, company should receive the full bond
        assert_eq!(company_gain, bond_amount);
    }
}
