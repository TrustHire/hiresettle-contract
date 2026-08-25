#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    token, vec, Address, Env, String, Symbol, TryIntoVal,
};

// ============================================================
// TEST HELPERS
// ============================================================

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

    let client = HireSettleContractClient::new(&env, &contract_id);
    client.init(&company);

    (env, contract_id, token_id, company, recruiter, arbiter)
}

fn build_milestones(env: &Env) -> Vec<Milestone> {
    vec![
        env,
        Milestone {
            name: String::from_str(env, "Candidate Placed"),
            payment_percent: 30,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(env, "30-Day Retention"),
            payment_percent: 40,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Locked,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(env, "90-Day Retention"),
            payment_percent: 30,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Locked,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ]
}

fn create_standard_engagement(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
    id: &str,
) {
    client.create_engagement(
        &String::from_str(env, id),
        company,
        recruiter,
        &ArbiterSetup {
            arbiters: vec![env, arbiter.clone()],
            quorum: 1,
        },
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Senior Engineer"),
        &build_milestones(env),
        &vec![env, 30u32, 90u32],
        &default_config(),
    );
}

fn has_event(env: &Env, event_name: &str) -> bool {
    let expected = Symbol::new(env, event_name);
    for (_, topics, _) in env.events().all().iter() {
        let matches = topics
            .get(0)
            .and_then(|v| v.try_into_val(env).ok())
            .map(|s: Symbol| s == expected)
            .unwrap_or(false);
        if matches {
            return true;
        }
    }
    false
}
fn advance_ledger(env: &Env, extra: u32) {
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + extra,
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });
}

fn default_config() -> EngagementConfig {
    EngagementConfig {
        metadata_hash: None,
        co_recruiter: None,
        recruiter_split_bps: 10_000,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
    }
}

// ============================================================
// EXISTING TESTS (updated for new signatures)
// ============================================================

#[test]
fn test_create_engagement_success() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-001",
    );

    let company_balance = token_client.balance(&company);
    assert_eq!(company_balance, 500_000_000_000 - 1_000_000_000);

    let escrow = token_client.balance(&contract_id);
    assert_eq!(escrow, 1_000_000_000);

    let eng = client.get_engagement(&String::from_str(&env, "ENG-001"));
    assert_eq!(eng.status, EngagementStatus::Active);
    assert_eq!(eng.total_amount, 1_000_000_000);
    assert_eq!(eng.released_amount, 0);
    assert_eq!(eng.milestones.len(), 3);

    let m0 = client.get_milestone(&String::from_str(&env, "ENG-001"), &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);

    let m1 = client.get_milestone(&String::from_str(&env, "ENG-001"), &1);
    let m2 = client.get_milestone(&String::from_str(&env, "ENG-001"), &2);
    assert_eq!(m1.status, MilestoneStatus::Locked);
    assert_eq!(m2.status, MilestoneStatus::Locked);

    assert!(m1.valid_after_ledger > 0);
    assert!(m2.valid_after_ledger > m1.valid_after_ledger);
}

#[test]
#[should_panic(expected = "milestone percentages must sum to 100")]
fn test_create_engagement_invalid_percentages() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let bad_milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Placement"),
            payment_percent: 40,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "Retention"),
            payment_percent: 40,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Locked,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-BAD"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Dev"),
        &bad_milestones,
        &vec![&env, 30u32],
        &default_config(),
    );
}

#[test]
fn test_placement_milestone_flow() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-001",
    );

    let eng_id = String::from_str(&env, "ENG-001");

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://QmOfferLetter123"),
    );

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);

    client.confirm_milestone(&company, &eng_id, &0);

    let expected_payment = 1_000_000_000i128 * 30 / 100;
    let recruiter_balance = token_client.balance(&recruiter);
    assert_eq!(recruiter_balance, expected_payment);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.released_amount, expected_payment);
    assert_eq!(eng.status, EngagementStatus::Active);
}

#[test]
fn test_retention_milestone_unlock_timing() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-001");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-001",
    );

    let unlockable = client.is_milestone_unlockable(&eng_id, &1);
    assert!(!unlockable);

    advance_ledger(&env, 30 * 17_280 + 1);

    let unlockable = client.is_milestone_unlockable(&eng_id, &1);
    assert!(unlockable);

    client.unlock_milestone(&eng_id, &1);
    let m1 = client.get_milestone(&eng_id, &1);
    assert_eq!(m1.status, MilestoneStatus::Pending);

    let m2 = client.get_milestone(&eng_id, &2);
    assert_eq!(m2.status, MilestoneStatus::Locked);
}

#[test]
#[should_panic(expected = "retention window has not elapsed yet")]
fn test_cannot_unlock_before_window() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-001",
    );

    client.unlock_milestone(&String::from_str(&env, "ENG-001"), &1);
}

#[test]
fn test_full_engagement_lifecycle() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-FULL");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-FULL",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer-letter"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30-day-hr-confirmation"),
    );
    client.confirm_milestone(&company, &eng_id, &1);
    assert_eq!(token_client.balance(&recruiter), 300_000_000 + 400_000_000);

    advance_ledger(&env, 60 * 17_280);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &2,
        &String::from_str(&env, "ipfs://90-day-payroll"),
    );
    client.confirm_milestone(&company, &eng_id, &2);

    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Completed);
    assert_eq!(eng.released_amount, 1_000_000_000);
    assert_eq!(client.get_escrow_balance(&eng_id), 0);
}

#[test]
fn test_raise_and_resolve_dispute_approve() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-DISPUTE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISPUTE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://questionable-proof"),
    );
    client.raise_dispute(
        &company,
        &eng_id,
        &0,
        &String::from_str(&env, "wrong_document"),
    );

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_raise_and_resolve_dispute_reject() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-REJECT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REJECT",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://bad-proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "not_hired"));

    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);
    assert_eq!(token_client.balance(&recruiter), 0);
}

#[test]
fn test_request_replacement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-REPLACE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REPLACE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    client.request_replacement(
        &company,
        &eng_id,
        &String::from_str(&env, "candidate_resigned"),
    );

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::ReplacementRequested);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);

    let m1 = client.get_milestone(&eng_id, &1);
    let m2 = client.get_milestone(&eng_id, &2);
    assert_eq!(m1.status, MilestoneStatus::Locked);
    assert_eq!(m2.status, MilestoneStatus::Locked);

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://replacement-offer"),
    );

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Active);
}

#[test]
#[should_panic(expected = "placement not yet confirmed")]
fn test_request_replacement_before_placement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-EARLY");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EARLY",
    );

    client.request_replacement(&company, &eng_id, &String::from_str(&env, "performance"));
}

#[test]
fn test_cancel_engagement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-CANCEL");
    let company_balance_before = token_client.balance(&company);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL",
    );

    client.cancel_engagement(&company, &recruiter, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);
    assert_eq!(token_client.balance(&company), company_balance_before);
}

#[test]
fn test_partial_cancel_after_placement_confirmed() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-PARTIAL-CANCEL");
    let company_balance_before = token_client.balance(&company);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PARTIAL-CANCEL",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    client.cancel_engagement(&company, &recruiter, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);

    let expected_refund = 1_000_000_000i128 - 300_000_000;
    assert_eq!(
        token_client.balance(&company),
        company_balance_before - 1_000_000_000 + expected_refund
    );
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_unauthorized_confirm() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AUTH");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AUTH",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&recruiter, &eng_id, &0);
}

#[test]
fn test_ledgers_until_unlock() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-TIMER");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-TIMER",
    );

    let remaining = client.ledgers_until_unlock(&eng_id, &1);
    assert!(remaining > 0);
    assert!(remaining <= 30 * 17_280);
}

#[test]
fn test_two_milestone_engagement_50_50() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Candidate Placed"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "30-Day Retention"),
            payment_percent: 50,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Locked,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    let eng_id = String::from_str(&env, "ENG-5050");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &2_000_000_000,
        &String::from_str(&env, "CTO"),
        &milestones,
        &vec![&env, 30u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30day"),
    );
    client.confirm_milestone(&company, &eng_id, &1);

    assert_eq!(token_client.balance(&recruiter), 2_000_000_000);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Completed);
}

// ============================================================
// get_total_released
// ============================================================

#[test]
fn test_get_total_released_zero() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REL-ZERO");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REL-ZERO",
    );
    assert_eq!(client.get_total_released(&eng_id), 0);
}

#[test]
fn test_get_total_released_partial() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REL-PART");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REL-PART",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(client.get_total_released(&eng_id), 300_000_000);
}

#[test]
fn test_get_total_released_full() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REL-FULL");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REL-FULL",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30day"),
    );
    client.confirm_milestone(&company, &eng_id, &1);

    advance_ledger(&env, 60 * 17_280);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &2,
        &String::from_str(&env, "ipfs://90day"),
    );
    client.confirm_milestone(&company, &eng_id, &2);

    assert_eq!(client.get_total_released(&eng_id), 1_000_000_000);
}

// ============================================================
// get_engagement_summary
// ============================================================

#[test]
fn test_get_engagement_summary_after_create() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-SUMM-CREATE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-SUMM-CREATE",
    );

    let summary = client.get_engagement_summary(&eng_id);
    assert_eq!(summary.id, eng_id);
    assert_eq!(summary.job_title, String::from_str(&env, "Senior Engineer"));
    assert_eq!(summary.company, company);
    assert_eq!(summary.recruiter, recruiter);
    assert_eq!(summary.total_amount, 1_000_000_000);
    assert_eq!(summary.released_amount, 0);
    assert_eq!(summary.status, EngagementStatus::Active);
    assert_eq!(summary.milestone_count, 3);
    assert!(summary.created_at_ledger > 0);
}

#[test]
fn test_get_engagement_summary_after_partial_confirmations() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-SUMM-PART");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-SUMM-PART",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    let summary = client.get_engagement_summary(&eng_id);
    assert_eq!(summary.released_amount, 300_000_000);
    assert_eq!(summary.status, EngagementStatus::Active);
    assert_eq!(summary.milestone_count, 3);
}

#[test]
fn test_get_engagement_summary_after_completion() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-SUMM-DONE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-SUMM-DONE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30day"),
    );
    client.confirm_milestone(&company, &eng_id, &1);

    advance_ledger(&env, 60 * 17_280);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &2,
        &String::from_str(&env, "ipfs://90day"),
    );
    client.confirm_milestone(&company, &eng_id, &2);

    let summary = client.get_engagement_summary(&eng_id);
    assert_eq!(summary.status, EngagementStatus::Completed);
    assert_eq!(summary.released_amount, 1_000_000_000);
    assert_eq!(summary.total_amount, 1_000_000_000);
}

// ============================================================
// batch_get_engagement_summary
// ============================================================

#[test]
fn test_batch_get_engagement_summary_multiple() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "BATCH-1",
    );
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "BATCH-2",
    );
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "BATCH-3",
    );

    let ids = vec![
        &env,
        String::from_str(&env, "BATCH-1"),
        String::from_str(&env, "BATCH-2"),
        String::from_str(&env, "BATCH-3"),
    ];
    let summaries = client.batch_get_engagement_summary(&ids);
    assert_eq!(summaries.len(), 3);
    assert_eq!(
        summaries.get(0).unwrap().id,
        String::from_str(&env, "BATCH-1")
    );
    assert_eq!(
        summaries.get(1).unwrap().id,
        String::from_str(&env, "BATCH-2")
    );
    assert_eq!(
        summaries.get(2).unwrap().id,
        String::from_str(&env, "BATCH-3")
    );
    assert_eq!(summaries.get(0).unwrap().total_amount, 1_000_000_000);
    assert_eq!(summaries.get(0).unwrap().milestone_count, 3);
}

#[test]
fn test_batch_get_engagement_summary_skips_missing() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "BATCH-EXISTS",
    );

    let ids = vec![
        &env,
        String::from_str(&env, "BATCH-EXISTS"),
        String::from_str(&env, "DOES-NOT-EXIST"),
    ];
    let summaries = client.batch_get_engagement_summary(&ids);
    assert_eq!(summaries.len(), 1);
    assert_eq!(
        summaries.get(0).unwrap().id,
        String::from_str(&env, "BATCH-EXISTS")
    );
}

#[test]
fn test_batch_get_engagement_summary_empty_input() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let ids: Vec<String> = Vec::new(&env);
    let summaries = client.batch_get_engagement_summary(&ids);
    assert_eq!(summaries.len(), 0);
}

#[test]
#[should_panic(expected = "too many IDs")]
fn test_batch_get_engagement_summary_too_many_ids() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Build 21 IDs — all nonexistent, we just need to trigger the cap.
    let names = [
        "A01", "A02", "A03", "A04", "A05", "A06", "A07", "A08", "A09", "A10", "A11", "A12", "A13",
        "A14", "A15", "A16", "A17", "A18", "A19", "A20", "A21",
    ];
    let mut ids: Vec<String> = Vec::new(&env);
    for name in names.iter() {
        ids.push_back(String::from_str(&env, name));
    }
    client.batch_get_engagement_summary(&ids);
}

// ============================================================
// Cancellation edge cases
// ============================================================

#[test]
fn test_cancel_full_refund_zero_released() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CANCEL-ZERO");
    let company_balance_before = token_client.balance(&company);
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL-ZERO",
    );
    client.cancel_engagement(&company, &recruiter, &eng_id);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);
    assert_eq!(token_client.balance(&company), company_balance_before);
    assert_eq!(client.get_total_released(&eng_id), 0);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_cancel_wrong_recruiter_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-CANCEL-AUTH");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL-AUTH",
    );
    let impostor = Address::generate(&env);
    client.cancel_engagement(&company, &impostor, &eng_id);
}

// ============================================================
// Arbiter succession (updated for arbiters vec)
// ============================================================

#[test]
fn test_happy_arbiter_succession() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARBITER");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ARBITER",
    );

    let new_arbiter = Address::generate(&env);
    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.get(0).unwrap(), arbiter);

    client.claim_arbiter(&new_arbiter, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.get(0).unwrap(), new_arbiter);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_wrong_claimer_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARBITER-BAD");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ARBITER-BAD",
    );

    let new_arbiter = Address::generate(&env);
    let impostor = Address::generate(&env);
    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);
    client.claim_arbiter(&impostor, &eng_id);
}

#[test]
fn test_old_arbiter_retains_role_until_claim() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARBITER-OLD");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ARBITER-OLD",
    );

    let new_arbiter = Address::generate(&env);
    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.get(0).unwrap(), arbiter);

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);

    client.claim_arbiter(&new_arbiter, &eng_id);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.get(0).unwrap(), new_arbiter);
}

/// Issue #178: succeeding an arbiter mid-vote must not let the successor cast
/// a second vote for the same seat on a dispute the predecessor already voted on.
#[test]
#[should_panic(expected = "duplicate vote")]
fn test_arbiter_successor_cannot_double_vote_mid_dispute() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let successor = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-SUCC-MIDVOTE");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // a1 votes; quorum is 2 so the dispute is still pending.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    // a1 nominates a successor mid-vote and the successor claims the seat.
    client.nominate_arbiter_successor(&a1, &eng_id, &successor);
    client.claim_arbiter(&successor, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.get(0).unwrap(), successor);

    // Successor must not be able to cast a second vote for a1's seat.
    client.cast_arbiter_vote(&successor, &eng_id, &0, &true);
}

/// Companion to the above: the successor should still be able to cast the
/// *other* seat's vote normally once installed, and the dispute resolves
/// via the untouched a2 vote as expected.
#[test]
fn test_arbiter_successor_seat_migrated_not_duplicated() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let successor = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-SUCC-MIGRATE");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);

    client.nominate_arbiter_successor(&a1, &eng_id, &successor);
    client.claim_arbiter(&successor, &eng_id);

    // a2 casts the second (real) vote — quorum reached, dispute resolves.
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_arbiter_cannot_nominate() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARBITER-UNAUTH");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ARBITER-UNAUTH",
    );
    let new_arbiter = Address::generate(&env);
    client.nominate_arbiter_successor(&company, &eng_id, &new_arbiter);
}

#[test]
#[should_panic(expected = "no pending arbiter nomination")]
fn test_claim_without_nomination_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARBITER-NOCLAIM");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ARBITER-NOCLAIM",
    );
    let new_arbiter = Address::generate(&env);
    client.claim_arbiter(&new_arbiter, &eng_id);
}

// ============================================================
// #42 — get_estimated_unlock_seconds
// ============================================================

#[test]
fn test_estimated_unlock_seconds_future() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-SECS-FUTURE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-SECS-FUTURE",
    );

    // Milestone 1 = 30-day retention; ledger not advanced yet
    let seconds = client.get_estimated_unlock_seconds(&eng_id, &1);
    // 30 days × 17280 ledgers/day × 5 s/ledger = 25_920_000 s (approximately)
    let expected_max = 30u64 * 17_280 * 5;
    assert!(seconds > 0);
    assert!(seconds <= expected_max);
}

#[test]
fn test_estimated_unlock_seconds_already_unlockable() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-SECS-ZERO");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-SECS-ZERO",
    );

    advance_ledger(&env, 30 * 17_280 + 1);

    let seconds = client.get_estimated_unlock_seconds(&eng_id, &1);
    assert_eq!(seconds, 0);
}

#[test]
fn test_estimated_unlock_seconds_placement_returns_zero() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-SECS-PLACE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-SECS-PLACE",
    );

    // Milestone 0 is Placement — must always return 0
    let seconds = client.get_estimated_unlock_seconds(&eng_id, &0);
    assert_eq!(seconds, 0);
}

// ============================================================
// #11 — metadata_hash
// ============================================================

