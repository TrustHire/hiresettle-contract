//! Unit tests for the HireSettle contract.
//!
//! Every test runs against a fresh `Env` built by `setup()`, which registers
//! the contract and a Stellar asset token, mocks all auths, mints escrow
//! funds to the company, and calls `init` with the company as admin. Tests
//! are grouped under `// ====` section banners, mostly one per GitHub issue.
//!
//! 368 tests, 142 of them `#[should_panic]` cases asserting on an exact panic
//! message (see the errors reference in `errors.rs`). Recount with
//! `grep -c '^#\[test\]' src/test.rs` or `cargo test`.
//!
//! | Category | Tests | Covers |
//! |---|---|---|
//! | Core lifecycle | 47 | create, submit proof, confirm, batch and force confirm, cancel, top-up, release totals, unlock timing and events, proof resubmission cooldown |
//! | Creation validation & limits | 52 | engagement ID format, job title, milestone count / name length / uniqueness, per-company active cap, role collisions, token allowlist, minimum amount, decimals-agnostic math |
//! | Disputes & arbitration | 49 | dispute window and reason codes, multi-arbiter quorum, vote tallies, arbiter succession and stale nominations, random pool panels, response-time bias, self-recusal, disputes during replacement |
//! | Replacement, transfer, expiry & no-show | 28 | replacement reason codes, recruiter transfer, `expire_engagement`, no-show penalty, rating attribution after transfer |
//! | Fees & payouts | 25 | platform fee and fee event, fee tiers, co-recruiter split and its renegotiation, streamed milestone payouts |
//! | Admin, pause & upgrade | 40 | global pause, per-engagement quarantine and the pause interaction matrix, two-step admin transfer, upgrade time-lock, version, fee waiver, per-token minimums, referrers |
//! | Queries & metadata | 32 | engagement summaries, counts and per-company / per-recruiter listings, metadata hash, public-engagement index, tag limits, recruiter verification |
//! | Pooled escrow, config cosigner, emergency multisig & rebates | 14 | issues #472–#475 |
//! | Cross-cutting edge cases | 17 | the `ADDITIONAL COMPREHENSIVE TEST COVERAGE` section |
//! | Timeline, fee snapshot, co-recruiter bond & panel resize | 33 | issues #501 and #505–#507 |
//! | Proof Merkle roots | 22 | issue #486: `submit_proof_root` vs `submit_proof` parity, root storage and replacement, `verify_proof_inclusion` valid / tampered / wrong-path cases |

#![cfg(test)]
extern crate std;

use super::*;
use soroban_sdk::{
    testutils::{Address as _, Events, Ledger},
    contract, contractimpl, token, vec, Address, Bytes, BytesN, Env, String, Symbol, TryIntoVal,
    Vec,
};

// ============================================================
// TEST HELPERS
// ============================================================

/// Minimal contract WASM used by issue #456 execute_upgrade success path.
#[allow(dead_code)]
const UPGRADE_DUMMY_WASM: &[u8] = include_bytes!("../testdata/upgrade_dummy.wasm");

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

/// Linear prerequisites (each milestone depends on the previous one) — the
/// issue #461 equivalent of the old strict sequential rule, so every test
/// built on this fixture doubles as a regression test for it.
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
            prerequisites: Vec::new(env),
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
            prerequisites: vec![env, 0u32],
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
            prerequisites: vec![env, 1u32],
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
            weights: None,
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
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
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
            prerequisites: Vec::new(&env),
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-BAD"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
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
            prerequisites: Vec::new(&env),
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            is_public: false,
            stream_duration_ledgers: None,
            recruiter_bond_amount: None,
            bundle_id: None,
            fund_from_pool: false,
            snapshot_fee_tier: false,
            co_recruiter_bond_amount: None,
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
            weights: None,
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
            is_public: false,
            stream_duration_ledgers: None,
            recruiter_bond_amount: None,
            bundle_id: None,
            fund_from_pool: false,
            snapshot_fee_tier: false,
            co_recruiter_bond_amount: None,
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
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SPLIT-60-40"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-BAD-SPLIT"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SPLIT-REM"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SUM-SPLIT"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-SPLIT-100"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
                prerequisites: Vec::new(&env),
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
// #239 — per-engagement pause (quarantine) and its interaction with the global pause
// ============================================================

#[test]
#[should_panic(expected = "EngagementPaused")]
fn test_engagement_pause_blocks_lifecycle_while_contract_runs() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-QUARANTINE");

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-QUARANTINE",
    );

    // Contract is running; only the single engagement is quarantined.
    assert!(!client.is_paused());
    client.pause_engagement(&company, &eng_id, &String::from_str(&env, "quarantine"));
    assert!(client.is_engagement_paused(&eng_id));

    // The engagement's own lifecycle call is rejected, other engagements are unaffected.
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
}

#[test]
fn test_global_unpause_does_not_clear_engagement_pause() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-QUARANTINE-INTERACTION");

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-QUARANTINE-INTERACTION",
    );

    // Quarantine the engagement, then pause the whole contract.
    client.pause_engagement(&company, &eng_id, &String::from_str(&env, "quarantine"));
    client.pause(&company);

    // Call is blocked while both guards are active.
    let res = client.try_submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    assert!(res.is_err());

    // Resuming the contract does NOT lift the per-engagement quarantine: the call
    // stays blocked purely because the engagement is still quarantined.
    client.unpause(&company);
    assert!(!client.is_paused());
    assert!(client.is_engagement_paused(&eng_id));

    let res = client.try_submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    assert!(res.is_err());

    // Only lifting the quarantine lets the call through.
    client.unpause_engagement(&company, &eng_id);
    assert!(!client.is_engagement_paused(&eng_id));
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
}

#[test]
fn test_engagement_unpause_does_not_clear_global_pause() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-QUARANTINE-GLOBAL");

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-QUARANTINE-GLOBAL",
    );

    // Pause the contract, then quarantine the engagement (allowed while paused).
    client.pause(&company);
    client.pause_engagement(&company, &eng_id, &String::from_str(&env, "quarantine"));

    // Lifting the engagement quarantine does NOT resume the globally paused contract.
    client.unpause_engagement(&company, &eng_id);
    assert!(!client.is_engagement_paused(&eng_id));
    assert!(client.is_paused());

    let res = client.try_submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    assert!(res.is_err());

    // Only resuming the contract lets the call through.
    client.unpause(&company);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
}

#[test]
fn test_engagement_pause_query_unknown_id_false() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Unknown engagement IDs report false rather than panicking.
    assert!(!client.is_engagement_paused(&String::from_str(&env, "ENG-DOES-NOT-EXIST")));
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
//         &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1, weights: None },
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
            prerequisites: Vec::new(&env),
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
            weights: None,
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
            weights: None,
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
            prerequisites: Vec::new(&env),
        });
    }

    client.create_engagement(
        &String::from_str(&env, "ENG-10-MILESTONES"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
        });
    }

    client.create_engagement(
        &String::from_str(&env, "ENG-11-MILESTONES"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Job Title"),
        &Vec::new(&env),
        &Vec::new(&env),
        &default_config(),
    );
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-64-CHAR"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-65-CHAR"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-EMPTY-NAME"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-PARTIAL-FAIL"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-UNIQUE"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-DUPLICATE"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            prerequisites: Vec::new(&env),
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
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-CASE-SENSITIVE"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            prerequisites: Vec::new(&env),
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
            weights: None,
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
            weights: None,
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
                prerequisites: Vec::new(&env),
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
            weights: None,
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
// Issue #144 — expire_engagement test coverage
// ============================================================



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
            weights: None,
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
    assert_eq!(token_client.balance(&a2), a2_balance_before + 3_000_000);
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
fn test_company_cosigner_can_confirm_milestone() {
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
        "ENG-CO-COSIGN-CONFIRM",
    );

    let company_cosigner = Address::generate(&env);
    client.set_company_cosigner(&company, &company_cosigner);

    let eng_id = String::from_str(&env, "ENG-CO-COSIGN-CONFIRM");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company_cosigner, &eng_id, &0);

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Confirmed);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_stranger_cannot_confirm_milestone_when_company_cosigner_set() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CO-COSIGN-STRANGER",
    );

    let company_cosigner = Address::generate(&env);
    client.set_company_cosigner(&company, &company_cosigner);

    let eng_id = String::from_str(&env, "ENG-CO-COSIGN-STRANGER");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Registering a cosigner authorizes that one address, not every caller.
    let stranger = Address::generate(&env);
    client.confirm_milestone(&stranger, &eng_id, &0);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_reregistering_company_cosigner_revokes_previous_cosigner() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-CO-COSIGN-REREG",
    );

    let old_cosigner = Address::generate(&env);
    let new_cosigner = Address::generate(&env);
    client.set_company_cosigner(&company, &old_cosigner);
    client.set_company_cosigner(&company, &new_cosigner);
    assert_eq!(client.get_company_cosigner(&company), Some(new_cosigner));

    let eng_id = String::from_str(&env, "ENG-CO-COSIGN-REREG");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // The replaced cosigner no longer has company authority.
    client.confirm_milestone(&old_cosigner, &eng_id, &0);
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




// ============================================================
// ADDITIONAL COMPREHENSIVE TEST COVERAGE
// ============================================================

/// Test that arbiter nomination cannot be done on a non-existent engagement
#[test]
#[should_panic(expected = "engagement not found")]
fn test_nominate_arbiter_on_nonexistent_engagement_rejected() {
    let (env, contract_id, _token_id, _company, _recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let new_arbiter = Address::generate(&env);
    let fake_eng_id = String::from_str(&env, "ENG-DOES-NOT-EXIST");

    client.nominate_arbiter_successor(&arbiter, &fake_eng_id, &new_arbiter);
}

/// Test that multiple companies can have independent engagement counts
#[test]
fn test_multiple_companies_independent_active_counts() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Setup second company with its own token
    let token_admin2 = Address::generate(&env);
    let token_id2 = env
        .register_stellar_asset_contract_v2(token_admin2.clone())
        .address();
    let token_client2 = token::StellarAssetClient::new(&env, &token_id2);
    let company2 = Address::generate(&env);
    token_client2.mint(&company2, &500_000_000_000);

    // Create engagements for both companies
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-C1-1",
    );
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-C1-2",
    );

    client.create_engagement(
        &String::from_str(&env, "ENG-C2-1"),
        &company2,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id2,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    assert_eq!(client.get_company_active_count(&company), 2);
    assert_eq!(client.get_company_active_count(&company2), 1);
    assert_eq!(client.get_engagement_count(), 3);
}

/// Test that force_confirm_milestone emits the correct event with all data
#[test]
fn test_force_confirm_milestone_event_contains_correct_data() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_confirm_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-FC-EVT-DATA");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-EVT-DATA",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    advance_ledger(&env, 101);

    client.force_confirm_milestone(&arbiter, &eng_id, &0);

    let expected = Symbol::new(&env, "milestone_force_confirmed");
    let mut found = false;
    for (_, topics, data) in env.events().all().iter() {
        let topic: Symbol = topics.get(0).unwrap().try_into_val(&env).unwrap();
        if topic == expected {
            let (idx, payment): (u32, i128) = data.try_into_val(&env).unwrap();
            assert_eq!(idx, 0);
            assert_eq!(payment, 300_000_000);
            found = true;
        }
    }
    assert!(
        found,
        "milestone_force_confirmed event not found with correct data"
    );
}

/// Test that co-recruiter receives correct split even with platform fee
#[test]
fn test_co_recruiter_split_with_platform_fee() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = Address::generate(&env);
    let co_recruiter = Address::generate(&env);

    // Set 2% platform fee (200 bps)
    client.set_platform_fee(&company, &200, &treasury);

    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter.clone()),
        recruiter_split_bps: 6_000, // 60% to primary, 40% to co
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-CO-PLAT-FEE"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );

    let eng_id = String::from_str(&env, "ENG-CO-PLAT-FEE");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // Milestone 0: 30% of 1_000_000_000 = 300_000_000
    // Platform fee: 300_000_000 * 200 / 10_000 = 6_000_000
    // After platform fee: 300_000_000 - 6_000_000 = 294_000_000
    // Primary (60%): 294_000_000 * 6000 / 10000 = 176_400_000
    // Co (40%): 294_000_000 - 176_400_000 = 117_600_000

    assert_eq!(token_client.balance(&treasury), 6_000_000);
    assert_eq!(token_client.balance(&recruiter), 176_400_000);
    assert_eq!(token_client.balance(&co_recruiter), 117_600_000);
}


/// Test that replacement milestone reset preserves correct milestone structure
#[test]
fn test_replacement_preserves_milestone_structure_after_multi_confirms() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let eng_id = String::from_str(&env, "ENG-REPL-STRUCT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-REPL-STRUCT",
    );

    // Confirm placement
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer-1"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);

    // Confirm first retention milestone
    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://30day-1"),
    );
    client.confirm_milestone(&company, &eng_id, &1);
    assert_eq!(token_client.balance(&recruiter), 700_000_000);

    // Request replacement
    client.request_replacement(&company, &eng_id, &String::from_str(&env, "candidate_left"));

    // Verify milestone structure: m0 (placement) resets to Pending. m1 was
    // already confirmed and paid out, so it is left untouched — request_replacement
    // never claws back a milestone that already released payment. m2 was never
    // confirmed, so it resets to Locked.
    let m0 = client.get_milestone(&eng_id, &0);
    let m1 = client.get_milestone(&eng_id, &1);
    let m2 = client.get_milestone(&eng_id, &2);

    assert_eq!(m0.status, MilestoneStatus::Pending);
    assert_eq!(m1.status, MilestoneStatus::Confirmed);
    assert_eq!(m2.status, MilestoneStatus::Locked);
    assert_eq!(m0.proof_hash, String::from_str(&env, ""));
    assert_eq!(m1.proof_hash, String::from_str(&env, "ipfs://30day-1"));
}

/// Test batch_get_engagement_summary with mixed valid and invalid IDs
#[test]
fn test_batch_get_engagement_summary_filters_invalid_ids() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "BATCH-V1",
    );
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "BATCH-V2",
    );

    let ids = vec![
        &env,
        String::from_str(&env, "BATCH-V1"),
        String::from_str(&env, "INVALID-1"),
        String::from_str(&env, "BATCH-V2"),
        String::from_str(&env, "INVALID-2"),
    ];

    let summaries = client.batch_get_engagement_summary(&ids);

    // Should only return the 2 valid engagements
    assert_eq!(summaries.len(), 2);
    assert_eq!(
        summaries.get(0).unwrap().id,
        String::from_str(&env, "BATCH-V1")
    );
    assert_eq!(
        summaries.get(1).unwrap().id,
        String::from_str(&env, "BATCH-V2")
    );
}

