#![cfg(test)]

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
        },
        Milestone {
            name: String::from_str(env, "30-Day Retention"),
            payment_percent: 40,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Locked,
        },
        Milestone {
            name: String::from_str(env, "90-Day Retention"),
            payment_percent: 30,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Locked,
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
        &None,
    );
}

fn has_event(env: &Env, event_name: &str) -> bool {
    let expected = Symbol::new(env, event_name);
    let events = env.events().all();
    for i in 0..events.len() {
        let (_, topics, _) = events.get(i).unwrap();
        let topic: Symbol = topics.get(0).unwrap().try_into_val(env).unwrap();
        if topic == expected {
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
        },
        Milestone {
            name: String::from_str(&env, "Retention"),
            payment_percent: 40,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Locked,
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-BAD"),
        &company,
        &recruiter,
        &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1 },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Dev"),
        &bad_milestones,
        &vec![&env, 30u32],
        &None,
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

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer-letter"));
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://30-day-hr-confirmation"));
    client.confirm_milestone(&company, &eng_id, &1);
    assert_eq!(token_client.balance(&recruiter), 300_000_000 + 400_000_000);

    advance_ledger(&env, 60 * 17_280);
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(&recruiter, &eng_id, &2, &String::from_str(&env, "ipfs://90-day-payroll"));
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

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://questionable-proof"));
    client.raise_dispute(&company, &eng_id, &0);

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

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://bad-proof"));
    client.raise_dispute(&company, &eng_id, &0);

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

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    client.request_replacement(&company, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::ReplacementRequested);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);

    let m1 = client.get_milestone(&eng_id, &1);
    let m2 = client.get_milestone(&eng_id, &2);
    assert_eq!(m1.status, MilestoneStatus::Locked);
    assert_eq!(m2.status, MilestoneStatus::Locked);

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://replacement-offer"));

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

    client.request_replacement(&company, &eng_id);
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

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
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

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof"));
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
        },
        Milestone {
            name: String::from_str(&env, "30-Day Retention"),
            payment_percent: 50,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Locked,
        },
    ];

    let eng_id = String::from_str(&env, "ENG-5050");
    client.create_engagement(
        &eng_id,
        &company, &recruiter,
        &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1 },
        &token_id,
        &2_000_000_000,
        &String::from_str(&env, "CTO"),
        &milestones,
        &vec![&env, 30u32],
        &None,
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-REL-ZERO");
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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-REL-FULL");

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SUMM-CREATE");

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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SUMM-PART");

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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SUMM-DONE");

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
// Cancellation edge cases
// ============================================================

#[test]
fn test_cancel_full_refund_zero_released() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CANCEL-ZERO");
    let company_balance_before = token_client.balance(&company);
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CANCEL-ZERO");
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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CANCEL-AUTH");
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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER");

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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-BAD");

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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-OLD");

    let new_arbiter = Address::generate(&env);
    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.get(0).unwrap(), arbiter);

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.raise_dispute(&company, &eng_id, &0);
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);

    client.claim_arbiter(&new_arbiter, &eng_id);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.get(0).unwrap(), new_arbiter);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_arbiter_cannot_nominate() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARBITER-UNAUTH");
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-UNAUTH");
    let new_arbiter = Address::generate(&env);
    client.nominate_arbiter_successor(&company, &eng_id, &new_arbiter);
}

#[test]
#[should_panic(expected = "no pending arbiter nomination")]
fn test_claim_without_nomination_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-ARBITER-NOCLAIM");
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-NOCLAIM");
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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SECS-FUTURE");

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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SECS-ZERO");

    advance_ledger(&env, 30 * 17_280 + 1);

    let seconds = client.get_estimated_unlock_seconds(&eng_id, &1);
    assert_eq!(seconds, 0);
}

#[test]
fn test_estimated_unlock_seconds_placement_returns_zero() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-SECS-PLACE");
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SECS-PLACE");

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
        &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1 },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &Some(cid.clone()),
    );

    let result = client.get_metadata_hash(&String::from_str(&env, "ENG-META"));
    assert_eq!(result, Some(cid));
}

#[test]
fn test_metadata_hash_absent() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-NOMETA");

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
        &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1 },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &Some(String::from_str(&env, "")),
    );
}

// ============================================================
// #9 — proof resubmission cooldown
// ============================================================

#[test]
#[should_panic(expected = "ResubmitTooSoon")]
fn test_proof_cooldown_blocks_resubmit() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-COOL");
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-COOL");

    // First submission — always allowed
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof1"));
    // Dispute + reject → back to Pending
    client.raise_dispute(&company, &eng_id, &0);
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Second submission immediately within cooldown — must panic
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof2"));
}

#[test]
fn test_proof_cooldown_passes_after_wait() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-COOL-PASS");
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-COOL-PASS");

    // First submission
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof1"));
    // Dispute + reject → back to Pending
    client.raise_dispute(&company, &eng_id, &0);
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Advance past the default cooldown (2_880 ledgers)
    advance_ledger(&env, 2_881);

    // Should succeed now
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof2"));
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);
}

#[test]
fn test_set_proof_cooldown_admin() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Admin (company) sets a very short cooldown of 1 ledger
    client.set_proof_cooldown(&company, &1u32);

    let eng_id = String::from_str(&env, "ENG-COOL-SET");
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-COOL-SET");

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof1"));
    client.raise_dispute(&company, &eng_id, &0);
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Advance by exactly 1 ledger (matching cooldown)
    advance_ledger(&env, 1);

    // Should succeed with cooldown = 1
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof2"));
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
        &ArbiterSetup { arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()], quorum: 2 },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &None,
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof"));
    client.raise_dispute(&company, &eng_id, &0);

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
        &ArbiterSetup { arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()], quorum: 2 },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &None,
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof"));
    client.raise_dispute(&company, &eng_id, &0);

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
        &ArbiterSetup { arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()], quorum: 2 },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &None,
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof"));
    client.raise_dispute(&company, &eng_id, &0);

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
    create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SINGLE");

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://proof"));
    client.raise_dispute(&company, &eng_id, &0);

    // Single arbiter, quorum=1 — one vote resolves immediately
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-BASIC",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-ACCEPT",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-MULTI",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-CAP",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-SELF",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-REJECT",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-REJECT-SELF",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-EXPIRE",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-OVERWRITE",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-AMEND-BOTH",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PEND-ACTIVE",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PEND-ACCEPT",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PEND-REJECT",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PEND-EXPIRED",
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