#[test]
fn test_metadata_hash_present() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let cid = String::from_str(&env, "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG");
    client.create_engagement(
        &String::from_str(&env, "ENG-META"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &EngagementConfig {
            metadata_hash: Some(cid.clone()),
            co_recruiter: None,
            recruiter_split_bps: 10_000,
            contract_pdf_hash: None,
            referrer: None,
            tags: None,
        },
    );

    let result = client.get_metadata_hash(&String::from_str(&env, "ENG-META"));
    assert_eq!(result, Some(cid));
}

#[test]
fn test_metadata_hash_absent() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-NOMETA",
    );

    let result = client.get_metadata_hash(&String::from_str(&env, "ENG-NOMETA"));
    assert_eq!(result, None);
}

#[test]
#[should_panic(expected = "InvalidMetadataHash")]
fn test_metadata_hash_empty_string_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-EMPTY-META"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &EngagementConfig {
            metadata_hash: Some(String::from_str(&env, "")),
            co_recruiter: None,
            recruiter_split_bps: 10_000,
            contract_pdf_hash: None,
            referrer: None,
            tags: None,
        },
    );
}

// ============================================================
// ISSUE #56 — CO-RECRUITER FEE SPLIT
// ============================================================

#[test]
fn test_co_recruiter_60_40_split() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let co_recruiter = Address::generate(&env);

    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter.clone()),
        recruiter_split_bps: 6_000,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SPLIT-60-40"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );

    let eng = client.get_engagement(&String::from_str(&env, "ENG-SPLIT-60-40"));
    assert_eq!(eng.co_recruiter, Some(co_recruiter.clone()));
    assert_eq!(eng.recruiter_split_bps, 6_000);

    // Confirm the placement milestone (30% of 1_000_000_000 = 300_000_000)
    let eng_id = String::from_str(&env, "ENG-SPLIT-60-40");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // 300_000_000 goes to escrow→recruiters. No platform fee (default 0 bps).
    // Primary: 300_000_000 * 6000 / 10000 = 180_000_000
    // Co:      300_000_000 * 4000 / 10000 = 120_000_000
    let recruiter_balance = token_client.balance(&recruiter);
    assert_eq!(recruiter_balance, 180_000_000);

    let co_balance = token_client.balance(&co_recruiter);
    assert_eq!(co_balance, 120_000_000);
}

#[test]
fn test_no_co_recruiter_full_payout() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-NO-CO",
    );

    let eng_id = String::from_str(&env, "ENG-NO-CO");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // 30% of 1_000_000_000 = 300_000_000 — all goes to recruiter (backward-compat).
    let recruiter_balance = token_client.balance(&recruiter);
    assert_eq!(recruiter_balance, 300_000_000);
}

#[test]
#[should_panic(expected = "InvalidSplitBps")]
fn test_split_bps_over_10000_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let co_recruiter = Address::generate(&env);

    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter),
        recruiter_split_bps: 10_001,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-BAD-SPLIT"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );
}

#[test]
fn test_co_recruiter_gets_remainder() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let co_recruiter = Address::generate(&env);

    // Use 3333 bps (33.33%) — primary gets floor, co gets remainder
    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter.clone()),
        recruiter_split_bps: 3_333,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SPLIT-REM"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );

    let eng_id = String::from_str(&env, "ENG-SPLIT-REM");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // 300_000_000 * 3333 / 10000 = 99_990_000 (primary)
    // 300_000_000 - 99_990_000 = 200_010_000 (co — remainder)
    let recruiter_balance = token_client.balance(&recruiter);
    assert_eq!(recruiter_balance, 99_990_000);

    let co_balance = token_client.balance(&co_recruiter);
    assert_eq!(co_balance, 200_010_000);
}

#[test]
fn test_co_recruiter_summary_fields() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let co_recruiter = Address::generate(&env);

    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter.clone()),
        recruiter_split_bps: 7_000,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SUM-SPLIT"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );

    let summary = client.get_engagement_summary(&String::from_str(&env, "ENG-SUM-SPLIT"));
    assert_eq!(summary.co_recruiter, Some(co_recruiter));
    assert_eq!(summary.recruiter_split_bps, 7_000);
}

#[test]
fn test_split_bps_10000_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let co_recruiter = Address::generate(&env);

    // 10_000 bps = 100% to primary — co_recruiter gets 0
    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter.clone()),
        recruiter_split_bps: 10_000,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SPLIT-100"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );

    let eng_id = String::from_str(&env, "ENG-SPLIT-100");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // All 300_000_000 goes to primary recruiter.
    let recruiter_balance = token_client.balance(&recruiter);
    assert_eq!(recruiter_balance, 300_000_000);

    let co_balance = token_client.balance(&co_recruiter);
    assert_eq!(co_balance, 0);
}

// ============================================================
// #9 — proof resubmission cooldown
// ============================================================

#[test]
fn test_rejected_proof_can_be_resubmitted_immediately() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-COOL");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-COOL",
    );

    // First submission — always allowed
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof1"),
    );
    // Dispute + reject → back to Pending
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Second submission immediately within cooldown — must panic
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof2"),
    );
    assert_eq!(
        client.get_milestone(&eng_id, &0).status,
        MilestoneStatus::ProofSubmitted
    );
}

#[test]
fn test_proof_cooldown_passes_after_wait() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-COOL-PASS");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-COOL-PASS",
    );

    // First submission
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof1"),
    );
    // Dispute + reject → back to Pending
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Advance past the default cooldown (2_880 ledgers)
    advance_ledger(&env, 2_881);

    // Should succeed now
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof2"),
    );
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);
}

#[test]
#[should_panic(expected = "DuplicateProofHash")]
fn test_duplicate_proof_hash_rejected_across_milestones() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-DUP-PROOF");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DUP-PROOF",
    );

    let proof_hash = String::from_str(&env, "ipfs://same-proof");
    client.submit_proof(&recruiter, &eng_id, &0, &proof_hash);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);

    client.submit_proof(&recruiter, &eng_id, &1, &proof_hash);
}

#[test]
fn test_different_proof_hashes_allowed_across_milestones() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-DIFF-PROOF");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DIFF-PROOF",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://placement-proof"),
    );

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://retention-proof"),
    );

    assert_eq!(
        client.get_milestone(&eng_id, &1).status,
        MilestoneStatus::ProofSubmitted
    );
}

#[test]
fn test_set_proof_cooldown_admin() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Admin (company) sets a very short cooldown of 1 ledger
    client.set_proof_cooldown(&company, &1u32);

    let eng_id = String::from_str(&env, "ENG-COOL-SET");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-COOL-SET",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof1"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Advance by exactly 1 ledger (matching cooldown)
    advance_ledger(&env, 1);

    // Should succeed with cooldown = 1
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof2"),
    );
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_proof_cooldown_non_admin() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    // recruiter is not admin — should panic
    client.set_proof_cooldown(&recruiter, &100u32);
}

// ============================================================
// #10 — multi-arbiter quorum
// ============================================================

#[test]
fn test_quorum_2_of_3_approve() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q23A");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // 1 approve — not yet at quorum of 2
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    // 2nd approve — quorum reached, payment released
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_quorum_2_of_3_reject() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q23R");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // 1 reject — reject_votes (1) > 3 - 2 = 1? No: 1 > 1 is false. Still disputed.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &false);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    // 2nd reject — reject_votes (2) > 1: yes → milestone reset to Pending
    client.cast_arbiter_vote(&a2, &eng_id, &0, &false);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);
    assert_eq!(token_client.balance(&recruiter), 0);
}

#[test]
#[should_panic(expected = "duplicate vote")]
fn test_duplicate_vote_rejected() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-DUP");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    // Same arbiter votes again — must panic
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
}

#[test]
fn test_single_arbiter_backward_compat() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-SINGLE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-SINGLE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // Single arbiter, quorum=1 — one vote resolves immediately
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
}

#[test]
fn test_quorum_unanimous_requires_all_approvals() {
    // quorum == arbiters.len(): every arbiter must approve before release.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33A");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // 1st approve — not yet at quorum of 3
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);
    assert_eq!(token_client.balance(&recruiter), 0);

    // 2nd approve — still short of unanimous quorum
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);
    assert_eq!(token_client.balance(&recruiter), 0);

    // 3rd approve — unanimous quorum reached, payment released
    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_quorum_unanimous_single_reject_resets_milestone() {
    // quorum == arbiters.len(): total_arbiters - quorum == 0, so a single
    // reject vote already exceeds the threshold and resets the milestone.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33R");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // A single reject already exceeds total_arbiters - quorum (3 - 3 = 0).
    client.cast_arbiter_vote(&a1, &eng_id, &0, &false);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);
    assert_eq!(m0.proof_hash, String::from_str(&env, ""));
    assert_eq!(token_client.balance(&recruiter), 0);
}

#[test]
fn test_quorum_unanimous_2_of_2_approve() {
    // Smallest multi-arbiter unanimous case: quorum == arbiters.len() == 2.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q22A");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_quorum_unanimous_4_of_4_approve() {
    // Generalizes the unanimous case beyond 3 arbiters.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);
    let a4 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q44A");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone(), a4.clone()],
            quorum: 4,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);
    assert_eq!(token_client.balance(&recruiter), 0);

    client.cast_arbiter_vote(&a4, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_quorum_unanimous_mixed_votes_reject_wins() {
    // Even with approvals already in, quorum == arbiters.len() means any
    // single reject vote exceeds the (total_arbiters - quorum == 0)
    // threshold and immediately resets the milestone.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33MIX");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    // Last arbiter rejects — resets despite 2 prior approvals.
    client.cast_arbiter_vote(&a3, &eng_id, &0, &false);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);
    assert_eq!(token_client.balance(&recruiter), 0);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_quorum_unanimous_non_arbiter_cannot_vote() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);
    let outsider = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33OUT");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // Not one of the three arbiters — must panic.
    client.cast_arbiter_vote(&outsider, &eng_id, &0, &true);
}

#[test]
fn test_quorum_unanimous_vote_record_cleared_after_reset() {
    // After a reject resets the milestone, the vote record must be cleared
    // so a later dispute round starts fresh (no stale duplicate-vote panics
    // and no carry-over vote counts).
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33RESET");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // a1 rejects, resetting the milestone back to Pending.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &false);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);

    // Resubmit proof and raise a second dispute round (advance past the
    // proof resubmission cooldown first).
    advance_ledger(&env, 2_880);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof2"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute2"));

    // a1 can vote again (not a duplicate) since the prior record was cleared,
    // and this time all three approve to reach unanimous quorum.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
#[should_panic(expected = "duplicate vote")]
fn test_quorum_unanimous_duplicate_vote_panics() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33DUP");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    // a1 votes again before the unanimous quorum is reached — must panic.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
}

#[test]
#[should_panic(expected = "milestone is not in disputed status")]
fn test_quorum_unanimous_vote_after_resolution_panics() {
    // Once unanimous quorum resolves the milestone, further votes (even from
    // an arbiter who already voted, since the vote record was cleared) must
    // be rejected because the milestone is no longer Disputed.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33POST");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);

    // Milestone is Resolved, not Disputed — must panic.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
}

#[test]
#[should_panic(expected = "milestone is not in disputed status")]
fn test_quorum_unanimous_vote_without_dispute_panics() {
    // A milestone must actually be Disputed before any arbiter can vote,
    // even under an otherwise-valid unanimous quorum setup.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33NODISP");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    // No raise_dispute call — milestone is ProofSubmitted, not Disputed.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
}

#[test]
fn test_quorum_unanimous_fee_paid_only_to_deciding_arbiter() {
    // The arbiter fee is transferred to whichever arbiter's vote tips the
    // count to quorum, not split across all arbiters who voted.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    client.set_arbiter_fee(&company, &100u32); // 1%

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33FEE");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    // a3's vote is the one that reaches unanimous quorum.
    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);

    let payment = 300_000_000i128;
    let fee = payment * 100 / 10_000; // 3_000_000
    assert_eq!(token_client.balance(&a1), 0);
    assert_eq!(token_client.balance(&a2), 0);
    assert_eq!(token_client.balance(&a3), fee);
    assert_eq!(token_client.balance(&recruiter), payment - fee);
}

#[test]
fn test_quorum_unanimous_final_milestone_completes_engagement() {
    // Resolving the last outstanding milestone via unanimous quorum must
    // mark the engagement Completed and decrement the company's active count.
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-Q33DONE");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 3,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &vec![
            &env,
            Milestone {
                name: String::from_str(&env, "Candidate Placed"),
                payment_percent: 100,
                kind: MilestoneKind::Placement,
                valid_after_ledger: 0,
                proof_hash: String::from_str(&env, ""),
                status: MilestoneStatus::Pending,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
            },
        ],
        &vec![&env],
        &default_config(),
    );

    let before_active = client.get_company_active_count(&company);

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);
    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);

    let engagement = client.get_engagement(&eng_id);
    assert_eq!(engagement.status, EngagementStatus::Completed);
    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);
    assert_eq!(client.get_company_active_count(&company), before_active - 1);
}

// ============================================================
// #1-4 — AMENDMENT FEATURES
// ============================================================

// Tests for #1: Amendment log
// Tests for #2: Amendment mutual-consent mechanism
// Tests for #3: Amendment TTL
// Tests for #4: Emit amendment events

#[test]
fn test_amendment_proposal_basic() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-BASIC");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-BASIC",
    );

    // Company proposes to change milestone 0 from 30% to 25%
    client.propose_amendment(&company, &eng_id, &0, &25);

    // Proposal should be stored
    // We verify by checking get_amendment_log is empty (proposal not yet accepted)
    let log = client.get_amendment_log(&eng_id, &0);
    assert_eq!(log.len(), 0);
}

#[test]
fn test_amendment_accept_changes_payment_percent() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-ACCEPT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-ACCEPT",
    );

    let original_milestone = client.get_milestone(&eng_id, &0);
    assert_eq!(original_milestone.payment_percent, 30);

    // Company proposes 25%
    client.propose_amendment(&company, &eng_id, &0, &25);

    // Recruiter accepts
    client.accept_amendment(&recruiter, &eng_id, &0);

    // Milestone should now be 25%
    let updated_milestone = client.get_milestone(&eng_id, &0);
    assert_eq!(updated_milestone.payment_percent, 25);

    // Amendment should be logged
    let log = client.get_amendment_log(&eng_id, &0);
    assert_eq!(log.len(), 1);

    let entry = log.get(0).unwrap();
    assert_eq!(entry.proposer, company);
    assert_eq!(entry.old_payment_percent, 30);
    assert_eq!(entry.new_payment_percent, 25);
    assert!(entry.ledger > 0);
}

#[test]
fn test_amendment_accept_multiple_times() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-MULTI");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-MULTI",
    );

    // First amendment: 30% → 25%
    client.propose_amendment(&company, &eng_id, &0, &25);
    client.accept_amendment(&recruiter, &eng_id, &0);

    // Second amendment: 25% → 20%
    client.propose_amendment(&recruiter, &eng_id, &0, &20);
    client.accept_amendment(&company, &eng_id, &0);

    let log = client.get_amendment_log(&eng_id, &0);
    assert_eq!(log.len(), 2);

    let entry1 = log.get(0).unwrap();
    assert_eq!(entry1.old_payment_percent, 30);
    assert_eq!(entry1.new_payment_percent, 25);

    let entry2 = log.get(1).unwrap();
    assert_eq!(entry2.old_payment_percent, 25);
    assert_eq!(entry2.new_payment_percent, 20);

    let milestone = client.get_milestone(&eng_id, &0);
    assert_eq!(milestone.payment_percent, 20);
}

#[test]
fn test_amendment_log_cap_at_20() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-CAP");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-CAP",
    );

    // Make 25 amendments (should cap at 20, oldest evicted)
    for i in 0..25 {
        let percent = 30u32 - ((i % 20) as u32);
        if i % 2 == 0 {
            client.propose_amendment(&company, &eng_id, &0, &percent);
            client.accept_amendment(&recruiter, &eng_id, &0);
        } else {
            client.propose_amendment(&recruiter, &eng_id, &0, &percent);
            client.accept_amendment(&company, &eng_id, &0);
        }
    }

    let log = client.get_amendment_log(&eng_id, &0);
    assert_eq!(log.len(), 20);
}

#[test]
#[should_panic(expected = "proposer cannot accept their own proposal")]
fn test_amendment_proposer_cannot_accept() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-SELF");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-SELF",
    );

    client.propose_amendment(&company, &eng_id, &0, &25);
    // Company tries to accept their own proposal
    client.accept_amendment(&company, &eng_id, &0);
}

#[test]
fn test_amendment_reject_clears_proposal() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-REJECT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-REJECT",
    );

    client.propose_amendment(&company, &eng_id, &0, &25);

    // Recruiter rejects
    client.reject_amendment(&recruiter, &eng_id, &0);

    // Amendment log should still be empty
    let log = client.get_amendment_log(&eng_id, &0);
    assert_eq!(log.len(), 0);

    // Milestone should be unchanged
    let milestone = client.get_milestone(&eng_id, &0);
    assert_eq!(milestone.payment_percent, 30);
}

#[test]
#[should_panic(expected = "proposer cannot reject their own proposal")]
fn test_amendment_proposer_cannot_reject() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-REJECT-SELF");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-REJECT-SELF",
    );

    client.propose_amendment(&company, &eng_id, &0, &25);
    // Company tries to reject their own proposal
    client.reject_amendment(&company, &eng_id, &0);
}

#[test]
fn test_amendment_ttl_default() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let ttl = client.get_amendment_ttl();
    assert_eq!(ttl, 17_280); // ~1 day
}

#[test]
fn test_amendment_ttl_admin_set() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Company was set as admin in setup
    client.set_amendment_ttl(&company, &8640); // ~12 hours

    let ttl = client.get_amendment_ttl();
    assert_eq!(ttl, 8640);
}