/// Test that proof resubmission cooldown is properly reset after dispute rejection
#[test]
fn test_proof_cooldown_reset_after_multiple_dispute_cycles() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set a short cooldown for testing
    client.set_proof_cooldown(&company, &10u32);

    let eng_id = String::from_str(&env, "ENG-COOL-MULTI");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-COOL-MULTI",
    );

    // First cycle: submit, dispute, reject
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof1"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute1"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Wait for cooldown
    advance_ledger(&env, 11);

    // Second cycle: submit, dispute, reject
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof2"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute2"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    // Wait for cooldown again
    advance_ledger(&env, 11);

    // Third attempt should succeed
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof3"),
    );

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);
    assert_eq!(m0.proof_hash, String::from_str(&env, "ipfs://proof3"));
}


/// Test that get_estimated_unlock_seconds handles edge case at exact boundary
#[test]
fn test_estimated_unlock_seconds_at_exact_unlock_time() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let eng_id = String::from_str(&env, "ENG-UNLOCK-EXACT");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-UNLOCK-EXACT",
    );

    let m1 = client.get_milestone(&eng_id, &1);
    let valid_after = m1.valid_after_ledger;

    // Advance to exactly the unlock ledger
    let ledgers_to_advance = valid_after - env.ledger().sequence();
    advance_ledger(&env, ledgers_to_advance);

    // At the exact boundary, should return 0
    let seconds = client.get_estimated_unlock_seconds(&eng_id, &1);
    assert_eq!(seconds, 0);

    // Should be unlockable
    assert!(client.is_milestone_unlockable(&eng_id, &1));
}

/// Test that dispute window closes exactly at the boundary
#[test]
#[should_panic(expected = "DisputeWindowClosed")]
fn test_dispute_window_closes_at_exact_boundary_plus_one() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_dispute_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-DW-EXACT-CLOSE");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-DW-EXACT-CLOSE",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );

    // Advance exactly to boundary + 1 (should close)
    advance_ledger(&env, 101);

    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "too_late"));
}

/// Test that token allowlist properly rejects unlisted tokens with allowlist enabled
#[test]
#[should_panic(expected = "TokenNotAllowed")]
fn test_token_allowlist_rejects_second_token_when_only_first_allowed() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Setup second token
    let token_admin2 = Address::generate(&env);
    let token_id2 = env
        .register_stellar_asset_contract_v2(token_admin2.clone())
        .address();
    let token_client2 = token::StellarAssetClient::new(&env, &token_id2);
    token_client2.mint(&company, &500_000_000_000);

    // Add only first token to allowlist and enable
    client.add_allowed_token(&company, &token_id);
    client.set_token_allowlist_enabled(&company, &true);

    // Try to create engagement with second (non-allowlisted) token
    client.create_engagement(
        &String::from_str(&env, "ENG-AL-REJECT-2"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id2,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
}


/// Test that max active per company cap prevents creation at exact limit
#[test]
#[should_panic(expected = "CompanyActiveLimitReached")]
fn test_max_active_per_company_enforces_exact_limit() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set limit to 2
    client.set_max_active_per_company(&company, &2u32);

    // Create exactly 2 engagements
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-LIMIT-1",
    );
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-LIMIT-2",
    );

    assert_eq!(client.get_company_active_count(&company), 2);

    // Third should fail at the exact limit
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-LIMIT-3",
    );
}

/// Test that multiple arbiters can vote in any order for quorum
#[test]
fn test_arbiter_votes_reach_quorum_in_different_order() {
    let (env, contract_id, token_id, company, recruiter, _) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);

    let eng_id = String::from_str(&env, "ENG-VOTE-ORDER");
    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2,
            weights: None,
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

    // Vote in reverse order: a3 first, then a1 (skipping a2)
    client.cast_arbiter_vote(&a3, &eng_id, &0, &true);
    let counts1 = client.get_arbiter_votes(&eng_id, &0);
    assert_eq!(counts1.approve_votes, 1);

    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);

    // Quorum reached (2 of 3)
    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}


/// Test that co-recruiter split handles remainder correctly with odd percentages
#[test]
fn test_co_recruiter_split_with_odd_percentage_remainder() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let co_recruiter = Address::generate(&env);

    // Use 3333 bps (33.33%) - will have remainder
    let config = EngagementConfig {
        metadata_hash: None,
        co_recruiter: Some(co_recruiter.clone()),
        recruiter_split_bps: 3_333,
        contract_pdf_hash: None,
        referrer: None,
        tags: None,
        is_public: false,
        stream_duration_ledgers: None,
        recruiter_bond_amount: None,
        bundle_id: None,
        fund_from_pool: false,
        snapshot_fee_tier: false,
        co_recruiter_bond_amount: None,
    };

    client.create_engagement(
        &String::from_str(&env, "ENG-CO-ODD"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );

    let eng_id = String::from_str(&env, "ENG-CO-ODD");
    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.confirm_milestone(&company, &eng_id, &0);

    // 300_000_000 * 3333 / 10000 = 99_990_000 (primary - floor)
    // 300_000_000 - 99_990_000 = 200_010_000 (co - gets remainder)
    let primary_balance = token_client.balance(&recruiter);
    let co_balance = token_client.balance(&co_recruiter);

    assert_eq!(primary_balance, 99_990_000);
    assert_eq!(co_balance, 200_010_000);
    assert_eq!(primary_balance + co_balance, 300_000_000);
}

/// Test that force_confirm_milestone works when called by any address after window
#[test]
fn test_force_confirm_callable_by_any_address() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    client.set_confirm_window(&company, &100u32);

    let eng_id = String::from_str(&env, "ENG-FC-ANY");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-FC-ANY",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    advance_ledger(&env, 101);

    // Random stranger can force-confirm after window
    let stranger = Address::generate(&env);
    client.force_confirm_milestone(&stranger, &eng_id, &0);

    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

/// Test that platform fee and arbiter fee stack correctly
#[test]
fn test_platform_fee_and_arbiter_fee_both_deducted() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = Address::generate(&env);

    // Set both fees: 1% platform (100 bps) and 0.5% arbiter (50 bps)
    client.set_platform_fee(&company, &100, &treasury);
    client.set_arbiter_fee(&company, &50);

    let eng_id = String::from_str(&env, "ENG-BOTH-FEES");
    create_standard_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        &arbiter,
        "ENG-BOTH-FEES",
    );

    client.submit_proof(
        &recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://proof"),
    );
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);

    // Milestone payment: 300_000_000
    // Platform fee: 300_000_000 * 100 / 10_000 = 3_000_000
    // After platform: 300_000_000 - 3_000_000 = 297_000_000
    // Arbiter fee: 297_000_000 * 50 / 10_000 = 1_485_000
    // To recruiter: 297_000_000 - 1_485_000 = 295_515_000

    assert_eq!(token_client.balance(&treasury), 3_000_000);
    assert_eq!(token_client.balance(&arbiter), 1_485_000);
    assert_eq!(token_client.balance(&recruiter), 295_515_000);
}

/// Test that get_engagements_by_company pagination works correctly at boundaries
#[test]
fn test_get_engagements_by_company_pagination_boundary() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Create exactly 10 engagements
    for i in 0..10 {
        let id = match i {
            0 => "ENG-PAGE-00",
            1 => "ENG-PAGE-01",
            2 => "ENG-PAGE-02",
            3 => "ENG-PAGE-03",
            4 => "ENG-PAGE-04",
            5 => "ENG-PAGE-05",
            6 => "ENG-PAGE-06",
            7 => "ENG-PAGE-07",
            8 => "ENG-PAGE-08",
            9 => "ENG-PAGE-09",
            _ => "ENG-PAGE-XX",
        };
        create_standard_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, id);
    }

    // Page 0, size 5: should get indices 0-4
    let page0 = client.get_engagements_by_company(&company, &0, &5);
    assert_eq!(page0.len(), 5);
    assert_eq!(page0.get(0).unwrap(), String::from_str(&env, "ENG-PAGE-00"));
    assert_eq!(page0.get(4).unwrap(), String::from_str(&env, "ENG-PAGE-04"));

    // Page 1, size 5: should get indices 5-9
    let page1 = client.get_engagements_by_company(&company, &1, &5);
    assert_eq!(page1.len(), 5);
    assert_eq!(page1.get(0).unwrap(), String::from_str(&env, "ENG-PAGE-05"));
    assert_eq!(page1.get(4).unwrap(), String::from_str(&env, "ENG-PAGE-09"));

    // Page 2, size 5: should get empty (no more items)
    let page2 = client.get_engagements_by_company(&company, &2, &5);
    assert_eq!(page2.len(), 0);
}

/// Test that milestone percentage sum validation rejects 99% or 101%
#[test]
#[should_panic(expected = "milestone percentages must sum to 100")]
fn test_create_engagement_rejects_99_percent_total() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let bad_milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "M1"),
            payment_percent: 49,
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
            payment_percent: 50,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
            prerequisites: Vec::new(&env),
        },
    ];

    client.create_engagement(
        &String::from_str(&env, "ENG-99PCT"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &bad_milestones,
        &vec![&env],
        &default_config(),
    );
}

// ============================================================
// remove_fee_tier — admin can delete a single fee tier
// ============================================================

/// Admin can remove a single fee tier by threshold without replacing the whole list.
#[test]
fn test_remove_fee_tier_deletes_single_tier() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set up three tiers
    let tiers = vec![
        &env,
        FeeTier {
            threshold: 1_000_000,
            bps: 400,
        },
        FeeTier {
            threshold: 10_000_000,
            bps: 300,
        },
        FeeTier {
            threshold: 100_000_000,
            bps: 200,
        },
    ];
    client.set_platform_fee(&company, &500u32, &company);
    client.set_fee_tiers(&company, &tiers);
    assert_eq!(client.get_fee_tiers().len(), 3);

    // Remove the middle tier
    client.remove_fee_tier(&company, &10_000_000);

    // Should have 2 tiers left
    let remaining = client.get_fee_tiers();
    assert_eq!(remaining.len(), 2);
    assert_eq!(remaining.get(0).unwrap().threshold, 1_000_000);
    assert_eq!(remaining.get(0).unwrap().bps, 400);
    assert_eq!(remaining.get(1).unwrap().threshold, 100_000_000);
    assert_eq!(remaining.get(1).unwrap().bps, 200);
}

/// Removing a non-existent tier should panic.
#[test]
#[should_panic(expected = "fee tier not found")]
fn test_remove_fee_tier_nonexistent_panics() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set up one tier
    let tiers = vec![
        &env,
        FeeTier {
            threshold: 1_000_000,
            bps: 400,
        },
    ];
    client.set_platform_fee(&company, &500u32, &company);
    client.set_fee_tiers(&company, &tiers);

    // Try to remove a tier that doesn't exist
    client.remove_fee_tier(&company, &5_000_000);
}

/// Removing the only tier should result in an empty list (flat fee).
#[test]
fn test_remove_fee_tier_last_tier_empties_list() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set up one tier
    let tiers = vec![
        &env,
        FeeTier {
            threshold: 1_000_000,
            bps: 400,
        },
    ];
    client.set_platform_fee(&company, &500u32, &company);
    client.set_fee_tiers(&company, &tiers);
    assert_eq!(client.get_fee_tiers().len(), 1);

    // Remove it
    client.remove_fee_tier(&company, &1_000_000);

    // Should be empty now
    assert_eq!(client.get_fee_tiers().len(), 0);
}

/// Non-admin cannot remove a fee tier.
#[test]
#[should_panic(expected = "unauthorized")]
fn test_remove_fee_tier_non_admin_rejected() {
    let (env, contract_id, _token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Set up one tier as admin
    let tiers = vec![
        &env,
        FeeTier {
            threshold: 1_000_000,
            bps: 400,
        },
    ];
    client.set_platform_fee(&company, &500u32, &company);
    client.set_fee_tiers(&company, &tiers);

    // Non-admin tries to remove
    client.remove_fee_tier(&recruiter, &1_000_000);
}

/// Removing a tier from an empty list should panic.
#[test]
#[should_panic(expected = "fee tier not found")]
fn test_remove_fee_tier_from_empty_list_panics() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // No tiers set - try to remove one
    client.remove_fee_tier(&company, &1_000_000);
}

// ============================================================
// ISSUE #465 — RECRUITER NO-SHOW PENALTY
// ============================================================

fn create_short_retention_engagement(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
    id: &str,
    config: &EngagementConfig,
) -> String {
    client.create_engagement(
        &String::from_str(env, id),
        company,
        recruiter,
        &ArbiterSetup {
            arbiters: vec![env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Engineer"),
        &build_milestones(env),
        &vec![env, 1u32, 2u32],
        config,
    )
}

#[test]
#[should_panic(expected = "NoShowDisabled")]
fn test_no_show_disabled_by_default() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "NS-1", &default_config(),
    );
    assert_eq!(client.get_no_show_deadline_ledgers(), 0);
    advance_ledger(&env, 1_000_000);
    client.trigger_no_show(&id, &0);
}

#[test]
#[should_panic(expected = "NoShowDeadlineNotReached")]
fn test_no_show_panics_before_deadline() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_no_show_deadline_ledgers(&company, &100);
    assert_eq!(client.get_no_show_deadline_ledgers(), 100);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "NS-2", &default_config(),
    );
    // Exactly at unlocked_at + deadline is still too early.
    advance_ledger(&env, 100);
    client.trigger_no_show(&id, &0);
}

#[test]
#[should_panic(expected = "milestone is not pending")]
fn test_no_show_panics_on_submitted_milestone() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_no_show_deadline_ledgers(&company, &100);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "NS-3", &default_config(),
    );
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "QmProof"));
    advance_ledger(&env, 101);
    client.trigger_no_show(&id, &0);
}

#[test]
#[should_panic(expected = "only placement milestones can be forfeited")]
fn test_no_show_panics_on_retention_milestone() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_no_show_deadline_ledgers(&company, &100);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "NS-4", &default_config(),
    );
    advance_ledger(&env, LEDGERS_PER_DAY + 101);
    client.unlock_milestone(&id, &1);
    client.trigger_no_show(&id, &1);
}

