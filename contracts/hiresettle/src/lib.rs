#![no_std]

use soroban_sdk::{
    contract, contractimpl, contracttype, token, Address, Env, String, Vec, Symbol,
};

// ============================================================
// DATA TYPES
// ============================================================

/// The status of a single milestone in the engagement
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum MilestoneStatus {
    /// Milestone is not yet available (retention window not elapsed)
    Locked,
    /// Milestone is available but recruiter has not submitted proof yet
    Pending,
    /// Recruiter has submitted proof — awaiting company confirmation
    ProofSubmitted,
    /// Company confirmed — payment released
    Confirmed,
    /// A dispute has been raised on this milestone
    Disputed,
    /// Arbiter resolved the dispute
    Resolved,
}

/// The type of milestone — determines unlock logic
#[contracttype]
#[derive(Clone, PartialEq)]
pub enum MilestoneKind {
    /// Unlocks immediately — recruiter submits offer letter proof
    Placement,
    /// Unlocks after a retention window (ledger-based timer)
    Retention,
}

/// A single milestone in the engagement
#[contracttype]
#[derive(Clone)]
pub struct Milestone {
    /// Human-readable name e.g. "Candidate Placed", "30-Day Retention"
    pub name: String,
    /// Percentage of total fee released at this milestone (all must sum to 100)
    pub payment_percent: u32,
    /// What type of milestone this is
    pub kind: MilestoneKind,
    /// For Retention milestones: the ledger sequence after which this milestone
    /// can be confirmed. For Placement milestones: set to 0.
    pub valid_after_ledger: u32,
    /// IPFS CID or URL of the supporting proof document
    pub proof_hash: String,
    /// Current status of this milestone
    pub status: MilestoneStatus,
}

/// The overall status of a recruitment engagement
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum EngagementStatus {
    /// Active — milestones in progress
    Active,
    /// All milestones confirmed — recruiter fully paid
    Completed,
    /// Company cancelled before placement — full refund issued
    Cancelled,
    /// Replacement requested — remaining balance frozen pending new placement
    ReplacementRequested,
}

/// The full engagement record stored on-chain
#[contracttype]
#[derive(Clone)]
pub struct Engagement {
    /// Unique engagement ID chosen by the company e.g. "ENG-2026-001"
    pub id: String,
    /// Company's Stellar address — locks fees, confirms retention
    pub company: Address,
    /// Recruiter's Stellar address — receives payments
    pub recruiter: Address,
    /// Arbiter's Stellar address — resolves disputes
    pub arbiter: Address,
    /// USDC Stellar Asset Contract address
    pub token: Address,
    /// Total recruiter fee locked in escrow (in stroops)
    pub total_amount: i128,
    /// Amount already released to the recruiter
    pub released_amount: i128,
    /// Job title stored as a short string (full metadata lives off-chain)
    pub job_title: String,
    /// Ledger sequence when the engagement was created
    pub created_at_ledger: u32,
    /// Ordered list of milestones
    pub milestones: Vec<Milestone>,
    /// Overall engagement status
    pub status: EngagementStatus,
}

// ============================================================
// STORAGE KEYS
// ============================================================

#[contracttype]
pub enum DataKey {
    Engagement(String),
    Admin,
}

// ============================================================
// CONTRACT
// ============================================================

#[contract]
pub struct HireSettleContract;

#[contractimpl]
impl HireSettleContract {

    // ----------------------------------------------------------
    // INIT
    // ----------------------------------------------------------

    /// Initialise the contract and set the admin.
    /// Called once by the deployer immediately after deployment.
    pub fn init(env: Env, admin: Address) {
        admin.require_auth();
        env.storage().instance().set(&DataKey::Admin, &admin);
    }

    // ----------------------------------------------------------
    // CREATE ENGAGEMENT
    // ----------------------------------------------------------