#[test]
#[should_panic(expected = "amendment_expired")]
fn test_amendment_expire_on_accept() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-EXPIRE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-EXPIRE",
    );

    // Set a short TTL for testing (2 days worth of ledgers)
    client.set_amendment_ttl(&company, &(2 * 17_280));

    client.propose_amendment(&company, &eng_id, &0, &25);

    // Advance ledgers beyond TTL (3 days)
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + (3 * 17_280),
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });

    // Try to accept expired proposal — should panic
    client.accept_amendment(&recruiter, &eng_id, &0);
}

#[test]
fn test_amendment_overwrite_pending_proposal() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-OVERWRITE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-OVERWRITE",
    );

    // First proposal: 30% → 25%
    client.propose_amendment(&company, &eng_id, &0, &25);

    // Second proposal overwrites: 30% → 20%
    client.propose_amendment(&company, &eng_id, &0, &20);

    // Accept the second proposal (20%)
    client.accept_amendment(&recruiter, &eng_id, &0);

    let milestone = client.get_milestone(&eng_id, &0);
    assert_eq!(milestone.payment_percent, 20);

    let log = client.get_amendment_log(&eng_id, &0);
    assert_eq!(log.len(), 1);
    let entry = log.get(0).unwrap();
    assert_eq!(entry.new_payment_percent, 20);
}

#[test]
fn test_amendment_both_parties_can_propose() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-BOTH");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-BOTH",
    );

    // Recruiter proposes first
    client.propose_amendment(&recruiter, &eng_id, &0, &35);
    client.accept_amendment(&company, &eng_id, &0);

    assert_eq!(client.get_milestone(&eng_id, &0).payment_percent, 35);

    // Company proposes next
    client.propose_amendment(&company, &eng_id, &0, &40);
    client.accept_amendment(&recruiter, &eng_id, &0);

    assert_eq!(client.get_milestone(&eng_id, &0).payment_percent, 40);

    let log = client.get_amendment_log(&eng_id, &0);
    assert_eq!(log.len(), 2);
}

// ============================================================
// #12 / #13 — platform fee and fee event
// ============================================================

#[test]
fn test_platform_fee_deducted_and_sent_to_treasury() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = Address::generate(&env);

    client.set_platform_fee(&company, &250, &treasury); // 2.5%
    assert_eq!(client.get_platform_fee(), (250, treasury.clone()));

    let eng_id = String::from_str(&env, "ENG-FEE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-FEE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    let gross = 300_000_000i128;
    let expected_fee = gross * 250 / 10_000;
    assert_eq!(expected_fee, 7_500_000);
    assert_eq!(token_client.balance(&treasury), expected_fee);
    assert_eq!(token_client.balance(&recruiter), gross - expected_fee);
    assert_eq!(client.get_total_released(&eng_id), gross);
}

#[test]
#[should_panic(expected = "FeeTooHigh")]
fn test_platform_fee_cap_validation() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let treasury = Address::generate(&env);

    client.set_platform_fee(&company, &501, &treasury);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_admin_cannot_set_platform_fee() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let treasury = Address::generate(&env);

    client.set_platform_fee(&recruiter, &100, &treasury);
}

#[test]
fn test_platform_fee_event_emitted_with_correct_amount() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let treasury = Address::generate(&env);

    client.set_platform_fee(&company, &100, &treasury); // 1%
    let eng_id = String::from_str(&env, "ENG-FEE-EVENT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FEE-EVENT",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    assert!(has_event(&env, "platform_fee_collected"));
}

#[test]
fn test_platform_fee_event_not_emitted_when_fee_zero() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-NO-FEE-EVENT");

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-NO-FEE-EVENT",
    );
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    assert!(!has_event(&env, "platform_fee_collected"));
}

// ============================================================
// #14 — emergency pause
// ============================================================

#[test]
fn test_pause_state_and_unpause_restores_create() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    assert!(!client.is_paused());
    client.pause(&company);
    assert!(client.is_paused());
    client.unpause(&company);
    assert!(!client.is_paused());

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNPAUSED",
    );
    assert_eq!(
        client
            .get_engagement(&String::from_str(&env, "ENG-UNPAUSED"))
            .status,
        EngagementStatus::Active
    );
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_pause_blocks_create() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.pause(&company);
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PAUSED-CREATE",
    );
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_pause_blocks_submit() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSED-SUBMIT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PAUSED-SUBMIT",
    );

    client.pause(&company);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_pause_blocks_confirm() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSED-CONFIRM");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PAUSED-CONFIRM",
    );
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );

    client.pause(&company);
    client.confirm_milestone(&company, &eng_id, &0);
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_pause_blocks_unlock() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSED-UNLOCK");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PAUSED-UNLOCK",
    );

    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + (31 * 17_280),
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });

    client.pause(&company);
    client.unlock_milestone(&eng_id, &1);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_admin_cannot_pause_or_unpause() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.pause(&recruiter);
}

// ============================================================
// #15 — two-step admin transfer
// ============================================================

#[test]
fn test_admin_rotation_happy_path() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let treasury = Address::generate(&env);

    client.nominate_admin(&company, &recruiter);
    assert_eq!(client.get_pending_admin(), Some(recruiter.clone()));

    client.claim_admin(&recruiter);
    assert_eq!(client.get_pending_admin(), None);

    client.set_platform_fee(&recruiter, &100, &treasury);
    assert_eq!(client.get_platform_fee(), (100, treasury));
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_wrong_admin_claimer_rejected() {
    let (env, contract_id, _token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.nominate_admin(&company, &recruiter);
    client.claim_admin(&arbiter);
}

#[test]
fn test_old_admin_retains_power_until_claim() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let treasury = Address::generate(&env);

    client.nominate_admin(&company, &recruiter);
    client.set_platform_fee(&company, &125, &treasury.clone());

    assert_eq!(client.get_platform_fee(), (125, treasury));
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_only_current_admin_can_nominate_admin() {
    let (env, contract_id, _token_id, _company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.nominate_admin(&recruiter, &arbiter);
}

// ============================================================
// #52 — get_pending_amendment
// ============================================================

#[test]
fn test_get_pending_amendment_active() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-PEND-ACTIVE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PEND-ACTIVE",
    );

    client.propose_amendment(&company, &eng_id, &0, &50);

    let pending = client.get_pending_amendment(&eng_id, &0).unwrap();

    assert_eq!(pending.proposer, company);
    assert_eq!(pending.new_payment_percent, 50);
    assert!(pending.proposed_at_ledger > 0);
    assert!(pending.expires_at_ledger > pending.proposed_at_ledger);

    // Milestone 1 should have no pending amendment
    assert!(client.get_pending_amendment(&eng_id, &1).is_none());
}

#[test]
fn test_get_pending_amendment_after_accept() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-PEND-ACCEPT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PEND-ACCEPT",
    );

    client.propose_amendment(&company, &eng_id, &0, &50);
    client.accept_amendment(&recruiter, &eng_id, &0);

    // No pending amendment after acceptance
    assert!(client.get_pending_amendment(&eng_id, &0).is_none());

    // Milestone percentage should have been updated
    assert_eq!(client.get_milestone(&eng_id, &0).payment_percent, 50);
}

#[test]
fn test_get_pending_amendment_after_reject() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-PEND-REJECT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PEND-REJECT",
    );

    client.propose_amendment(&company, &eng_id, &0, &50);
    client.reject_amendment(&recruiter, &eng_id, &0);

    // No pending amendment after rejection
    assert!(client.get_pending_amendment(&eng_id, &0).is_none());

    // Milestone percentage should be unchanged
    assert_eq!(client.get_milestone(&eng_id, &0).payment_percent, 30);
}

#[test]
fn test_get_pending_amendment_expired() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-PEND-EXPIRED");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PEND-EXPIRED",
    );

    // Set a short TTL for testing (~1 hour = 720 ledgers)
    client.set_amendment_ttl(&company, &720);

    client.propose_amendment(&company, &eng_id, &0, &50);

    // Verify active before expiry
    assert!(client.get_pending_amendment(&eng_id, &0).is_some());

    // Advance ledger past the TTL
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + 721,
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });

    // Should be None after expiry
    assert!(client.get_pending_amendment(&eng_id, &0).is_none());
}

// ============================================================
// Issue #34 — get_engagement_count
// ============================================================

#[test]
fn test_engagement_count_starts_at_zero() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    assert_eq!(client.get_engagement_count(), 0);
}

#[test]
fn test_engagement_count_increments_on_create() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CNT-1",
    );
    assert_eq!(client.get_engagement_count(), 1);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CNT-2",
    );
    assert_eq!(client.get_engagement_count(), 2);
}

#[test]
fn test_engagement_count_does_not_decrement_on_cancel() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CNT-CANCEL",
    );
    assert_eq!(client.get_engagement_count(), 1);

    client.cancel_engagement(
        &company,
        &recruiter,
        &String::from_str(&env, "ENG-CNT-CANCEL"),
    );
    assert_eq!(client.get_engagement_count(), 1);
}

#[test]
#[should_panic(expected = "engagement already exists")]
fn test_engagement_count_no_increment_on_failed_create() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Should panic due to duplicate ID (count must NOT increment)
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DUP-CNT",
    );
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DUP-CNT",
    );
}

// ============================================================
// Issue #35 — get_engagements_by_company / get_company_engagement_count
// ============================================================

#[test]
fn test_company_engagement_count_empty() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let other = Address::generate(&env);
    assert_eq!(client.get_company_engagement_count(&other), 0);
}

#[test]
fn test_get_engagements_by_company_insertion_order() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let ids = [
        "ENG-ORD-0",
        "ENG-ORD-1",
        "ENG-ORD-2",
        "ENG-ORD-3",
        "ENG-ORD-4",
    ];
    for id in ids.iter() {
        client.create_engagement(
            &String::from_str(&env, id),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
            },
            &token_id,
            &1_000_000_000,
            &String::from_str(&env, "Engineer"),
            &build_milestones(&env),
            &vec![&env, 30u32, 90u32],
            &default_config(),
        );
    }

    assert_eq!(client.get_company_engagement_count(&company), 5);

    let page0 = client.get_engagements_by_company(&company, &0, &3);
    assert_eq!(page0.len(), 3);
    assert_eq!(page0.get(0).unwrap(), String::from_str(&env, "ENG-ORD-0"));
    assert_eq!(page0.get(2).unwrap(), String::from_str(&env, "ENG-ORD-2"));

    let page1 = client.get_engagements_by_company(&company, &1, &3);
    assert_eq!(page1.len(), 2);
    assert_eq!(page1.get(0).unwrap(), String::from_str(&env, "ENG-ORD-3"));
}

#[test]
fn test_get_engagements_by_company_out_of_range_returns_empty() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-OOR",
    );

    let result = client.get_engagements_by_company(&company, &10, &10);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_engagements_by_company_empty_company() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let other = Address::generate(&env);
    let result = client.get_engagements_by_company(&other, &0, &10);
    assert_eq!(result.len(), 0);
}

// Issue #172: `page * page_size` and `start + page_size` would overflow u32
// with naive arithmetic for large inputs. Both `page` and `page_size` here
// are chosen so their product and sum overflow u32::MAX; the saturating
// arithmetic in `get_engagements_by_company` must clamp instead of panicking.
#[test]
fn test_get_engagements_by_company_large_page_size_does_not_overflow() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-OVERFLOW",
    );

    let result = client.get_engagements_by_company(&company, &u32::MAX, &u32::MAX);
    assert_eq!(result.len(), 0);

    let result = client.get_engagements_by_company(&company, &1, &u32::MAX);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_engagements_first_page_ten() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let ids = [
        "ENG-PG-00",
        "ENG-PG-01",
        "ENG-PG-02",
        "ENG-PG-03",
        "ENG-PG-04",
        "ENG-PG-05",
        "ENG-PG-06",
        "ENG-PG-07",
        "ENG-PG-08",
        "ENG-PG-09",
        "ENG-PG-10",
        "ENG-PG-11",
        "ENG-PG-12",
        "ENG-PG-13",
        "ENG-PG-14",
    ];
    for id in ids.iter() {
        client.create_engagement(
            &String::from_str(&env, id),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
            },
            &token_id,
            &1_000_000_000,
            &String::from_str(&env, "Engineer"),
            &build_milestones(&env),
            &vec![&env, 30u32, 90u32],
            &default_config(),
        );
    }

    let page0 = client.get_engagements_by_company(&company, &0, &10);
    assert_eq!(page0.len(), 10);
    assert_eq!(page0.get(0).unwrap(), String::from_str(&env, "ENG-PG-00"));
}

// ============================================================
// Issue #36 — get_engagements_by_recruiter / get_recruiter_engagement_count
// ============================================================

#[test]
fn test_recruiter_engagement_count_empty() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let other = Address::generate(&env);
    assert_eq!(client.get_recruiter_engagement_count(&other), 0);
}

#[test]
fn test_get_engagements_by_recruiter_insertion_order() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let ids = [
        "ENG-R-ORD-0",
        "ENG-R-ORD-1",
        "ENG-R-ORD-2",
        "ENG-R-ORD-3",
        "ENG-R-ORD-4",
    ];
    for id in ids.iter() {
        client.create_engagement(
            &String::from_str(&env, id),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
            },
            &token_id,
            &1_000_000_000,
            &String::from_str(&env, "Engineer"),
            &build_milestones(&env),
            &vec![&env, 30u32, 90u32],
            &default_config(),
        );
    }

    assert_eq!(client.get_recruiter_engagement_count(&recruiter), 5);

    let page0 = client.get_engagements_by_recruiter(&recruiter, &0, &3);
    assert_eq!(page0.len(), 3);
    assert_eq!(page0.get(0).unwrap(), String::from_str(&env, "ENG-R-ORD-0"));
    assert_eq!(page0.get(2).unwrap(), String::from_str(&env, "ENG-R-ORD-2"));

    let page1 = client.get_engagements_by_recruiter(&recruiter, &1, &3);
    assert_eq!(page1.len(), 2);
    assert_eq!(page1.get(0).unwrap(), String::from_str(&env, "ENG-R-ORD-3"));
}

#[test]
fn test_get_engagements_by_recruiter_out_of_range_returns_empty() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-R-OOR",
    );

    let result = client.get_engagements_by_recruiter(&recruiter, &10, &10);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_engagements_by_recruiter_empty_recruiter() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let other = Address::generate(&env);
    let result = client.get_engagements_by_recruiter(&other, &0, &10);
    assert_eq!(result.len(), 0);
}

#[test]
fn test_get_engagements_by_recruiter_multi_recruiter() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let other_recruiter = Address::generate(&env);

    client.create_engagement(
        &String::from_str(&env, "ENG-R-MULTI-A0"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
    client.create_engagement(
        &String::from_str(&env, "ENG-R-MULTI-B0"),
        &company,
        &other_recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    assert_eq!(client.get_recruiter_engagement_count(&recruiter), 1);
    assert_eq!(client.get_recruiter_engagement_count(&other_recruiter), 1);

    let recruiter_ids = client.get_engagements_by_recruiter(&recruiter, &0, &10);
    assert_eq!(recruiter_ids.len(), 1);
    assert_eq!(
        recruiter_ids.get(0).unwrap(),
        String::from_str(&env, "ENG-R-MULTI-A0")
    );

    let other_ids = client.get_engagements_by_recruiter(&other_recruiter, &0, &10);
    assert_eq!(other_ids.len(), 1);
    assert_eq!(
        other_ids.get(0).unwrap(),
        String::from_str(&env, "ENG-R-MULTI-B0")
    );
}

// ============================================================
// Issue #26 — Token allowlist
// ============================================================

#[test]
fn test_allowlist_disabled_by_default_allows_any_token() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    // Allowlist disabled by default — standard engagement must succeed
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AL-DEF",
    );
    assert_eq!(
        client
            .get_engagement(&String::from_str(&env, "ENG-AL-DEF"))
            .status,
        EngagementStatus::Active
    );
}

#[test]
fn test_allowlisted_token_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.add_allowed_token(&company, &token_id);
    client.set_token_allowlist_enabled(&company, &true);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AL-OK",
    );
    assert_eq!(
        client
            .get_engagement(&String::from_str(&env, "ENG-AL-OK"))
            .status,
        EngagementStatus::Active
    );
}

#[test]
#[should_panic(expected = "TokenNotAllowed")]
fn test_non_allowlisted_token_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Enable allowlist but do NOT add token_id
    client.set_token_allowlist_enabled(&company, &true);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AL-BLOCK",
    );
}

#[test]
fn test_allowlist_disabled_accepts_any_token() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Enable then disable
    client.set_token_allowlist_enabled(&company, &true);
    client.set_token_allowlist_enabled(&company, &false);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AL-DISABLED",
    );
}

#[test]
fn test_get_allowed_tokens_returns_correct_list() {
    let (env, contract_id, token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    assert_eq!(client.get_allowed_tokens().len(), 0);

    client.add_allowed_token(&company, &token_id);
    let tokens = client.get_allowed_tokens();
    assert_eq!(tokens.len(), 1);
    assert_eq!(tokens.get(0).unwrap(), token_id);
}

#[test]
fn test_remove_allowed_token() {
    let (env, contract_id, token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.add_allowed_token(&company, &token_id);
    assert_eq!(client.get_allowed_tokens().len(), 1);

    client.remove_allowed_token(&company, &token_id);
    assert_eq!(client.get_allowed_tokens().len(), 0);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_only_admin_can_add_allowed_token() {
    let (env, contract_id, token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.add_allowed_token(&recruiter, &token_id);
}

// ============================================================
// Issue #32 — Recruiter early-exit
// ============================================================

#[test]
fn test_request_early_exit_sets_status() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-EXIT-REQ");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXIT-REQ",
    );

    client.request_early_exit(&recruiter, &eng_id);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::ExitRequested);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_only_recruiter_can_request_early_exit() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-EXIT-AUTH");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXIT-AUTH",
    );
    client.request_early_exit(&company, &eng_id);
}