#[test]
fn test_no_show_forfeits_share_to_company_refund_path() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    client.set_no_show_deadline_ledgers(&company, &100);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "NS-5", &default_config(),
    );
    let company_before = token_client.balance(&company);

    advance_ledger(&env, 101);
    client.trigger_no_show(&id, &0);

    assert!(has_event(&env, "milestone_no_show"));
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Resolved);
    // Nothing was paid to the recruiter or counted as released.
    assert_eq!(client.get_total_released(&id), 0);
    assert_eq!(client.get_unlock_progress(&id), (1, 3));
    assert_eq!(token_client.balance(&recruiter), 0);

    // Cancelling returns the full escrow, forfeited share included.
    client.cancel_engagement(&company, &recruiter, &id);
    assert_eq!(token_client.balance(&company), company_before + 1_000_000_000);
    assert_eq!(token_client.balance(&contract_id), 0);
}

#[test]
fn test_no_show_share_excluded_from_recruiter_payout_on_completion() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    client.set_no_show_deadline_ledgers(&company, &100);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "NS-6", &default_config(),
    );
    let company_before = token_client.balance(&company);

    advance_ledger(&env, 101);
    client.trigger_no_show(&id, &0);

    // A forfeited milestone does not block later milestones.
    advance_ledger(&env, LEDGERS_PER_DAY);
    client.unlock_milestone(&id, &1);
    client.submit_proof(&recruiter, &id, &1, &String::from_str(&env, "QmRet1"));
    client.confirm_milestone(&company, &id, &1);
    advance_ledger(&env, LEDGERS_PER_DAY);
    client.unlock_milestone(&id, &2);
    client.submit_proof(&recruiter, &id, &2, &String::from_str(&env, "QmRet2"));
    client.confirm_milestone(&company, &id, &2);

    assert_eq!(client.get_engagement(&id).status, EngagementStatus::Completed);
    assert_eq!(client.get_total_released(&id), 700_000_000);
    assert_eq!(token_client.balance(&recruiter), 700_000_000);
    // The forfeited 30% came back to the company on completion.
    assert_eq!(token_client.balance(&company), company_before + 300_000_000);
    assert_eq!(token_client.balance(&contract_id), 0);
}

#[test]
fn test_no_show_clock_restarts_after_rejected_dispute() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_no_show_deadline_ledgers(&company, &100);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "NS-7", &default_config(),
    );
    advance_ledger(&env, 90);
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "QmProof"));
    client.raise_dispute(&company, &id, &0, &String::from_str(&env, "bad"));
    client.cast_arbiter_vote(&arbiter, &id, &0, &false);
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Pending);

    // Past the original deadline but not the restarted one.
    advance_ledger(&env, 50);
    let early = client.try_trigger_no_show(&id, &0);
    assert!(early.is_err());

    advance_ledger(&env, 51);
    client.trigger_no_show(&id, &0);
    assert_eq!(client.get_milestone(&id, &0).status, MilestoneStatus::Resolved);
}

// ============================================================
// ISSUE #466 — STREAMING MILESTONE PAYOUT
// ============================================================

fn stream_config(duration: u32) -> EngagementConfig {
    let mut config = default_config();
    config.stream_duration_ledgers = Some(duration);
    config
}

#[test]
fn test_unstreamed_milestone_pays_lump_sum() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ST-1", &default_config(),
    );
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "QmProof"));
    client.confirm_milestone(&company, &id, &0);

    assert_eq!(token_client.balance(&recruiter), 300_000_000);
    assert_eq!(client.get_streamed_payout_status(&id, &0), (0, 0));
}

#[test]
fn test_streamed_milestone_vests_linearly() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ST-2", &stream_config(1_000),
    );
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "QmProof"));
    client.confirm_milestone(&company, &id, &0);

    // Confirmed and counted as released, but nothing transferred yet.
    assert_eq!(client.get_total_released(&id), 300_000_000);
    assert_eq!(token_client.balance(&recruiter), 0);
    assert_eq!(client.get_streamed_payout_status(&id, &0), (0, 300_000_000));

    // No ledgers elapsed: zero, no panic.
    assert_eq!(client.claim_streamed_payout(&recruiter, &id, &0), 0);

    advance_ledger(&env, 500);
    assert_eq!(client.claim_streamed_payout(&recruiter, &id, &0), 150_000_000);
    assert_eq!(token_client.balance(&recruiter), 150_000_000);

    advance_ledger(&env, 600);
    assert_eq!(client.claim_streamed_payout(&recruiter, &id, &0), 150_000_000);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
    assert_eq!(
        client.get_streamed_payout_status(&id, &0),
        (300_000_000, 300_000_000)
    );
    assert_eq!(client.claim_streamed_payout(&recruiter, &id, &0), 0);
}

#[test]
fn test_streamed_payout_rounds_down_and_pays_exact_remainder() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ST-3", &stream_config(7),
    );
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "QmProof"));
    client.confirm_milestone(&company, &id, &0);

    // 300_000_000 * 3 / 7 = 128_571_428.57… → rounds down.
    advance_ledger(&env, 3);
    assert_eq!(client.claim_streamed_payout(&recruiter, &id, &0), 128_571_428);

    advance_ledger(&env, 4);
    assert_eq!(client.claim_streamed_payout(&recruiter, &id, &0), 171_428_572);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_streamed_payout_is_permissionless_but_pays_recruiter() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ST-4", &stream_config(10),
    );
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "QmProof"));
    client.confirm_milestone(&company, &id, &0);
    advance_ledger(&env, 10);

    env.set_auths(&[]);
    client.claim_streamed_payout(&recruiter, &id, &0);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_streamed_payout_rejects_non_recruiter() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let id = create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ST-5", &stream_config(10),
    );
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "QmProof"));
    client.confirm_milestone(&company, &id, &0);
    client.claim_streamed_payout(&arbiter, &id, &0);
}

#[test]
#[should_panic(expected = "InvalidStreamDuration")]
fn test_zero_stream_duration_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_short_retention_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ST-6", &stream_config(0),
    );
}

// ============================================================
// ISSUE #467 — RANDOM ARBITER PANEL FROM POOL
// ============================================================

fn create_random_panel_engagement(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    id: &str,
    panel_size: u32,
    quorum: u32,
) -> String {
    client.create_engagement_random_panel(
        &String::from_str(env, id),
        company,
        recruiter,
        &RandomArbiterSetup { panel_size, quorum },
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Engineer"),
        &build_milestones(env),
        &vec![env, 30u32, 90u32],
        &default_config(),
    )
}

#[test]
fn test_arbiter_pool_add_remove() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let a = Address::generate(&env);
    let b = Address::generate(&env);
    client.add_arbiter_pool_member(&company, &a);
    client.add_arbiter_pool_member(&company, &b);
    assert_eq!(client.get_arbiter_pool(), vec![&env, a.clone(), b.clone()]);
    client.remove_arbiter_pool_member(&company, &a);
    assert_eq!(client.get_arbiter_pool(), vec![&env, b]);
    assert!(client.try_add_arbiter_pool_member(&company, &Address::generate(&env)).is_ok());
    assert!(client.try_remove_arbiter_pool_member(&company, &a).is_err());
}

#[test]
#[should_panic(expected = "AlreadyInPool")]
fn test_arbiter_pool_rejects_duplicate() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let a = Address::generate(&env);
    client.add_arbiter_pool_member(&company, &a);
    client.add_arbiter_pool_member(&company, &a);
}

#[test]
fn test_random_panel_is_distinct_and_drawn_from_pool() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    for _ in 0..6 {
        client.add_arbiter_pool_member(&company, &Address::generate(&env));
    }
    // A removed member must never be drawn afterwards.
    let removed = client.get_arbiter_pool().get(0).unwrap();
    client.remove_arbiter_pool_member(&company, &removed);
    let pool = client.get_arbiter_pool();

    for n in 0..10u32 {
        let id = std::format!("RP-{}", n);
        let id = create_random_panel_engagement(
            &env, &client, &token_id, &company, &recruiter, &id, 3, 2,
        );
        assert!(has_event(&env, "arbiters_drawn"));
        let engagement = client.get_engagement(&id);
        assert_eq!(engagement.arbiters.len(), 3);
        assert_eq!(engagement.quorum, 2);
        for i in 0..engagement.arbiters.len() {
            let a = engagement.arbiters.get(i).unwrap();
            assert!(pool.contains(&a));
            assert!(a != removed);
            for j in (i + 1)..engagement.arbiters.len() {
                assert!(a != engagement.arbiters.get(j).unwrap());
            }
        }
    }
}

#[test]
#[should_panic(expected = "ArbiterPoolTooSmall")]
fn test_random_panel_panics_when_pool_too_small() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.add_arbiter_pool_member(&company, &Address::generate(&env));
    client.add_arbiter_pool_member(&company, &Address::generate(&env));
    create_random_panel_engagement(&env, &client, &token_id, &company, &recruiter, "RP-X", 3, 1);
}

#[test]
fn test_random_panel_never_draws_engagement_parties() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let a = Address::generate(&env);
    client.add_arbiter_pool_member(&company, &company);
    client.add_arbiter_pool_member(&company, &a);
    client.add_arbiter_pool_member(&company, &recruiter);

    let id = create_random_panel_engagement(&env, &client, &token_id, &company, &recruiter, "RP-P", 1, 1);
    assert_eq!(client.get_engagement(&id).arbiters, vec![&env, a]);

    let too_big = client.try_create_engagement_random_panel(
        &String::from_str(&env, "RP-Q"),
        &company,
        &recruiter,
        &RandomArbiterSetup { panel_size: 2, quorum: 1 },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
    assert!(too_big.is_err());
}

// ============================================================
// ISSUE #468 — RESPONSE-TIME-WEIGHTED PANEL BIAS
// ============================================================

/// Gives `good` a fast, complete voting record and `bad` a dispute it never
/// voted on.
fn build_arbiter_track_records(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    good: &Address,
    bad: &Address,
) {
    let id = String::from_str(env, "TRACK-1");
    client.create_engagement(
        &id,
        company,
        recruiter,
        &ArbiterSetup {
            arbiters: vec![env, good.clone(), bad.clone()],
            quorum: 1,
            weights: None,
        },
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Engineer"),
        &build_milestones(env),
        &vec![env, 30u32, 90u32],
        &default_config(),
    );
    client.submit_proof(recruiter, &id, &0, &String::from_str(env, "QmProof"));
    client.raise_dispute(company, &id, &0, &String::from_str(env, "check"));
    advance_ledger(env, 10);
    client.cast_arbiter_vote(good, &id, &0, &true);
}

#[test]
fn test_arbiter_selection_weight_reflects_track_record() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let good = Address::generate(&env);
    let bad = Address::generate(&env);
    let newbie = Address::generate(&env);

    assert_eq!(client.get_arbiter_selection_weight(&newbie), 50);
    build_arbiter_track_records(&env, &client, &token_id, &company, &recruiter, &good, &bad);

    let good_stats = client.get_arbiter_stats(&good).unwrap();
    assert_eq!(good_stats.disputes_assigned, 1);
    assert_eq!(good_stats.votes_cast, 1);
    assert_eq!(good_stats.total_response_ledgers, 10);
    // completion 100, speed 100 * 17_280 / 17_290 = 99.
    assert_eq!(client.get_arbiter_selection_weight(&good), 99);
    // Never voted: floored at 1, still eligible.
    assert_eq!(client.get_arbiter_selection_weight(&bad), 1);
    assert_eq!(client.get_arbiter_selection_weight(&newbie), 50);
}

#[test]
fn test_weighted_draw_favours_strong_track_record() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let good = Address::generate(&env);
    let bad = Address::generate(&env);
    let newbie = Address::generate(&env);
    build_arbiter_track_records(&env, &client, &token_id, &company, &recruiter, &good, &bad);

    let pool = vec![&env, bad.clone(), newbie.clone(), good.clone()];
    let (mut good_n, mut bad_n, mut newbie_n) = (0u32, 0u32, 0u32);
    env.cost_estimate().budget().reset_unlimited();
    // The test Env seeds its PRNG with a fixed seed, so this is deterministic.
    env.as_contract(&contract_id, || {
        for _ in 0..1_500 {
            let panel =
                HireSettleContract::draw_arbiter_panel(&env, &pool, 1, &company, &recruiter);
            let drawn = panel.get(0).unwrap();
            if drawn == good {
                good_n += 1;
            } else if drawn == bad {
                bad_n += 1;
            } else {
                newbie_n += 1;
            }
        }
    });

    // Weights 99 / 50 / 1 → expected ≈ 990 / 500 / 10 of 1 500.
    assert!(good_n > 850, "good drawn {} times", good_n);
    assert!(newbie_n > 350, "newbie drawn {} times", newbie_n);
    assert!(bad_n < 50, "bad drawn {} times", bad_n);
    assert!(good_n > newbie_n && newbie_n > bad_n);
}

// ============================================================
// ADMIN CONFIG — VERSION, FEE WAIVER, TOKEN MIN AMOUNT, REFERRERS
// ============================================================

#[test]
fn test_get_version_returns_default_when_unset() {
    let (env, contract_id, _token_id, _company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    assert_eq!(client.get_version(), String::from_str(&env, "0.2.0"));
}

#[test]
fn test_set_version_updates_version_and_emits_event() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_version(&company, &String::from_str(&env, "1.0.0"));

    assert!(has_event(&env, "version_set"));
    assert_eq!(client.get_version(), String::from_str(&env, "1.0.0"));
}

#[test]
#[should_panic(expected = "VersionTooLong")]
fn test_set_version_rejects_string_over_32_chars() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // 33 characters — one over MAX_VERSION_LENGTH.
    client.set_version(
        &company,
        &String::from_str(&env, "123456789012345678901234567890123"),
    );
}

#[test]
fn test_waive_platform_fee_marks_engagement_waived() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-WAIVE",
    );
    let id = String::from_str(&env, "ENG-WAIVE");

    assert!(!client.is_fee_waived(&id));
    client.waive_platform_fee(&company, &id);

    assert!(has_event(&env, "platform_fee_waived"));
    assert!(client.is_fee_waived(&id));
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_waive_platform_fee_non_admin_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-WAIVE-2",
    );

    client.waive_platform_fee(&recruiter, &String::from_str(&env, "ENG-WAIVE-2"));
}

