#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, token, Address, Env, String, Symbol, Vec};

const MAX_PLATFORM_FEE_BPS: u32 = 500;

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

/// Lightweight summary of an engagement for paginated list views.
/// Omits full milestone details to reduce data transfer.
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

/// Platform fee configuration.
#[contracttype]
#[derive(Clone)]
pub struct PlatformFee {
    pub bps: u32,
    pub treasury: Address,
}

// ============================================================
// STORAGE KEYS
// ============================================================

#[contracttype]
pub enum DataKey {
    Engagement(String),
    Admin,
    PendingArbiter(String),
    PlatformFee,
    Paused,
    PendingAdmin,
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
        env.storage().persistent().set(&DataKey::Paused, &false);
        env.storage().persistent().set(
            &DataKey::PlatformFee,
            &PlatformFee {
                bps: 0,
                treasury: admin,
            },
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
            panic!("unauthorized");
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
        Self::assert_not_paused(&env);
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

        env.storage().persistent().extend_ttl(
            &DataKey::Engagement(engagement_id.clone()),
            100_000,
            6_300_000,
        );

        env.events().publish(
            (
                Symbol::new(&env, "engagement_created"),
                engagement_id.clone(),
            ),
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
        Self::assert_not_paused(&env);
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
            (
                Symbol::new(&env, "milestone_unlocked"),
                engagement_id.clone(),
            ),
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
        Self::assert_not_paused(&env);
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
        Self::assert_not_paused(&env);
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

        // Calculate and release payment, deducting the configured platform fee.
        let payment = (engagement.total_amount * milestone.payment_percent as i128) / 100;
        let platform_fee = Self::get_platform_fee_internal(&env);
        let fee_amount = (payment * platform_fee.bps as i128) / 10_000;
        let recruiter_payment = payment - fee_amount;
        engagement.released_amount += payment;

        let token_client = token::Client::new(&env, &engagement.token);
        if fee_amount > 0 {
            token_client.transfer(
                &env.current_contract_address(),
                &platform_fee.treasury,
                &fee_amount,
            );
            env.events().publish(
                (
                    Symbol::new(&env, "platform_fee_collected"),
                    engagement_id.clone(),
                ),
                (milestone_index, fee_amount, platform_fee.treasury),
            );
        }
        token_client.transfer(
            &env.current_contract_address(),
            &engagement.recruiter,
            &recruiter_payment,
        );

        milestone.status = MilestoneStatus::Confirmed;
        engagement
            .milestones
            .set(milestone_index, milestone.clone());

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
            (
                Symbol::new(&env, "milestone_confirmed"),
                engagement_id.clone(),
            ),
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
    pub fn raise_dispute(env: Env, company: Address, engagement_id: String, milestone_index: u32) {
        Self::assert_not_paused(&env);
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
        Self::assert_not_paused(&env);
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
        Self::assert_not_paused(&env);
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
            (
                Symbol::new(&env, "replacement_requested"),
                engagement_id.clone(),
            ),
            engagement_id.clone(),
        );
    }

    // ----------------------------------------------------------
    // CANCEL ENGAGEMENT
    // ----------------------------------------------------------

    /// Cancel the engagement at any point, refunding the unreleased escrow balance to
    /// the company while leaving already-confirmed milestone payouts intact.
    ///
    /// Requires mutual consent: both company and recruiter must authorise this call.
    /// Previously released funds are never clawed back; refund = total - released.
    ///
    /// # Arguments
    /// - `company`         — must match the engagement's company
    /// - `recruiter`       — must match the engagement's recruiter (mutual consent)
    /// - `engagement_id`   — the engagement to cancel
    pub fn cancel_engagement(
        env: Env,
        company: Address,
        recruiter: Address,
        engagement_id: String,
    ) {
        Self::assert_not_paused(&env);
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

        // Refund only the unreleased balance; confirmed payouts remain with the recruiter
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
            (
                Symbol::new(&env, "engagement_cancelled"),
                engagement_id.clone(),
            ),
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
    pub fn is_milestone_unlockable(env: Env, engagement_id: String, milestone_index: u32) -> bool {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        let milestone = engagement
            .milestones
            .get(milestone_index)
            .unwrap_or_else(|| panic!("invalid milestone index"));

        milestone.status == MilestoneStatus::Locked
            && env.ledger().sequence() >= milestone.valid_after_ledger
    }

    /// Get ledgers remaining until a Locked milestone can be unlocked.
    /// Returns 0 if already unlockable.
    pub fn ledgers_until_unlock(env: Env, engagement_id: String, milestone_index: u32) -> u32 {
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

    /// Get total amount released to the recruiter across all milestones.
    /// Returns 0 for a new engagement with no confirmed milestones.
    pub fn get_total_released(env: Env, engagement_id: String) -> i128 {
        let engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.released_amount
    }

    /// Get a lightweight summary of an engagement for paginated list views.
    /// Contains only the fields the list UI needs — omits full milestone details.
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

    /// Current arbiter nominates a successor address.
    /// The successor must call `claim_arbiter` to complete the transfer.
    /// Old arbiter retains their role until the nominee claims.
    ///
    /// # Arguments
    /// - `arbiter`         — current arbiter (must sign)
    /// - `engagement_id`   — the engagement
    /// - `successor`       — address of the nominated successor
    pub fn nominate_arbiter_successor(
        env: Env,
        arbiter: Address,
        engagement_id: String,
        successor: Address,
    ) {
        Self::assert_not_paused(&env);
        arbiter.require_auth();

        let engagement = Self::get_engagement_internal(&env, &engagement_id);

        if arbiter != engagement.arbiter {
            panic!("unauthorized");
        }

        env.storage()
            .persistent()
            .set(&DataKey::PendingArbiter(engagement_id.clone()), &successor);

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

    /// Nominated successor claims the arbiter role, completing the succession.
    /// Rejects any address that does not match the pending nomination.
    ///
    /// # Arguments
    /// - `nominee`         — must match the address stored by `nominate_arbiter_successor`
    /// - `engagement_id`   — the engagement
    pub fn claim_arbiter(env: Env, nominee: Address, engagement_id: String) {
        Self::assert_not_paused(&env);
        nominee.require_auth();

        let pending: Address = env
            .storage()
            .persistent()
            .get(&DataKey::PendingArbiter(engagement_id.clone()))
            .unwrap_or_else(|| panic!("no pending arbiter nomination"));

        if nominee != pending {
            panic!("unauthorized");
        }

        let mut engagement = Self::get_engagement_internal(&env, &engagement_id);
        engagement.arbiter = nominee.clone();

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
    // INTERNAL HELPERS
    // ----------------------------------------------------------

    fn get_engagement_internal(env: &Env, engagement_id: &String) -> Engagement {
        env.storage()
            .persistent()
            .get(&DataKey::Engagement(engagement_id.clone()))
            .unwrap_or_else(|| panic!("engagement not found"))
    }

    fn get_admin(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Admin)
            .unwrap_or_else(|| panic!("admin not initialized"))
    }

    fn assert_admin(env: &Env, admin: &Address) {
        admin.require_auth();
        if *admin != Self::get_admin(env) {
            panic!("unauthorized");
        }
    }

    fn is_paused_internal(env: &Env) -> bool {
        env.storage()
            .persistent()
            .get(&DataKey::Paused)
            .unwrap_or(false)
    }

    fn assert_not_paused(env: &Env) {
        if Self::is_paused_internal(env) {
            panic!("ContractPaused");
        }
    }

    fn get_platform_fee_internal(env: &Env) -> PlatformFee {
        env.storage()
            .persistent()
            .get(&DataKey::PlatformFee)
            .unwrap_or_else(|| PlatformFee {
                bps: 0,
                treasury: Self::get_admin(env),
            })
    }
}

mod test;