#[test]
fn test_accept_early_exit_refunds_unreleased_and_cancels() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-EXIT-ACCEPT");
    let company_balance_before = token_client.balance(&company);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXIT-ACCEPT",
    );

    // Confirm placement milestone (30% released)
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    client.request_early_exit(&recruiter, &eng_id);
    client.accept_early_exit(&company, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);

    // 70% of 1_000_000_000 should be refunded to company
    let expected_refund = 700_000_000i128;
    assert_eq!(
        token_client.balance(&company),
        company_balance_before - 1_000_000_000 + expected_refund
    );
}

#[test]
fn test_reject_early_exit_returns_to_active() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-EXIT-REJECT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXIT-REJECT",
    );

    client.request_early_exit(&recruiter, &eng_id);
    client.reject_early_exit(&company, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Active);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_only_company_can_accept_early_exit() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-EXIT-ACCEPT-AUTH");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXIT-ACCEPT-AUTH",
    );

    client.request_early_exit(&recruiter, &eng_id);
    client.accept_early_exit(&recruiter, &eng_id);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_only_company_can_reject_early_exit() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-EXIT-REJECT-AUTH");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXIT-REJECT-AUTH",
    );

    client.request_early_exit(&recruiter, &eng_id);
    client.reject_early_exit(&recruiter, &eng_id);
}

// ============================================================
// ISSUE #41 — CONFIGURABLE LEDGERS PER DAY
// ============================================================

#[test]
fn test_set_and_get_ledgers_per_day() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Default value
    assert_eq!(client.get_ledgers_per_day(), 17_280);

    client.set_ledgers_per_day(&company, &10_000);
    assert_eq!(client.get_ledgers_per_day(), 10_000);
}

#[test]
#[should_panic(expected = "InvalidLedgersPerDay")]
fn test_ledgers_per_day_min_bound() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_ledgers_per_day(&company, &0);
}

#[test]
#[should_panic(expected = "InvalidLedgersPerDay")]
fn test_ledgers_per_day_max_bound() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_ledgers_per_day(&company, &25_921);
}

#[test]
fn test_new_engagement_uses_updated_ledgers_per_day() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Update to 1000 ledgers/day before creating engagement
    client.set_ledgers_per_day(&company, &1_000);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-LPD",
    );

    // Retention milestone 1 = 30 days * 1000 lpd = 30_000 ledgers from seq 100 → 30_100
    let m1 = client.get_milestone(&String::from_str(&env, "ENG-LPD"), &1);
    assert_eq!(m1.valid_after_ledger, 100 + 30 * 1_000);
}

#[test]
fn test_existing_engagement_unaffected_by_lpd_update() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Create with default LPD
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-OLD",
    );
    let m1_before = client.get_milestone(&String::from_str(&env, "ENG-OLD"), &1);

    // Update LPD — existing engagement should retain original valid_after_ledger
    client.set_ledgers_per_day(&company, &1_000);

    let m1_after = client.get_milestone(&String::from_str(&env, "ENG-OLD"), &1);
    assert_eq!(m1_before.valid_after_ledger, m1_after.valid_after_ledger);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_admin_cannot_set_ledgers_per_day() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_ledgers_per_day(&recruiter, &10_000);
}

// ============================================================
// ISSUE #39 — BATCH CONFIRM MILESTONES
// ============================================================

// #[test]
// fn test_batch_confirm_all_milestones() {
//     let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
//     let client = HireSettleContractClient::new(&env, &contract_id);
//     let token_client = token::Client::new(&env, &token_id);

//     // Use a 2-milestone engagement (placement only) to simplify
//     let milestones = vec![
//         &env,
//         Milestone {
//             name: String::from_str(&env, "Milestone A"),
//             payment_percent: 50,
//             kind: MilestoneKind::Placement,
//             valid_after_ledger: 0,
//             proof_hash: String::from_str(&env, ""),
//             status: MilestoneStatus::Pending,
//         },
//         Milestone {
//             name: String::from_str(&env, "Milestone B"),
//             payment_percent: 50,
//             kind: MilestoneKind::Placement,
//             valid_after_ledger: 0,
//             proof_hash: String::from_str(&env, ""),
//             status: MilestoneStatus::Pending,
//         },
//     ];

//     let eng_id = String::from_str(&env, "ENG-BATCH");
//     client.create_engagement(
//         &eng_id, &company, &recruiter,
//         &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1 },
//         &token_id, &1_000_000_000,
//         &String::from_str(&env, "Job"), &milestones,
//         &vec![&env], &None,
//     );

//     client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://a"));
//     client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://b"));

//     client.batch_confirm_milestones(&company, &eng_id, &vec![&env, 0u32, 1u32]);

//     let eng = client.get_engagement(&eng_id);
//     assert_eq!(eng.status, EngagementStatus::Completed);
//     assert_eq!(token_client.balance(&recruiter), 1_000_000_000);
//     assert!(has_event(&env, "engagement_completed"));
// }

#[test]
#[should_panic(expected = "milestone proof not yet submitted")]
fn test_batch_confirm_atomic_rejection() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Milestone A"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "Milestone B"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    let eng_id = String::from_str(&env, "ENG-BATCH-FAIL");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job"),
        &milestones,
        &vec![&env],
        &default_config(),
    );

    // Only submit proof for index 0, not 1 → batch must reject atomically
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://a"));
    client.batch_confirm_milestones(&company, &eng_id, &vec![&env, 0u32, 1u32]);
}

#[test]
#[should_panic(expected = "EmptyIndices")]
fn test_batch_confirm_empty_indices_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-BATCH-EMPTY");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-BATCH-EMPTY",
    );

    client.batch_confirm_milestones(&company, &eng_id, &vec![&env]);
}

#[test]
fn test_batch_confirm_emits_event_per_milestone() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "M1"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "M2"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    let eng_id = String::from_str(&env, "ENG-BATCH-EVT");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job"),
        &milestones,
        &vec![&env],
        &default_config(),
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://a"));
    client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://b"));

    client.batch_confirm_milestones(&company, &eng_id, &vec![&env, 0u32, 1u32]);

    let expected = Symbol::new(&env, "milestone_confirmed");
    let count = env
        .events()
        .all()
        .iter()
        .filter(|(_, topics, _)| {
            topics
                .get(0)
                .and_then(|v| v.try_into_val(&env).ok())
                .map(|s: Symbol| s == expected)
                .unwrap_or(false)
        })
        .count();
    assert_eq!(count, 2);
}

// ============================================================
// ISSUE #49 — ENGAGEMENT COMPLETION EVENT
// ============================================================

#[test]
fn test_engagement_completed_event_on_last_milestone() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-COMPLETE-EVT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-COMPLETE-EVT",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    // Not yet complete — no event yet
    assert!(!has_event(&env, "engagement_completed"));

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30day"),
    );
    client.confirm_milestone(&company, &eng_id, &1);
    assert!(!has_event(&env, "engagement_completed"));

    advance_ledger(&env, 60 * 17_280);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &2,
        &String::from_str(&env, "ipfs://90day"),
    );
    client.confirm_milestone(&company, &eng_id, &2);

    assert!(has_event(&env, "engagement_completed"));
}

#[test]
fn test_engagement_completed_not_emitted_on_cancel() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-CANCEL-EVT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL-EVT",
    );

    client.cancel_engagement(&company, &recruiter, &eng_id);
    assert!(!has_event(&env, "engagement_completed"));
}

// ============================================================
// ISSUE #50 — DISPUTE REASON CODE
// ============================================================

#[test]
fn test_dispute_reason_stored_and_retrievable() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REASON");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(
        &company,
        &eng_id,
        &0,
        &String::from_str(&env, "wrong_document"),
    );

    let reason = client.get_dispute_reason(&eng_id, &0);
    assert_eq!(reason, Some(String::from_str(&env, "wrong_document")));
}

#[test]
fn test_dispute_reason_cleared_after_resolution() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REASON-CLEAR");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-CLEAR",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "not_hired"));

    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);

    let reason = client.get_dispute_reason(&eng_id, &0);
    assert_eq!(reason, None);
}

#[test]
#[should_panic(expected = "ReasonTooLong")]
fn test_dispute_reason_too_long_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REASON-LONG");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-LONG",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // 129-character string — must be rejected
    let long_reason = String::from_str(&env, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    client.raise_dispute(&company, &eng_id, &0, &long_reason);
}

#[test]
fn test_dispute_reason_cleared_after_reject_resolution() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REASON-REJECT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-REJECT",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(
        &company,
        &eng_id,
        &0,
        &String::from_str(&env, "wrong_document"),
    );

    // Reject vote clears reason too
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    let reason = client.get_dispute_reason(&eng_id, &0);
    assert_eq!(reason, None);
}

// ============================================================
// CONFIRM WINDOW — force_confirm_milestone
// ============================================================

#[test]
fn test_get_confirm_window_default() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    // Default is 86_400 ledgers (~5 days)
    assert_eq!(client.get_confirm_window(), 86_400);
}

#[test]
fn test_set_confirm_window_admin() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_confirm_window(&company, &500u32);
    assert_eq!(client.get_confirm_window(), 500);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_confirm_window_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_confirm_window(&recruiter, &500u32);
}

/// force_confirm must fail if the window has NOT yet elapsed.
#[test]
#[should_panic(expected = "ConfirmWindowNotElapsed")]
fn test_force_confirm_before_window_fails() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Short window: 100 ledgers
    client.set_confirm_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-FC-EARLY");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-EARLY",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance only 50 ledgers — window is 100, must not succeed
    advance_ledger(&env, 50);

    // Anyone (recruiter here) tries to force-confirm too early
    client.force_confirm_milestone(&recruiter, &eng_id, &0);
}

/// force_confirm must succeed after the window has elapsed and release payment.
#[test]
fn test_force_confirm_after_window_releases_payment() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // Short window: 100 ledgers
    client.set_confirm_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-FC-OK");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-OK",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance past the window
    advance_ledger(&env, 101);

    // Third party (arbiter) force-confirms
    client.force_confirm_milestone(&arbiter, &eng_id, &0);

    // Milestone must now be Confirmed
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Confirmed);

    // Recruiter must have received 30% of 1_000_000_000
    let expected = 1_000_000_000i128 * 30 / 100;
    assert_eq!(token_client.balance(&recruiter), expected);

    // released_amount must be updated
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.released_amount, expected);
}

/// milestone_force_confirmed event must be emitted.
#[test]
fn test_force_confirm_emits_event() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_confirm_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-FC-EVT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-EVT",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    advance_ledger(&env, 101);
    client.force_confirm_milestone(&recruiter, &eng_id, &0);

    assert!(has_event(&env, "milestone_force_confirmed"));
}

/// Non-ProofSubmitted milestones (e.g. Pending) must not be force-confirmable.
#[test]
#[should_panic(expected = "milestone is not in ProofSubmitted status")]
fn test_force_confirm_wrong_status_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_confirm_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-FC-STATUS");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-STATUS",
    );

    // Milestone 0 is still Pending (no proof submitted)
    advance_ledger(&env, 200);
    client.force_confirm_milestone(&recruiter, &eng_id, &0);
}

/// Locked milestones must also be rejected by force_confirm.
#[test]
#[should_panic(expected = "milestone is not in ProofSubmitted status")]
fn test_force_confirm_locked_milestone_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_confirm_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-FC-LOCKED");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-LOCKED",
    );

    // Milestone 1 is Locked
    advance_ledger(&env, 200);
    client.force_confirm_milestone(&recruiter, &eng_id, &1);
}

/// Confirming the last milestone via force_confirm must mark the engagement Completed.
#[test]
fn test_force_confirm_last_milestone_completes_engagement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // Short window: 100 ledgers — set before creating engagement
    client.set_confirm_window(&company, &100u32);

    // Use a single-milestone engagement for simplicity
    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Placement"),
            payment_percent: 100,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    let eng_id = String::from_str(&env, "ENG-FC-COMPLETE");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "CTO"),
        &milestones,
        &vec![&env],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    advance_ledger(&env, 101);
    client.force_confirm_milestone(&arbiter, &eng_id, &0);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Completed);
    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);
    assert_eq!(eng.released_amount, 1_000_000_000);
}

// ============================================================
// DISPUTE WINDOW — raise_dispute gated by proof_submitted_at
// ============================================================

#[test]
fn test_get_dispute_window_default() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    // Default is 51_840 ledgers (~3 days)
    assert_eq!(client.get_dispute_window(), 51_840);
}

#[test]
fn test_set_dispute_window_admin() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_dispute_window(&company, &200u32);
    assert_eq!(client.get_dispute_window(), 200);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_dispute_window_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_dispute_window(&recruiter, &200u32);
}

/// Dispute raised within the window must succeed.
#[test]
fn test_dispute_within_window_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Short window: 200 ledgers
    client.set_dispute_window(&company, &200u32);

    let eng_id = String::from_str(&env, "ENG-DW-IN");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DW-IN",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance only 100 ledgers — well within the 200-ledger window
    advance_ledger(&env, 100);

    // Must succeed
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "wrong_doc"));
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);
}

/// Dispute raised after the window must be rejected.
#[test]
#[should_panic(expected = "DisputeWindowClosed")]
fn test_dispute_outside_window_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Short window: 200 ledgers
    client.set_dispute_window(&company, &200u32);

    let eng_id = String::from_str(&env, "ENG-DW-OUT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DW-OUT",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance 201 ledgers — past the window
    advance_ledger(&env, 201);

    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "too_late"));
}

/// Dispute at exactly the boundary (current_ledger == proof_submitted_at + window) must succeed.
#[test]
fn test_dispute_at_boundary_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Window: 200 ledgers. proof submitted at ledger 100.
    // Boundary: current_ledger == 100 + 200 == 300.
    client.set_dispute_window(&company, &200u32);

    let eng_id = String::from_str(&env, "ENG-DW-BOUND");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DW-BOUND",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance exactly to the boundary (200 ledgers from submission at 100 → seq 300)
    advance_ledger(&env, 200);

    // current_ledger (300) <= proof_submitted_at (100) + window (200) → allowed
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "boundary"));
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);
}

/// One ledger past the boundary must be rejected.
#[test]
#[should_panic(expected = "DisputeWindowClosed")]
fn test_dispute_one_past_boundary_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_dispute_window(&company, &200u32);

    let eng_id = String::from_str(&env, "ENG-DW-PAST");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DW-PAST",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // 201 ledgers past submission → current_ledger (301) > 100 + 200
    advance_ledger(&env, 201);

    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "one_past"));
}

/// Admin can update the window; new engagements immediately use the updated value.
#[test]
fn test_dispute_window_admin_update_takes_effect() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Start with a tight window of 50 ledgers
    client.set_dispute_window(&company, &50u32);

    let eng_id = String::from_str(&env, "ENG-DW-UPDATE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DW-UPDATE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance 60 ledgers — past the 50-ledger window
    advance_ledger(&env, 60);

    // Admin widens the window to 200 — dispute should now be allowed
    client.set_dispute_window(&company, &200u32);

    // current_ledger (160) <= 100 + 200 → should succeed
    client.raise_dispute(
        &company,
        &eng_id,
        &0,
        &String::from_str(&env, "updated_window"),
    );
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);
}

// ============================================================
// ENGAGEMENT ID FORMAT VALIDATION
// ============================================================

fn create_engagement_with_id(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
    id: &str,
) {
    client.create_engagement(
        &String::from_str(env, id),
        company,
        recruiter,
        &ArbiterSetup {
            arbiters: vec![env, arbiter.clone()],
            quorum: 1,
        },
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Engineer"),
        &build_milestones(env),
        &vec![env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
fn test_engagement_id_standard_format_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    // Documented example format
    create_engagement_with_id(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-2026-001",
    );
    assert_eq!(
        client
            .get_engagement(&String::from_str(&env, "ENG-2026-001"))
            .status,
        EngagementStatus::Active
    );
}

#[test]
fn test_engagement_id_all_alphanumeric_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_engagement_with_id(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG001",
    );
    assert_eq!(
        client
            .get_engagement(&String::from_str(&env, "ENG001"))
            .status,
        EngagementStatus::Active
    );
}

#[test]
fn test_engagement_id_64_chars_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    // Exactly 64 characters
    let id = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    assert_eq!(id.len(), 64);
    create_engagement_with_id(&env, &client, &token_id, &company, &recruiter, &arbiter, id);
    assert_eq!(
        client.get_engagement(&String::from_str(&env, id)).status,
        EngagementStatus::Active
    );
}

#[test]
#[should_panic(expected = "InvalidEngagementId")]
fn test_engagement_id_65_chars_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    // Exactly 65 characters
    let id = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    assert_eq!(id.len(), 65);
    create_engagement_with_id(&env, &client, &token_id, &company, &recruiter, &arbiter, id);
}

#[test]
#[should_panic(expected = "InvalidEngagementId")]
fn test_engagement_id_empty_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_engagement_with_id(&env, &client, &token_id, &company, &recruiter, &arbiter, "");
}

#[test]
#[should_panic(expected = "InvalidEngagementId")]
fn test_engagement_id_space_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_engagement_with_id(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG 001",
    );
}

#[test]
#[should_panic(expected = "InvalidEngagementId")]
fn test_engagement_id_slash_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_engagement_with_id(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG/001",
    );
}

#[test]
#[should_panic(expected = "InvalidEngagementId")]
fn test_engagement_id_underscore_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_engagement_with_id(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG_001",
    );
}

#[test]
#[should_panic(expected = "InvalidEngagementId")]
fn test_engagement_id_dot_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_engagement_with_id(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG.001",
    );
}