#[test]
fn test_token_min_amount_override_takes_precedence_over_global() {
    let (env, contract_id, token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let other_token = Address::generate(&env);

    assert_eq!(client.get_token_min_amount(&token_id), None);
    assert_eq!(client.get_effective_min_amount(&token_id), client.get_min_amount());

    client.set_token_min_amount(&company, &token_id, &5_000_000);

    assert_eq!(client.get_token_min_amount(&token_id), Some(5_000_000));
    assert_eq!(client.get_effective_min_amount(&token_id), 5_000_000);
    // A token without an override still falls back to the global floor.
    assert_eq!(
        client.get_effective_min_amount(&other_token),
        client.get_min_amount()
    );
}

#[test]
#[should_panic(expected = "InvalidMinAmount")]
fn test_set_token_min_amount_rejects_non_positive() {
    let (env, contract_id, token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_token_min_amount(&company, &token_id, &0);
}

#[test]
fn test_remove_token_min_amount_falls_back_to_global() {
    let (env, contract_id, token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.set_token_min_amount(&company, &token_id, &5_000_000);
    client.remove_token_min_amount(&company, &token_id);

    assert!(has_event(&env, "token_min_amount_removed"));
    assert_eq!(client.get_token_min_amount(&token_id), None);
    assert_eq!(client.get_effective_min_amount(&token_id), client.get_min_amount());
}

#[test]
fn test_add_and_remove_referrer() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let ref_a = Address::generate(&env);
    let ref_b = Address::generate(&env);

    assert_eq!(client.get_referrers().len(), 0);

    client.add_referrer(&company, &ref_a);
    client.add_referrer(&company, &ref_b);
    assert_eq!(client.get_referrers(), vec![&env, ref_a.clone(), ref_b.clone()]);

    client.remove_referrer(&company, &ref_a);
    assert!(has_event(&env, "referrer_removed"));
    assert_eq!(client.get_referrers(), vec![&env, ref_b]);
}

#[test]
#[should_panic(expected = "referrer already exists")]
fn test_add_referrer_rejects_duplicate() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let referrer = Address::generate(&env);

    client.add_referrer(&company, &referrer);
    client.add_referrer(&company, &referrer);
}

// ============================================================
// ISSUES #472–#475
// ============================================================

#[test]
fn test_pool_deposit_create_withdraw_roundtrip() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let deposit = 5_000_000_000i128;
    client.deposit_company_balance(&company, &token_id, &deposit);
    assert_eq!(client.get_company_balance(&company, &token_id), deposit);

    let mut config = default_config();
    config.fund_from_pool = true;
    let amount = 1_000_000_000i128;
    client.create_engagement(
        &String::from_str(&env, "POOL-1"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &amount,
        &String::from_str(&env, "Pooled Role"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );

    assert_eq!(client.get_company_balance(&company, &token_id), deposit - amount);
    assert_eq!(token_client.balance(&contract_id), deposit); // deposit still held; engagement drew from pool accounting

    let remainder = deposit - amount;
    client.withdraw_company_balance(&company, &token_id, &remainder);
    assert_eq!(client.get_company_balance(&company, &token_id), 0);
}

#[test]
#[should_panic(expected = "InsufficientCompanyBalance")]
fn test_pool_funded_create_insufficient_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.deposit_company_balance(&company, &token_id, &500_000_000);
    let before = client.get_company_balance(&company, &token_id);

    let mut config = default_config();
    config.fund_from_pool = true;
    let _ = client.create_engagement(
        &String::from_str(&env, "POOL-INSUF"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Too Big"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &config,
    );
    // unreachable — but if it somehow succeeded the balance must be untouched
    let _ = before;
}

#[test]
fn test_pool_insufficient_does_not_consume_balance() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    client.deposit_company_balance(&company, &token_id, &500_000_000);
    assert_eq!(client.get_company_balance(&company, &token_id), 500_000_000);

    let mut config = default_config();
    config.fund_from_pool = true;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        client.create_engagement(
            &String::from_str(&env, "POOL-SAFE"),
            &company,
            &recruiter,
            &ArbiterSetup {
                arbiters: vec![&env, arbiter.clone()],
                quorum: 1,
                weights: None,
            },
            &token_id,
            &1_000_000_000,
            &String::from_str(&env, "Too Big"),
            &build_milestones(&env),
            &vec![&env, 30u32, 90u32],
            &config,
        );
    }));
    assert!(result.is_err());
    assert_eq!(client.get_company_balance(&company, &token_id), 500_000_000);
}

#[test]
fn test_non_pool_funded_create_still_transfers() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    let before = token_client.balance(&company);
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "POOL-OFF",
    );
    assert_eq!(token_client.balance(&company), before - 1_000_000_000);
    assert_eq!(client.get_company_balance(&company, &token_id), 0);
}

#[test]
fn test_cosigner_default_off_setters_apply_immediately() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let treasury = Address::generate(&env);

    assert!(client.get_config_cosigner().is_none());
    client.set_platform_fee(&company, &250, &treasury);
    assert_eq!(client.get_platform_fee(), (250, treasury));
}

#[test]
fn test_sensitive_setter_requires_cosigner_accept() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let cosigner = Address::generate(&env);
    let treasury = Address::generate(&env);
    let treasury2 = Address::generate(&env);

    client.set_platform_fee(&company, &100, &treasury);
    client.set_config_cosigner(&company, &Some(cosigner.clone()));
    client.set_sensitive_functions(&company, &vec![&env, FN_SET_PLATFORM_FEE]);

    client.set_platform_fee(&company, &250, &treasury2);
    // Live value unchanged until cosigner accepts.
    assert_eq!(client.get_platform_fee(), (100, treasury.clone()));

    let pending = client.get_pending_config_change(&1u64).unwrap();
    assert_eq!(pending.fn_id, FN_SET_PLATFORM_FEE);
    assert_eq!(pending.u32_val, 250);

    client.accept_config_change(&cosigner, &1u64);
    assert_eq!(client.get_platform_fee(), (250, treasury2));
}

#[test]
fn test_non_sensitive_setter_applies_with_cosigner() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let cosigner = Address::generate(&env);

    client.set_config_cosigner(&company, &Some(cosigner));
    // Only platform fee marked sensitive — min_amount stays immediate.
    client.set_sensitive_functions(&company, &vec![&env, FN_SET_PLATFORM_FEE]);
    client.set_min_amount(&company, &200_000);
    assert_eq!(client.get_min_amount(), 200_000);
}

#[test]
fn test_emergency_pause_threshold_and_duplicate() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);
    let s3 = Address::generate(&env);

    client.set_emergency_signers(&company, &vec![&env, s1.clone(), s2.clone(), s3.clone()], &2);
    assert!(!client.is_paused());

    client.cast_emergency_pause_vote(&s1, &None);
    assert!(!client.is_paused());

    client.cast_emergency_pause_vote(&s2, &None);
    assert!(client.is_paused());

    // unpause remains admin-only
    client.unpause(&company);
    assert!(!client.is_paused());
}

#[test]
#[should_panic(expected = "already voted")]
fn test_emergency_duplicate_vote_rejected() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);

    client.set_emergency_signers(&company, &vec![&env, s1.clone(), s2.clone()], &2);
    client.cast_emergency_pause_vote(&s1, &None);
    client.cast_emergency_pause_vote(&s1, &None);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_emergency_cannot_unpause() {
    let (env, contract_id, _token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let s1 = Address::generate(&env);
    let s2 = Address::generate(&env);

    client.set_emergency_signers(&company, &vec![&env, s1.clone(), s2.clone()], &2);
    client.cast_emergency_pause_vote(&s1, &None);
    client.cast_emergency_pause_vote(&s2, &None);
    assert!(client.is_paused());
    // Signer trying to unpause must fail.
    client.unpause(&s1);
}

#[test]
fn test_fee_rebate_default_zero_unchanged() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = Address::generate(&env);

    assert_eq!(client.get_fee_rebate_bps(), 0);
    client.set_platform_fee(&company, &250, &treasury); // 2.5%
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "REBATE-0",
    );
    client.submit_proof(
        &recruiter,
        &String::from_str(&env, "REBATE-0"),
        &0,
        &String::from_str(&env, "QmProof"),
    );
    client.confirm_milestone(&company, &String::from_str(&env, "REBATE-0"), &0);

    // 30% of 1e9 = 3e8; 2.5% fee = 7_500_000
    assert_eq!(token_client.balance(&treasury), 7_500_000);
    assert_eq!(client.get_company_rebate_balance(&company, &token_id), 0);
}

#[test]
fn test_fee_rebate_credits_and_offsets() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = Address::generate(&env);

    client.set_platform_fee(&company, &1000.min(MAX_PLATFORM_FEE_BPS), &treasury); // use max 500 = 5%
    client.set_platform_fee(&company, &500, &treasury); // 5%
    client.set_fee_rebate_bps(&company, &2000.min(MAX_PLATFORM_FEE_BPS)); // wait max is 500
    client.set_fee_rebate_bps(&company, &500); // 5% of fee goes to rebate

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "REBATE-1",
    );
    let id = String::from_str(&env, "REBATE-1");
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "Qm1"));
    client.confirm_milestone(&company, &id, &0);

    // payment 300_000_000, fee 5% = 15_000_000
    // rebate credit = 15_000_000 * 500 / 10000 = 750_000
    // treasury gets 14_250_000
    assert_eq!(token_client.balance(&treasury), 14_250_000);
    assert_eq!(client.get_company_rebate_balance(&company, &token_id), 750_000);

    // Unlock + confirm next milestone to consume rebate offset.
    advance_ledger(&env, 30 * LEDGERS_PER_DAY);
    client.unlock_milestone(&id, &1);
    client.submit_proof(&recruiter, &id, &1, &String::from_str(&env, "Qm2"));
    client.confirm_milestone(&company, &id, &1);

    // payment 400_000_000, fee 5% = 20_000_000
    // offset 750_000 from rebate → remaining 19_250_000
    // credit = 19_250_000 * 500 / 10000 = 962_500
    // treasury += 19_250_000 - 962_500 = 18_287_500
    // total treasury = 14_250_000 + 18_287_500 = 32_537_500
    // rebate balance = 0 - 750_000 + 962_500 = 962_500
    assert_eq!(token_client.balance(&treasury), 32_537_500);
    assert_eq!(client.get_company_rebate_balance(&company, &token_id), 962_500);
}

#[test]
fn test_redeem_company_rebate() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = Address::generate(&env);

    client.set_platform_fee(&company, &500, &treasury);
    client.set_fee_rebate_bps(&company, &500);
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "REBATE-R",
    );
    let id = String::from_str(&env, "REBATE-R");
    client.submit_proof(&recruiter, &id, &0, &String::from_str(&env, "Qm"));
    client.confirm_milestone(&company, &id, &0);

    let rebate = client.get_company_rebate_balance(&company, &token_id);
    assert!(rebate > 0);
    let before = token_client.balance(&company);
    client.redeem_company_rebate(&company, &token_id, &rebate);
    assert_eq!(client.get_company_rebate_balance(&company, &token_id), 0);
    assert_eq!(token_client.balance(&company), before + rebate);
}

#[test]
#[should_panic(expected = "InsufficientRebateBalance")]
fn test_redeem_company_rebate_over_balance() {
    let (env, contract_id, token_id, company, _recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.redeem_company_rebate(&company, &token_id, &1);
}

// === AUTO #457/#471/#476/#477 TESTS ===

// ============================================================
// #457 — global × per-engagement pause interaction matrix
// ============================================================

#[test]
fn test_pause_matrix_both_off_confirm_succeeds() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSE-OK");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PAUSE-OK",
    );
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    assert!(!client.is_paused());
    assert!(!client.is_engagement_paused(&eng_id));
    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(
        client.get_engagement(&eng_id).milestones.get(0).unwrap().status,
        MilestoneStatus::Confirmed
    );
}

#[test]
#[should_panic(expected = "EngagementPaused")]
fn test_pause_matrix_engagement_on_confirm_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSE-ENG");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PAUSE-ENG",
    );
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.pause_engagement(&company, &eng_id, &String::from_str(&env, "quarantine"));
    client.confirm_milestone(&company, &eng_id, &0);
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_pause_matrix_global_on_confirm_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSE-GLO");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PAUSE-GLO",
    );
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.pause(&company);
    client.confirm_milestone(&company, &eng_id, &0);
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_pause_matrix_both_on_global_error_first() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSE-BOTH");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PAUSE-BOTH",
    );
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    client.pause(&company);
    client.pause_engagement(&company, &eng_id, &String::from_str(&env, "quarantine"));
    client.confirm_milestone(&company, &eng_id, &0);
}

#[test]
#[should_panic(expected = "EngagementPaused")]
fn test_pause_matrix_readme_worked_example_stays_quarantined() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-PAUSE-EX");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PAUSE-EX",
    );
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://offer"));
    // pause_engagement → pause → unpause leaves engagement quarantined
    client.pause_engagement(&company, &eng_id, &String::from_str(&env, "quarantine"));
    client.pause(&company);
    client.unpause(&company);
    assert!(!client.is_paused());
    assert!(client.is_engagement_paused(&eng_id));
    client.confirm_milestone(&company, &eng_id, &0);
}

// ============================================================
// #471 — co-recruiter split renegotiation
// ============================================================

#[test]
fn test_split_amendment_applies_to_future_not_past() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let co = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-SPLIT-AMD");

    client.create_engagement(
        &eng_id,
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &EngagementConfig {
            metadata_hash: None,
            co_recruiter: Some(co.clone()),
            recruiter_split_bps: 6_000,
            contract_pdf_hash: None,
            referrer: None,
            tags: None,
            is_public: false,
            stream_duration_ledgers: None,
            recruiter_bond_amount: None,
            bundle_id: None,
            fund_from_pool: false,
            snapshot_fee_tier: false,
            co_recruiter_bond_amount: None,
        },
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://a"));
    client.confirm_milestone(&company, &eng_id, &0);
    let after_first_recruiter = token_client.balance(&recruiter);
    let after_first_co = token_client.balance(&co);
    assert_eq!(after_first_recruiter, 180_000_000);
    assert_eq!(after_first_co, 120_000_000);

    client.propose_split_amendment(&recruiter, &eng_id, &5_000);
    client.accept_split_amendment(&co, &eng_id);
    assert_eq!(client.get_engagement(&eng_id).recruiter_split_bps, 5_000);
    let log = client.get_split_amendment_log(&eng_id);
    assert_eq!(log.len(), 1);
    assert_eq!(log.get(0).unwrap().old_split_bps, 6_000);
    assert_eq!(log.get(0).unwrap().new_split_bps, 5_000);

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
    client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://b"));
    client.confirm_milestone(&company, &eng_id, &1);
    assert_eq!(token_client.balance(&recruiter) - after_first_recruiter, 200_000_000);
    assert_eq!(token_client.balance(&co) - after_first_co, 200_000_000);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_split_amendment_proposer_cannot_accept() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let co = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-SPLIT-SELF");
    client.create_engagement(
        &eng_id, &company, &recruiter,
        &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1, weights: None },
        &token_id, &1_000_000_000, &String::from_str(&env, "Engineer"),
        &build_milestones(&env), &vec![&env, 30u32, 90u32],
        &EngagementConfig {
            metadata_hash: None, co_recruiter: Some(co.clone()), recruiter_split_bps: 7_000,
            contract_pdf_hash: None, referrer: None, tags: None, is_public: false,
            stream_duration_ledgers: None, recruiter_bond_amount: None, bundle_id: None,
            fund_from_pool: false,
            snapshot_fee_tier: false,
            co_recruiter_bond_amount: None,
        },
    );
    client.propose_split_amendment(&recruiter, &eng_id, &5_000);
    client.accept_split_amendment(&recruiter, &eng_id);
}

