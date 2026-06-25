#![cfg(test)]

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Ledger},
    token, vec, Address, Env, String,
};

// ============================================================
// TEST HELPERS
// ============================================================

/// Standard setup: deploy contract + mock USDC + create all party addresses
fn setup() -> (Env, Address, Address, Address, Address, Address) {
    let env = Env::default();
    env.mock_all_auths();

    // Set high TTLs before registering so the contract instance survives large ledger jumps in tests.
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

    let company  = Address::generate(&env);
    let recruiter = Address::generate(&env);
    let arbiter  = Address::generate(&env);

    // Fund the company with 50,000 USDC (500_000_000_000 stroops)
    token_client.mint(&company, &500_000_000_000);

    let client = HireSettleContractClient::new(&env, &contract_id);
    client.init(&company);

    (env, contract_id, token_id, company, recruiter, arbiter)
}

/// Build the standard 3-milestone set (Placement 30% + 30-day 40% + 90-day 30%)
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
            valid_after_ledger: 0, // set by contract
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Locked,
        },
        Milestone {
            name: String::from_str(env, "90-Day Retention"),
            payment_percent: 30,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0, // set by contract
            proof_hash: String::from_str(env, ""),
            status: MilestoneStatus::Locked,
        },
    ]
}

/// Helper: create a standard engagement
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
        arbiter,
        token_id,
        &1_000_000_000, // 100 USDC
        &String::from_str(env, "Senior Engineer"),
        &build_milestones(env),
        &vec![env, 30u32, 90u32], // 30-day and 90-day retention windows
    );
}

// ============================================================
// TESTS
// ============================================================

#[test]
fn test_create_engagement_success() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-001",
    );

    // Company balance should decrease
    let company_balance = token_client.balance(&company);
    assert_eq!(company_balance, 500_000_000_000 - 1_000_000_000);

    // Contract holds the escrow
    let escrow = token_client.balance(&contract_id);
    assert_eq!(escrow, 1_000_000_000);

    // Engagement record is correct
    let eng = client.get_engagement(&String::from_str(&env, "ENG-001"));
    assert_eq!(eng.status, EngagementStatus::Active);
    assert_eq!(eng.total_amount, 1_000_000_000);
    assert_eq!(eng.released_amount, 0);
    assert_eq!(eng.milestones.len(), 3);

    // Milestone 0 (Placement) should be Pending immediately
    let m0 = client.get_milestone(&String::from_str(&env, "ENG-001"), &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);

    // Milestone 1 and 2 (Retention) should be Locked
    let m1 = client.get_milestone(&String::from_str(&env, "ENG-001"), &1);
    let m2 = client.get_milestone(&String::from_str(&env, "ENG-001"), &2);
    assert_eq!(m1.status, MilestoneStatus::Locked);
    assert_eq!(m2.status, MilestoneStatus::Locked);

    // Retention milestones have valid_after_ledger set (> 0)
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
            payment_percent: 40, // 40 + 40 = 80, not 100
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
        &arbiter,
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Dev"),
        &bad_milestones,
        &vec![&env, 30u32],
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

    // Recruiter submits placement proof
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://QmOfferLetter123"),
    );

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);

    // Company confirms placement — 30% released
    client.confirm_milestone(&company, &eng_id, &0);

    let expected_payment = 1_000_000_000i128 * 30 / 100; // 300_000_000
    let recruiter_balance = token_client.balance(&recruiter);
    assert_eq!(recruiter_balance, expected_payment);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.released_amount, expected_payment);
    assert_eq!(eng.status, EngagementStatus::Active); // still active (2 more milestones)
}

#[test]
fn test_retention_milestone_unlock_timing() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-001");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-001",
    );

    // Check milestone 1 is not yet unlockable (we're at the creation ledger)
    let unlockable = client.is_milestone_unlockable(&eng_id, &1);
    assert!(!unlockable);

    // Advance ledger past the 30-day window (30 days × 17280 ledgers/day)
    let thirty_day_ledgers: u32 = 30 * 17_280;
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + thirty_day_ledgers + 1,
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });

    // Now it should be unlockable
    let unlockable = client.is_milestone_unlockable(&eng_id, &1);
    assert!(unlockable);

    // Unlock it
    client.unlock_milestone(&eng_id, &1);
    let m1 = client.get_milestone(&eng_id, &1);
    assert_eq!(m1.status, MilestoneStatus::Pending);

    // Milestone 2 (90-day) should still be Locked
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

    // Try to unlock milestone 1 immediately — should panic
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

    // --- Milestone 0: Placement (30%) ---
    client.submit_proof(
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://offer-letter"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    // --- Advance ledger 31 days, unlock + confirm Milestone 1 (40%) ---
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
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter, &eng_id, &1,
        &String::from_str(&env, "ipfs://30-day-hr-confirmation"),
    );
    client.confirm_milestone(&company, &eng_id, &1);
    assert_eq!(token_client.balance(&recruiter), 300_000_000 + 400_000_000);

    // --- Advance ledger 91 days total, unlock + confirm Milestone 2 (30%) ---
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + (60 * 17_280), // 31+60 = 91 days
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(
        &recruiter, &eng_id, &2,
        &String::from_str(&env, "ipfs://90-day-payroll"),
    );
    client.confirm_milestone(&company, &eng_id, &2);

    // Recruiter should have received the full 100 USDC
    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);

    // Engagement should be Completed
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Completed);
    assert_eq!(eng.released_amount, 1_000_000_000);

    // Escrow balance should be 0
    assert_eq!(client.get_escrow_balance(&eng_id), 0);
}