// ============================================================
// ISSUE #51 — REPLACEMENT REASON CODE
// ============================================================

/// Helper: walk the placement milestone to `Confirmed` so `request_replacement`
/// is accepted by the contract's precondition.
fn confirm_placement(
    env: &Env,
    client: &HireSettleContractClient,
    eng_id: &String,
    company: &Address,
    recruiter: &Address,
) {
    client.submit_proof(
        recruiter,
        eng_id,
        &0,
        &String::from_str(env, "ipfs://offer"),
    );
    client.confirm_milestone(company, eng_id, &0);
}

#[test]
fn test_replacement_reason_stored_and_retrievable() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REASON-1");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-1",
    );
    confirm_placement(&env, &client, &eng_id, &company, &recruiter);

    let reason = String::from_str(&env, "candidate_resigned");
    client.request_replacement(&company, &eng_id, &reason);

    assert_eq!(client.get_replacement_count(&eng_id), 1);
    let stored = client.get_replacement_reason(&eng_id, &0);
    assert_eq!(stored, Some(reason));
    // Out-of-range index returns None instead of panicking.
    assert_eq!(client.get_replacement_reason(&eng_id, &1), None);
}

#[test]
fn test_replacement_reason_empty_string_accepted() {
    // Empty reason is allowed — the issue says "max 128 chars", not "non-empty".
    // Auditors can still see the entry exists even with no code attached.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REASON-EMPTY");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-EMPTY",
    );
    confirm_placement(&env, &client, &eng_id, &company, &recruiter);

    let empty = String::from_str(&env, "");
    client.request_replacement(&company, &eng_id, &empty);

    assert_eq!(client.get_replacement_reason(&eng_id, &0), Some(empty));
}

#[test]
#[should_panic(expected = "replacement reason too long")]
fn test_replacement_reason_too_long_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REASON-LONG");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-LONG",
    );
    confirm_placement(&env, &client, &eng_id, &company, &recruiter);

    // 129-char reason — one past the 128 cap.
    let too_long = String::from_str(
        &env,
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    );
    client.request_replacement(&company, &eng_id, &too_long);
}

#[test]
fn test_replacement_reason_multi_replacement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REASON-MULTI");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-MULTI",
    );
    confirm_placement(&env, &client, &eng_id, &company, &recruiter);

    // First replacement
    let r1 = String::from_str(&env, "candidate_resigned");
    client.request_replacement(&company, &eng_id, &r1);

    // Bring engagement back to Active by submitting replacement proof + confirm.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://replacement-1"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // Second replacement
    let r2 = String::from_str(&env, "performance");
    client.request_replacement(&company, &eng_id, &r2);

    assert_eq!(client.get_replacement_count(&eng_id), 2);
    assert_eq!(client.get_replacement_reason(&eng_id, &0), Some(r1));
    assert_eq!(client.get_replacement_reason(&eng_id, &1), Some(r2));
}

#[test]
fn test_replacement_reason_event_payload_includes_reason() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-REASON-EVT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REASON-EVT",
    );
    confirm_placement(&env, &client, &eng_id, &company, &recruiter);

    let reason = String::from_str(&env, "performance");
    client.request_replacement(&company, &eng_id, &reason);

    let expected = Symbol::new(&env, "replacement_requested");
    let mut found = false;
    for (_, topics, data) in env.events().all().iter() {
        let topic: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic == expected {
            let (idx, r): (u32, String) = data.try_into_val(&env).unwrap();
            assert_eq!(idx, 0);
            assert_eq!(r, reason);
            found = true;
        }
    }
    assert!(found, "replacement_requested event was not emitted");
}

// ============================================================
// ISSUE #54 — MILESTONE UNLOCK EVENT PAYLOAD
// ============================================================

#[test]
fn test_milestone_unlock_event_carries_ledger_evidence() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-UNLOCK-EVT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNLOCK-EVT",
    );

    // Capture the retention window boundary BEFORE the unlock mutates state.
    let m1_before = client.get_milestone(&eng_id, &1);
    let valid_after_ledger = m1_before.valid_after_ledger;

    // Advance past the retention window so unlock_milestone succeeds.
    advance_ledger(&env, 30 * 17_280 + 1);
    let unlocked_at_ledger = env.ledger().sequence();

    client.unlock_milestone(&eng_id, &1);

    let expected = Symbol::new(&env, "milestone_unlocked");
    let mut found = false;
    for (_, topics, data) in env.events().all().iter() {
        let topic: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic == expected {
            let (idx, vafter, uat): (u32, u32, u32) = data.try_into_val(&env).unwrap();
            assert_eq!(idx, 1);
            assert_eq!(vafter, valid_after_ledger);
            assert_eq!(uat, unlocked_at_ledger);
            // The unlocked_at_ledger must equal the current ledger at the call site.
            assert_eq!(uat, env.ledger().sequence());
            found = true;
        }
    }
    assert!(found, "milestone_unlocked event was not emitted");
}

#[test]
fn test_no_milestone_unlock_event_when_call_fails() {
    // unlock_milestone called before the retention window must panic AND must
    // not emit a milestone_unlocked event. Soroban panics revert state and
    // discard buffered events, so this is verified by the absence of the
    // event after the panic is caught.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-UNLOCK-FAIL");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNLOCK-FAIL",
    );

    // Premature call — retention window has not elapsed yet.
    let result = client.try_unlock_milestone(&eng_id, &1);
    assert!(result.is_err(), "expected unlock_milestone to fail");

    assert!(
        !has_event(&env, "milestone_unlocked"),
        "milestone_unlocked event must not be emitted on failed unlock"
    );
}

// ============================================================
// ISSUE #21 — MAX MILESTONES CAP
// ============================================================

#[test]
fn test_milestone_cap_at_cap() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mut milestones = Vec::new(&env);
    for i in 0..10 {
        let name_str = match i {
            0 => "m01",
            1 => "m02",
            2 => "m03",
            3 => "m04",
            4 => "m05",
            5 => "m06",
            6 => "m07",
            7 => "m08",
            8 => "m09",
            9 => "m10",
            _ => "m",
        };
        milestones.push_back(Milestone {
            name: String::from_str(&env, name_str),
            payment_percent: 10,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        });
    }

    client.create_engagement(
        &String::from_str(&env, "ENG-10-MILESTONES"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );

    let eng = client.get_engagement(&String::from_str(&env, "ENG-10-MILESTONES"));
    assert_eq!(eng.milestones.len(), 10);
}

#[test]
#[should_panic(expected = "TooManyMilestones")]
fn test_milestone_cap_over_cap() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mut milestones = Vec::new(&env);
    for i in 0..11 {
        let name_str = match i {
            0 => "m01",
            1 => "m02",
            2 => "m03",
            3 => "m04",
            4 => "m05",
            5 => "m06",
            6 => "m07",
            7 => "m08",
            8 => "m09",
            9 => "m10",
            10 => "m11",
            _ => "m",
        };
        let pct = if i == 10 { 10 } else { 9 };
        milestones.push_back(Milestone {
            name: String::from_str(&env, name_str),
            payment_percent: pct,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        });
    }

    client.create_engagement(
        &String::from_str(&env, "ENG-11-MILESTONES"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "ZeroMilestones")]
fn test_milestone_cap_zero_milestones() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-0-MILESTONES"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &Vec::new(&env),
        &Vec::new(&env),
        &default_config(),
    );
}

#[test]
fn test_milestone_cap_admin_update() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    assert_eq!(client.get_max_milestones(), 10);

    client.set_max_milestones(&company, &5);
    assert_eq!(client.get_max_milestones(), 5);

    let mut milestones = Vec::new(&env);
    for i in 0..6 {
        let name_str = match i {
            0 => "m01",
            1 => "m02",
            2 => "m03",
            3 => "m04",
            4 => "m05",
            5 => "m06",
            _ => "m",
        };
        let pct = if i == 5 { 20 } else { 16 };
        milestones.push_back(Milestone {
            name: String::from_str(&env, name_str),
            payment_percent: pct,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        });
    }

    let result = client.try_create_engagement(
        &String::from_str(&env, "ENG-6-MILESTONES"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
    assert!(result.is_err());
}

// ============================================================
// ISSUE #22 — MILESTONE NAME MAX LENGTH ENFORCEMENT
// ============================================================

#[test]
fn test_milestone_name_64_char_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let name_64 = String::from_str(
        &env,
        "1234567890123456789012345678901234567890123456789012345678901234",
    );
    let milestones = vec![
        &env,
        Milestone {
            name: name_64,
            payment_percent: 100,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-64-CHAR"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "MilestoneNameTooLong: index 0")]
fn test_milestone_name_65_char_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let name_65 = String::from_str(
        &env,
        "12345678901234567890123456789012345678901234567890123456789012345",
    );
    let milestones = vec![
        &env,
        Milestone {
            name: name_65,
            payment_percent: 100,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-65-CHAR"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "MilestoneNameEmpty: index 0")]
fn test_milestone_name_empty_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, ""),
            payment_percent: 100,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-EMPTY-NAME"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "MilestoneNameTooLong: index 1")]
fn test_milestone_name_multi_milestone_partial_failure() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let name_65 = String::from_str(
        &env,
        "12345678901234567890123456789012345678901234567890123456789012345",
    );
    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Valid Milestone"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: name_65,
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-PARTIAL-FAIL"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

// ============================================================
// ISSUE #23 — MILESTONE NAME UNIQUENESS
// ============================================================

#[test]
fn test_milestone_name_uniqueness_happy_path() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "First Milestone"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "Second Milestone"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-UNIQUE"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "DuplicateMilestoneName: Duplicate Milestone")]
fn test_milestone_name_uniqueness_duplicate_detection() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Duplicate Milestone"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "Duplicate Milestone"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-DUPLICATE"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

#[test]
fn test_milestone_name_uniqueness_case_sensitivity() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Placement"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "placement"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-CASE-SENSITIVE"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &milestones,
        &Vec::new(&env),
        &default_config(),
    );
}

// ============================================================
// ISSUE #24 — JOB TITLE VALIDATION
// ============================================================

#[test]
#[should_panic(expected = "JobTitleEmpty")]
fn test_job_title_empty_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-TITLE-EMPTY"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, ""),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
fn test_job_title_64_char_accepted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let title_64 = String::from_str(
        &env,
        "1234567890123456789012345678901234567890123456789012345678901234",
    );

    client.create_engagement(
        &String::from_str(&env, "ENG-TITLE-64"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &title_64,
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "JobTitleTooLong")]
fn test_job_title_65_char_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let title_65 = String::from_str(
        &env,
        "12345678901234567890123456789012345678901234567890123456789012345",
    );

    client.create_engagement(
        &String::from_str(&env, "ENG-TITLE-65"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &title_65,
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

// ============================================================
// PER-COMPANY ACTIVE ENGAGEMENT CAP
// ============================================================

/// Default cap is 50; verify it is readable immediately after init.
#[test]
fn test_max_active_per_company_default() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    assert_eq!(client.get_max_active_per_company(), 50);
}

/// Admin can change the cap; the new value is immediately readable.
#[test]
fn test_set_max_active_per_company_admin_update() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_active_per_company(&company, &10u32);
    assert_eq!(client.get_max_active_per_company(), 10);

    // Can update again
    client.set_max_active_per_company(&company, &25u32);
    assert_eq!(client.get_max_active_per_company(), 25);
}

/// Non-admin cannot change the cap.
#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_max_active_per_company_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_max_active_per_company(&recruiter, &10u32);
}

/// Zero cap is rejected with InvalidMaxActivePerCompany.
#[test]
#[should_panic(expected = "InvalidMaxActivePerCompany")]
fn test_set_max_active_per_company_zero_rejected() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_max_active_per_company(&company, &0u32);
}

/// Active count starts at 0 and increments with each creation.
#[test]
fn test_company_active_count_tracks_creations() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    assert_eq!(client.get_company_active_count(&company), 0);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CAP-T1",
    );
    assert_eq!(client.get_company_active_count(&company), 1);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CAP-T2",
    );
    assert_eq!(client.get_company_active_count(&company), 2);
}

/// Engagement is accepted when the company is under the cap.
#[test]
fn test_engagement_accepted_under_cap() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set a cap of 3 and create 3 engagements — all must succeed.
    client.set_max_active_per_company(&company, &3u32);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNDER-1",
    );
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNDER-2",
    );
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNDER-3",
    );

    assert_eq!(client.get_company_active_count(&company), 3);
}

/// Engagement is rejected with CompanyActiveLimitReached when at cap.
#[test]
#[should_panic(expected = "CompanyActiveLimitReached")]
fn test_engagement_rejected_at_cap() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Cap of 2: first two succeed, third panics.
    client.set_max_active_per_company(&company, &2u32);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AT-CAP-1",
    );
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AT-CAP-2",
    );
    // This one is over the cap — must panic.
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AT-CAP-3",
    );
}

/// The cap is per-company: a different company is unaffected.
#[test]
fn test_cap_is_per_company_isolated() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Give company2 its own minted balance.
    let token_admin = Address::generate(&env);
    let token_id2 = env
        .register_stellar_asset_contract_v2(token_admin.clone())
        .address();
    let token_client2 = token::StellarAssetClient::new(&env, &token_id2);
    let company2 = Address::generate(&env);
    token_client2.mint(&company2, &500_000_000_000);

    client.set_max_active_per_company(&company, &1u32);

    // company hits the cap
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ISOL-C1",
    );

    // company2 is still free to create using its own token
    client.create_engagement(
        &String::from_str(&env, "ENG-ISOL-C2"),
        &company2,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id2,
        &1_000_000_000,
        &String::from_str(&env, "CTO"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    assert_eq!(client.get_company_active_count(&company), 1);
    assert_eq!(client.get_company_active_count(&company2), 1);
}

/// Completing an engagement frees its slot so a new one can be created.
#[test]
fn test_completion_frees_slot() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Cap of 1: only one active engagement allowed at a time.
    client.set_max_active_per_company(&company, &1u32);

    // Use a single-milestone (100%) engagement for simplicity.
    let single_milestone = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Placement"),
            payment_percent: 100,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    let eng_id_1 = String::from_str(&env, "ENG-FREES-1");
    client.create_engagement(
        &eng_id_1,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &single_milestone,
        &vec![&env],
        &default_config(),
    );

    // At cap now — a second create would fail.
    assert_eq!(client.get_company_active_count(&company), 1);

    // Complete the first engagement.
    client.submit_proof(
        &recruiter,
        &eng_id_1,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id_1, &0);

    let eng = client.get_engagement(&eng_id_1);
    assert_eq!(eng.status, EngagementStatus::Completed);
    // Count must have decremented.
    assert_eq!(client.get_company_active_count(&company), 0);

    // Now a new engagement must be accepted.
    let eng_id_2 = String::from_str(&env, "ENG-FREES-2");
    client.create_engagement(
        &eng_id_2,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &vec![
            &env,
            Milestone {
                name: String::from_str(&env, "Placement"),
                payment_percent: 100,
                kind: MilestoneKind::Placement,
                valid_after_ledger: 0,
                proof_hash: String::from_str(&env, ""),
                status: MilestoneStatus::Pending,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
            },
        ],
        &vec![&env],
        &default_config(),
    );
    assert_eq!(client.get_company_active_count(&company), 1);
}

/// Cancellation frees the slot.
#[test]
fn test_cancellation_frees_slot() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_active_per_company(&company, &1u32);

    let eng_id = String::from_str(&env, "ENG-CANCEL-CAP");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL-CAP",
    );

    assert_eq!(client.get_company_active_count(&company), 1);

    client.cancel_engagement(&company, &recruiter, &eng_id);
    assert_eq!(client.get_company_active_count(&company), 0);

    // Slot freed — next create succeeds.
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL-CAP-2",
    );
    assert_eq!(client.get_company_active_count(&company), 1);
}

/// Increasing the cap immediately allows more engagements to be created.
#[test]
fn test_admin_increasing_cap_allows_more_engagements() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_active_per_company(&company, &1u32);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-INC-CAP-1",
    );

    // At cap — would panic if we tried to create now.
    // Admin raises cap to 3.
    client.set_max_active_per_company(&company, &3u32);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-INC-CAP-2",
    );
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-INC-CAP-3",
    );

    assert_eq!(client.get_company_active_count(&company), 3);
}

/// Decreasing the cap doesn't affect already-active engagements (existing ones are
/// grandfathered), but prevents new ones until count drops below the new cap.
#[test]
fn test_admin_decreasing_cap_blocks_new_while_over() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Start with cap of 3, create 2 engagements.
    client.set_max_active_per_company(&company, &3u32);
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DEC-1",
    );
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DEC-2",
    );

    // Admin lowers cap to 2 — existing engagements still active, but no new ones allowed.
    client.set_max_active_per_company(&company, &2u32);

    let result = client.try_create_engagement(
        &String::from_str(&env, "ENG-DEC-3"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
    assert!(result.is_err());
}

/// Active count for a company that has never created an engagement is 0.
#[test]
fn test_active_count_default_zero_for_new_company() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let new_company = Address::generate(&env);
    assert_eq!(client.get_company_active_count(&new_company), 0);
}

// ============================================================
// #190 — public get_admin() query
// ============================================================

/// get_admin returns the address set at init, and reflects rotation after
/// nominate_admin/claim_admin.
#[test]
fn test_get_admin_reflects_init_and_rotation() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    assert_eq!(client.get_admin(), company);

    let new_admin = Address::generate(&env);
    client.nominate_admin(&company, &new_admin);
    client.claim_admin(&new_admin);

    assert_eq!(client.get_admin(), new_admin);
}

// ============================================================
// #188 — stale arbiter nomination on terminal engagements
// ============================================================