#[test]
#[should_panic(expected = "amendment_expired")]
fn test_split_amendment_expires() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let co = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-SPLIT-TTL");
    client.create_engagement(
        &eng_id, &company, &recruiter,
        &ArbiterSetup { arbiters: vec![&env, arbiter.clone()], quorum: 1, weights: None },
        &token_id, &1_000_000_000, &String::from_str(&env, "Engineer"),
        &build_milestones(&env), &vec![&env, 30u32, 90u32],
        &EngagementConfig {
            metadata_hash: None, co_recruiter: Some(co.clone()), recruiter_split_bps: 7_000,
            contract_pdf_hash: None, referrer: None, tags: None, is_public: false,
            stream_duration_ledgers: None, recruiter_bond_amount: None, bundle_id: None,
            fund_from_pool: false,
            snapshot_fee_tier: false,
            co_recruiter_bond_amount: None,
        },
    );
    client.set_amendment_ttl(&company, &100);
    client.propose_split_amendment(&recruiter, &eng_id, &5_000);
    advance_ledger(&env, 101);
    assert!(client.get_pending_split_amendment(&eng_id).is_none());
    client.accept_split_amendment(&co, &eng_id);
}

// ============================================================
// #476 — recruiter verification badge
// ============================================================

#[test]
fn test_recruiter_verified_flag_defaults_and_lifecycle() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let other = Address::generate(&env);

    assert!(!client.is_recruiter_verified(&recruiter));
    assert!(!client.is_recruiter_verified(&other));

    client.set_recruiter_verified(&company, &recruiter, &true);
    assert!(client.is_recruiter_verified(&recruiter));

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-VERIFIED",
    );
    create_standard_engagement(
        &env, &client, &token_id, &company, &other, &arbiter, "ENG-UNVERIFIED",
    );
    assert_eq!(
        client.get_engagement(&String::from_str(&env, "ENG-VERIFIED")).status,
        EngagementStatus::Active
    );
    assert_eq!(
        client.get_engagement(&String::from_str(&env, "ENG-UNVERIFIED")).status,
        EngagementStatus::Active
    );

    client.set_recruiter_verified(&company, &recruiter, &false);
    assert!(!client.is_recruiter_verified(&recruiter));
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_non_admin_cannot_set_recruiter_verified() {
    let (env, contract_id, _token_id, _company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    client.set_recruiter_verified(&recruiter, &arbiter, &true);
}

// ============================================================
// #477 — arbiter self-recusal
// ============================================================

#[test]
#[should_panic(expected = "ArbiterRecused")]
fn test_recused_arbiter_cannot_vote() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RECUSE-VOTE");
    client.create_engagement(
        &eng_id, &company, &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2, weights: None,
        },
        &token_id, &1_000_000_000, &String::from_str(&env, "Engineer"),
        &build_milestones(&env), &vec![&env, 30u32, 90u32], &default_config(),
    );
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p0"));
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "bad"));
    client.recuse_arbiter(&a3, &eng_id, &0);
    client.cast_arbiter_vote(&a3, &eng_id, &0, &false);
}

#[test]
fn test_recuse_arbiter_reject_quorum_and_scope() {
    let (env, contract_id, token_id, company, recruiter, _arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let a1 = Address::generate(&env);
    let a2 = Address::generate(&env);
    let a3 = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RECUSE");

    client.create_engagement(
        &eng_id, &company, &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, a1.clone(), a2.clone(), a3.clone()],
            quorum: 2,
            weights: None,
        },
        &token_id, &1_000_000_000, &String::from_str(&env, "Engineer"),
        &build_milestones(&env), &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p0"));
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "bad"));

    client.recuse_arbiter(&a3, &eng_id, &0);
    let recused = client.get_recused_arbiters(&eng_id, &0);
    assert_eq!(recused.len(), 1);
    assert_eq!(recused.get(0).unwrap(), a3);

    // Approve quorum unchanged: one approve alone does not resolve.
    client.cast_arbiter_vote(&a1, &eng_id, &0, &true);
    assert_eq!(
        client.get_engagement(&eng_id).milestones.get(0).unwrap().status,
        MilestoneStatus::Disputed
    );
    // After recusal, one reject exceeds (active 2 - quorum 2) = 0.
    client.cast_arbiter_vote(&a2, &eng_id, &0, &false);
    assert_eq!(
        client.get_engagement(&eng_id).milestones.get(0).unwrap().status,
        MilestoneStatus::Pending
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
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p0b"));
    client.confirm_milestone(&company, &eng_id, &0);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(&recruiter, &eng_id, &1, &String::from_str(&env, "ipfs://p1"));
    client.raise_dispute(&company, &eng_id, &1, &String::from_str(&env, "bad2"));

    assert_eq!(client.get_recused_arbiters(&eng_id, &1).len(), 0);
    client.cast_arbiter_vote(&a3, &eng_id, &1, &true);
    assert_eq!(
        client.get_engagement(&eng_id).milestones.get(1).unwrap().status,
        MilestoneStatus::Disputed
    );
}

// ============================================================
// #449 — remove_token_min_amount reverts to admin-wide floor
// ============================================================

#[test]
fn test_remove_token_min_amount_reverts_to_global_floor() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let global_min: i128 = 2_000_000;
    let token_min: i128 = 8_000_000;
    client.set_min_amount(&company, &global_min);
    client.set_token_min_amount(&company, &token_id, &token_min);

    assert_eq!(client.get_effective_min_amount(&token_id), token_min);

    // While the override is active, an amount between global and token min is rejected.
    let between = token_min - 1;
    let below_override = client.try_create_engagement(
        &String::from_str(&env, "ENG-TKMIN-BELOW"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &between,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
    assert!(below_override.is_err());

    client.remove_token_min_amount(&company, &token_id);

    assert_eq!(client.get_token_min_amount(&token_id), None);
    assert_eq!(client.get_effective_min_amount(&token_id), global_min);

    // Immediately after removal, create_engagement enforces the global floor:
    // between (was below token min) now succeeds; one below global still fails.
    client.create_engagement(
        &String::from_str(&env, "ENG-TKMIN-OK"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &between,
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );

    let below_global = client.try_create_engagement(
        &String::from_str(&env, "ENG-TKMIN-GLOBAL"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &(global_min - 1),
        &String::from_str(&env, "Engineer"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
    assert!(below_global.is_err());
}

// ============================================================
// #450 — rating attribution after recruiter transfer
// ============================================================

#[test]
fn test_rating_attributed_to_incoming_recruiter_after_transfer() {
    let (env, contract_id, token_id, company, original_recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let incoming_recruiter = Address::generate(&env);
    let eng_id = String::from_str(&env, "ENG-RATE-XFER");

    // Two milestones with a short retention window so ledger advances stay
    // within persistent TTL (long 30/90-day fixtures archive balances).
    let milestones = vec![
        &env,
        Milestone {
            name: String::from_str(&env, "Candidate Placed"),
            payment_percent: 40,
            kind: MilestoneKind::Placement,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Pending,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
            prerequisites: Vec::new(&env),
        },
        Milestone {
            name: String::from_str(&env, "Short Retention"),
            payment_percent: 60,
            kind: MilestoneKind::Retention,
            valid_after_ledger: 0,
            proof_hash: String::from_str(&env, ""),
            status: MilestoneStatus::Locked,
            proof_submitted_at: 0,
            replacement_paid_out: 0,
            prerequisites: vec![&env, 0u32],
        },
    ];

    client.create_engagement(
        &eng_id,
        &company,
        &original_recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Transfer Rating"),
        &milestones,
        &vec![&env, 1u32],
        &default_config(),
    );

    // Pay out the placement milestone to the original recruiter before transfer.
    client.submit_proof(
        &original_recruiter,
        &eng_id,
        &0,
        &String::from_str(&env, "ipfs://offer"),
    );
    client.confirm_milestone(&company, &eng_id, &0);
    let released_before_transfer = client.get_engagement_summary(&eng_id).released_amount;
    assert_eq!(released_before_transfer, 400_000_000);

    client.propose_recruiter_transfer(&original_recruiter, &eng_id, &incoming_recruiter);
    client.accept_recruiter_transfer(&company, &eng_id);
    assert_eq!(client.get_engagement(&eng_id).recruiter, incoming_recruiter);

    // Past payout history is unchanged by the transfer.
    assert_eq!(
        client.get_engagement_summary(&eng_id).released_amount,
        released_before_transfer
    );

    // Complete the remaining milestone as the incoming recruiter.
    advance_ledger(&env, 17_280 + 1);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof(
        &incoming_recruiter,
        &eng_id,
        &1,
        &String::from_str(&env, "ipfs://retention"),
    );
    client.confirm_milestone(&company, &eng_id, &1);

    assert_eq!(
        client.get_engagement(&eng_id).status,
        EngagementStatus::Completed
    );

    client.rate_recruiter(&company, &eng_id, &5);

    let incoming = client.get_recruiter_rating(&incoming_recruiter).unwrap();
    assert_eq!(incoming.total_stars, 5);
    assert_eq!(incoming.rating_count, 1);

    // Original recruiter's tally is unaffected.
    assert!(client.get_recruiter_rating(&original_recruiter).is_none());
}

// ============================================================
// #451 — get_public_engagement_ids excludes non-public
// ============================================================

#[test]
fn test_get_public_engagement_ids_excludes_private() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mut public_cfg = default_config();
    public_cfg.is_public = true;

    // Mix of public and private (default) engagements.
    client.create_engagement(
        &String::from_str(&env, "ENG-PRIV-1"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Private One"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
    client.create_engagement(
        &String::from_str(&env, "ENG-PUB-1"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Public One"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &public_cfg,
    );
    client.create_engagement(
        &String::from_str(&env, "ENG-PRIV-2"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Private Two"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &default_config(),
    );
    client.create_engagement(
        &String::from_str(&env, "ENG-PUB-2"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Public Two"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &public_cfg,
    );
    client.create_engagement(
        &String::from_str(&env, "ENG-PUB-3"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Public Three"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &public_cfg,
    );

    // Paginate across all pages (page_size=2) and collect unique public IDs.
    let mut collected: Vec<String> = Vec::new(&env);
    for page in 0u32..5u32 {
        let page_ids = client.get_public_engagement_ids(&page, &2);
        for i in 0..page_ids.len() {
            let id = page_ids.get(i).unwrap();
            // No duplicates across pages.
            for j in 0..collected.len() {
                assert_ne!(collected.get(j).unwrap(), id);
            }
            collected.push_back(id);
        }
        if page_ids.len() < 2 {
            break;
        }
    }

    assert_eq!(collected.len(), 3);
    assert_eq!(collected.get(0).unwrap(), String::from_str(&env, "ENG-PUB-1"));
    assert_eq!(collected.get(1).unwrap(), String::from_str(&env, "ENG-PUB-2"));
    assert_eq!(collected.get(2).unwrap(), String::from_str(&env, "ENG-PUB-3"));

    // Private engagements remain fully readable; visibility only affects the index.
    let priv_id = String::from_str(&env, "ENG-PRIV-1");
    let eng = client.get_engagement(&priv_id);
    assert!(!eng.is_public);
    assert_eq!(eng.id, priv_id);
    let summary = client.get_engagement_summary(&priv_id);
    assert_eq!(summary.id, priv_id);
    assert_eq!(summary.total_amount, 1_000_000_000);
}

// ============================================================
// #452 — engagement tag 10 / 32-char / empty limits + dedup
// ============================================================

fn tag_of_len(env: &Env, len: u32, fill: char) -> String {
    let mut s = std::string::String::new();
    for _ in 0..len {
        s.push(fill);
    }
    String::from_str(env, &s)
}

#[test]
fn test_create_engagement_max_tag_boundary_and_dedup() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    // Exactly 10 tags of exactly 32 characters each — the max allowed combination.
    let mut tags = Vec::new(&env);
    for i in 0u32..10u32 {
        // Distinct 32-char tags so indexing covers all ten.
        let mut s = std::string::String::new();
        for _ in 0..31 {
            s.push('a');
        }
        s.push(char::from_u32('0' as u32 + i).unwrap());
        tags.push_back(String::from_str(&env, &s));
    }
    let mut cfg = default_config();
    cfg.tags = Some(tags);

    client.create_engagement(
        &String::from_str(&env, "ENG-TAGS-MAX"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Tagged Role"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &cfg,
    );

    let stored = client.get_tags(&String::from_str(&env, "ENG-TAGS-MAX"));
    assert_eq!(stored.len(), 10);
    assert_eq!(stored.get(0).unwrap().len(), 32);

    // De-duplication: repeated tag indexes the engagement once.
    let dup_tag = String::from_str(&env, "backend-rust");
    let mut dup_cfg = default_config();
    dup_cfg.tags = Some(vec![
        &env,
        dup_tag.clone(),
        String::from_str(&env, "frontend"),
        dup_tag.clone(),
    ]);
    client.create_engagement(
        &String::from_str(&env, "ENG-TAGS-DUP"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Dup Tags"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &dup_cfg,
    );
    assert_eq!(client.get_engagement_tag_count(&dup_tag), 1);
}

#[test]
#[should_panic(expected = "TooManyTags")]
fn test_create_engagement_eleven_tags_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mut tags = Vec::new(&env);
    for i in 0u32..11u32 {
        tags.push_back(String::from_str(
            &env,
            &std::format!("tag-{}", i),
        ));
    }
    let mut cfg = default_config();
    cfg.tags = Some(tags);

    client.create_engagement(
        &String::from_str(&env, "ENG-TAGS-11"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Too Many"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &cfg,
    );
}

#[test]
#[should_panic(expected = "TagTooLong")]
fn test_create_engagement_tag_33_chars_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mut cfg = default_config();
    cfg.tags = Some(vec![&env, tag_of_len(&env, 33, 'x')]);

    client.create_engagement(
        &String::from_str(&env, "ENG-TAGS-LONG"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Long Tag"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &cfg,
    );
}

#[test]
#[should_panic(expected = "TagEmpty")]
fn test_create_engagement_empty_tag_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    let mut cfg = default_config();
    cfg.tags = Some(vec![&env, String::from_str(&env, "")]);

    client.create_engagement(
        &String::from_str(&env, "ENG-TAGS-EMPTY"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Empty Tag"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &cfg,
    );
}


// ============================================================
// ESCROW TOP-UP, PROOF VALIDATION, PAUSE AND PROGRESS QUERIES
// ============================================================

#[test]
fn test_top_up_escrow_increases_total_and_balance() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-TOPUP",
    );
    let eng_id = String::from_str(&env, "ENG-TOPUP");

    client.top_up_escrow(&company, &eng_id, &250_000_000);
    // `env.events().all()` only holds the most recent invocation's events, so
    // check before any further client calls.
    assert!(has_event(&env, "escrow_topped_up"));

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.total_amount, 1_250_000_000);
    assert_eq!(eng.released_amount, 0);
    assert_eq!(client.get_escrow_balance(&eng_id), 1_250_000_000);
    assert_eq!(token_client.balance(&contract_id), 1_250_000_000);
    assert_eq!(
        token_client.balance(&company),
        500_000_000_000 - 1_250_000_000
    );
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_top_up_escrow_by_stranger_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-TOPUP-X",
    );

    let stranger = Address::generate(&env);
    client.top_up_escrow(
        &stranger,
        &String::from_str(&env, "ENG-TOPUP-X"),
        &100_000_000,
    );
}

#[test]
#[should_panic(expected = "InvalidProofHash")]
fn test_submit_proof_empty_hash_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-EMPTY-PROOF",
    );

    client.submit_proof(
        &recruiter,
        &String::from_str(&env, "ENG-EMPTY-PROOF"),
        &0,
        &String::from_str(&env, ""),
    );
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_submit_proof_blocked_while_contract_paused() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PAUSED",
    );

    // `setup` initialises the contract with `company` as admin.
    client.pause(&company);
    assert!(client.is_paused());

    client.submit_proof(
        &recruiter,
        &String::from_str(&env, "ENG-PAUSED"),
        &0,
        &String::from_str(&env, "ipfs://offer-letter"),
    );
}

#[test]
fn test_unlock_progress_and_statuses_track_unlocks() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-PROGRESS",
    );
    let eng_id = String::from_str(&env, "ENG-PROGRESS");

    assert_eq!(client.get_unlock_progress(&eng_id), (1, 3));
    let statuses = client.get_all_milestone_statuses(&eng_id);
    assert_eq!(statuses.len(), 3);
    assert_eq!(statuses.get(0).unwrap(), MilestoneStatus::Pending);
    assert_eq!(statuses.get(1).unwrap(), MilestoneStatus::Locked);
    assert_eq!(statuses.get(2).unwrap(), MilestoneStatus::Locked);
    assert!(client.ledgers_until_unlock(&eng_id, &1) > 0);

    advance_ledger(&env, 31 * 17_280);
    assert_eq!(client.ledgers_until_unlock(&eng_id, &1), 0);
    client.unlock_milestone(&eng_id, &1);

    assert_eq!(client.get_unlock_progress(&eng_id), (2, 3));
    let statuses = client.get_all_milestone_statuses(&eng_id);
    assert_eq!(statuses.get(1).unwrap(), MilestoneStatus::Pending);
    assert_eq!(statuses.get(2).unwrap(), MilestoneStatus::Locked);
}

#[test]
#[should_panic(expected = "engagement already exists")]
fn test_create_engagement_duplicate_id_rejected() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-DUP",
    );
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-DUP",
    );
}