#[test]
fn test_raise_and_resolve_dispute_approve() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-DISPUTE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-DISPUTE",
    );

    client.submit_proof(
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://questionable-proof"),
    );

    // Company disputes
    client.raise_dispute(&company, &eng_id, &0);
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Disputed);

    // Arbiter approves — payment released
    client.resolve_dispute(&arbiter, &eng_id, &0, &true);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_raise_and_resolve_dispute_reject() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-REJECT");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-REJECT",
    );

    client.submit_proof(
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://bad-proof"),
    );
    client.raise_dispute(&company, &eng_id, &0);

    // Arbiter rejects — milestone reset to Pending
    client.resolve_dispute(&arbiter, &eng_id, &0, &false);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);

    // No payment released
    assert_eq!(token_client.balance(&recruiter), 0);
}

#[test]
fn test_request_replacement() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-REPLACE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-REPLACE",
    );

    // Recruiter places candidate — 30% paid
    client.submit_proof(
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    // Candidate leaves — company requests replacement
    client.request_replacement(&company, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::ReplacementRequested);

    // Placement milestone should be reset to Pending
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Pending);

    // Retention milestones should be reset to Locked
    let m1 = client.get_milestone(&eng_id, &1);
    let m2 = client.get_milestone(&eng_id, &2);
    assert_eq!(m1.status, MilestoneStatus::Locked);
    assert_eq!(m2.status, MilestoneStatus::Locked);

    // Recruiter submits proof for the replacement candidate
    client.submit_proof(
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://replacement-offer"),
    );

    // Engagement should be Active again after new proof is submitted
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-EARLY",
    );

    // Try replacement before placement is confirmed — should panic
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CANCEL",
    );

    // Cancel before any milestone confirmed (both parties consent)
    client.cancel_engagement(&company, &recruiter, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);

    // Company should get full refund
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PARTIAL-CANCEL",
    );

    // Confirm placement — 30% (300_000_000) released to recruiter
    client.submit_proof(
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    // Both parties consent to cancel — only unreleased 70% refunded to company
    client.cancel_engagement(&company, &recruiter, &eng_id);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Cancelled);

    let expected_refund = 1_000_000_000i128 - 300_000_000;
    assert_eq!(
        token_client.balance(&company),
        company_balance_before - 1_000_000_000 + expected_refund
    );

    // Recruiter keeps previously released funds
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
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Recruiter tries to confirm their own milestone — should panic
    client.confirm_milestone(&recruiter, &eng_id, &0);
}

#[test]
fn test_ledgers_until_unlock() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-TIMER");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-TIMER",
    );

    let remaining = client.ledgers_until_unlock(&eng_id, &1);
    // Should be approximately 30 × 17280 = 518400 ledgers
    assert!(remaining > 0);
    assert!(remaining <= 30 * 17_280);
}

#[test]
fn test_two_milestone_engagement_50_50() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    // Startup style: 50% on placement, 50% on 30-day retention
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
        &company, &recruiter, &arbiter, &token_id,
        &2_000_000_000, // 200 USDC
        &String::from_str(&env, "CTO"),
        &milestones,
        &vec![&env, 30u32],
    );

    // Confirm placement — 100 USDC (50%)
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);

    // Advance past 30 days, unlock and confirm retention
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
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://30day"));
    client.confirm_milestone(&company, &eng_id, &1);

    assert_eq!(token_client.balance(&recruiter), 2_000_000_000);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.status, EngagementStatus::Completed);
}

// ============================================================
// #47 — get_total_released
// ============================================================

#[test]
fn test_get_total_released_zero() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REL-ZERO");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-REL-ZERO",
    );

    assert_eq!(client.get_total_released(&eng_id), 0);
}

#[test]
fn test_get_total_released_partial() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REL-PART");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-REL-PART",
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.confirm_milestone(&company, &eng_id, &0);

    // 30% of 1_000_000_000
    assert_eq!(client.get_total_released(&eng_id), 300_000_000);
}