    /// Create a new recruitment engagement and lock USDC in escrow.
    ///
    /// The company defines the fee structure via milestones. Each milestone
    /// has a `kind` (Placement or Retention) and for Retention milestones
    /// a `retention_days` value that is converted to a ledger-based timer.
    ///
    /// Stellar produces a new ledger approximately every 5 seconds.
    /// retention_days × 86400s ÷ 5s per ledger = ledger offset.
    ///
    /// # Arguments
    /// - `engagement_id`   — unique string ID for this engagement
    /// - `company`         — company address (must sign this tx)
    /// - `recruiter`       — recruiter address (receives payments)
    /// - `arbiter`         — arbiter address (resolves disputes)
    /// - `token`           — USDC Stellar Asset Contract address
    /// - `total_amount`    — total recruiter fee in stroops
    /// - `job_title`       — short job title string (≤64 chars)
    /// - `milestones`      — ordered milestone list
    /// - `retention_days`  — Vec of retention windows in days (one per Retention milestone)
    pub fn create_engagement(
        env: Env,
        engagement_id: String,
        company: Address,
        recruiter: Address,
        arbiter: Address,
        token: Address,
        total_amount: i128,
        job_title: String,
        milestones: Vec<Milestone>,
        retention_days: Vec<u32>,
    ) -> String {
        company.require_auth();

        // Validate amount
        if total_amount <= 0 {
            panic!("amount must be greater than zero");
        }

        // Validate milestone percentages sum to 100
        let mut total_percent: u32 = 0;
        for i in 0..milestones.len() {
            let m = milestones.get(i).unwrap();
            total_percent += m.payment_percent;
        }
        if total_percent != 100 {
            panic!("milestone percentages must sum to 100");
        }

        // Ensure engagement ID is unique
        if env
            .storage()
            .persistent()
            .has(&DataKey::Engagement(engagement_id.clone()))
        {
            panic!("engagement already exists");
        }

        let current_ledger = env.ledger().sequence();

        // Assign valid_after_ledger for each Retention milestone
        // Ledgers per day = 86400 ÷ 5 = 17280
        const LEDGERS_PER_DAY: u32 = 17_280;
        let mut retention_index: u32 = 0;
        let mut resolved_milestones: Vec<Milestone> = Vec::new(&env);

        for i in 0..milestones.len() {
            let mut m = milestones.get(i).unwrap();
            match m.kind {
                MilestoneKind::Placement => {
                    // Placement milestones are immediately available
                    m.valid_after_ledger = 0;
                    m.status = MilestoneStatus::Pending;
                }
                MilestoneKind::Retention => {
                    // Assign the retention window from the retention_days Vec
                    let days = retention_days.get(retention_index).unwrap_or(30);
                    retention_index += 1;
                    let unlock_ledger = current_ledger + (days * LEDGERS_PER_DAY);
                    m.valid_after_ledger = unlock_ledger;
                    // Retention milestones start Locked until the window passes
                    m.status = MilestoneStatus::Locked;
                }
            }
            resolved_milestones.push_back(m);
        }

        // Transfer USDC from company to this contract (escrow)
        let token_client = token::Client::new(&env, &token);
        token_client.transfer(&company, &env.current_contract_address(), &total_amount);

        let engagement = Engagement {
            id: engagement_id.clone(),
            company,
            recruiter,
            arbiter,
            token,
            total_amount,
            released_amount: 0,
            job_title,
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

    /// Unlock a Retention milestone once its time window has elapsed.
    /// Anyone can call this — it simply checks the current ledger against
    /// the milestone's `valid_after_ledger` and moves it from Locked → Pending.
    ///
    /// The backend cron job calls this automatically at day 30 and day 90,
    /// but it can also be triggered manually from the frontend.
    ///
    /// # Arguments
    /// - `engagement_id`    — the engagement
    /// - `milestone_index`  — the Retention milestone to unlock
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
    /// The proof hash is an IPFS CID or URL to the supporting document:
    ///   - Placement: signed offer letter
    ///   - 30-day retention: HR confirmation email or payroll record
    ///   - 90-day retention: payroll record or employment certificate
    ///
    /// # Arguments
    /// - `recruiter`        — must match the engagement's recruiter
    /// - `engagement_id`    — the engagement
    /// - `milestone_index`  — the milestone to submit proof for
    /// - `proof_hash`       — IPFS CID or URL of the supporting document
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

        milestone.proof_hash = proof_hash;
        milestone.status = MilestoneStatus::ProofSubmitted;
        engagement.milestones.set(milestone_index, milestone);

        // If this is a replacement placement (ReplacementRequested → proof submitted),
        // restore the engagement to Active so the rest of the flow continues
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

    /// Company confirms a ProofSubmitted milestone and triggers payment release.
    /// For Retention milestones, the contract double-checks that the current
    /// ledger is past the `valid_after_ledger` as a safety guard.
    ///
    /// # Arguments
    /// - `company`          — must match the engagement's company
    /// - `engagement_id`    — the engagement
    /// - `milestone_index`  — the milestone to confirm
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

        // For Retention milestones: safety check that the window has elapsed
        if milestone.kind == MilestoneKind::Retention {
            let current_ledger = env.ledger().sequence();
            if current_ledger < milestone.valid_after_ledger {
                panic!("retention window has not elapsed — cannot confirm yet");
            }
        }

        // Calculate and release payment
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

        // Check if all milestones are done → complete the engagement
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

    /// Company raises a dispute on a ProofSubmitted milestone.
    /// Typical reason: the offer letter is for a different role, or the
    /// candidate left before the retention window actually ended.
    ///
    /// # Arguments
    /// - `company`          — must match the engagement's company
    /// - `engagement_id`    — the engagement
    /// - `milestone_index`  — the milestone to dispute
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
    // RESOLVE DISPUTE
    // ----------------------------------------------------------

    /// Arbiter resolves a disputed milestone.
    ///
    /// - `approve = true`  → payment released to recruiter, milestone → Resolved
    /// - `approve = false` → milestone reset to Pending, recruiter must resubmit
    ///
    /// # Arguments
    /// - `arbiter`          — must match the engagement's arbiter
    /// - `engagement_id`    — the engagement
    /// - `milestone_index`  — the disputed milestone
    /// - `approve`          — true = release payment, false = reject and reset
    pub fn resolve_dispute(
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

        if arbiter != engagement.arbiter {
            panic!("unauthorized");
        }

        let mut milestone = engagement.milestones.get(milestone_index).unwrap();

        if milestone.status != MilestoneStatus::Disputed {
            panic!("milestone is not in disputed status");
        }

        if approve {
            let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
            engagement.released_amount += payment;

            let token_client = token::Client::new(&env, &engagement.token);
            token_client.transfer(
                &env.current_contract_address(),
                &engagement.recruiter,
                &payment,
            );

            milestone.status = MilestoneStatus::Resolved;
        } else {
            // Rejected — reset to Pending so recruiter can resubmit
            milestone.status = MilestoneStatus::Pending;
            milestone.proof_hash = String::from_str(&env, "");
        }

        engagement.milestones.set(milestone_index, milestone);

        // Check completion
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
            (Symbol::new(&env, "dispute_resolved"), engagement_id.clone()),
            (milestone_index, approve),
        );
    }

    // ----------------------------------------------------------
    // REQUEST REPLACEMENT
    // ----------------------------------------------------------

    /// Company requests a candidate replacement.
    /// This is triggered when a placed candidate leaves before the 90-day
    /// retention milestone. The remaining balance stays frozen in escrow.
    /// The recruiter must find a replacement and submit proof for the
    /// Placement milestone again (which is reset to Pending).
    ///
    /// Rules:
    /// - Only callable if Milestone 0 (Placement) is Confirmed
    /// - The 90-day Retention milestone must NOT be Confirmed yet
    /// - Any confirmed tranches are kept — only unreleased amounts are frozen
    ///
    /// # Arguments
    /// - `company`         — must match the engagement's company
    /// - `engagement_id`   — the engagement
    pub fn request_replacement(env: Env, company: Address, engagement_id: String) {
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("engagement is not active");
        }

        if company != engagement.company {
            panic!("unauthorized");
        }

        // Ensure at least Milestone 0 (Placement) has been confirmed —
        // otherwise the company should just cancel the engagement
        let placement_confirmed = {
            let m0 = engagement.milestones.get(0).unwrap();
            m0.status == MilestoneStatus::Confirmed || m0.status == MilestoneStatus::Resolved
        };

        if !placement_confirmed {
            panic!("placement not yet confirmed — use cancel_engagement instead");
        }

        // Reset all Retention milestones that are not yet confirmed back to Locked
        // and reset the Placement milestone to Pending for the replacement candidate
        let current_ledger = env.ledger().sequence();
        const LEDGERS_PER_DAY: u32 = 17_280;

        for i in 0..engagement.milestones.len() {
            let mut m = engagement.milestones.get(i).unwrap();
            match m.kind {
                MilestoneKind::Placement => {
                    if m.status == MilestoneStatus::Confirmed
                        || m.status == MilestoneStatus::Resolved
                    {
                        // Reset placement for the replacement candidate
                        m.status = MilestoneStatus::Pending;
                        m.proof_hash = String::from_str(&env, "");
                    }
                }
                MilestoneKind::Retention => {
                    // Reset retention windows — restart the clock from now
                    if m.status != MilestoneStatus::Confirmed
                        && m.status != MilestoneStatus::Resolved
                    {
                        // Approximate day count from valid_after_ledger delta
                        let original_days =
                            (m.valid_after_ledger - engagement.created_at_ledger) / LEDGERS_PER_DAY;
                        m.valid_after_ledger = current_ledger + (original_days * LEDGERS_PER_DAY);
                        m.status = MilestoneStatus::Locked;
                        m.proof_hash = String::from_str(&env, "");
                    }
                }
            }
            engagement.milestones.set(i, m);
        }

        // Deduct the placement fee that was already paid from total_amount
        // so only the unreleased portion is tracked going forward
        // (released_amount already tracks this correctly)

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

    /// Cancel the engagement before any milestones are confirmed.
    /// Returns the full locked amount to the company.
    /// Not callable once the Placement milestone has been confirmed —
    /// use `request_replacement` instead.
    ///
    /// # Arguments
    /// - `company`         — must match the engagement's company
    /// - `engagement_id`   — the engagement to cancel
    pub fn cancel_engagement(env: Env, company: Address, engagement_id: String) {
        company.require_auth();

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);

        if engagement.status != EngagementStatus::Active {
            panic!("engagement is not active");
        }

        if company != engagement.company {
            panic!("unauthorized");
        }

        // Disallow cancellation if any milestone has been confirmed
        for i in 0..engagement.milestones.len() {
            let m = engagement.milestones.get(i).unwrap();
            if m.status == MilestoneStatus::Confirmed || m.status == MilestoneStatus::Resolved {
                panic!("cannot cancel: milestones already confirmed — use request_replacement");
            }
        }

        // Refund entire locked amount to company
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

    /// Get the full engagement record by ID
    pub fn get_engagement(env: Env, engagement_id: String) -> Engagement {
        Self::get_engagement_internal(&env, &engagement_id)
    }

    /// Get a single milestone from an engagement
    pub fn get_milestone(env: Env, engagement_id: String, milestone_index: u32) -> Milestone {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"))
    }

    /// Get the amount of USDC still locked in escrow
    pub fn get_escrow_balance(env: Env, engagement_id: String) -> i128 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.total_amount - engagement.released_amount
    }

    /// Check whether a Locked retention milestone can be unlocked now.
    /// Returns true if `current_ledger >= milestone.valid_after_ledger`.
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

    
}

mod test;