// ============================================================
// ISSUE #501 — ENGAGEMENT TIMELINE
// ============================================================

/// Builds an engagement whose history mixes every timeline kind, on distinct
/// ledgers, with the second amendment last so the merge has to sort:
///
/// | ledger | event |
/// |---|---|
/// | 110 | split amendment proposed by the recruiter, accepted |
/// | 120 | dispute raised on milestone 0 (then approved) |
/// | 130 | replacement requested → Active → ReplacementRequested |
/// | 140 | replacement proof submitted → ReplacementRequested → Active |
/// | 150 | split amendment proposed by the co-recruiter, accepted |
fn build_timeline_history(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
) -> (String, Address) {
    let co = Address::generate(env);
    let eng_id = String::from_str(env, "ENG-TL");
    let mut cfg = default_config();
    cfg.co_recruiter = Some(co.clone());
    cfg.recruiter_split_bps = 7_000;
    client.create_engagement(
        &eng_id,
        company,
        recruiter,
        &ArbiterSetup {
            arbiters: vec![env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Timeline"),
        &build_milestones(env),
        &vec![env, 30u32, 90u32],
        &cfg,
    );

    advance_ledger(env, 10);
    client.propose_split_amendment(recruiter, &eng_id, &6_000);
    client.accept_split_amendment(&co, &eng_id);

    advance_ledger(env, 10);
    client.submit_proof(recruiter, &eng_id, &0, &String::from_str(env, "ipfs://p1"));
    client.raise_dispute(company, &eng_id, &0, &String::from_str(env, "bad"));
    client.cast_arbiter_vote(arbiter, &eng_id, &0, &true);

    advance_ledger(env, 10);
    client.request_replacement(company, &eng_id, &String::from_str(env, "left"));

    advance_ledger(env, 10);
    client.submit_proof(recruiter, &eng_id, &0, &String::from_str(env, "ipfs://p2"));

    advance_ledger(env, 10);
    client.propose_split_amendment(&co, &eng_id, &5_000);
    client.accept_split_amendment(recruiter, &eng_id);

    (eng_id, co)
}

fn timeline_entry(
    kind: TimelineKind,
    milestone_index: Option<u32>,
    actor: Option<Address>,
    ledger: u32,
    source_index: u32,
) -> TimelineEntry {
    TimelineEntry {
        kind,
        milestone_index,
        actor,
        ledger,
        source_index,
    }
}

#[test]
fn test_timeline_merges_every_kind_in_ledger_order() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, co) =
        build_timeline_history(&env, &client, &token_id, &company, &recruiter, &arbiter);

    let expected = vec![
        &env,
        timeline_entry(TimelineKind::Amendment, None, Some(recruiter.clone()), 110, 0),
        timeline_entry(TimelineKind::Dispute, Some(0), Some(company.clone()), 120, 0),
        timeline_entry(TimelineKind::Replacement, None, Some(company.clone()), 130, 0),
        timeline_entry(TimelineKind::StatusChange, None, Some(company.clone()), 130, 0),
        timeline_entry(TimelineKind::StatusChange, None, Some(recruiter.clone()), 140, 1),
        timeline_entry(TimelineKind::Amendment, None, Some(co.clone()), 150, 1),
    ];
    assert_eq!(client.get_engagement_timeline(&eng_id, &0, &100), expected);
}

#[test]
fn test_timeline_matches_underlying_histories() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, co) =
        build_timeline_history(&env, &client, &token_id, &company, &recruiter, &arbiter);

    let amendments = client.get_split_amendment_log(&eng_id);
    assert_eq!(amendments.len(), 2);
    assert_eq!(amendments.get(0).unwrap().proposer, recruiter);
    assert_eq!(amendments.get(0).unwrap().ledger, 110);
    assert_eq!(amendments.get(1).unwrap().proposer, co);
    assert_eq!(amendments.get(1).unwrap().ledger, 150);

    assert_eq!(client.get_replacement_count(&eng_id), 1);
    assert_eq!(
        client.get_replacement_reason(&eng_id, &0),
        Some(String::from_str(&env, "left"))
    );
    assert_eq!(
        client.get_replacement_record(&eng_id, &0),
        Some(ReplacementRecord {
            requested_by: company.clone(),
            ledger: 130,
        })
    );

    // The live dispute reason is gone once resolved; the history keeps it.
    assert_eq!(client.get_dispute_reason(&eng_id, &0), None);
    assert_eq!(
        client.get_dispute_history(&eng_id),
        vec![
            &env,
            DisputeHistoryEntry {
                milestone_index: 0,
                raised_by: company.clone(),
                reason: String::from_str(&env, "bad"),
                ledger: 120,
            },
        ]
    );

    assert_eq!(
        client.get_status_history(&eng_id),
        vec![
            &env,
            StatusChangeEntry {
                old_status: EngagementStatus::Active,
                new_status: EngagementStatus::ReplacementRequested,
                actor: Some(company.clone()),
                ledger: 130,
            },
            StatusChangeEntry {
                old_status: EngagementStatus::ReplacementRequested,
                new_status: EngagementStatus::Active,
                actor: Some(recruiter.clone()),
                ledger: 140,
            },
        ]
    );
}

#[test]
fn test_timeline_empty_for_fresh_and_unknown_engagements() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-TL-EMPTY",
    );

    assert_eq!(
        client
            .get_engagement_timeline(&String::from_str(&env, "ENG-TL-EMPTY"), &0, &10)
            .len(),
        0
    );
    assert_eq!(
        client
            .get_engagement_timeline(&String::from_str(&env, "ENG-NOPE"), &0, &10)
            .len(),
        0
    );
}

#[test]
fn test_timeline_pagination() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, _) =
        build_timeline_history(&env, &client, &token_id, &company, &recruiter, &arbiter);
    let all = client.get_engagement_timeline(&eng_id, &0, &100);
    assert_eq!(all.len(), 6);

    let first = client.get_engagement_timeline(&eng_id, &0, &4);
    assert_eq!(first.len(), 4);
    for i in 0..4 {
        assert_eq!(first.get(i).unwrap(), all.get(i).unwrap());
    }
    let second = client.get_engagement_timeline(&eng_id, &1, &4);
    assert_eq!(second.len(), 2);
    assert_eq!(second.get(0).unwrap(), all.get(4).unwrap());
    assert_eq!(second.get(1).unwrap(), all.get(5).unwrap());

    assert_eq!(client.get_engagement_timeline(&eng_id, &2, &4).len(), 0);
    assert_eq!(client.get_engagement_timeline(&eng_id, &0, &0).len(), 0);
    assert_eq!(
        client
            .get_engagement_timeline(&eng_id, &u32::MAX, &u32::MAX)
            .len(),
        0
    );
}

#[test]
fn test_status_history_records_completion() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-TL-DONE");
    create_single_milestone_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-TL-DONE", default_config(),
    );
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p"));
    client.confirm_milestone(&company, &eng_id, &0);

    let timeline = client.get_engagement_timeline(&eng_id, &0, &10);
    assert_eq!(
        timeline,
        vec![
            &env,
            timeline_entry(TimelineKind::StatusChange, None, Some(company.clone()), 100, 0),
        ]
    );
    let status = client.get_status_history(&eng_id).get(0).unwrap();
    assert_eq!(status.old_status, EngagementStatus::Active);
    assert_eq!(status.new_status, EngagementStatus::Completed);
}

// ============================================================
// ISSUE #505 — FEE-TIER SNAPSHOT MODE
// ============================================================

/// One Placement milestone paying 100 %, so a single confirmation completes
/// the engagement.
fn create_single_milestone_engagement(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
    id: &str,
    config: EngagementConfig,
) {
    client.create_engagement(
        &String::from_str(env, id),
        company,
        recruiter,
        &ArbiterSetup {
            arbiters: vec![env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Single"),
        &vec![
            env,
            Milestone {
                name: String::from_str(env, "Placed"),
                payment_percent: 100,
                kind: MilestoneKind::Placement,
                valid_after_ledger: 0,
                proof_hash: String::from_str(env, ""),
                status: MilestoneStatus::Pending,
                proof_submitted_at: 0,
                replacement_paid_out: 0,
                prerequisites: Vec::new(env),
            },
        ],
        &Vec::new(env),
        &config,
    );
}

/// Base fee 500 bps with one tier at 300 bps for engagements ≥ 500M, so the
/// standard 1B engagement resolves to 300 bps at creation.
fn setup_fee_tiers(env: &Env, client: &HireSettleContractClient, admin: &Address) -> Address {
    let treasury = Address::generate(env);
    client.set_platform_fee(admin, &500, &treasury);
    client.set_fee_tiers(
        admin,
        &vec![
            env,
            FeeTier {
                threshold: 500_000_000,
                bps: 300,
            },
        ],
    );
    treasury
}

fn lower_fee_tier_to_100(env: &Env, client: &HireSettleContractClient, admin: &Address) {
    client.set_fee_tiers(
        admin,
        &vec![
            env,
            FeeTier {
                threshold: 500_000_000,
                bps: 100,
            },
        ],
    );
}

fn submit_and_confirm_placement(
    env: &Env,
    client: &HireSettleContractClient,
    company: &Address,
    recruiter: &Address,
    id: &str,
) {
    let eng_id = String::from_str(env, id);
    client.submit_proof(recruiter, &eng_id, &0, &String::from_str(env, "ipfs://p"));
    client.confirm_milestone(company, &eng_id, &0);
}

#[test]
fn test_fee_tier_default_tracks_live_tier_changes() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = setup_fee_tiers(&env, &client, &company);

    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-FT-LIVE",
    );
    assert_eq!(
        client.get_fee_tier_snapshot(&String::from_str(&env, "ENG-FT-LIVE")),
        None
    );

    lower_fee_tier_to_100(&env, &client, &company);
    submit_and_confirm_placement(&env, &client, &company, &recruiter, "ENG-FT-LIVE");

    // 30 % of 1B = 300M at the new live 100 bps.
    assert_eq!(token_client.balance(&treasury), 3_000_000);
    assert_eq!(token_client.balance(&recruiter), 297_000_000);
}

#[test]
fn test_fee_tier_snapshot_ignores_later_tier_changes() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = setup_fee_tiers(&env, &client, &company);

    let mut cfg = default_config();
    cfg.snapshot_fee_tier = true;
    client.create_engagement(
        &String::from_str(&env, "ENG-FT-SNAP"),
        &company,
        &recruiter,
        &ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 1,
            weights: None,
        },
        &token_id,
        &1_000_000_000,
        &String::from_str(&env, "Snapshot"),
        &build_milestones(&env),
        &vec![&env, 30u32, 90u32],
        &cfg,
    );
    assert_eq!(
        client.get_fee_tier_snapshot(&String::from_str(&env, "ENG-FT-SNAP")),
        Some(300)
    );

    lower_fee_tier_to_100(&env, &client, &company);
    submit_and_confirm_placement(&env, &client, &company, &recruiter, "ENG-FT-SNAP");

    // Still charged the 300 bps tier resolved at creation.
    assert_eq!(token_client.balance(&treasury), 9_000_000);
    assert_eq!(token_client.balance(&recruiter), 291_000_000);
}