#[test]
fn test_get_total_released_full() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-REL-FULL");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-REL-FULL",
    );

    // Confirm placement
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.confirm_milestone(&company, &eng_id, &0);

    // Advance 31 days, unlock + confirm 30-day retention
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
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://30day"));
    client.confirm_milestone(&company, &eng_id, &1);

    // Advance to 91 days, unlock + confirm 90-day retention
    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + (60 * 17_280),
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(&recruiter, &eng_id, &2, &String::from_str(&env, "ipfs://90day"));
    client.confirm_milestone(&company, &eng_id, &2);

    assert_eq!(client.get_total_released(&eng_id), 1_000_000_000);
}

// ============================================================
// #48 — get_engagement_summary
// ============================================================

#[test]
fn test_get_engagement_summary_after_create() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-SUMM-CREATE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SUMM-CREATE",
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SUMM-PART",
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-SUMM-DONE",
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.confirm_milestone(&company, &eng_id, &0);

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
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://30day"));
    client.confirm_milestone(&company, &eng_id, &1);

    env.ledger().set(soroban_sdk::testutils::LedgerInfo {
        timestamp: 0,
        protocol_version: 22,
        sequence_number: env.ledger().sequence() + (60 * 17_280),
        network_id: Default::default(),
        base_reserve: 5_000_000,
        min_temp_entry_ttl: 16,
        min_persistent_entry_ttl: 100_000,
        max_entry_ttl: 6_300_000,
    });
    client.unlock_milestone(&eng_id, &2);
    client.submit_proof(&recruiter, &eng_id, &2, &String::from_str(&env, "ipfs://90day"));
    client.confirm_milestone(&company, &eng_id, &2);

    let summary = client.get_engagement_summary(&eng_id);

    assert_eq!(summary.status, EngagementStatus::Completed);
    assert_eq!(summary.released_amount, 1_000_000_000);
    assert_eq!(summary.total_amount, 1_000_000_000);
}

// ============================================================
// #46 — partial cancellation (additional tests)
// ============================================================

#[test]
fn test_cancel_full_refund_zero_released() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-CANCEL-ZERO");
    let company_balance_before = token_client.balance(&company);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CANCEL-ZERO",
    );

    // No milestones confirmed — should refund entire amount
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
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CANCEL-AUTH",
    );

    // Pass wrong address as recruiter — should be rejected
    let impostor = Address::generate(&env);
    client.cancel_engagement(&company, &impostor, &eng_id);
}

// ============================================================
// #45 — arbiter succession
// ============================================================

#[test]
fn test_happy_arbiter_succession() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-ARBITER");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER",
    );

    let new_arbiter = Address::generate(&env);

    // Current arbiter nominates a successor
    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);

    // Old arbiter still active — can still raise dispute logic is unchanged
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiter, arbiter);

    // Nominee claims the role
    client.claim_arbiter(&new_arbiter, &eng_id);

    // New arbiter is now active
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiter, new_arbiter);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_wrong_claimer_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-ARBITER-BAD");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-BAD",
    );

    let new_arbiter = Address::generate(&env);
    let impostor = Address::generate(&env);

    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);

    // Wrong address tries to claim — should panic
    client.claim_arbiter(&impostor, &eng_id);
}

#[test]
fn test_old_arbiter_retains_role_until_claim() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-ARBITER-OLD");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-OLD",
    );

    let new_arbiter = Address::generate(&env);

    client.nominate_arbiter_successor(&arbiter, &eng_id, &new_arbiter);

    // Old arbiter is still the arbiter until claim completes
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiter, arbiter);

    // Old arbiter can still resolve disputes: submit a proof, raise dispute, then resolve
    client.submit_proof(
        &recruiter, &eng_id, &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.raise_dispute(&company, &eng_id, &0);
    // Old arbiter resolves — should not panic
    client.resolve_dispute(&arbiter, &eng_id, &0, &true);

    // Now successor claims
    client.claim_arbiter(&new_arbiter, &eng_id);
    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiter, new_arbiter);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_arbiter_cannot_nominate() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-ARBITER-UNAUTH");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-UNAUTH",
    );

    let new_arbiter = Address::generate(&env);

    // Company tries to nominate — should panic
    client.nominate_arbiter_successor(&company, &eng_id, &new_arbiter);
}

#[test]
#[should_panic(expected = "no pending arbiter nomination")]
fn test_claim_without_nomination_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-ARBITER-NOCLAIM");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-ARBITER-NOCLAIM",
    );

    let new_arbiter = Address::generate(&env);

    // No nomination made — claim should panic
    client.claim_arbiter(&new_arbiter, &eng_id);
}