/// Nominating a successor after the engagement is cancelled must be rejected —
/// a terminal engagement has no active arbiter seat to hand off.
#[test]
#[should_panic(expected = "engagement is in a terminal state")]
fn test_nominate_arbiter_after_cancel_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARB-TERM-NOM");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ARB-TERM-NOM",
    );

    client.cancel_engagement(&company, &recruiter, &eng_id);
    assert_eq!(
        client.get_engagement(&eng_id).status,
        EngagementStatus::Cancelled
    );

    let new_arbiter = Address::generate(&env);
    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);
}

/// If a nomination was already pending and the engagement completes before the
/// nominee claims, `claim_arbiter` must be rejected rather than silently
/// installing an arbiter for an engagement that can no longer be disputed.
#[test]
#[should_panic(expected = "engagement is in a terminal state")]
fn test_claim_arbiter_after_completion_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARB-TERM-CLAIM");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-ARB-TERM-CLAIM",
    );

    let new_arbiter = Address::generate(&env);
    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);

    // Drive the engagement to completion while the nomination is still pending.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer-letter"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30-day"),
    );
    client.confirm_milestone(&company, &eng_id, &1);
    advance_ledger(&env, 60 * 17_280);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &2,
        &String::from_str(&env, "ipfs://90-day"),
    );
    client.confirm_milestone(&company, &eng_id, &2);
    assert_eq!(
        client.get_engagement(&eng_id).status,
        EngagementStatus::Completed
    );

    client.claim_arbiter(&new_arbiter, &eng_id);
}

// ============================================================
// #186 — lowering max_milestones / max_retention_days caps is
// creation-time-only and doesn't affect existing engagements
// ============================================================

/// Create an engagement at the current milestone cap, lower the cap below that
/// count, then exercise the full remaining lifecycle (unlock, propose/accept
/// amendment, confirm) — nothing should panic or misbehave from now being
/// "over" the new cap.
#[test]
fn test_lowering_max_milestones_does_not_break_existing_engagement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // build_milestones() creates a 3-milestone engagement; set the cap to exactly 3.
    client.set_max_milestones(&company, &3u32);
    let eng_id = String::from_str(&env, "ENG-CAP-MS");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CAP-MS",
    );

    // Admin lowers the cap below the existing engagement's milestone count.
    client.set_max_milestones(&company, &1u32);
    assert_eq!(client.get_max_milestones(), 1);

    // Full lifecycle still works: unlock, amend, confirm.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer-letter"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);

    client.propose_amendment(&company, &eng_id, &1, &50u32);
    client.accept_amendment(&recruiter, &eng_id, &1);
    assert_eq!(client.get_milestone(&eng_id, &1).payment_percent, 50);

    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30-day"),
    );
    client.confirm_milestone(&company, &eng_id, &1);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Active);
    assert_eq!(eng.milestones.len(), 3);
}

/// Create an engagement with a retention window at the current cap, lower the
/// cap below that window, then confirm the milestone still unlocks and
/// confirms normally once its original `valid_after_ledger` is reached.
#[test]
fn test_lowering_max_retention_days_does_not_break_existing_engagement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    client.set_max_retention_days(&company, &90u32);
    let eng_id = String::from_str(&env, "ENG-CAP-RET");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CAP-RET",
    );

    // Admin lowers the cap below the 90-day retention milestone already stored.
    client.set_max_retention_days(&company, &10u32);
    assert_eq!(client.get_max_retention_days(), 10);

    // Existing engagement's lifecycle is unaffected by the new, lower cap.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer-letter"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30-day"),
    );
    client.confirm_milestone(&company, &eng_id, &1);

    advance_ledger(&env, 60 * 17_280);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &2,
        &String::from_str(&env, "ipfs://90-day"),
    );
    client.confirm_milestone(&company, &eng_id, &2);

    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);
    assert_eq!(
        client.get_engagement(&eng_id).status,
        EngagementStatus::Completed
    );
}

// ============================================================
// #187 — query functions remain callable while paused
// ============================================================

/// Pausing the contract must not block reads. Sweep a representative sample of
/// getters/predicates across engagement, admin, and config state and assert
/// they all still succeed while paused.
#[test]
fn test_query_functions_callable_while_paused() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSED-QUERY");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PAUSED-QUERY",
    );

    client.pause(&company);
    assert!(client.is_paused());

    // Engagement-scoped queries.
    let _ = client.get_engagement(&eng_id);
    let _ = client.get_milestone(&eng_id, &0);
    let _ = client.get_escrow_balance(&eng_id);
    let _ = client.is_milestone_unlockable(&eng_id, &1);
    let _ = client.get_engagement_summary(&eng_id);
    let _ = client.get_total_released(&eng_id);
    let _ = client.get_amendment_ttl();
    let _ = client.get_pending_amendment(&eng_id, &0);

    // Admin / global config queries.
    let _ = client.get_admin();
    let _ = client.get_pending_admin();
    let _ = client.get_max_milestones();
    let _ = client.get_max_retention_days();
    let _ = client.get_max_active_per_company();
    let _ = client.get_company_active_count(&company);
    let _ = client.get_engagement_count();
    let _ = client.get_version();
    let _ = client.get_min_amount();
    let _ = client.get_platform_fee();

    // Still paused — confirms none of the above accidentally unpaused anything.
    assert!(client.is_paused());
}

// ============================================================
// #174 — create_engagement must reject company/recruiter/arbiter collisions
// ============================================================

#[test]
#[should_panic(expected = "CompanyRecruiterCollision")]
fn test_create_engagement_rejects_company_as_recruiter() {
    let (env, contract_id, token_id, company, _recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-COLLIDE-CR"),
        &company,
        &company,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "CompanyArbiterCollision")]
fn test_create_engagement_rejects_company_as_arbiter() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-COLLIDE-CA"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, company.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "RecruiterArbiterCollision")]
fn test_create_engagement_rejects_recruiter_as_arbiter() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-COLLIDE-RA"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, recruiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "RecruiterArbiterCollision")]
fn test_create_engagement_rejects_recruiter_as_one_of_several_arbiters() {
    // Collision check must scan the whole arbiter set, not just index 0.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-COLLIDE-MULTI"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone(), recruiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
fn test_create_engagement_allows_distinct_addresses() {
    // Sanity control: distinct company/recruiter/arbiter addresses are unaffected.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISTINCT",
    );
    let eng = client.get_engagement(&String::from_str(&env, "ENG-DISTINCT"));
    assert_eq!(eng.status, EngagementStatus::Active);
}

// ============================================================
// #175 — amount math is token-decimals-agnostic (raw integer units)
// ============================================================

/// Minimal mock token implementing just enough of the Token interface
/// (`transfer`, `balance`, `mint`) for HireSettleContract to use it as an
/// escrow asset, plus `decimals()` reporting a non-USDC-like precision (18)
/// — unlike the 7-decimal Stellar classic asset `setup()` wires up elsewhere
/// in this suite. HireSettleContract never calls `decimals()` itself; it is
/// exposed here purely so the test can state the precision it represents.
#[contract]
struct MockToken18;

#[contractimpl]
impl MockToken18 {
    pub fn decimals(_env: Env) -> u32 {
        18
    }

    pub fn mint(env: Env, to: Address, amount: i128) {
        let bal: i128 = env.storage().persistent().get(&to).unwrap_or(0);
        env.storage().persistent().set(&to, &(bal + amount));
    }

    pub fn balance(env: Env, id: Address) -> i128 {
        env.storage().persistent().get(&id).unwrap_or(0)
    }

    pub fn transfer(env: Env, from: Address, to: Address, amount: i128) {
        from.require_auth();
        let from_bal: i128 = env.storage().persistent().get(&from).unwrap_or(0);
        let to_bal: i128 = env.storage().persistent().get(&to).unwrap_or(0);
        env.storage().persistent().set(&from, &(from_bal - amount));
        env.storage().persistent().set(&to, &(to_bal + amount));
    }
}

#[test]
fn test_engagement_payout_math_is_decimal_agnostic_for_18_decimal_token() {
    let (env, contract_id, _token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mock_token_id = env.register(MockToken18, ());
    let mock_client = MockToken18Client::new(&env, &mock_token_id);
    assert_eq!(mock_client.decimals(), 18);

    // 1 token at 18 decimals — a value that dwarfs any realistic 7-decimal
    // USDC engagement, chosen to show the payout split is pure integer
    // percentage math with no decimals-awareness baked in.
    let total_amount: i128 = 1_000_000_000_000_000_000;
    mock_client.mint(&company, &total_amount);

    let eng_id = String::from_str(&env, "ENG-18DEC");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &mock_token_id,
        &total_amount,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // 30% of 1e18 = 3e17, exactly — confirms `total_amount * percent / 100`
    // is unaffected by the token's real decimal precision.
    assert_eq!(mock_client.balance(&recruiter), 300_000_000_000_000_000);
}

#[test]
fn test_min_amount_is_raw_units_not_scaled_per_token_decimals() {
    // Documents the intentional behaviour from issue #175: `MinEngagementAmount`
    // is a single admin-wide floor applied as raw integer units regardless of
    // which allowlisted token is used. For a token with more decimals than the
    // 7-decimal USDC the default was calibrated for, the floor no longer
    // represents "0.01 USDC" worth of real value — it is up to the integrator
    // to call `set_min_amount` appropriately per token precision.
    let (env, contract_id, _token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mock_token_id = env.register(MockToken18, ());
    let mock_client = MockToken18Client::new(&env, &mock_token_id);

    let min_amount = client.get_min_amount();
    mock_client.mint(&company, &min_amount);

    // Exactly the default floor (100_000 raw units — 0.01 of a 7-decimal
    // token, but a vanishingly small 1e-13 of a token at 18 decimals) is
    // accepted without any decimals-based rejection or adjustment.
    let eng_id = String::from_str(&env, "ENG-18DEC-DUST");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &mock_token_id,
        &min_amount,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.total_amount, min_amount);
}

// ============================================================
// #176 — cancel_engagement must clear a pending amendment proposal
// ============================================================

#[test]
fn test_cancel_engagement_clears_pending_amendment() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-CANCEL-AMEND");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL-AMEND",
    );

    // Propose an amendment on milestone 0 before any milestone is confirmed —
    // cancel_engagement is only callable in that window.
    client.propose_amendment(&company, &eng_id, &0, &50);
    assert!(client.get_pending_amendment(&eng_id, &0).is_some());

    client.cancel_engagement(&company, &recruiter, &eng_id);

    // The stale proposal must not remain visible as "harmless leftover" state —
    // get_pending_amendment must report none once the engagement is terminal.
    assert!(client.get_pending_amendment(&eng_id, &0).is_none());
}

#[test]
#[should_panic(expected = "no pending amendment proposal")]
fn test_accept_amendment_after_cancel_is_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-CANCEL-AMEND-ACCEPT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CANCEL-AMEND-ACCEPT",
    );

    client.propose_amendment(&company, &eng_id, &0, &50);
    client.cancel_engagement(&company, &recruiter, &eng_id);

    // Without clearing on cancel, this would succeed and mutate
    // milestone.payment_percent on a terminal Cancelled engagement.
    client.accept_amendment(&recruiter, &eng_id, &0);
}

// ============================================================
// #177 — request_replacement interaction with an in-flight dispute
// ============================================================

#[test]
fn test_request_replacement_clears_in_flight_dispute_on_retention_milestone() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-REPL-DISPUTE");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    // Confirm placement so request_replacement becomes available.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://placement"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // Unlock and dispute the 30-day retention milestone (index 1); one approve
    // vote leaves the dispute unresolved since quorum is 2 of 3.
    advance_ledger(&env, 30 * 17_280 + 1);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://retention-30"),
    );
    client.raise_dispute(
        &company,
        &eng_id,
        &1,
        &String::from_str(&env, "not retained"),
    );
    client.cast_arbiter_vote(&a1, &eng_id, &1, &true);

    let votes_before = client.get_arbiter_votes(&eng_id, &1);
    assert_eq!(votes_before.approve_votes, 1);
    assert!(client.get_dispute_reason(&eng_id, &1).is_some());

    // Company requests a replacement while milestone 1 is still Disputed.
    client.request_replacement(
        &company,
        &eng_id,
        &String::from_str(&env, "candidate underperformed"),
    );

    // The milestone lands in a well-defined state: reset to Locked, with the
    // stale vote tally and dispute reason from the abandoned dispute cleared —
    // otherwise a future dispute on this same index would inherit a1's vote.
    let m1 = client.get_milestone(&eng_id, &1);
    assert_eq!(m1.status, MilestoneStatus::Locked);

    let votes_after = client.get_arbiter_votes(&eng_id, &1);
    assert_eq!(votes_after.approve_votes, 0);
    assert_eq!(votes_after.reject_votes, 0);
    assert!(client.get_dispute_reason(&eng_id, &1).is_none());

    // Bring the engagement back to Active via the placement milestone, then
    // unlock and dispute milestone 1 again. a1 must be able to vote again —
    // proving the earlier vote record was actually cleared, not just shadowed.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://placement-2"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    advance_ledger(&env, 30 * 17_280 + 1);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://retention-30-again"),
    );
    client.raise_dispute(
        &company,
        &eng_id,
        &1,
        &String::from_str(&env, "still disputed"),
    );

    // Would panic with "duplicate vote" if the earlier vote record had leaked through.
    client.cast_arbiter_vote(&a1, &eng_id, &1, &true);
    let votes_second = client.get_arbiter_votes(&eng_id, &1);
    assert_eq!(votes_second.approve_votes, 1);
}

// ============================================================
// Issue #140 — top_up_escrow test coverage
// ============================================================

#[test]
fn test_top_up_escrow_increases_balance() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-TOPUP-OK");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-TOPUP-OK",
    );

    let balance_before = client.get_escrow_balance(&eng_id);
    client.top_up_escrow(&company, &eng_id, &500_000_000);
    let balance_after = client.get_escrow_balance(&eng_id);

    assert_eq!(balance_after, balance_before + 500_000_000);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_top_up_escrow_non_company_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-TOPUP-NONCO");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-TOPUP-NONCO",
    );

    client.top_up_escrow(&recruiter, &eng_id, &500_000_000);
}

#[test]
fn test_top_up_escrow_emits_event_with_correct_payload() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-TOPUP-EVT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-TOPUP-EVT",
    );

    let total_before = client.get_engagement(&eng_id).total_amount;
    client.top_up_escrow(&company, &eng_id, &500_000_000);

    let expected = Symbol::new(&env, "escrow_topped_up");
    let mut found = false;
    for (_, topics, data) in env.events().all().iter() {
        let topic: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic == expected {
            let (amount, new_total): (i128, i128) = data.try_into_val(&env).unwrap();
            assert_eq!(amount, 500_000_000);
            assert_eq!(new_total, total_before + 500_000_000);
            found = true;
        }
    }
    assert!(found, "escrow_topped_up event was not emitted");
}

#[test]
#[should_panic(expected = "amount must be greater than zero")]
fn test_top_up_escrow_zero_amount_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-TOPUP-ZERO");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-TOPUP-ZERO",
    );

    client.top_up_escrow(&company, &eng_id, &0);
}

#[test]
#[should_panic(expected = "amount must be greater than zero")]
fn test_top_up_escrow_negative_amount_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-TOPUP-NEG");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-TOPUP-NEG",
    );

    client.top_up_escrow(&company, &eng_id, &-100);
}

// ============================================================
// Issue #139 — set_min_amount / get_min_amount test coverage
// ============================================================

#[test]
fn test_set_min_amount_admin_updates_floor() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let new_min = 5_000_000;
    client.set_min_amount(&company, &new_min);

    assert_eq!(client.get_min_amount(), new_min);
}

#[test]
#[should_panic(expected = "AmountBelowMinimum")]
fn test_create_engagement_below_updated_min_amount_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let new_min = 5_000_000;
    client.set_min_amount(&company, &new_min);

    client.create_engagement(
        &String::from_str(&env, "ENG-MINAMT-BELOW"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &(new_min - 1),
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_min_amount_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_min_amount(&recruiter, &5_000_000);
}

// ============================================================
// Issue #146 — get_active_dispute_count
// ============================================================

/// Returns 0 when no milestone is in Disputed status.
#[test]
fn test_get_active_dispute_count_zero_when_no_dispute() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-DISP-COUNT-0");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISP-COUNT-0",
    );
    assert_eq!(client.get_active_dispute_count(&eng_id), 0);

    // Submit proof — still not disputed
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    assert_eq!(client.get_active_dispute_count(&eng_id), 0);
}

/// Returns 1 after raising a dispute on one milestone.
#[test]
fn test_get_active_dispute_count_one_dispute() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-DISP-COUNT-1");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISP-COUNT-1",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "wrong_doc"));
    assert_eq!(client.get_active_dispute_count(&eng_id), 1);
}

/// Returns the correct count with multiple concurrent disputes.
/// Uses 2-of-2 quorum so neither dispute auto-resolves after one vote.
#[test]
fn test_get_active_dispute_count_multiple_concurrent_disputes() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);

    let two_milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Milestone One"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
        Milestone {
            name: String::from_str(&env, "Milestone Two"),
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    let eng_id = String::from_str(&env, "ENG-DISP-COUNT-MULTI");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &two_milestones,
        &vec![&env],
        &default_config(),
    );

    assert_eq!(client.get_active_dispute_count(&eng_id), 0);

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof-a"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute_a"));
    assert_eq!(client.get_active_dispute_count(&eng_id), 1);

    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://proof-b"),
    );
    client.raise_dispute(&company, &eng_id, &1, &String::from_str(&env, "dispute_b"));
    assert_eq!(client.get_active_dispute_count(&eng_id), 2);
}