#[test]
fn test_fee_tier_snapshot_freezes_base_rate_when_no_tier_matches() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = Address::generate(&env);
    client.set_platform_fee(&company, &500, &treasury);

    let mut cfg = default_config();
    cfg.snapshot_fee_tier = true;
    create_single_milestone_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-FT-BASE", cfg,
    );
    assert_eq!(
        client.get_fee_tier_snapshot(&String::from_str(&env, "ENG-FT-BASE")),
        Some(500)
    );

    lower_fee_tier_to_100(&env, &client, &company);
    submit_and_confirm_placement(&env, &client, &company, &recruiter, "ENG-FT-BASE");

    assert_eq!(token_client.balance(&treasury), 50_000_000);
}

#[test]
fn test_fee_tier_snapshot_composes_with_referral_discount() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let treasury = setup_fee_tiers(&env, &client, &company);
    let referrer = Address::generate(&env);
    client.add_referrer(&company, &referrer);
    client.set_referral_discount_bps(&company, &50);

    let mut cfg = default_config();
    cfg.snapshot_fee_tier = true;
    cfg.referrer = Some(referrer);
    create_single_milestone_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-FT-REF", cfg,
    );

    lower_fee_tier_to_100(&env, &client, &company);
    // The discount is applied at payout time, on top of the frozen tier.
    client.set_referral_discount_bps(&company, &100);
    submit_and_confirm_placement(&env, &client, &company, &recruiter, "ENG-FT-REF");

    // Snapshot 300 bps − live 100 bps discount = 200 bps of 1B.
    assert_eq!(token_client.balance(&treasury), 20_000_000);
    assert_eq!(token_client.balance(&recruiter), 980_000_000);
}

// ============================================================
// ISSUE #506 — CO-RECRUITER COLLATERAL BOND
// ============================================================

const RECRUITER_BOND: i128 = 100_000_000;
const CO_BOND: i128 = 50_000_000;
const BOND_FUNDS: i128 = 1_000_000_000;

/// Single-milestone engagement with a co-recruiter on a 70/30 split and both
/// bonds posted. Returns the funded co-recruiter.
fn create_co_bonded_engagement(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
    id: &str,
) -> Address {
    let co = Address::generate(env);
    let minter = token::StellarAssetClient::new(env, token_id);
    minter.mint(recruiter, &BOND_FUNDS);
    minter.mint(&co, &BOND_FUNDS);

    let mut cfg = default_config();
    cfg.co_recruiter = Some(co.clone());
    cfg.recruiter_split_bps = 7_000;
    cfg.recruiter_bond_amount = Some(RECRUITER_BOND);
    cfg.co_recruiter_bond_amount = Some(CO_BOND);
    create_single_milestone_engagement(env, client, token_id, company, recruiter, arbiter, id, cfg);
    co
}

/// Submit a proof, dispute it, and have the arbiter reject it.
fn reject_placement_proof(
    env: &Env,
    client: &HireSettleContractClient,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
    eng_id: &String,
) {
    client.submit_proof(recruiter, eng_id, &0, &String::from_str(env, "ipfs://bad"));
    client.raise_dispute(company, eng_id, &0, &String::from_str(env, "fake"));
    client.cast_arbiter_vote(arbiter, eng_id, &0, &false);
}

#[test]
fn test_co_bond_ignored_without_co_recruiter() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CB-NONE");

    let mut cfg = default_config();
    cfg.co_recruiter_bond_amount = Some(CO_BOND);
    create_single_milestone_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CB-NONE", cfg,
    );

    assert_eq!(client.get_co_recruiter_bond(&eng_id), None);
    assert_eq!(token_client.balance(&contract_id), 1_000_000_000);

    submit_and_confirm_placement(&env, &client, &company, &recruiter, "ENG-CB-NONE");
    assert_eq!(token_client.balance(&recruiter), 1_000_000_000);
    assert_eq!(token_client.balance(&contract_id), 0);
    assert_eq!(client.get_engagement(&eng_id).status, EngagementStatus::Completed);
}

#[test]
fn test_co_bond_escrowed_and_returned_on_clean_completion() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CB-CLEAN");
    let co = create_co_bonded_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CB-CLEAN",
    );

    assert_eq!(client.get_co_recruiter_bond(&eng_id), Some((CO_BOND, false)));
    assert_eq!(token_client.balance(&co), BOND_FUNDS - CO_BOND);
    assert_eq!(
        token_client.balance(&contract_id),
        1_000_000_000 + RECRUITER_BOND + CO_BOND
    );

    submit_and_confirm_placement(&env, &client, &company, &recruiter, "ENG-CB-CLEAN");

    // Both bonds come back in full alongside the 70/30 payout.
    assert_eq!(client.get_co_recruiter_bond(&eng_id), Some((CO_BOND, false)));
    assert_eq!(client.get_recruiter_bond(&eng_id), Some((RECRUITER_BOND, false)));
    assert_eq!(token_client.balance(&co), BOND_FUNDS + 300_000_000);
    assert_eq!(token_client.balance(&recruiter), BOND_FUNDS + 700_000_000);
    assert_eq!(token_client.balance(&contract_id), 0);
}

#[test]
fn test_co_bond_forfeits_co_share_on_unresolved_rejection() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CB-FORFEIT");
    let co = create_co_bonded_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CB-FORFEIT",
    );
    let company_before = token_client.balance(&company);

    reject_placement_proof(&env, &client, &company, &recruiter, &arbiter, &eng_id);
    client.cancel_engagement(&company, &recruiter, &eng_id);

    // Recruiter bond: the default 100 % forfeit. Co-recruiter bond: 100 % of
    // its 30 % payout share, i.e. 15M of 50M.
    let co_forfeit = 15_000_000;
    assert_eq!(client.get_recruiter_bond(&eng_id), Some((RECRUITER_BOND, true)));
    assert_eq!(client.get_co_recruiter_bond(&eng_id), Some((CO_BOND, true)));
    assert_eq!(token_client.balance(&recruiter), BOND_FUNDS - RECRUITER_BOND);
    assert_eq!(token_client.balance(&co), BOND_FUNDS - co_forfeit);
    assert_eq!(
        token_client.balance(&company),
        company_before + 1_000_000_000 + RECRUITER_BOND + co_forfeit
    );
    assert_eq!(token_client.balance(&contract_id), 0);
}

#[test]
fn test_co_bond_forfeit_scales_with_forfeit_bps() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CB-HALF");
    client.set_bond_forfeit_bps(&company, &5_000);
    let co = create_co_bonded_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CB-HALF",
    );

    reject_placement_proof(&env, &client, &company, &recruiter, &arbiter, &eng_id);
    client.cancel_engagement(&company, &recruiter, &eng_id);

    // 50M × 50 % × 30 % = 7.5M; the recruiter bond loses 50 %.
    assert_eq!(token_client.balance(&co), BOND_FUNDS - 7_500_000);
    assert_eq!(token_client.balance(&recruiter), BOND_FUNDS - RECRUITER_BOND / 2);
}

#[test]
fn test_co_bond_returned_when_rejected_proof_later_confirmed() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CB-RECOVER");
    let co = create_co_bonded_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CB-RECOVER",
    );

    reject_placement_proof(&env, &client, &company, &recruiter, &arbiter, &eng_id);
    submit_and_confirm_placement(&env, &client, &company, &recruiter, "ENG-CB-RECOVER");

    assert_eq!(client.get_recruiter_bond(&eng_id), Some((RECRUITER_BOND, false)));
    assert_eq!(client.get_co_recruiter_bond(&eng_id), Some((CO_BOND, false)));
    assert_eq!(token_client.balance(&co), BOND_FUNDS + 300_000_000);
    assert_eq!(token_client.balance(&recruiter), BOND_FUNDS + 700_000_000);
}

#[test]
fn test_co_bond_returned_when_rejected_proof_later_approved_by_arbiter() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    let eng_id = String::from_str(&env, "ENG-CB-APPROVE");
    let co = create_co_bonded_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CB-APPROVE",
    );

    reject_placement_proof(&env, &client, &company, &recruiter, &arbiter, &eng_id);
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://good"));
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "again"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);

    assert_eq!(client.get_engagement(&eng_id).status, EngagementStatus::Completed);
    assert_eq!(client.get_recruiter_bond(&eng_id), Some((RECRUITER_BOND, false)));
    assert_eq!(client.get_co_recruiter_bond(&eng_id), Some((CO_BOND, false)));
    assert_eq!(token_client.balance(&co), BOND_FUNDS + 300_000_000);
}

#[test]
#[should_panic(expected = "InvalidBondAmount")]
fn test_co_bond_rejects_non_positive_amount() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let mut cfg = default_config();
    cfg.co_recruiter = Some(Address::generate(&env));
    cfg.recruiter_split_bps = 7_000;
    cfg.co_recruiter_bond_amount = Some(0);
    create_single_milestone_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-CB-ZERO", cfg,
    );
}

// ============================================================
// ISSUE #507 — ADMIN ARBITER PANEL RESIZE
// ============================================================

fn create_panel_engagement(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter_setup: ArbiterSetup,
    id: &str,
) -> String {
    let eng_id = String::from_str(env, id);
    client.create_engagement(
        &eng_id,
        company,
        recruiter,
        &arbiter_setup,
        token_id,
        &1_000_000_000,
        &String::from_str(env, "Panel"),
        &build_milestones(env),
        &vec![env, 30u32, 90u32],
        &default_config(),
    );
    eng_id
}

fn two_arbiter_engagement(
    env: &Env,
    client: &HireSettleContractClient,
    token_id: &Address,
    company: &Address,
    recruiter: &Address,
    arbiter: &Address,
    quorum: u32,
) -> (String, Address) {
    let second = Address::generate(env);
    let eng_id = create_panel_engagement(
        env,
        client,
        token_id,
        company,
        recruiter,
        ArbiterSetup {
            arbiters: vec![env, arbiter.clone(), second.clone()],
            quorum,
            weights: None,
        },
        "ENG-PANEL",
    );
    (eng_id, second)
}

#[test]
fn test_admin_add_arbiter_keeps_quorum_by_default() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, _) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 2);

    let third = Address::generate(&env);
    client.admin_add_arbiter(&company, &eng_id, &third, &None);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters.len(), 3);
    assert_eq!(eng.arbiters.get(2).unwrap(), third);
    assert_eq!(eng.quorum, 2);
}

#[test]
fn test_admin_add_arbiter_with_new_quorum_and_new_arbiter_votes() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let token_client = token::Client::new(&env, &token_id);
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-GROW",
    );
    let eng_id = String::from_str(&env, "ENG-GROW");

    let second = Address::generate(&env);
    client.admin_add_arbiter(&company, &eng_id, &second, &Some(2));
    assert_eq!(client.get_engagement(&eng_id).quorum, 2);

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p"));
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "check"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &true);
    assert_eq!(client.get_milestone(&eng_id, &0).status, MilestoneStatus::Disputed);
    client.cast_arbiter_vote(&second, &eng_id, &0, &true);
    assert_eq!(client.get_milestone(&eng_id, &0).status, MilestoneStatus::Resolved);
    assert_eq!(token_client.balance(&recruiter), 300_000_000);
}

#[test]
fn test_admin_add_arbiter_on_weighted_panel_gets_weight_one() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = create_panel_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        ArbiterSetup {
            arbiters: vec![&env, arbiter.clone()],
            quorum: 3,
            weights: Some(vec![&env, 3u32]),
        },
        "ENG-PANEL-W",
    );

    client.admin_add_arbiter(&company, &eng_id, &Address::generate(&env), &Some(4));

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiter_weights, Some(vec![&env, 3u32, 1u32]));
    assert_eq!(eng.quorum, 4);
}

#[test]
#[should_panic(expected = "DuplicateArbiter")]
fn test_admin_add_arbiter_rejects_existing_arbiter() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, second) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);
    client.admin_add_arbiter(&company, &eng_id, &second, &None);
}

#[test]
#[should_panic(expected = "RecruiterArbiterCollision")]
fn test_admin_add_arbiter_rejects_recruiter() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, _) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);
    client.admin_add_arbiter(&company, &eng_id, &recruiter, &None);
}

#[test]
#[should_panic(expected = "invalid quorum")]
fn test_admin_add_arbiter_rejects_unreachable_new_quorum() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, _) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);
    client.admin_add_arbiter(&company, &eng_id, &Address::generate(&env), &Some(4));
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_admin_add_arbiter_rejects_non_admin() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, _) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);
    client.admin_add_arbiter(&recruiter, &eng_id, &Address::generate(&env), &None);
}

#[test]
#[should_panic(expected = "QuorumUnreachable")]
fn test_admin_remove_arbiter_panics_when_quorum_becomes_unreachable() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, second) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 2);
    client.admin_remove_arbiter(&company, &eng_id, &second, &None);
}

#[test]
fn test_admin_remove_arbiter_with_reduced_quorum_succeeds() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, second) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 2);

    client.admin_remove_arbiter(&company, &eng_id, &second, &Some(1));

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters, vec![&env, arbiter.clone()]);
    assert_eq!(eng.quorum, 1);
}

#[test]
fn test_admin_remove_arbiter_keeps_reachable_quorum() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, second) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);

    client.admin_remove_arbiter(&company, &eng_id, &arbiter, &None);

    let eng = client.get_engagement(&eng_id);
    assert_eq!(eng.arbiters, vec![&env, second.clone()]);
    assert_eq!(eng.quorum, 1);
}

#[test]
#[should_panic(expected = "invalid quorum")]
fn test_admin_remove_arbiter_rejects_unreachable_new_quorum() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, second) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 2);
    client.admin_remove_arbiter(&company, &eng_id, &second, &Some(2));
}

#[test]
#[should_panic(expected = "QuorumUnreachable")]
fn test_admin_remove_arbiter_checks_weighted_quorum() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let heavy = Address::generate(&env);
    let eng_id = create_panel_engagement(
        &env,
        &client,
        &token_id,
        &company,
        &recruiter,
        ArbiterSetup {
            arbiters: vec![&env, heavy.clone(), arbiter.clone()],
            quorum: 2,
            weights: Some(vec![&env, 2u32, 1u32]),
        },
        "ENG-PANEL-W",
    );
    // Two arbiters remain by headcount, but only weight 1.
    client.admin_remove_arbiter(&company, &eng_id, &heavy, &None);
}

#[test]
#[should_panic(expected = "at least one arbiter required")]
fn test_admin_remove_last_arbiter_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-LAST",
    );
    client.admin_remove_arbiter(&company, &String::from_str(&env, "ENG-LAST"), &arbiter, &None);
}

#[test]
#[should_panic(expected = "ArbiterNotFound")]
fn test_admin_remove_unknown_arbiter_panics() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, _) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);
    client.admin_remove_arbiter(&company, &eng_id, &Address::generate(&env), &None);
}

