#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env, String, Vec, Symbol,
};

// ============================================================
// DATA TYPES
// ============================================================

#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum MilestoneStatus {
    Locked,
    Pending,
    ProofSubmitted,
    Confirmed,
    Disputed,
    Resolved,
}

#[contracttype]
#[derive(Clone, PartialEq)]
pub enum MilestoneKind {
    Placement,
    Retention,
}

#[contracttype]
#[derive(Clone)]
pub struct Milestone {
    pub name: String,
    pub payment_percent: u32,
    pub kind: MilestoneKind,
    pub valid_after_ledger: u32,
    pub proof_hash: String,
    pub status: MilestoneStatus,
}

#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum EngagementStatus {
    Active,
    Completed,
    Cancelled,
    ReplacementRequested,
}

/// A history entry for a milestone amendment
#[contracttype]
#[derive(Clone)]
pub struct AmendmentEntry {
    /// The party who proposed the amendment
    pub proposer: Address,
    /// Previous payment percentage
    pub old_payment_percent: u32,
    /// New payment percentage
    pub new_payment_percent: u32,
    /// Ledger when the amendment was accepted
    pub ledger: u32,
}

/// A pending amendment proposal for a milestone
#[contracttype]
#[derive(Clone)]
pub struct AmendmentProposal {
    /// The party who proposed the amendment
    pub proposer: Address,
    /// New payment percentage being proposed
    pub new_payment_percent: u32,
    /// Ledger when the proposal was made
    pub proposed_at_ledger: u32,
    /// Ledger at which the proposal expires if not accepted
    pub expires_at_ledger: u32,
}

/// The full engagement record stored on-chain
#[contracttype]
#[derive(Clone)]
pub struct Engagement {
    pub id: String,
    pub company: Address,
    pub recruiter: Address,
    /// Ordered list of arbiters; quorum of these must agree to resolve a dispute.
    pub arbiters: Vec<Address>,
    /// Number of arbiter votes required to resolve a dispute (M of N).
    pub quorum: u32,
    pub token: Address,
    pub total_amount: i128,
    pub released_amount: i128,
    pub job_title: String,
    /// Optional IPFS CID linking to full job description / contract terms off-chain.
    pub metadata_hash: Option<String>,
    pub created_at_ledger: u32,
    pub milestones: Vec<Milestone>,
    pub status: EngagementStatus,
}

#[contracttype]
#[derive(Clone)]
pub struct EngagementSummary {
    pub id: String,
    pub job_title: String,
    pub company: Address,
    pub recruiter: Address,
    pub total_amount: i128,
    pub released_amount: i128,
    pub status: EngagementStatus,
    pub milestone_count: u32,
    pub created_at_ledger: u32,
}

/// Per-dispute vote tally stored on-chain until the dispute resolves.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterVoteRecord {
    pub approve_votes: u32,
    pub reject_votes: u32,
    pub voted: Vec<Address>,
}

/// Returned by `get_arbiter_votes`.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterVoteCounts {
    pub approve_votes: u32,
    pub reject_votes: u32,
}

/// Bundles the arbiter list and quorum for `create_engagement`, keeping the
/// parameter count within Soroban's 10-parameter limit.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterSetup {
    pub arbiters: Vec<Address>,
    pub quorum: u32,
}

/// Stored under `DataKey::PendingArbiter` during succession.
#[contracttype]
#[derive(Clone)]
pub struct ArbiterNomination {
    pub current: Address,
    pub nominee: Address,
}

// ============================================================
// STORAGE KEYS
// ============================================================

#[contracttype]
pub enum DataKey {
    Engagement(String),
    Admin,
    /// Pending arbiter succession nomination for an engagement.
    PendingArbiter(String),
    /// Admin-configurable proof resubmission cooldown in ledgers (default 2 880).
    ProofCooldown,
    /// Ledger at which the last proof was submitted for (engagement_id, milestone_index).
    LastProofAt(String, u32),
    /// Running vote tally for a disputed (engagement_id, milestone_index).
    ArbiterVotes(String, u32),
    AmendmentProposal(String, u32),
    AmendmentLog(String, u32),
    AmendmentTTL,
}

// ============================================================
// CONTRACT
// ============================================================

#[contract]
pub struct HireSettleContract;