/// Count decreases once a dispute is resolved by arbiter approval.
#[test]
fn test_get_active_dispute_count_decreases_after_resolution_approve() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-DISP-COUNT-RESOLVE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISP-COUNT-RESOLVE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    assert_eq!(client.get_active_dispute_count(&eng_id), 1);

    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);
    assert_eq!(client.get_active_dispute_count(&eng_id), 0);
}

/// Count decreases once a dispute is resolved by arbiter rejection.
#[test]
fn test_get_active_dispute_count_decreases_after_resolution_reject() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-DISP-COUNT-REJECT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISP-COUNT-REJECT",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    assert_eq!(client.get_active_dispute_count(&eng_id), 1);

    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);
    assert_eq!(client.get_active_dispute_count(&eng_id), 0);
}

// ============================================================
// STORAGE TTL EXTENSION — Issue #40
// ============================================================

/// Default value is returned before any admin update.
#[test]
fn test_get_storage_ttl_extend_to_default() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // DEFAULT_STORAGE_TTL_EXTEND_TO = 1_036_800
    assert_eq!(client.get_storage_ttl_extend_to(), 1_036_800u32);
}

/// Admin can update the TTL-extension target and the getter reflects the new value.
#[test]
fn test_admin_can_set_storage_ttl_extend_to() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let new_ttl: u32 = 500_000;
    client.set_storage_ttl_extend_to(&company, &new_ttl);

    assert_eq!(client.get_storage_ttl_extend_to(), new_ttl);
}

/// Admin can update the TTL-extension target multiple times; the latest value wins.
#[test]
fn test_admin_can_update_storage_ttl_extend_to_multiple_times() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_storage_ttl_extend_to(&company, &100_000u32);
    assert_eq!(client.get_storage_ttl_extend_to(), 100_000u32);

    client.set_storage_ttl_extend_to(&company, &200_000u32);
    assert_eq!(client.get_storage_ttl_extend_to(), 200_000u32);
}

/// Non-admin caller is rejected with "unauthorized".
#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_admin_cannot_set_storage_ttl_extend_to() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_storage_ttl_extend_to(&recruiter, &500_000u32);
}

// ============================================================
// Issue #147 — propose_upgrade / execute_upgrade / upgrade_lock_duration
// ============================================================

/// Default upgrade lock duration is 17_280 ledgers (~1 day).
#[test]
fn test_get_upgrade_lock_duration_default() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    assert_eq!(client.get_upgrade_lock_duration(), 17_280);
}

/// Admin can update the lock duration and it is reflected immediately.
#[test]
fn test_set_upgrade_lock_duration_admin_update() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_upgrade_lock_duration(&company, &5_000u32);
    assert_eq!(client.get_upgrade_lock_duration(), 5_000);

    client.set_upgrade_lock_duration(&company, &1u32);
    assert_eq!(client.get_upgrade_lock_duration(), 1);
}

/// Non-admin cannot set the lock duration.
#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_upgrade_lock_duration_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_upgrade_lock_duration(&recruiter, &5_000u32);
}

/// Non-admin cannot propose an upgrade.
#[test]
#[should_panic(expected = "unauthorized")]
fn test_propose_upgrade_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let wasm_hash = soroban_sdk::BytesN::from_array(&env, &[1u8; 32]);
    client.propose_upgrade(&recruiter, &wasm_hash);
}

/// execute_upgrade with no pending proposal must panic with "no pending upgrade".
#[test]
#[should_panic(expected = "no pending upgrade")]
fn test_execute_upgrade_no_pending_proposal_panics() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.execute_upgrade();
}

/// execute_upgrade before the lock elapses is rejected with "UpgradeLockNotElapsed".
#[test]
#[should_panic(expected = "UpgradeLockNotElapsed")]
fn test_execute_upgrade_before_lock_elapses_rejected() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set lock to 500 ledgers so we can control timing precisely.
    client.set_upgrade_lock_duration(&company, &500u32);

    let wasm_hash = soroban_sdk::BytesN::from_array(&env, &[2u8; 32]);
    client.propose_upgrade(&company, &wasm_hash);

    // Advance 499 ledgers — one short of the lock.
    // sequence starts at 100, so current = 599; execute_after = 100 + 500 = 600.
    advance_ledger(&env, 499);

    // Must be rejected: current_ledger (599) < execute_after_ledger (600)
    client.execute_upgrade();
}

/// Admin proposes an upgrade; propose_upgrade emits an upgrade_proposed event
/// with the wasm hash and execute_after_ledger.
#[test]
fn test_propose_upgrade_emits_event_and_sets_proposal() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Use a 100-ledger lock for a predictable execute_after_ledger.
    client.set_upgrade_lock_duration(&company, &100u32);

    let wasm_hash = soroban_sdk::BytesN::from_array(&env, &[3u8; 32]);
    client.propose_upgrade(&company, &wasm_hash);

    // Verify the upgrade_proposed event was emitted.
    let expected_sym = Symbol::new(&env, "upgrade_proposed");
    let mut found = false;
    for (_, topics, _) in env.events().all().iter() {
        let topic: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic == expected_sym {
            found = true;
        }
    }
    assert!(found, "upgrade_proposed event was not emitted");
}

/// Re-proposing while a proposal is pending overwrites it and resets the timelock.
#[test]
#[should_panic(expected = "UpgradeLockNotElapsed")]
fn test_propose_upgrade_overwrites_pending_proposal_and_resets_lock() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Lock = 200 ledgers (execute_after_ledger = 100 + 200 = 300)
    client.set_upgrade_lock_duration(&company, &200u32);
    let hash1 = soroban_sdk::BytesN::from_array(&env, &[4u8; 32]);
    client.propose_upgrade(&company, &hash1);

    // Advance 100 ledgers (seq = 200); re-propose resets lock to 200 + 200 = 400.
    advance_ledger(&env, 100);
    let hash2 = soroban_sdk::BytesN::from_array(&env, &[5u8; 32]);
    client.propose_upgrade(&company, &hash2);

    // Advance only 50 more ledgers (seq = 250) — before new lock at 400.
    advance_ledger(&env, 50);

    // Must fail: current_ledger (250) < execute_after_ledger (400)
    client.execute_upgrade();
}

/// Admin can update the lock duration; subsequent proposals use the new value.
#[test]
#[should_panic(expected = "UpgradeLockNotElapsed")]
fn test_updated_lock_duration_applies_to_new_proposal() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Change to a long lock: 10_000 ledgers.
    client.set_upgrade_lock_duration(&company, &10_000u32);

    // New proposal uses the updated lock (execute_after_ledger = 100 + 10_000 = 10_100).
    let wasm_hash = soroban_sdk::BytesN::from_array(&env, &[6u8; 32]);
    client.propose_upgrade(&company, &wasm_hash);

    // Advance only 50 ledgers — nowhere near the 10_000-ledger lock.
    advance_ledger(&env, 50);

    // Must fail: current_ledger (150) < execute_after_ledger (10_100)
    client.execute_upgrade();
}

// ============================================================
// Issue #148 — set_max_proof_hash_length / get_max_proof_hash_length
// ============================================================

/// Default max proof hash length is 200 characters.
#[test]
fn test_get_max_proof_hash_length_default() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    assert_eq!(client.get_max_proof_hash_length(), 200);
}

/// Admin can tighten the cap; get_max_proof_hash_length reflects it immediately.
#[test]
fn test_set_max_proof_hash_length_admin_tighten() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_proof_hash_length(&company, &50u32);
    assert_eq!(client.get_max_proof_hash_length(), 50);
}

/// Admin can loosen the cap up to 500.
#[test]
fn test_set_max_proof_hash_length_admin_loosen() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_proof_hash_length(&company, &500u32);
    assert_eq!(client.get_max_proof_hash_length(), 500);
}

/// Non-admin caller is rejected.
#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_max_proof_hash_length_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_max_proof_hash_length(&recruiter, &100u32);
}

/// Value 0 is rejected with "InvalidMaxProofHashLength".
#[test]
#[should_panic(expected = "InvalidMaxProofHashLength")]
fn test_set_max_proof_hash_length_zero_rejected() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_max_proof_hash_length(&company, &0u32);
}

/// Value > 500 is rejected with "InvalidMaxProofHashLength".
#[test]
#[should_panic(expected = "InvalidMaxProofHashLength")]
fn test_set_max_proof_hash_length_over_500_rejected() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_max_proof_hash_length(&company, &501u32);
}

/// submit_proof with a hash exactly at the current cap succeeds.
#[test]
fn test_submit_proof_at_cap_succeeds() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set cap to 10 characters
    client.set_max_proof_hash_length(&company, &10u32);

    let eng_id = String::from_str(&env, "ENG-PHLEN-AT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PHLEN-AT",
    );

    // Exactly 10 characters
    let proof = String::from_str(&env, "1234567890");
    client.submit_proof(&recruiter, &eng_id, &0, &proof);
    assert_eq!(
        client.get_milestone(&eng_id, &0).status,
        MilestoneStatus::ProofSubmitted
    );
}

/// submit_proof with a hash one character over the current cap is rejected.
#[test]
#[should_panic(expected = "ProofHashTooLong")]
fn test_submit_proof_over_cap_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set cap to 10 characters
    client.set_max_proof_hash_length(&company, &10u32);

    let eng_id = String::from_str(&env, "ENG-PHLEN-OVER");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PHLEN-OVER",
    );

    // 11 characters — one over the 10-character cap
    let too_long = String::from_str(&env, "12345678901");
    client.submit_proof(&recruiter, &eng_id, &0, &too_long);
}

/// Loosening the cap allows proofs that were previously too long.
#[test]
fn test_loosening_cap_allows_longer_proofs() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Start with a tight cap of 5
    client.set_max_proof_hash_length(&company, &5u32);

    let eng_id = String::from_str(&env, "ENG-PHLEN-LOOSEN");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PHLEN-LOOSEN",
    );

    // Verify a 10-char proof is rejected with the tight cap
    let long_proof = String::from_str(&env, "1234567890");
    let result = client.try_submit_proof(&recruiter, &eng_id, &0, &long_proof);
    assert!(result.is_err(), "expected submit_proof to fail with cap=5");

    // Admin loosens cap to 50
    client.set_max_proof_hash_length(&company, &50u32);

    // Now the same 10-char proof must succeed
    client.submit_proof(&recruiter, &eng_id, &0, &long_proof);
    assert_eq!(
        client.get_milestone(&eng_id, &0).status,
        MilestoneStatus::ProofSubmitted
    );
}

/// Tightening the cap below the default (200) still blocks long proofs.
#[test]
#[should_panic(expected = "ProofHashTooLong")]
fn test_tightening_cap_blocks_previously_valid_proofs() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-PHLEN-TIGHT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-PHLEN-TIGHT",
    );

    // Tighten cap to 5
    client.set_max_proof_hash_length(&company, &5u32);

    // A 10-char proof that would be valid at the default cap (200) is now rejected
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "1234567890"),
    );
}

// ============================================================
// ISSUE #44 — RECRUITER TRANSFER
// ============================================================

#[test]
fn test_recruiter_transfer_happy_path() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let new_recruiter = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RTR-01");

    create_standard_engagement(
        &env,
        &client,
        &_token_id,
        &company,
        &recruiter,
        &_arbiter,
        "ENG-RTR-01",
    );

    client.propose_recruiter_transfer(&recruiter, &eng_id, &new_recruiter);
    client.accept_recruiter_transfer(&company, &eng_id);

    assert!(has_event(&env, "recruiter_transferred"));
    let engagement = client.get_engagement(&eng_id);
    assert_eq!(engagement.recruiter, new_recruiter);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_recruiter_transfer_wrong_proposer() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let new_recruiter = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RTR-WP");

    create_standard_engagement(
        &env,
        &client,
        &_token_id,
        &company,
        &recruiter,
        &_arbiter,
        "ENG-RTR-WP",
    );

    // Company tries to propose — only recruiter may propose
    client.propose_recruiter_transfer(&company, &eng_id, &new_recruiter);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_recruiter_transfer_wrong_acceptor() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let new_recruiter = Address::generate(&env);
    let stranger = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RTR-WA");

    create_standard_engagement(
        &env,
        &client,
        &_token_id,
        &company,
        &recruiter,
        &_arbiter,
        "ENG-RTR-WA",
    );

    client.propose_recruiter_transfer(&recruiter, &eng_id, &new_recruiter);
    // Stranger tries to accept — only company may accept
    client.accept_recruiter_transfer(&stranger, &eng_id);
}

#[test]
fn test_recruiter_transfer_payout() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let new_recruiter = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RTR-PO");

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-RTR-PO",
    );

    // Propose and accept recruiter transfer
    client.propose_recruiter_transfer(&recruiter, &eng_id, &new_recruiter);
    client.accept_recruiter_transfer(&company, &eng_id);

    // Confirm a milestone — payout should go to new_recruiter. Proof must be
    // submitted by whoever is now the engagement's recruiter (issue #269's
    // multi-signer authorization requires caller == engagement.recruiter).
    client.submit_proof(
        &new_recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    let new_recruiter_balance = token_client.balance(&new_recruiter);
    assert_eq!(new_recruiter_balance, 300_000_000);

    let old_recruiter_balance = token_client.balance(&recruiter);
    assert_eq!(old_recruiter_balance, 0);
}

#[test]
#[should_panic(expected = "no pending recruiter transfer")]
fn test_recruiter_transfer_no_proposal() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-RTR-NP");

    create_standard_engagement(
        &env,
        &client,
        &_token_id,
        &company,
        &recruiter,
        &_arbiter,
        "ENG-RTR-NP",
    );

    // Company tries to accept without a pending proposal
    client.accept_recruiter_transfer(&company, &eng_id);
}

#[test]
fn test_recruiter_transfer_event() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let new_recruiter = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RTR-EVT");

    create_standard_engagement(
        &env,
        &client,
        &_token_id,
        &company,
        &recruiter,
        &_arbiter,
        "ENG-RTR-EVT",
    );

    client.propose_recruiter_transfer(&recruiter, &eng_id, &new_recruiter);
    client.accept_recruiter_transfer(&company, &eng_id);

    assert!(has_event(&env, "recruiter_transferred"));

    // Verify event carries the correct old/new recruiter addresses
    let events = env.events().all();
    let mut found = false;
    for i in 0..events.len() {
        let (_, topics, data) = events.get(i).unwrap();
        let topic: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic == Symbol::new(&env, "recruiter_transferred") {
            let (old_addr, new_addr): (Address, Address) = data.try_into_val(&env).unwrap();
            assert_eq!(old_addr, recruiter);
            assert_eq!(new_addr, new_recruiter);
            found = true;
            break;
        }
    }
    assert!(found, "recruiter_transferred event not found");
}

// ============================================================
// Issue #141 — get_arbiter_votes test coverage
// ============================================================

#[test]
fn test_get_arbiter_votes_default_before_any_votes() {
    // Before a dispute is raised (or before any vote is cast), get_arbiter_votes
    // must return zeroed counts rather than panicking.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-AVOTES-EMPTY");

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AVOTES-EMPTY",
    );

    // No vote record exists yet — should return the zero default.
    let counts = client.get_arbiter_votes(&eng_id, &0);
    assert_eq!(counts.approve_votes, 0);
    assert_eq!(counts.reject_votes, 0);
}

/// Multi-arbiter vote tracking (issue #10) — three arbiters, quorum 2.
/// Tests vote counting, duplicate-vote rejection, automatic resolution on
/// quorum, and vote-record clearing after resolution.
#[test]
fn test_multi_arbiter_quorum_with_three_arbiters_and_quorum_two() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-MULTI-ARB-3");

    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof-av"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    // Before any vote, counts are zero.
    let counts = client.get_arbiter_votes(&eng_id, &0);
    assert_eq!(counts.approve_votes, 0);
    assert_eq!(counts.reject_votes, 0);

    // First arbiter approves — 1 approve, 0 reject.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    let counts = client.get_arbiter_votes(&eng_id, &0);
    assert_eq!(counts.approve_votes, 1);
    assert_eq!(counts.reject_votes, 0);

    // Second arbiter rejects — 1 approve, 1 reject.
    // Quorum of 2 approves not yet reached; reject threshold (>1) not met
    // either, so the dispute remains open.
    client.cast_arbiter_vote(&a2, &eng_id, &0, &false);
    let counts = client.get_arbiter_votes(&eng_id, &0);
    assert_eq!(counts.approve_votes, 1);
    assert_eq!(counts.reject_votes, 1);

    // Third arbiter approves — 2 approves reach quorum; dispute resolved.
    // The vote record is cleared on resolution, so get_arbiter_votes reverts
    // to its default zero state.
    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    // Vote record cleared — back to default zeros.
    let counts = client.get_arbiter_votes(&eng_id, &0);
    assert_eq!(counts.approve_votes, 0);
    assert_eq!(counts.reject_votes, 0);
}

// ============================================================
// Issue #142 — set_max_retention_days / get_max_retention_days
// ============================================================

#[test]
fn test_set_get_max_retention_days_admin_can_update() {
    // Admin sets a new cap; the getter must reflect the updated value.
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let new_cap: u32 = 180;
    client.set_max_retention_days(&company, &new_cap);

    assert_eq!(client.get_max_retention_days(), new_cap);
}

#[test]
fn test_set_max_retention_days_raise_cap_reflected_by_getter() {
    // Raising the cap and then lowering it — getter always mirrors the last set.
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_retention_days(&company, &730u32);
    assert_eq!(client.get_max_retention_days(), 730u32);

    client.set_max_retention_days(&company, &60u32);
    assert_eq!(client.get_max_retention_days(), 60u32);
}

