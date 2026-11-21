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