const LEDGERS_PER_DAY: u32 = 17_280;        // 86 400s ÷ 5s per ledger
const DEFAULT_PROOF_COOLDOWN: u32 = 2_880;   // ~4 hours

#[contractimpl]
impl HireSettleContract {

    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    pub fn init(env: Env, admin: Address) {
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
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
            panic!("unauthorized");
        }
        env.storage().instance().set(&DataKey::ProofCooldown, &ledgers);
    }

    fn get_proof_cooldown(env: &Env) -> u32 {
        env.storage()
            .instance()
            .get(&DataKey::ProofCooldown)
            .unwrap_or(DEFAULT_PROOF_COOLDOWN)
    }

    // ----------------------------------------------------------
    // CREATE ENGAGEMENT
    // ----------------------------------------------------------

    /// Create a new recruitment engagement and lock USDC in escrow.
    ///
    /// # Arguments
    /// - `engagement_id`   — unique string ID for this engagement
    /// - `company`         — company address (must sign this tx)
    /// - `recruiter`       — recruiter address (receives payments)
    /// - `arbiters`        — ordered list of arbiter addresses (min 1)
    /// - `quorum`          — number of arbiter approvals required to release on dispute (M of N)
    /// - `token`           — USDC Stellar Asset Contract address
    /// - `total_amount`    — total recruiter fee in stroops
    /// - `job_title`       — short job title string
    /// - `milestones`      — ordered milestone list
    /// - `retention_days`  — Vec of retention windows in days (one per Retention milestone)
    /// - `metadata_hash`   — optional IPFS CID for off-chain job description; empty string rejected
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
        metadata_hash: Option<String>,
    ) -> String {
        company.require_auth();

        if total_amount <= 0 {
            panic!("amount must be greater than zero");
        }

        let arbiters = arbiter_setup.arbiters;
        let quorum = arbiter_setup.quorum;

        if arbiters.is_empty() {
            panic!("at least one arbiter required");
        }

        if quorum == 0 || quorum > arbiters.len() {
            panic!("invalid quorum");
        }

        // Reject empty metadata hash — caller must either omit or provide a real CID.
        if let Some(ref hash) = metadata_hash {
            if hash.len() == 0 {
                panic!("InvalidMetadataHash");
            }
        }

        let mut total_percent: u32 = 0;
        for i in 0..milestones.len() {
            total_percent += milestones.get(i).unwrap().payment_percent;
        }
        if total_percent != 100 {
            panic!("milestone percentages must sum to 100");
        }

        if env
            .storage()
            .persistent()
            .has(&DataKey::Engagement(engagement_id.clone()))
        {
            panic!("engagement already exists");
        }

        let current_ledger = env.ledger().sequence();
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
                    m.valid_after_ledger = current_ledger + (days * LEDGERS_PER_DAY);
                    m.status = MilestoneStatus::Locked;
                }
            }
            resolved_milestones.push_back(m);
        }

        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&company, &env.current_contract_address(), &total_amount);

        let engagement = Engagement {
            id: engagement_id.clone(),
            company,
            recruiter,
            arbiters,
            quorum,
            token,
            total_amount,
            released_amount: 0,
            job_title,
            metadata_hash,
            created_at_ledger: current_ledger,
            milestones: resolved_milestones,
            status: EngagementStatus::Active,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.storage()
            .persistent()
            .extend_ttl(&DataKey::Engagement(engagement_id.clone()), 100_000, 6_300_000);

        env.events().publish(
            (Symbol::new(&env, "engagement_created"), engagement_id.clone()),
            engagement_id.clone(),
        );

        engagement_id
    }

    // ----------------------------------------------------------
    // UNLOCK RETENTION MILESTONE
    // ----------------------------------------------------------

    pub fn unlock_milestone(env: Env, engagement_id: String, milestone_index: u32) {
        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("engagement is not active");
        }

        let mut milestone = engagement.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::Locked {
            panic!("milestone is not locked");
        }

        if milestone.kind != MilestoneKind::Retention {
            panic!("only retention milestones can be unlocked this way");
        }

        let current_ledger = env.ledger().sequence();
        if current_ledger < milestone.valid_after_ledger {
            panic!("retention window has not elapsed yet");
        }

        milestone.status = MilestoneStatus::Pending;
        engagement.milestones.set(milestone_index, milestone);

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (Symbol::new(&env, "milestone_unlocked"), engagement_id.clone()),
            milestone_index,
        );
    }

    // ----------------------------------------------------------
    // SUBMIT PROOF
    // ----------------------------------------------------------

    /// Recruiter submits a proof document for a Pending milestone.
    /// Rejects with `ResubmitTooSoon` if called again within the configured
    /// cooldown window after the previous submission on the same milestone.
    pub fn submit_proof(
        env: Env,
        recruiter: Address,
        engagement_id: String,
        milestone_index: u32,
        proof_hash: String,
    ) {
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("engagement is not active");
        }

        if recruiter != engagement.recruiter {
            panic!("unauthorized");
        }

        let mut milestone = engagement.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::Pending {
            panic!("milestone is not pending");
        }

        // Rate-limit resubmissions — first submission (no stored ledger) is always allowed.
        let last_key = DataKey::LastProofAt(engagement_id.clone(), milestone_index);
        let current_ledger = env.ledger().sequence();
        if let Some(last_at) = env.storage().persistent().get::<DataKey, u32>(&last_key) {
            let cooldown = Self::get_proof_cooldown(&env);
            if current_ledger < last_at + cooldown {
                panic!("ResubmitTooSoon");
            }
        }

        // Record this submission ledger for future cooldown checks.
        env.storage().persistent().set(&last_key, &current_ledger);
        env.storage()
            .persistent()
            .extend_ttl(&last_key, 100_000, 6_300_000);

        milestone.proof_hash = proof_hash;
        milestone.status = MilestoneStatus::ProofSubmitted;
        engagement.milestones.set(milestone_index, milestone);

        if engagement.status == EngagementStatus::ReplacementRequested {
            engagement.status = EngagementStatus::Active;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (Symbol::new(&env, "proof_submitted"), engagement_id.clone()),
            milestone_index,
        );
    }

    // ----------------------------------------------------------
    // CONFIRM MILESTONE
    // ----------------------------------------------------------

    pub fn confirm_milestone(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("engagement is not active");
        }

        if company != engagement.company {
            panic!("unauthorized");
        }

        let mut milestone = engagement.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("milestone proof not yet submitted");
        }

        if milestone.kind == MilestoneKind::Retention {
            let current_ledger = env.ledger().sequence();
            if current_ledger < milestone.valid_after_ledger {
                panic!("retention window has not elapsed — cannot confirm yet");
            }
        }

        let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        engagement.released_amount += payment;

        let token_client = token::Client::new(&env, &engagement.token);
        token_client.transfer(
            &env.current_contract_address(),
            &engagement.recruiter,
            &payment,
        );

        milestone.status = MilestoneStatus::Confirmed;
        engagement.milestones.set(milestone_index, milestone.clone());

        let all_done = (0..engagement.milestones.len()).all(|i| {
            let s = engagement.milestones.get(i).unwrap().status;
            s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
        });

        if all_done {
            engagement.status = EngagementStatus::Completed;
        }

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (Symbol::new(&env, "milestone_confirmed"), engagement_id.clone()),
            (milestone_index, payment),
        );
    }

    // ----------------------------------------------------------
    // RAISE DISPUTE
    // ----------------------------------------------------------

    pub fn raise_dispute(
        env: Env,
        company: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("engagement is not active");
        }

        if company != engagement.company {
            panic!("unauthorized");
        }

        let mut milestone = engagement.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::ProofSubmitted {
            panic!("can only dispute a submitted proof");
        }

        milestone.status = MilestoneStatus::Disputed;
        engagement.milestones.set(milestone_index, milestone);

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (Symbol::new(&env, "dispute_raised"), engagement_id.clone()),
            milestone_index,
        );
    }

    // ----------------------------------------------------------
    // CAST ARBITER VOTE  (#10 multi-arbiter quorum)
    // ----------------------------------------------------------

    /// Each arbiter calls this to cast their vote on a Disputed milestone.
    /// The dispute resolves automatically once either:
    ///   - `approve_votes >= quorum`  → payment released, milestone → Resolved
    ///   - `reject_votes > arbiters.len() - quorum`  → proof cleared, milestone → Pending
    ///
    /// Duplicate votes from the same arbiter are rejected.
    pub fn cast_arbiter_vote(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        milestone_index: u32,
        approve: bool,
    ) {
        arbiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("engagement is not active");
        }

        let is_arbiter = (0..engagement.arbiters.len())
            .any(|i| engagement.arbiters.get(i).unwrap() == arbiter);
        if !is_arbiter {
            panic!("unauthorized");
        }

        let mut milestone = engagement.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        let vote_key = DataKey::ArbiterVotes(engagement_id.clone(), milestone_index);
        let mut record: ArbiterVoteRecord = env
            .storage()
            .persistent()
            .get(&vote_key)
            .unwrap_or(ArbiterVoteRecord {
                approve_votes: 0,
                reject_votes: 0,
                voted: Vec::new(&env),
            });

        // Reject duplicate votes.
        for i in 0..record.voted.len() {
            if record.voted.get(i).unwrap() == arbiter {
                panic!("duplicate vote");
            }
        }

        record.voted.push_back(arbiter.clone());
        if approve {
            record.approve_votes += 1;
        } else {
            record.reject_votes += 1;
        }

        let total_arbiters = engagement.arbiters.len();
        let quorum = engagement.quorum;

        env.events().publish(
            (Symbol::new(&env, "arbiter_voted"), engagement_id.clone()),
            (milestone_index, approve),
        );

        if record.approve_votes >= quorum {
            let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
            engagement.released_amount += payment;

            let token_client = token::Client::new(&env, &engagement.token);
            token_client.transfer(
                &env.current_contract_address(),
                &engagement.recruiter,
                &payment,
            );

            milestone.status = MilestoneStatus::Resolved;
            engagement.milestones.set(milestone_index, milestone);

            let all_done = (0..engagement.milestones.len()).all(|i| {
                let s = engagement.milestones.get(i).unwrap().status;
                s == MilestoneStatus::Confirmed || s == MilestoneStatus::Resolved
            });
            if all_done {
                engagement.status = EngagementStatus::Completed;
            }

            env.storage().persistent().remove(&vote_key);

            env.events().publish(
                (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
                (milestone_index, true),
            );
        } else if record.reject_votes > total_arbiters - quorum {
            milestone.status = MilestoneStatus::Pending;
            milestone.proof_hash = String::from_str(&env, "");
            engagement.milestones.set(milestone_index, milestone);

            env.storage().persistent().remove(&vote_key);

            env.events().publish(
                (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
                (milestone_index, false),
            );
        } else {
            env.storage().persistent().set(&vote_key, &record);
            env.storage()
                .persistent()
                .extend_ttl(&vote_key, 100_000, 6_300_000);
        }

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);
    }

    // ----------------------------------------------------------
    // REQUEST REPLACEMENT
    // ----------------------------------------------------------

    pub fn request_replacement(env: Env, company: Address, engagement_id: String) {
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("engagement is not active");
        }

        if company != engagement.company {
            panic!("unauthorized");
        }

        let placement_confirmed = {
            let m0 = engagement.milestones.get(0).unwrap();
            m0.status == MilestoneStatus::Confirmed || m0.status == MilestoneStatus::Resolved
        };

        if !placement_confirmed {
            panic!("placement not yet confirmed — use cancel_engagement instead");
        }

        let current_ledger = env.ledger().sequence();

        for i in 0..engagement.milestones.len() {
            let mut m = engagement.milestones.get(i).unwrap();
            match m.kind {
                MilestoneKind::Placement => {
                    if m.status == MilestoneStatus::Confirmed
                        || m.status == MilestoneStatus::Resolved
                    {
                        m.status = MilestoneStatus::Pending;
                        m.proof_hash = String::from_str(&env, "");
                        // Clear cooldown so the replacement candidate can submit immediately.
                        env.storage()
                            .persistent()
                            .remove(&DataKey::LastProofAt(engagement_id.clone(), i));
                    }
                }
                MilestoneKind::Retention => {
                    if m.status != MilestoneStatus::Confirmed
                        && m.status != MilestoneStatus::Resolved
                    {
                        let original_days =
                            (m.valid_after_ledger - engagement.created_at_ledger) / LEDGERS_PER_DAY;
                        m.valid_after_ledger = current_ledger + (original_days * LEDGERS_PER_DAY);
                        m.status = MilestoneStatus::Locked;
                        m.proof_hash = String::from_str(&env, "");
                        env.storage()
                            .persistent()
                            .remove(&DataKey::LastProofAt(engagement_id.clone(), i));
                    }
                }
            }
            engagement.milestones.set(i, m);
        }

        engagement.status = EngagementStatus::ReplacementRequested;

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (Symbol::new(&env, "replacement_requested"), engagement_id.clone()),
            engagement_id.clone(),
        );
    }

    // ----------------------------------------------------------
    // CANCEL ENGAGEMENT
    // ----------------------------------------------------------

    pub fn cancel_engagement(
        env: Env,
        company: Address,
        recruiter: Address,
        engagement_id: String,
    ) {
        company.require_auth();
        recruiter.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active
            && engagement.status != EngagementStatus::ReplacementRequested
        {
            panic!("engagement is not active");
        }

        if company != engagement.company {
            panic!("unauthorized");
        }

        if recruiter != engagement.recruiter {
            panic!("unauthorized");
        }

        let refund = engagement.total_amount - engagement.released_amount;
        let token_client = token::Client::new(&env, &engagement.token);
        token_client.transfer(
            &env.current_contract_address(),
            &engagement.company,
            &refund,
        );

        engagement.status = EngagementStatus::Cancelled;

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (Symbol::new(&env, "engagement_cancelled"), engagement_id.clone()),
            refund,
        );
    }

    // ----------------------------------------------------------
    // READ-ONLY QUERIES
    // ----------------------------------------------------------

    pub fn get_engagement(env: Env, engagement_id: String) -> Engagement {
        Self::get_engagement_internal(&env, &engagement_id)
    }

    pub fn get_milestone(env: Env, engagement_id: String, milestone_index: u32) -> Milestone {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"))
    }

    pub fn get_escrow_balance(env: Env, engagement_id: String) -> i128 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.total_amount - engagement.released_amount
    }

    pub fn is_milestone_unlockable(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> bool {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"));

        milestone.status == MilestoneStatus::Locked
            && env.ledger().sequence() >= milestone.valid_after_ledger
    }

    pub fn ledgers_until_unlock(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> u32 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"));

        let current = env.ledger().sequence();
        if current >= milestone.valid_after_ledger {
            0
        } else {
            milestone.valid_after_ledger - current
        }
    }

    /// Returns approximate seconds until a Locked retention milestone unlocks.
    /// Returns 0 if the milestone is already unlockable or is a Placement milestone.
    pub fn get_estimated_unlock_seconds(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> u64 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"));

        if milestone.kind == MilestoneKind::Placement {
            return 0;
        }

        let current = env.ledger().sequence();
        if current >= milestone.valid_after_ledger {
            return 0;
        }

        let ledgers_remaining = (milestone.valid_after_ledger - current) as u64;
        // LEDGERS_PER_DAY = 86_400 / 5, so seconds_per_ledger = 86_400 / LEDGERS_PER_DAY = 5
        ledgers_remaining * (86_400u64 / LEDGERS_PER_DAY as u64)
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
            .unwrap_or(ArbiterVoteRecord {
                approve_votes: 0,
                reject_votes: 0,
                voted: Vec::new(&env),
            });
        ArbiterVoteCounts {
            approve_votes: record.approve_votes,
            reject_votes: record.reject_votes,
        }
    }

    pub fn get_total_released(env: Env, engagement_id: String) -> i128 {
        Self::get_engagement_internal(&env, &engagement_id).released_amount
    }

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
        }
    }

    // ----------------------------------------------------------
    // ARBITER SUCCESSION
    // ----------------------------------------------------------

    /// Current arbiter nominates a successor. The successor must call `claim_arbiter`.
    /// Any arbiter in the engagement's arbiter list may initiate succession for their slot.
    pub fn nominate_arbiter_successor(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        successor: Address,
    ) {
        arbiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        let is_arbiter = (0..engagement.arbiters.len())
            .any(|i| engagement.arbiters.get(i).unwrap() == arbiter);
        if !is_arbiter {
            panic!("unauthorized");
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
            (Symbol::new(&env, "arbiter_nominated"), engagement_id.clone()),
            successor,
        );
    }

    /// Nominated successor claims the arbiter slot, replacing the nominating arbiter.
    pub fn claim_arbiter(env: Env, nominee: Address, engagement_id: String) {
        nominee.require_auth();

        let nomination: ArbiterNomination = env
            .storage()
            .persistent()
            .get(&DataKey::PendingArbiter(engagement_id.clone()))
            .unwrap_or_else(|| panic!("no pending arbiter nomination"));

        if nominee != nomination.nominee {
            panic!("unauthorized");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        // Replace the nominating arbiter's slot with the nominee.
        for i in 0..engagement.arbiters.len() {
            if engagement.arbiters.get(i).unwrap() == nomination.current {
                engagement.arbiters.set(i, nominee.clone());
                break;
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
    // AMENDMENT PROPOSAL MANAGEMENT
    // ----------------------------------------------------------

    /// Admin sets the amendment proposal TTL in ledgers.
    /// Default is 17,280 ledgers (~1 day).
    ///
    /// # Arguments
    /// - `admin`   — must be the contract admin
    /// - `ledgers` — number of ledgers before a proposal expires
    pub fn set_amendment_ttl(env: Env, admin: Address, ledgers: u32) {
        admin.require_auth();

        let stored_admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("admin not set"));

        if admin != stored_admin {
            panic!("unauthorized");
        }

        env.storage()
            .persistent()
            .set(&DataKey::AmendmentTTL, &ledgers);

        env.storage()
            .persistent()
            .extend_ttl(&DataKey::AmendmentTTL, 100_000, 6_300_000);
    }

    /// Get the current amendment proposal TTL in ledgers.
    /// Returns 17,280 if not yet set.
    pub fn get_amendment_ttl(env: Env) -> u32 {
        env.storage()
            .persistent()
            .get(&DataKey::AmendmentTTL)
            .unwrap_or(17_280)
    }

    /// Propose a milestone payment-percent amendment.
    /// Either company or recruiter can propose; the other party must accept.
    /// Only one pending proposal may exist per milestone; a new proposal overwrites.
    ///
    /// # Arguments
    /// - `proposer`             — company or recruiter (must sign)
    /// - `engagement_id`        — the engagement
    /// - `milestone_index`      — the milestone to amend
    /// - `new_payment_percent`  — the proposed new payment percent
    pub fn propose_amendment(
        env: Env,
        proposer: Address,
        engagement_id: String,
        milestone_index: u32,
        new_payment_percent: u32,
    ) {
        proposer.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if proposer != engagement.company && proposer != engagement.recruiter {
            panic!("unauthorized");
        }

        let _ = engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"));

        if new_payment_percent > 100 {
            panic!("payment percent must be 0-100");
        }

        let current_ledger = env.ledger().sequence();
        let ttl = env
            .storage()
            .persistent()
            .get(&DataKey::AmendmentTTL)
            .unwrap_or(17_280);

        let proposal = AmendmentProposal {
            proposer: proposer.clone(),
            new_payment_percent,
            proposed_at_ledger: current_ledger,
            expires_at_ledger: current_ledger + ttl,
        };

        env.storage()
            .persistent()
            .set(
                &DataKey::AmendmentProposal(engagement_id.clone(), milestone_index),
                &proposal,
            );

        env.storage().persistent().extend_ttl(
            &DataKey::AmendmentProposal(engagement_id.clone(), milestone_index),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "amendment_proposed"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                proposer,
                new_payment_percent,
                current_ledger + ttl,
            ),
        );
    }

    /// Accept a pending amendment proposal, applying the change immediately.
    /// The acceptor must be the other party (not the proposer).
    /// The milestone's payment percent is updated, and an AmendmentEntry is recorded.
    /// If the proposal is expired (current_ledger > expires_at_ledger), reject with "expired".
    ///
    /// # Arguments
    /// - `acceptor`        — company or recruiter (must sign, must NOT be the proposer)
    /// - `engagement_id`   — the engagement
    /// - `milestone_index` — the milestone to accept the amendment for
    pub fn accept_amendment(
        env: Env,
        acceptor: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        acceptor.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if acceptor != engagement.company && acceptor != engagement.recruiter {
            panic!("unauthorized");
        }

        let proposal: AmendmentProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AmendmentProposal(engagement_id.clone(), milestone_index))
            .unwrap_or_else(|| panic!("no pending amendment proposal"));

        if acceptor == proposal.proposer {
            panic!("proposer cannot accept their own proposal");
        }

        let current_ledger = env.ledger().sequence();

        if current_ledger > proposal.expires_at_ledger {
            env.storage()
                .persistent()
                .remove(&DataKey::AmendmentProposal(engagement_id.clone(), milestone_index));

            env.events().publish(
                (
                    Symbol::new(&env, "amendment_rejected"),
                    engagement_id.clone(),
                ),
                (milestone_index, acceptor, Symbol::new(&env, "expired")),
            );

            panic!("amendment_expired");
        }

        let mut milestone = engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"));

        let old_payment_percent = milestone.payment_percent;
        milestone.payment_percent = proposal.new_payment_percent;
        engagement.milestones.set(milestone_index, milestone);

        let amendment_entry = AmendmentEntry {
            proposer: proposal.proposer.clone(),
            old_payment_percent,
            new_payment_percent: proposal.new_payment_percent,
            ledger: current_ledger,
        };

        let mut log: Vec<AmendmentEntry> = env
            .storage()
            .persistent()
            .get(&DataKey::AmendmentLog(engagement_id.clone(), milestone_index))
            .unwrap_or_else(|| Vec::new(&env));

        log.push_back(amendment_entry);

        if log.len() > 20 {
            log.remove(0);
        }

        env.storage()
            .persistent()
            .set(
                &DataKey::AmendmentLog(engagement_id.clone(), milestone_index),
                &log,
            );

        env.storage().persistent().extend_ttl(
            &DataKey::AmendmentLog(engagement_id.clone(), milestone_index),
            100_000,
            6_300_000,
        );

        env.storage()
            .persistent()
            .remove(&DataKey::AmendmentProposal(engagement_id.clone(), milestone_index));

        env.storage()
            .persistent()
            .set(&DataKey::Engagement(engagement_id.clone()), &engagement);

        env.events().publish(
            (
                Symbol::new(&env, "amendment_accepted"),
                engagement_id.clone(),
            ),
            (milestone_index, acceptor, old_payment_percent, proposal.new_payment_percent),
        );
    }

    /// Reject a pending amendment proposal.
    /// Either company or recruiter can reject (if they're not the proposer).
    /// On rejection, the proposal is cleared and amendment_rejected event is emitted.
    ///
    /// # Arguments
    /// - `rejector`        — company or recruiter (must sign, must NOT be the proposer)
    /// - `engagement_id`   — the engagement
    /// - `milestone_index` — the milestone with the proposal to reject
    pub fn reject_amendment(
        env: Env,
        rejector: Address,
        engagement_id: String,
        milestone_index: u32,
    ) {
        rejector.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if rejector != engagement.company && rejector != engagement.recruiter {
            panic!("unauthorized");
        }

        let proposal: AmendmentProposal = env
            .storage()
            .persistent()
            .get(&DataKey::AmendmentProposal(engagement_id.clone(), milestone_index))
            .unwrap_or_else(|| panic!("no pending amendment proposal"));

        if rejector == proposal.proposer {
            panic!("proposer cannot reject their own proposal");
        }

        env.storage()
            .persistent()
            .remove(&DataKey::AmendmentProposal(engagement_id.clone(), milestone_index));

        env.events().publish(
            (
                Symbol::new(&env, "amendment_rejected"),
                engagement_id.clone(),
            ),
            (
                milestone_index,
                rejector,
                Symbol::new(&env, "declined"),
            ),
        );
    }

    // ----------------------------------------------------------
    // AMENDMENT LOG QUERIES
    // ----------------------------------------------------------

    /// Get the amendment history for a milestone.
    /// Returns entries in chronological order (oldest first).
    /// Capped at 20 entries per milestone (FIFO eviction).
    ///
    /// # Arguments
    /// - `engagement_id`   — the engagement
    /// - `milestone_index` — the milestone
    pub fn get_amendment_log(
        env: Env,
        engagement_id: String,
        milestone_index: u32,
    ) -> Vec<AmendmentEntry> {
        env.storage()
            .persistent()
            .get(&DataKey::AmendmentLog(engagement_id, milestone_index))
            .unwrap_or_else(|| Vec::new(&env))
    }

    // ----------------------------------------------------------
    // INTERNAL HELPERS
    // ----------------------------------------------------------

    fn get_engagement_internal(env: &Env, engagement_id: &String) -> Engagement {
        env.storage()
            .persistent()
            .get(&DataKey::Engagement(engagement_id.clone()))
            .unwrap_or_else(|| panic!("engagement not found"))
    }
}

mod test;