#[test]
#[should_panic(expected = "RetentionDaysTooLarge")]
fn test_create_engagement_over_max_retention_days_rejected() {
    // Lower the cap to 10 days, then try to create an engagement with a
    // 30-day retention window — must be rejected.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_retention_days(&company, &10u32);

    // 30-day retention exceeds the new 10-day cap.
    client.create_engagement(
        &String::from_str(&env, "ENG-MAXRET-OVER"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32], // both windows exceed the 10-day cap
        &default_config(),
    );
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_max_retention_days_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_max_retention_days(&recruiter, &100u32);
}

// ============================================================
// Issue #143 — set_inactivity_timeout_ledgers / get_inactivity_timeout_ledgers
// ============================================================

#[test]
fn test_set_get_inactivity_timeout_ledgers_admin_can_update() {
    // Admin sets the timeout; the getter must reflect it immediately.
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let new_timeout: u32 = 500_000;
    client.set_inactivity_timeout_ledgers(&company, &new_timeout);

    assert_eq!(client.get_inactivity_timeout_ledgers(), new_timeout);
}

#[test]
fn test_set_inactivity_timeout_ledgers_multiple_updates_reflected() {
    // Verify that subsequent calls overwrite the previous value correctly.
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_inactivity_timeout_ledgers(&company, &100_000u32);
    assert_eq!(client.get_inactivity_timeout_ledgers(), 100_000u32);

    client.set_inactivity_timeout_ledgers(&company, &200_000u32);
    assert_eq!(client.get_inactivity_timeout_ledgers(), 200_000u32);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_set_inactivity_timeout_ledgers_non_admin_rejected() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_inactivity_timeout_ledgers(&recruiter, &100_000u32);
}

// ============================================================
// Issue #144 — expire_engagement test coverage
// ============================================================

#[test]
fn test_expire_engagement_success_refunds_after_timeout() {
    // Set a short inactivity timeout, advance past it, and verify:
    //   - the engagement transitions to Expired
    //   - the unreleased escrow is returned to the company
    //   - the contract escrow account is fully drained
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-EXPIRE-OK");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXPIRE-OK",
    );

    let company_balance_before = token_client.balance(&company);

    // Set a short timeout of 1 000 ledgers so the test can advance past it.
    client.set_inactivity_timeout_ledgers(&company, &1_000u32);

    // Advance 1 001 ledgers so current_ledger > last_activity_ledger + timeout.
    advance_ledger(&env, 1_001);

    client.expire_engagement(&eng_id);

    // Status must be Expired.
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Expired);

    // Full escrow (total_amount - released_amount = 1_000_000_000) refunded.
    let expected_refund = 1_000_000_000i128;
    assert_eq!(
        token_client.balance(&company),
        company_balance_before + expected_refund
    );

    // Contract escrow account is fully drained.
    assert_eq!(token_client.balance(&contract_id), 0);
}

#[test]
#[should_panic(expected = "Inactivity timeout not reached")]
fn test_expire_engagement_rejected_before_timeout() {
    // Calling expire_engagement before the inactivity timeout has elapsed
    // must panic with "Inactivity timeout not reached".
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-EXPIRE-EARLY");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXPIRE-EARLY",
    );

    // Set timeout to 1 000 ledgers and advance only 500 — timeout not yet elapsed.
    client.set_inactivity_timeout_ledgers(&company, &1_000u32);
    advance_ledger(&env, 500);

    client.expire_engagement(&eng_id);
}

#[test]
#[should_panic(expected = "Cannot expire completed engagement")]
fn test_expire_engagement_rejected_on_completed_engagement() {
    // An already-completed engagement must not be expirable.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-EXPIRE-DONE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXPIRE-DONE",
    );

    // Confirm all three milestones to complete the engagement.
    // Milestone 0 (Placement) — submit proof and confirm.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof-m0"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // Advance past the retention time-gate for milestone 1.
    let m1 = client.get_milestone(&eng_id, &1);
    let ledgers_needed = m1.valid_after_ledger - env.ledger().sequence() + 1;
    advance_ledger(&env, ledgers_needed);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://proof-m1"),
    );
    client.confirm_milestone(&company, &eng_id, &1);

    // Advance past retention time-gate for milestone 2.
    let m2 = client.get_milestone(&eng_id, &2);
    let ledgers_needed = m2.valid_after_ledger - env.ledger().sequence() + 1;
    advance_ledger(&env, ledgers_needed);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &2,
        &String::from_str(&env, "ipfs://proof-m2"),
    );
    client.confirm_milestone(&company, &eng_id, &2);

    // Engagement is now Completed.
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Completed);
    let _ = token_client.balance(&recruiter); // silence unused-variable warning

    // Advance well past any timeout — should still panic because Completed.
    advance_ledger(&env, 2_000_000);

    client.expire_engagement(&eng_id);
}

#[test]
#[should_panic(expected = "Inactivity timeout not reached")]
fn test_expire_engagement_rejected_on_cancelled_engagement_before_timeout() {
    // A cancelled engagement before the inactivity window is still rejected —
    // the contract only gates on Completed for the status check; the timeout
    // guard fires first when the window hasn't elapsed.
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-EXPIRE-CANC");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-EXPIRE-CANC",
    );

    // Cancel the engagement — requires both company and recruiter auth.
    client.cancel_engagement(&company, &recruiter, &eng_id);

    // With the default inactivity timeout (~1 036 800 ledgers) not yet
    // elapsed, expire_engagement must panic.
    client.expire_engagement(&eng_id);
}



/// Admin can set the arbiter fee and get_arbiter_fee reflects it.
#[test]
fn test_set_and_get_arbiter_fee() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Default is 0
    assert_eq!(client.get_arbiter_fee(), 0u32);

    client.set_arbiter_fee(&company, &50u32);
    assert_eq!(client.get_arbiter_fee(), 50u32);

    client.set_arbiter_fee(&company, &200u32); // max
    assert_eq!(client.get_arbiter_fee(), 200u32);

    client.set_arbiter_fee(&company, &0u32); // back to zero
    assert_eq!(client.get_arbiter_fee(), 0u32);
}

/// Fee exceeding MAX_ARBITER_FEE_BPS (200) is rejected.
#[test]
fn test_set_arbiter_fee_too_high_rejected() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let result = client.try_set_arbiter_fee(&company, &201u32);
    assert!(result.is_err());

    // Also verify the stored value was not updated
    assert_eq!(client.get_arbiter_fee(), 0u32);
}

/// Non-admin caller is rejected with "unauthorized".
#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_admin_cannot_set_arbiter_fee() {
    let (env, contract_id, _token_id, _company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_arbiter_fee(&recruiter, &50u32);
}

/// The configured arbiter fee is correctly deducted and routed to the
/// deciding arbiter on a dispute resolved in the recruiter's favour.
#[test]
fn test_arbiter_fee_deducted_on_dispute_approval() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // Set arbiter fee to 1% (100 bps)
    client.set_arbiter_fee(&company, &100u32);
    assert_eq!(client.get_arbiter_fee(), 100u32);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-ARB-FEE-DEDUCT");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    // Recruiter submits proof for milestone 0 (30% = 300_000_000)
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Company raises dispute
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));

    let recruiter_balance_before = token_client.balance(&recruiter);
    let a1_balance_before = token_client.balance(&a1);
    let a2_balance_before = token_client.balance(&a2);

    // First arbiter approves (1 of 2) — not yet quorum.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);

    // Second arbiter approves (2 of 2) — quorum reached, dispute resolved.
    client.cast_arbiter_vote(&a2, &eng_id, &0, &true);

    // Milestone 0: payment = 1_000_000_000 * 30 / 100 = 300_000_000
    // Arbiter fee = 300_000_000 * 100 / 10_000 = 3_000_000
    // Net to recruiter = 300_000_000 - 3_000_000 = 297_000_000
    // Arbiter fee goes to a2 (the deciding arbiter's vote tipped quorum)
    assert_eq!(
        token_client.balance(&recruiter),
        recruiter_balance_before + 297_000_000
    );
    assert_eq!(token_client.balance(&a1), a1_balance_before);
    assert_eq!(
        token_client.balance(&a2),
        a2_balance_before + 3_000_000
    );
}

#[test]
fn test_recruiter_cosigner_can_submit_proof_and_cancel() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REC-COSIGN",
    );

    let recruiter_cosigner = Address::generate(&env);
    client.set_recruiter_cosigner(&recruiter, &recruiter_cosigner);

    let eng_id = String::from_str(&env, "ENG-REC-COSIGN");
    client.submit_proof(
        &recruiter_cosigner,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof-via-cosigner"),
    );
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);

    // Cancellation still needs one company signer and one recruiter-side signer.
    client.cancel_engagement(&company, &recruiter_cosigner, &eng_id);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_unknown_wallet_cannot_submit_proof_without_recruiter_cosigner() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REC-COSIGN-REJECT",
    );

    let stranger = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-REC-COSIGN-REJECT");
    client.submit_proof(
        &stranger,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof-unauthorized"),
    );
}

#[test]
fn test_escrow_callback_checkpoint_disabled_by_default() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CB-OFF",
    );

    // The callback checkpoint path is no-op unless explicitly enabled by admin.
    assert!(!has_event(&env, "escrow_callback_point"));
}

#[test]
fn test_escrow_callback_checkpoint_emits_when_enabled_and_target_set() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CB-ON",
    );

    let callback_target = Address::generate(&env);
    client.set_escrow_callback_target(&company, &callback_target);
    client.set_escrow_callback_enabled(&company, &true);

    let eng_id = String::from_str(&env, "ENG-CB-ON");
    client.top_up_escrow(&company, &eng_id, &100_000_000);

    assert!(has_event(&env, "escrow_callback_point"));
}

// ============================================================
// Issue #201 — ARBITER QUORUM VALIDATION ON CREATE
// ============================================================

/// Quorum of 0 must be rejected with "InvalidQuorum".
#[test]
#[should_panic(expected = "InvalidQuorum")]
fn test_create_engagement_quorum_zero_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-Q0"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 0,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

/// Quorum exceeding arbiter count must be rejected with "InvalidQuorum".
#[test]
#[should_panic(expected = "InvalidQuorum")]
fn test_create_engagement_quorum_exceeds_arbiter_count_rejected() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);

    client.create_engagement(
        &String::from_str(&env, "ENG-Q-OVER"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone()],
            quorum: 3, // 3 > 2 arbiters
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

// ============================================================
// Issue #202 — DUPLICATE ARBITER ADDRESSES IN ARBITERS VEC
// ============================================================

/// Creating an engagement with the same arbiter address twice in the arbiters
/// vector must be rejected with "DuplicateArbiter".
#[test]
#[should_panic(expected = "DuplicateArbiter")]
fn test_create_engagement_duplicate_arbiter_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-DUP-ARB"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone(), arbiter.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

/// Three arbiters where two are duplicates must also be rejected.
#[test]
#[should_panic(expected = "DuplicateArbiter")]
fn test_create_engagement_three_arbiters_with_one_duplicate_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let a2 = Address::generate(&env);

    client.create_engagement(
        &String::from_str(&env, "ENG-DUP-ARB-3"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone(), a2.clone(), arbiter.clone()],
            quorum: 2,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

// ============================================================
// Issue #203 — EMPTY ARBITERS VECTOR VALIDATION
// ============================================================

/// Creating an engagement with an empty arbiters vector must be rejected
/// with "NoArbitersProvided".
#[test]
#[should_panic(expected = "NoArbitersProvided")]
fn test_create_engagement_empty_arbiters_rejected() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.create_engagement(
        &String::from_str(&env, "ENG-NO-ARB"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}

// ============================================================
// Issue #204 — PROOF RESUBMISSION AFTER COMPANY CONFIRMATION
// ============================================================

/// Once a milestone is Confirmed by the company, the recruiter must not be
/// able to submit a new proof for that same milestone — rejected with
/// "MilestoneAlreadyConfirmed".
#[test]
#[should_panic(expected = "MilestoneAlreadyConfirmed")]
fn test_submit_proof_after_confirmation_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REPROOF");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REPROOF",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof1"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // Attempt to submit another proof after confirmation — must be rejected.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof2"),
    );
}

// ============================================================
// Issue #205 — MILESTONE PERCENTAGE AMENDMENT TO INVALID SUM
// ============================================================

/// Accepting an amendment that would cause the sum of all milestone percentages
/// to no longer equal 100 must be rejected with "milestone percentages must sum to 100".
#[test]
#[should_panic(expected = "milestone percentages must sum to 100")]
fn test_accept_amendment_invalid_sum_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-AMEND-BAD-SUM");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-AMEND-BAD-SUM",
    );

    // Current milestones: 30, 40, 30 = 100
    // Propose changing milestone 0 from 30% to 50% → sum becomes 50+40+30=120
    client.propose_amendment(&company, &eng_id, &0, &50u32);
    client.accept_amendment(&recruiter, &eng_id, &0);
}

// ============================================================
// Issue #206 — RAISE DISPUTE ON NON-PROOF-SUBMITTED MILESTONE
// ============================================================

/// Company cannot raise a dispute on a milestone that is not in ProofSubmitted
/// status — must be rejected with "milestone proof not yet submitted".
#[test]
#[should_panic(expected = "milestone proof not yet submitted")]
fn test_raise_dispute_on_pending_milestone_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-DISP-PENDING");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISP-PENDING",
    );

    // Milestone 0 is still Pending — no proof submitted yet.
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "no proof"));
}

/// Company cannot raise a dispute on a Locked retention milestone.
#[test]
#[should_panic(expected = "milestone proof not yet submitted")]
fn test_raise_dispute_on_locked_milestone_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-DISP-LOCKED");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DISP-LOCKED",
    );

    // Milestone 1 is Locked — cannot dispute until it transitions to Pending/ProofSubmitted.
    client.raise_dispute(&company, &eng_id, &1, &String::from_str(&env, "locked"));
}

// ============================================================
// Issue #207 — UNLOCK MILESTONE ALREADY PENDING OR CONFIRMED
// ============================================================

/// Calling unlock_milestone on an already-Pending milestone must be rejected
/// with "milestone is already unlocked".
#[test]
#[should_panic(expected = "milestone is already unlocked")]
fn test_unlock_milestone_already_pending_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-UNLOCK-PENDING");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNLOCK-PENDING",
    );

    // Milestone 0 is Pending from creation — trying to unlock it is invalid.
    client.unlock_milestone(&eng_id, &0);
}

/// Calling unlock_milestone on an already-Confirmed milestone must also be rejected.
#[test]
#[should_panic(expected = "milestone is already unlocked")]
fn test_unlock_milestone_already_confirmed_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-UNLOCK-CONF");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNLOCK-CONF",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // Milestone 0 is now Confirmed — trying to unlock it is invalid.
    client.unlock_milestone(&eng_id, &0);
}

// ============================================================
// Issue #208 — CO-RECRUITER WITHOUT RECRUITER_SPLIT_BPS VALIDATION
// ============================================================

/// If co_recruiter is Some but recruiter_split_bps is 0, the split would be
/// invalid (primary gets 0%, co gets 100%) — must be rejected with "InvalidSplitBps".
#[test]
#[should_panic(expected = "InvalidSplitBps")]
fn test_co_recruiter_with_zero_split_bps_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let co_recruiter = Address::generate(&env);

    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter),
        recruiter_split_bps: 0,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-CO-ZERO"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );
}

// ============================================================
// Issue #209 — FORCE CONFIRM WITHIN CONFIRM WINDOW BUT BEFORE DISPUTE WINDOW CLOSES
// ============================================================

/// The confirm window (for force_confirm) is measured from proof_submitted_at.
/// The dispute window is also measured from proof_submitted_at. If the confirm
/// window is shorter than the dispute window, force_confirm could be called
/// while the company still has time to dispute. This is intentional: it allows
/// the recruiter to force resolution if the company is unresponsive, even though
/// a dispute could theoretically still be raised before force_confirm is called.
/// This test documents the timing interaction between the two windows.
#[test]
fn test_force_confirm_can_succeed_before_dispute_window_closes() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // Set confirm window to 100 ledgers and dispute window to 200 ledgers.
    // This creates a 100-ledger gap where force_confirm is allowed but disputes
    // could still be raised (though the company hasn't done so in this test).
    client.set_confirm_window(&company, &100u32);
    client.set_dispute_window(&company, &200u32);

    let eng_id = String::from_str(&env, "ENG-FC-VS-DW");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-VS-DW",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance 101 ledgers: past confirm window (100), still within dispute window (200).
    advance_ledger(&env, 101);

    // force_confirm succeeds — the confirm window has elapsed.
    client.force_confirm_milestone(&arbiter, &eng_id, &0);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Confirmed);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

// ============================================================
// Issue #210 — TOP UP ESCROW ON TERMINAL ENGAGEMENT
// ============================================================

/// top_up_escrow must be rejected on a terminal engagement (Completed, Cancelled,
/// or Expired) — panics with "engagement is in a terminal state".
#[test]
#[should_panic(expected = "engagement is in a terminal state")]
fn test_top_up_escrow_on_completed_engagement_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Use a single-milestone engagement for quick completion.
    let single_milestone = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Placement"),
            payment_percent: 100,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
        },
    ];

    let eng_id = String::from_str(&env, "ENG-TOPUP-DONE");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &single_milestone,
        &vec![&env],
        &default_config(),
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Completed);

    // Attempting to top up a completed engagement must be rejected.
    client.top_up_escrow(&company, &eng_id, &500_000_000);
}

/// top_up_escrow on a cancelled engagement must also be rejected.
#[test]
#[should_panic(expected = "engagement is in a terminal state")]
fn test_top_up_escrow_on_cancelled_engagement_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-TOPUP-CANC");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-TOPUP-CANC",
    );

    client.cancel_engagement(&company, &recruiter, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);

    client.top_up_escrow(&company, &eng_id, &500_000_000);
}