#[test]
#[should_panic(expected = "PanelChangeDuringDispute")]
fn test_admin_add_arbiter_rejected_during_dispute() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, _) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 2);
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p"));
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "check"));
    client.admin_add_arbiter(&company, &eng_id, &Address::generate(&env), &None);
}

#[test]
#[should_panic(expected = "PanelChangeDuringDispute")]
fn test_admin_remove_arbiter_rejected_during_dispute() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, second) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);
    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p"));
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "check"));
    client.admin_remove_arbiter(&company, &eng_id, &second, &None);
}

#[test]
fn test_admin_remove_arbiter_clears_its_nomination_and_delegate() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let (eng_id, second) =
        two_arbiter_engagement(&env, &client, &token_id, &company, &recruiter, &arbiter, 1);
    let delegate = Address::generate(&env);
    client.set_arbiter_vote_delegate(&second, &eng_id, &Some(delegate));
    client.nominate_arbiter_successor(&second, &eng_id, &Address::generate(&env));

    client.admin_remove_arbiter(&company, &eng_id, &second, &None);

    assert_eq!(client.get_arbiter_vote_delegate(&eng_id, &second), None);
    // The pending nomination for the removed slot is gone.
    let result = client.try_claim_arbiter(&Address::generate(&env), &eng_id);
    assert!(result.is_err());
}

// ============================================================
// #486 — milestone proof Merkle root + inclusion proofs
// ============================================================

/// Which proof submission path a shared #486 test body exercises.
#[derive(Clone, Copy)]
enum ProofMode {
    Hash,
    Root,
}

/// Hash an evidence item into a Merkle leaf, as a recruiter would off-chain.
fn merkle_leaf(env: &Env, item: &str) -> BytesN<32> {
    env.crypto()
        .sha256(&Bytes::from_slice(env, item.as_bytes()))
        .to_bytes()
}

/// Independent re-implementation of the contract's sorted-pair level hash,
/// so the tests do not just check the contract against itself.
fn merkle_parent(env: &Env, a: &BytesN<32>, b: &BytesN<32>) -> BytesN<32> {
    let (a, b) = (a.to_array(), b.to_array());
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    let mut data = Bytes::from_array(env, &lo);
    data.extend_from_array(&hi);
    env.crypto().sha256(&data).to_bytes()
}

/// Four-leaf tree `root = H(H(l0, l1), H(l2, l3))`. Returns the leaves,
/// the two level-one nodes and the root.
fn four_leaf_tree(env: &Env) -> (Vec<BytesN<32>>, BytesN<32>, BytesN<32>, BytesN<32>) {
    let leaves = vec![
        env,
        merkle_leaf(env, "offer-letter.pdf"),
        merkle_leaf(env, "signed-contract.pdf"),
        merkle_leaf(env, "payroll-entry.csv"),
        merkle_leaf(env, "badge-photo.jpg"),
    ];
    let n01 = merkle_parent(env, &leaves.get(0).unwrap(), &leaves.get(1).unwrap());
    let n23 = merkle_parent(env, &leaves.get(2).unwrap(), &leaves.get(3).unwrap());
    let root = merkle_parent(env, &n01, &n23);
    (leaves, n01, n23, root)
}

/// Submit proof for `milestone_index` through the path `mode` selects, using
/// `tag` to derive a distinct hash or root per call.
fn submit_proof_via(
    env: &Env,
    client: &HireSettleContractClient,
    recruiter: &Address,
    eng_id: &String,
    milestone_index: u32,
    mode: ProofMode,
    tag: &str,
) {
    match mode {
        ProofMode::Hash => {
            client.submit_proof(recruiter, eng_id, &milestone_index, &String::from_str(env, tag))
        }
        ProofMode::Root => {
            client.submit_proof_root(recruiter, eng_id, &milestone_index, &merkle_leaf(env, tag))
        }
    }
}

fn assert_submission_transitions_milestone(mode: ProofMode) {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-SUBMIT");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-SUBMIT",
    );

    submit_proof_via(&env, &client, &recruiter, &eng_id, 0, mode, "proof-a");

    let m0 = client.get_milestone(&eng_id, &0);
    assert_eq!(m0.status, MilestoneStatus::ProofSubmitted);
    assert_eq!(m0.proof_submitted_at, env.ledger().sequence());
    assert!(!m0.proof_hash.is_empty());
    assert!(has_event(&env, "proof_submitted"));
}

fn assert_rejected_proof_resubmits_and_confirms(mode: ProofMode) {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-RESUB");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-RESUB",
    );

    submit_proof_via(&env, &client, &recruiter, &eng_id, 0, mode, "proof-a");
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);
    assert_eq!(client.get_milestone(&eng_id, &0).status, MilestoneStatus::Pending);

    submit_proof_via(&env, &client, &recruiter, &eng_id, 0, mode, "proof-b");
    assert_eq!(
        client.get_milestone(&eng_id, &0).status,
        MilestoneStatus::ProofSubmitted
    );
    assert!(has_event(&env, "proof_resubmitted"));

    client.confirm_milestone(&company, &eng_id, &0);
    assert_eq!(client.get_milestone(&eng_id, &0).status, MilestoneStatus::Confirmed);
}

fn submit_on_locked_milestone(mode: ProofMode) {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-LOCKED");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-LOCKED",
    );
    submit_proof_via(&env, &client, &recruiter, &eng_id, 1, mode, "proof-a");
}

fn submit_by_wrong_recruiter(mode: ProofMode) {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-AUTH");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-AUTH",
    );
    let stranger = Address::generate(&env);
    submit_proof_via(&env, &client, &stranger, &eng_id, 0, mode, "proof-a");
}

fn submit_while_paused(mode: ProofMode) {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-PAUSED");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-PAUSED",
    );
    client.pause(&company);
    submit_proof_via(&env, &client, &recruiter, &eng_id, 0, mode, "proof-a");
}

#[test]
fn test_submit_proof_transitions_milestone() {
    assert_submission_transitions_milestone(ProofMode::Hash);
}

#[test]
fn test_submit_proof_root_transitions_milestone() {
    assert_submission_transitions_milestone(ProofMode::Root);
}

#[test]
fn test_submit_proof_rejected_resubmit_and_confirm() {
    assert_rejected_proof_resubmits_and_confirms(ProofMode::Hash);
}

#[test]
fn test_submit_proof_root_rejected_resubmit_and_confirm() {
    assert_rejected_proof_resubmits_and_confirms(ProofMode::Root);
}

#[test]
#[should_panic(expected = "milestone is not pending")]
fn test_submit_proof_locked_milestone_rejected() {
    submit_on_locked_milestone(ProofMode::Hash);
}

#[test]
#[should_panic(expected = "milestone is not pending")]
fn test_submit_proof_root_locked_milestone_rejected() {
    submit_on_locked_milestone(ProofMode::Root);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_submit_proof_wrong_recruiter_rejected() {
    submit_by_wrong_recruiter(ProofMode::Hash);
}

#[test]
#[should_panic(expected = "unauthorized")]
fn test_submit_proof_root_wrong_recruiter_rejected() {
    submit_by_wrong_recruiter(ProofMode::Root);
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_submit_proof_rejected_while_paused() {
    submit_while_paused(ProofMode::Hash);
}

#[test]
#[should_panic(expected = "ContractPaused")]
fn test_submit_proof_root_rejected_while_paused() {
    submit_while_paused(ProofMode::Root);
}

#[test]
fn test_submit_proof_root_stores_root_and_hex_proof_hash() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-STORE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-STORE",
    );
    let (_, _, _, root) = four_leaf_tree(&env);

    client.submit_proof_root(&recruiter, &eng_id, &0, &root);

    assert_eq!(client.get_proof_merkle_root(&eng_id, &0), Some(root.clone()));
    // "merkle:" + 64 hex characters.
    let proof_hash = client.get_milestone(&eng_id, &0).proof_hash;
    assert_eq!(proof_hash.len(), 7 + 64);
    let mut buf = [0u8; 71];
    proof_hash.copy_into_slice(&mut buf);
    assert!(buf.starts_with(b"merkle:"));
    assert!(buf[7..]
        .iter()
        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c)));
    assert!(has_event(&env, "proof_root_submitted"));
}

#[test]
fn test_get_proof_merkle_root_none_for_single_hash_proof() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-NONE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-NONE",
    );
    assert_eq!(client.get_proof_merkle_root(&eng_id, &0), None);

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://p"));

    assert_eq!(client.get_proof_merkle_root(&eng_id, &0), None);
    let (leaves, _, _, _) = four_leaf_tree(&env);
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaves.get(0).unwrap(), &Vec::new(&env)));
}

#[test]
fn test_verify_proof_inclusion_accepts_every_valid_leaf() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-VALID");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-VALID",
    );
    let (leaves, n01, n23, root) = four_leaf_tree(&env);
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);

    let paths = [
        vec![&env, leaves.get(1).unwrap(), n23.clone()],
        vec![&env, leaves.get(0).unwrap(), n23.clone()],
        vec![&env, leaves.get(3).unwrap(), n01.clone()],
        vec![&env, leaves.get(2).unwrap(), n01.clone()],
    ];
    for (i, path) in paths.iter().enumerate() {
        assert!(
            client.verify_proof_inclusion(&eng_id, &0, &leaves.get(i as u32).unwrap(), path),
            "leaf {} should verify",
            i
        );
    }
}

#[test]
fn test_verify_proof_inclusion_rejects_tampered_leaf() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-TAMPER");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-TAMPER",
    );
    let (leaves, _, n23, root) = four_leaf_tree(&env);
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);

    let path = vec![&env, leaves.get(1).unwrap(), n23];
    let forged = merkle_leaf(&env, "offer-letter-EDITED.pdf");
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &forged, &path));
}

#[test]
fn test_verify_proof_inclusion_rejects_incorrect_path() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-PATH");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-PATH",
    );
    let (leaves, n01, n23, root) = four_leaf_tree(&env);
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);
    let leaf0 = leaves.get(0).unwrap();

    // Wrong sibling at the second level.
    let wrong_sibling = vec![&env, leaves.get(1).unwrap(), n01.clone()];
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaf0, &wrong_sibling));
    // Truncated path.
    let truncated = vec![&env, leaves.get(1).unwrap()];
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaf0, &truncated));
    // Empty path for a multi-leaf tree.
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaf0, &Vec::new(&env)));
    // Extra trailing node.
    let extended = vec![&env, leaves.get(1).unwrap(), n23.clone(), n23];
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaf0, &extended));
}

#[test]
fn test_verify_proof_inclusion_single_leaf_tree() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-SINGLE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-SINGLE",
    );
    let only = merkle_leaf(&env, "only-item.pdf");
    client.submit_proof_root(&recruiter, &eng_id, &0, &only);

    assert!(client.verify_proof_inclusion(&eng_id, &0, &only, &Vec::new(&env)));
    assert!(!client.verify_proof_inclusion(
        &eng_id,
        &0,
        &merkle_leaf(&env, "other-item.pdf"),
        &Vec::new(&env)
    ));
}

#[test]
fn test_verify_proof_inclusion_rejects_overlong_path() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-DEEP");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-DEEP",
    );

    // Build a genuine 33-level chain so only the depth cap can reject it.
    let leaf = merkle_leaf(&env, "deep-item");
    let mut path: Vec<BytesN<32>> = Vec::new(&env);
    let mut node = leaf.clone();
    for i in 0..33u32 {
        let sibling = merkle_leaf(&env, if i % 2 == 0 { "even" } else { "odd" });
        node = merkle_parent(&env, &node, &sibling);
        path.push_back(sibling);
    }
    client.submit_proof_root(&recruiter, &eng_id, &0, &node);

    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaf, &path));
}

#[test]
fn test_rejected_root_proof_is_no_longer_verifiable() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-REJECT");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-REJECT",
    );
    let (leaves, _, n23, root) = four_leaf_tree(&env);
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);

    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    assert_eq!(client.get_proof_merkle_root(&eng_id, &0), None);
    let path = vec![&env, leaves.get(1).unwrap(), n23];
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaves.get(0).unwrap(), &path));
}

#[test]
fn test_single_hash_resubmission_replaces_root() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-REPLACE");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-REPLACE",
    );
    let (leaves, _, n23, root) = four_leaf_tree(&env);
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    client.submit_proof(&recruiter, &eng_id, &0, &String::from_str(&env, "ipfs://single"));

    assert_eq!(client.get_proof_merkle_root(&eng_id, &0), None);
    let path = vec![&env, leaves.get(1).unwrap(), n23];
    assert!(!client.verify_proof_inclusion(&eng_id, &0, &leaves.get(0).unwrap(), &path));
}

#[test]
fn test_root_resubmission_replaces_root() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-REROOT");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-REROOT",
    );
    let (_, _, _, root) = four_leaf_tree(&env);
    let new_root = merkle_leaf(&env, "revised-evidence-set");
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);
    client.raise_dispute(&company, &eng_id, &0, &String::from_str(&env, "dispute"));
    client.cast_arbiter_vote(&arbiter, &eng_id, &0, &false);

    client.submit_proof_root(&recruiter, &eng_id, &0, &new_root);

    assert_eq!(client.get_proof_merkle_root(&eng_id, &0), Some(new_root));
}

#[test]
#[should_panic(expected = "DuplicateProofHash")]
fn test_duplicate_proof_root_rejected_across_milestones() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-DUP");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-DUP",
    );
    let (_, _, _, root) = four_leaf_tree(&env);
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof_root(&recruiter, &eng_id, &1, &root);
}

#[test]
fn test_proof_roots_are_tracked_per_milestone() {
    let (env, contract_id, token_id, company, recruiter, arbiter) = setup();
    let client = HireSettleContractClient::new(&env, &contract_id);
    let eng_id = String::from_str(&env, "ENG-486-PERMS");
    create_standard_engagement(
        &env, &client, &token_id, &company, &recruiter, &arbiter, "ENG-486-PERMS",
    );
    let (leaves, _, n23, root) = four_leaf_tree(&env);
    let other_root = merkle_leaf(&env, "retention-evidence-set");
    client.submit_proof_root(&recruiter, &eng_id, &0, &root);

    advance_ledger(&env, 31 * 17_280);
    client.unlock_milestone(&eng_id, &1);
    client.submit_proof_root(&recruiter, &eng_id, &1, &other_root);

    assert_eq!(client.get_proof_merkle_root(&eng_id, &0), Some(root));
    assert_eq!(client.get_proof_merkle_root(&eng_id, &1), Some(other_root));
    let path = vec![&env, leaves.get(1).unwrap(), n23];
    assert!(client.verify_proof_inclusion(&eng_id, &0, &leaves.get(0).unwrap(), &path));
    assert!(!client.verify_proof_inclusion(&eng_id, &1, &leaves.get(0).unwrap(), &path));
}
