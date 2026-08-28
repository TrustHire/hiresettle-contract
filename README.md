h# HireSettle — Recruitment Fee Settlement Contract

A [Soroban](https://soroban.stellar.org/) smart contract deployed on Stellar for managing recruitment fee settlements through milestone-based escrow payments. Built with `#![no_std]` Rust and the Soroban SDK.

---
## Setup & Installation

### Prerequisites

Before getting started, ensure you have:

- Rust (latest stable version)
- Cargo
- Soroban CLI
- Stellar CLI
- Git

### Clone the Repository

```bash
git clone https://github.com/TrustHire/hiresettle-contract.git
cd hiresettle-contract
```

### Build the Contract

```bash
cd contracts/hiresettle
cargo build
```

### Run the Tests

```bash
cargo test
```

## Overview

## Glossary

- **Engagement**: An agreement or project contract initiated between parties on the platform.
- **Milestone**: A specific checkpoint or deliverable within an engagement that triggers payouts upon approval.
- **Retention**: Funds held in escrow until all engagement conditions or warranty periods are satisfied.
- **Cosigner**: An authorized third party required to sign or approve specific actions or state transitions.
- **Super-arbiter**: A high-level administrative entity with authority to resolve escalations and dispute deadlocks.
- **Quorum**: The minimum threshold of votes or signatures required to execute a governance or arbitration action.
- **Amendment**: A formal modification proposed and agreed upon to update the terms of an active engagement.


HireSettle governs the relationship between a **hiring company** and a **recruiter** by locking the total agreed fee in an escrow wallet at engagement creation. As the recruiter delivers on each milestone (placement, 30-day retention, 90-day retention, etc.), the company confirms the deliverable, releasing a proportional payment from escrow. If disputes arise, an M-of-N arbiter panel votes to resolve them. The contract handles the entire lifecycle: creation, proof submission, confirmation, dispute, replacement, early exit, cancellation, and expiry.

### Core Concepts

- **Engagement** — A single recruitment contract identified by a unique string ID. Stores the company, recruiter, arbiters, token, total fee, milestone list, and lifecycle status.
- **Milestone** — A discrete payment trigger with a name, payment percentage, type (`Placement` or `Retention`), time-gate ledger, proof hash, and status (`Locked` → `Pending` → `ProofSubmitted` → `Confirmed` / `Disputed` → `Resolved`).
- **Escrow** — Funds are transferred from the company to the contract at creation. Milestone payments release proportional amounts to the recruiter (minus platform fees). Remaining escrow is refunded on cancellation or expiry.
- **Dispute Resolution** — The company raises a dispute on a proof-submitted milestone. Arbiters vote approve/reject; if `approve_votes >= quorum`, payment is released; if `reject_votes > arbiters.len() - quorum`, the proof is cleared and the milestone returns to `Pending`.

### Milestone State Machine

Each milestone transitions through the following states. For `Retention` milestones the initial state is `Locked`; `Placement` milestones start in `Pending`.

```

   ┌────────┐  unlock   ┌─────────┐  submit   ┌───────────────┐
   │ Locked │ ────────> │ Pending │ ────────> │ ProofSubmitted │
   └────────┘           └─────────┘            └───────┬───────┘
                        ^                              │
                        │          ┌────────────────────┼────────────────┐
                        │          │                    │                │
                        │          v                    v                │
                        │   ┌──────────┐         ┌──────────┐           │
                        │   │ Resolved │         │ Disputed │           │
                        │   └──────────┘         └─────┬────┘           │
                        │                              │                │
                        │              ┌───────────────┼────────────┐   │
                        │              │               │            │   │
                        │              v               v            │   │
                        │    ┌──────────────────┐ ┌───────────┐     │   │
                        │    │ Reject quorum    │ │ Approve   │     │   │
                        │    │ (back to Pending)│ │ quorum    │     │   │
                        │    └──────────────────┘ │ (released)│     │   │
                        │                         └─────┬─────┘     │   │
                        │                               │           │   │
                        └───────────────────────────────┼───────────┘   │
                                                         │               │
                                                         v               │
                                                    ┌──────────┐         │
                                                    │ Confirmed│ <───────┘
                                                    └──────────┘

   force_confirm_milestone can also move ProofSubmitted → Confirmed
   after the confirm window elapses.

   batch_confirm_milestones can confirm multiple ProofSubmitted
   milestones atomically.
```

**Dispute Resolution Flow** (via `cast_arbiter_vote`):

1. Company calls `raise_dispute` → milestone moves to `Disputed`.
2. Arbiters call `cast_arbiter_vote(approve=true/false)`.
3. When `approve_votes >= quorum` → payment released, milestone → `Confirmed`.
4. When `reject_votes > arbiters.len() - quorum` → proof cleared, milestone returns to `Pending`.
5. Vote tally stored as `ArbiterVoteRecord`; duplicate votes rejected.

---

## Key Features

| Feature | Description |
|---|---|
| **Milestone-based escrow** | Funds locked at creation; released per-milestone on confirmation. Percentages sum to 100%. |
| **Placement & Retention types** | Placement starts `Pending`; retention starts `Locked` and requires a ledger time-gate to elapse. |
| **Multi-arbiter disputes** | M-of-N arbiter voting to resolve disputes. Approve releases payment; reject resets milestone. |
| **Co-recruiter split** | Optional `co_recruiter` address with configurable basis-point split for shared-fee engagements. |
| **Proof cooldown** | Configurable minimum ledger gap between proof resubmissions (default ~4 hours). |
| **Admin configuration** | Platform fee (up to 5%), token allowlist, max milestones, max retention days, inactivity timeout, confirm/dispute windows, arbiter fee (up to 2%), proof hash length, ledgers-per-day, storage TTL, upgrade lock duration. |
| **Auto / force confirm** | If the company does not act within the confirm window (~5 days), any address can force-confirm. |
| **Batch confirmation** | Confirm multiple milestones atomically in a single transaction. |
| **Recruiter early exit** | Recruiter requests exit; company accepts (refunds remaining escrow) or rejects (returns to Active). |
| **Candidate replacement** | Company requests replacement, resetting milestones and adjusting retention timers. |
| **Engagement expiry** | Permissionless keeper function — expires after an inactivity timeout (~60 days), refunds company. |
| **Amendment proposals** | Company or recruiter proposes a `payment_percent` change; the other party must accept within a TTL. Amendment history is logged (capped at 20 entries). |
| **Arbiter succession** | Arbiters can nominate and claim successors for their slots. |
| **Contract upgrade** | Admin proposes a WASM upgrade with a mandatory time-lock (~1 day); execution is permissionless after the lock. |
| **Admin renouncement** | Admin can permanently renounce, making the contract immutable. All admin-gated functions fail after renouncement. |
| **Contract PDF attestation** | Optional `contract_pdf_hash` (e.g. SHA-256 of the signed PDF) stored at creation for audit trail. |
| **Engagement listing** | Per-company paginated engagement ID list (issue #35) and global engagement counter (issue #34). |
| **Unlock progress query** | `get_unlock_progress()` returns `(unlocked_count, total)` — how many milestones are past `Locked` status. |

---

## Data Types

### `Engagement`
The full on-chain record:
- `id`, `company`, `recruiter`, `arbiters`, `quorum`, `token`
- `total_amount`, `released_amount`, `job_title`
- `metadata_hash` (optional IPFS CID), `contract_pdf_hash` (optional attestation hash)
- `created_at_ledger`, `last_activity_ledger`
- `milestones` (Vec<Milestone>), `status`, `co_recruiter`, `recruiter_split_bps`

### `Milestone`
- `name`, `payment_percent`, `kind` (Placement | Retention), `valid_after_ledger`
- `proof_hash`, `status` (Locked | Pending | ProofSubmitted | Confirmed | Disputed | Resolved), `proof_submitted_at`

### `EngagementConfig`
Passed at creation to stay within Soroban's 10-parameter limit:
- `metadata_hash` (Option<String>), `contract_pdf_hash` (Option<String>)
- `co_recruiter` (Option<Address>), `recruiter_split_bps` (u32)

### `EngagementStatus`
`Active` → `Completed` | `Cancelled` | `Expired` | `ReplacementRequested` | `ExitRequested`

### `EngagementSummary`

```rust
pub struct EngagementSummary {
    pub id: String,                  // unique engagement identifier
    pub job_title: String,           // short title set at creation
    pub company: Address,
    pub recruiter: Address,
    pub total_amount: i128,          // total fee locked (stroops)
    pub released_amount: i128,       // amount paid out so far
    pub status: EngagementStatus,
    pub milestone_count: u32,        // total milestones (does not change)
    pub created_at_ledger: u32,
}
```

Lightweight read-only view returned by `get_engagement_summary`, omitting the milestone vector for efficient dashboard listing.

### `ConfigSnapshot`

```rust
pub struct ConfigSnapshot {
    pub version: String,
    pub admin: Address,
    pub admin_renounced: bool,
    pub paused: bool,
    pub platform_fee_bps: u32,
    pub platform_fee_treasury: Address,
    pub arbiter_fee_bps: u32,
    pub super_arbiter: Option<Address>,
    pub confirm_window_ledgers: u32,
    pub dispute_window_ledgers: u32,
    pub proof_cooldown_ledgers: u32,
    pub due_soon_window_ledgers: u32,
    pub amendment_ttl_ledgers: u32,
    pub extension_ttl_ledgers: u32,
    pub inactivity_timeout_ledgers: u32,
    pub storage_ttl_extend_to: u32,
    pub upgrade_lock_duration_ledgers: u32,
    pub ledgers_per_day: u32,
    pub max_milestones: u32,
    pub max_retention_days: u32,
    pub max_replacements: u32,
    pub max_active_per_company: u32,
    pub max_proof_hash_length: u32,
    pub min_engagement_amount: i128,
    pub token_allowlist_enabled: bool,
}
```

Every admin-configurable contract parameter in one read-only struct, returned by
`get_config_snapshot`. Each field mirrors the value its dedicated getter would
return at the same ledger, including the defaults applied to parameters that were
never explicitly set.

### `AmendmentEntry`

```rust
pub struct AmendmentEntry {
    pub proposer: Address,           // company or recruiter who proposed
    pub old_payment_percent: u32,
    pub new_payment_percent: u32,
    pub ledger: u32,                 // ledger when the amendment was accepted
}
```

History entry recorded when a milestone payment-percent amendment is accepted.

### `AmendmentProposal`

```rust
pub struct AmendmentProposal {
    pub proposer: Address,
    pub new_payment_percent: u32,
    pub proposed_at_ledger: u32,
    pub expires_at_ledger: u32,      // proposal TTL; expires if not accepted in time
}
```

A pending milestone amendment proposal; either party may propose and the other must accept before expiry.

### `ArbiterSetup`

```rust
pub struct ArbiterSetup {
    pub arbiters: Vec<Address>,      // ordered list eligible to vote on disputes
    pub quorum: u32,                 // M-of-N votes required to resolve
}
```

Bundled argument passed to `create_engagement` to configure the arbitration panel (keeps parameter count within Soroban's 10-arg limit).

### `ArbiterVoteRecord`

```rust
pub struct ArbiterVoteRecord {
    pub approve_votes: u32,
    pub reject_votes: u32,
    pub voted: Vec<Address>,         // prevents double-voting
}
```

Per-dispute vote tally stored on-chain until the dispute resolves (approve or reject quorum is reached).

### `ArbiterVoteCounts`

```rust
pub struct ArbiterVoteCounts {
    pub approve_votes: u32,
    pub reject_votes: u32,
}
```
```

Returned by `get_arbiter_votes` — lightweight view of the current tally without the voter list.

### `ArbiterNomination`

```rust
pub struct ArbiterNomination {
    pub current: Address,            // nominating arbiter
    pub nominee: Address,            // successor
}
```

Stored under `DataKey::PendingArbiter` during arbiter succession; the nominee calls `claim_arbiter` to finalise.

### `UpgradeProposal`

```rust
pub struct UpgradeProposal {
    pub new_wasm_hash: BytesN<32>,   // new contract WASM hash
    pub execute_after_ledger: u32,   // earliest ledger at which execution is allowed
}
```

Pending contract WASM upgrade proposal; subject to an admin-configurable time-lock (default 17,280 ledgers ≈ 1 day).

## `PlatformFee`

The contract can deduct a configurable fee from every milestone payout before the recruiter is paid.

### Configuration
- **Set Fee**: `set_platform_fee(admin, bps, treasury)` — admin only. `bps` is capped at 500 (5%); exceeding it panics with `FeeTooHigh`.
- **Read Fee**: `get_platform_fee()` — returns `(bps, treasury)`. Permissionless.
- **Default**: 0 bps, treasury = admin, set at `init`. No fee is taken until an admin raises it.

### How it's applied
On `confirm_milestone`, `batch_confirm_milestones`, and `force_confirm_milestone`:

Platform fee is deducted from each milestone payment before release to the recruiter.

gross_share = total_amount × payment_percent ÷ 100
fee_amount = gross_share × platform_fee_bps ÷ 10_000
net_payment = gross_share − fee_amount // this is what the recruiter actually receives

`fee_amount` is transferred to `treasury`; a `platform_fee_collected` event `(milestone_index, fee_amount, treasury)` is emitted whenever `fee_amount > 0` (no event when the fee is 0). Disputes resolved via `cast_arbiter_vote` do **not** deduct the platform fee — they deduct a separate, arbiter-only fee instead (see `set_arbiter_fee`).

When fee tiers are configured, `platform_fee_bps` in the formula above is the **resolved** rate for that engagement's `total_amount` (see [Fee tiers](#fee-tiers)), not necessarily the base `PlatformFee.bps` returned by `get_platform_fee()`.

### Fee tiers

Admin can scale the platform fee down for larger engagements instead of charging every size the same flat `bps`. Each bracket is a `FeeTier`. Tiers are optional: an empty list means the base `PlatformFee.bps` applies to every engagement.

#### Configuration
- **Set Tiers**: `set_fee_tiers(admin, tiers)` — admin only. Replaces the entire list. Pass an empty vector to clear all tiers (back to a flat fee). Emits `fee_tiers_set` with payload `tiers.len()`.
- **Read Tiers**: `get_fee_tiers()` — returns `Vec<FeeTier>`. Permissionless. Empty vector = no tiering.
- **Default**: no tiers stored. `get_platform_fee()` still returns the base `(bps, treasury)` even when tiers are set; it does not return the resolved rate for a given amount.

```rust
pub struct FeeTier {
    pub threshold: i128,             // inclusive min engagement.total_amount
    pub bps: u32,                    // platform fee for this bracket (basis points)
}
```

#### How a tier is selected

At fee-calculation time the contract walks the stored list from **highest `threshold` to lowest** and uses the first tier whose `threshold` is `<= engagement.total_amount`. If no tier matches, the base `PlatformFee.bps` applies.

Example with base fee `200` bps and two tiers `(10_000_000, 150)` then `(100_000_000, 100)`:

| `total_amount` | Rate used |
|---|---|
| `5_000_000` | `200` (below every threshold → base) |
| `10_000_000` | `150` |
| `100_000_000` | `100` (highest matching threshold wins) |

#### Validation

`set_fee_tiers` panics unless all of the following hold:

| Rule | Panic |
|---|---|
| At most 10 tiers | `too many fee tiers` |
| Each `bps` ≤ current base `PlatformFee.bps` | `tier bps exceeds base platform fee` |
| Each `threshold` > 0 | `tier threshold must be positive` |
| Thresholds strictly ascending | `tiers must be sorted by ascending threshold` |

The 500 bps `FeeTooHigh` cap is enforced on `set_platform_fee`, not here. A tier cannot charge **more** than the current base fee (tiers are discounts for larger size, not surcharges). Caller must also be admin and the contract must not be paused (`ContractPaused`, `NoAdmin`, `unauthorized`).

### Referral discount

Engagements may carry an optional `referrer` address (set at creation via
`Engagement` / `EngagementConfig`). That field alone does not change fees.

Admin setup:

- `add_referrer` / `remove_referrer` / `get_referrers` — maintain the recognised
  referrer list stored under `DataKey::Referrers`.
- `set_referral_discount_bps(admin, bps)` / `get_referral_discount_bps()` —
  configure how many basis points to subtract from the platform fee when the
  engagement’s `referrer` is on that list (max 500 bps, same cap as the platform
  fee). Default discount is `0`.

At milestone confirmation (`confirm_milestone`, `batch_confirm_milestones`,
`force_confirm_milestone`), the contract computes an **effective** platform-fee
rate as follows:

1. Start from the configured platform fee `bps` (or the matching **fee tier**
   rate when `set_fee_tiers` is in use — larger `total_amount` can qualify for a
   lower tier `bps`).
2. If `engagement.referrer` is `Some(addr)` **and** `addr` is in the recognised
   referrer list, subtract `referral_discount_bps` from that rate.
3. Floor at `0` (the platform fee never goes negative).

Then:

```text
fee_amount  = payment × effective_bps ÷ 10_000
net_payment = payment − fee_amount 
```
If there is no referrer, the referrer is not on the list, or the discount is `0`, the effective rate is unchanged from the base / tier rate.

Referral discount applies only to the **platform** fee path above. Arbiter fees from dispute resolution 
(`cast_arbiter_vote` / `set_arbiter_fee`) are separate and are not reduced by the referral discount.

```rust
pub struct PlatformFee {
    pub bps: u32,                    // fee in basis points (max 500 = 5%)
    pub treasury: Address,           // fee recipient
}
```

### `DataKey`

```rust
pub enum DataKey {
    Engagement(String),              // full engagement record by ID (persistent)
    Admin,                           // current admin address (instance)
    PendingArbiter(String),          // pending arbiter succession nomination
    PlatformFee,                     // bps + treasury config (persistent)
    FeeTiers,                        // Vec<FeeTier> size brackets (persistent)
    Paused,                          // pause-guard bool (persistent)
    PendingAdmin,                    // nominated admin successor (persistent)
    ProofCooldown,                   // min ledgers between resubmissions (instance)
    LastProofAt(String, u32),        // ledger of last proof submission
    ArbiterVotes(String, u32),       // running vote tally for a dispute
    AmendmentProposal(String, u32),  // active amendment proposal
    AmendmentLog(String, u32),       // amendment history entries
    AmendmentTTL,                    // proposal expiry duration (persistent)
    // … additional keys for counts, allowlist, timeouts, etc.
}
```

Contract storage key space enumerating all persistent and instance-stored values. Instance keys reset between transactions; persistent keys survive across ledgers.

---

## Amendments

Either the company or the recruiter may propose a change to a milestone's `payment_percent`. Amendments are scoped to a single milestone per proposal and require explicit acceptance from the counterparty before taking effect.

### Propose → Accept / Reject Flow

1. **Propose** — `propose_amendment(proposer, engagement_id, milestone_index, new_payment_percent)`
   - Caller must be the engagement's `company` or `recruiter`; must sign the transaction.
   - `new_payment_percent` is validated to be within 0–100 (inclusive).
   - A new proposal overwrites any existing pending proposal for the same milestone (only one pending per milestone at a time).
   - Emits an `amendment_proposed` event.
   - The pending state is stored as an [`AmendmentProposal`](#amendmentproposal) struct under `DataKey::AmendmentProposal(engagement_id, milestone_index)`.

2. **Accept** — `accept_amendment(acceptor, engagement_id, milestone_index)`
   - Caller must be the *other* party (the one who did **not** propose). A proposer cannot accept their own proposal.
   - The milestone's `payment_percent` is updated to the proposed value immediately.
   - An [`AmendmentEntry`](#amendmententry) is appended to the milestone's amendment log (see below) recording the old/new percentages, the proposer, and the acceptance ledger.
   - The pending proposal is cleared from storage.
   - Emits an `amendment_accepted` event.

3. **Reject** — `reject_amendment(rejector, engagement_id, milestone_index)`
   - Caller must be the *other* party (the one who did **not** propose). A proposer cannot reject their own proposal.
   - The pending proposal is cleared from storage without any change to the milestone.
   - Emits an `amendment_rejected` event with reason `declined`.

### TTL and Expiry

The contract exposes several admin-configurable time-based constants (TTLs, windows, and durations). The table below lists every such constant, its default value, and the setter/getter pair used to manage it. All values are denominated in **ledgers** (≈ 5 s each) unless otherwise noted.

| Constant | Default (ledgers) | Approx. Duration | Setter | Getter |
|---|---|---|---|---|
| `storage_ttl_extend_to` | 1,036,800 | ~60 days | `set_storage_ttl_extend_to(admin, ledgers)` | `get_storage_ttl_extend_to()` |
| `confirm_window` | 86,400 | ~5 days | `set_confirm_window(admin, ledgers)` | `get_confirm_window()` |
| `dispute_window` | 51,840 | ~3 days | `set_dispute_window(admin, ledgers)` | `get_dispute_window()` |
| `amendment_ttl` | 17,280 | ~1 day | `set_amendment_ttl(admin, ledgers)` | `get_amendment_ttl()` |
| `extension_ttl` | 17,280 | ~1 day | `set_extension_ttl(admin, ledgers)` | `get_extension_ttl()` |
| `upgrade_lock_duration` | 17,280 | ~1 day | `set_upgrade_lock_duration(admin, ledgers)` | `get_upgrade_lock_duration()` |
| `inactivity_timeout` | 1,036,800 | ~60 days | `set_inactivity_timeout_ledgers(admin, ledgers)` | `get_inactivity_timeout_ledgers()` |
| `due_soon_window` | 17,280 | ~1 day | `set_due_soon_window(admin, ledgers)` | `get_due_soon_window()` |
| `proof_cooldown` | 2,880 | ~4 hours | `set_proof_cooldown(admin, ledgers)` | `get_config_snapshot().proof_cooldown_ledgers` |
| `super_arbiter_response_window` | 51,840 | ~3 days | `set_super_arbiter_deadline(admin, ledgers)` | `get_super_arbiter_deadline()` |
| `ledgers_per_day` | 17,280 | 1 day | `set_ledgers_per_day(admin, value)` | `get_ledgers_per_day()` |

> **Tip:** `get_config_snapshot()` returns **all** of the above values in a single read-only call, so off-chain indexers do not need ~11 separate round-trips to reconstruct the current configuration.

Every proposal carries an expiry ledger computed as `proposed_at_ledger + amendment_ttl` (see [`AmendmentProposal.expires_at_ledger`](#amendmentproposal)).

- The default TTL is **17,280 ledgers** (≈ 1 day at 5 s/ledger).
- Admin can change the default globally via `set_amendment_ttl(ledgers)`; the current value is queried with `get_amendment_ttl()`.
- If the current ledger exceeds `expires_at_ledger`, the proposal is considered *expired*:
  - Calling `accept_amendment` on an expired proposal clears it, emits an `amendment_rejected` event with reason `expired`, and panics with `amendment_expired`.
  - `get_pending_amendment` automatically treats expired proposals as non-existent and returns `None`.
- Expired proposals do **not** auto-clean from storage on ledger tick; they are lazily cleared on the next `accept_amendment`, `reject_amendment`, or overwritten by the next `propose_amendment` for the same milestone.

### Withdrawing a Proposal

`accept_amendment` and `reject_amendment` are both restricted to the
counterparty, so before issue #238 a proposer who changed their mind had to wait
out the TTL. `withdraw_amendment_proposal(proposer, engagement_id,
milestone_index)` lets the **original proposer** clear their own pending proposal
immediately.

Only the address recorded on the proposal may withdraw it — being the
engagement's company or recruiter is not sufficient. Afterwards
`get_pending_amendment` reports `None` and a fresh proposal can be made for the
same milestone. The amendment log is untouched: a withdrawn proposal was never
applied, so it is not part of the milestone's amendment history. An
expired-but-uncleared proposal can still be withdrawn, since that is exactly the
storage cleanup the caller intends.

### What an Amendment Can Change

Only one field is mutable via the amendment system:

| Field | Type | Description |
|---|---|---|
| `milestone.payment_percent` | `u32` | Percentage of `total_amount` released when the milestone confirms. Must be 0–100 inclusive. |

An amendment does **not** change the total escrow, milestone status, proof hash, retention time-gates, arbiter configuration, or any other engagement field. Percentage-sum validation across all milestones is not re-enforced at amendment time; integrators are expected to ensure the combined set across all milestones still sums to 100 after applying accepted amendments.

### Amendment History (Log)

Each time an amendment is accepted, an [`AmendmentEntry`](#amendmententry) is appended to the per-milestone log:

- Accessible via `get_amendment_log(engagement_id, milestone_index)` which returns entries in chronological order (oldest first).
- The log is **FIFO-capped at 20 entries per milestone** — once the cap is reached, the oldest entry is evicted on the next accepted amendment.
- The pending proposal itself is not part of the log until it is accepted; use `get_pending_amendment` to inspect a live proposal.

---

## Feedback Ratings

Once an engagement reaches `Completed`, each side may rate the other once:

- `submit_recruiter_rating(company, engagement_id, rating)` — the company rates
  the recruiter (issue #242).
- `submit_company_rating(recruiter, engagement_id, rating)` — the recruiter
  rates the company (issue #243).

`rating` must be 1–5; anything else panics with `InvalidRating`. Ratings are
folded into a running tally keyed by the rated party's **address**, not the
engagement, so reputation accumulates across every engagement that address
completes. Query with `get_recruiter_rating(recruiter)` /
`get_company_rating(company)` (issue #244).

Each engagement contributes at most one rating per side — a second submission
panics with `AlreadyRated`, so neither party can inflate or bury a counterparty's
score by rating a single job repeatedly. The two sides are independent: both may
rate each other for the same engagement. Use `is_recruiter_rated` /
`is_company_rated` to hide a rating prompt instead of surfacing the failure.

Ratings are credited to whoever holds the role at completion time. If the
recruiter role was transferred mid-engagement via `accept_recruiter_transfer`,
the incoming address receives the rating — matching where the milestone payouts
went.

## Token Allowlist

The contract supports an optional allowlist to restrict which tokens can be used for escrow.

### Configuration
- **Toggle Allowlist**: The allowlist is toggled on or off using `set_token_allowlist_enabled(admin, enabled)`.
- **Add Token**: New tokens are added to the allowlist with `add_allowed_token(admin, token_address)`.
- **Remove Token**: Tokens are removed with `remove_allowed_token(admin, token_address)`.

### Engagement Creation
When the token allowlist is enabled, `create_engagement` will panic with `TokenNotAllowed` if the `token` passed is not in the allowed tokens list. If the allowlist is disabled, any valid SAC token is accepted.

## Cosigners

A company or recruiter may register a **cosigner** — a secondary address that is
equally authorized to perform that party's actions (e.g. confirming milestones,
submitting proof, approving amendments). This lets a primary signer delegate
operations to a bot, vault, or backup key without sharing the primary key.

Registration is one-way per party and only the primary address may set its own
cosigner:

- `set_company_cosigner(company, cosigner)` — delegates company-side actions.
- `set_recruiter_cosigner(recruiter, cosigner)` — delegates recruiter-side actions.

Once registered, the contract's `is_authorized_company` / `is_authorized_recruiter`
checks accept **either** the primary signer **or** the cosigner. A cosigned action
is therefore still a single on-chain call — it just uses the cosigner as the
authenticated caller instead of the primary.

### Single-signer vs. cosigned flow

```mermaid
sequenceDiagram
    participant P as Primary (company/recruiter)
    participant C as Cosigner
    participant S as Contract

    Note over P,S: Normal single-signer call
    P->>S: action (e.g. confirm_milestone)
    S->>S: require_auth(P) + is_authorized?(caller == P)
    S-->>P: success

    Note over P,C,S: Cosigned call (after set_*_cosigner)
    P->>S: set_company_cosigner(P, C)
    S-->>S: store cosigner = C

    C->>S: action (same function, authenticated as C)
    S->>S: require_auth(C) + is_authorized?(caller == P OR caller == C)
    S-->>C: success
```

In both cases the function signature is identical; only the authenticated caller
changes. Integrators adding a cosigner do not need new entry points — they simply
route the existing call through the cosigner's key.

## Public Function Reference

### Admin & Configuration

Functions that manage contract-wide settings, admin succession, and operational state. All state-changing admin functions require the caller to authenticate as the current admin via `require_auth()`.

| Function | Caller | Purpose | Panics |
|---|---|---|---|
| `set_platform_fee(admin, bps, treasury)` | Admin | Set platform fee in basis points (max 500 = 5%) and recipient treasury. | `ContractPaused`, `NoAdmin`, `unauthorized`, `FeeTooHigh` |
| `get_platform_fee()` → `(u32, Address)` | Anyone | Return current **base** `(bps, treasury)`; defaults to `(0, admin)`. Does not resolve fee tiers. | — |
| `set_fee_tiers(admin, tiers)` | Admin | Replace the fee-tier list (max 10, strictly ascending `threshold`, each `bps` ≤ base platform fee). Empty vec clears tiers. | `ContractPaused`, `NoAdmin`, `unauthorized`, `too many fee tiers`, `tier bps exceeds base platform fee`, `tier threshold must be positive`, `tiers must be sorted by ascending threshold` |
| `get_fee_tiers()` → `Vec<FeeTier>` | Anyone | Return configured tiers; empty vec means flat base fee. | — |
| `set_version(admin, version)` | Admin | Set contract version string (max 32 chars). | `NoAdmin`, `unauthorized`, `VersionTooLong` |
| `set_min_amount(admin, amount)` | Admin | Set minimum engagement amount in raw token units. | `NoAdmin`, `unauthorized` |
| `pause(admin)` | Admin | Pause all state-changing operations. | `NoAdmin`, `unauthorized` |
| `unpause(admin)` | Admin | Resume state-changing operations. | `NoAdmin`, `unauthorized` |
| `is_paused()` → `bool` | Anyone | Return `true` if contract is currently paused. | — |
| `pause_engagement(admin, engagement_id)` | Admin | Quarantine a single engagement, blocking its state-changing operations without halting the contract. Works even when globally paused. | `NoAdmin`, `unauthorized`, `engagement not found` |
| `unpause_engagement(admin, engagement_id)` | Admin | Lift the quarantine on a single engagement. | `NoAdmin`, `unauthorized`, `engagement not found` |
| `is_engagement_paused(engagement_id)` → `bool` | Anyone | Return `true` if this specific engagement is quarantined. Unknown IDs return `false`. | — |
| `set_due_soon_window(admin, ledgers)` | Admin | Set how many ledgers before a retention deadline a milestone counts as "due soon" (default 17 280 ≈ 1 day). | `NoAdmin`, `unauthorized`, `InvalidDueSoonWindow` |
| `nominate_admin(current_admin, new_admin)` | Admin | Nominate an admin successor. The nominee must call `claim_admin` to finalize. | `ContractPaused`, `NoAdmin`, `unauthorized` |
| `claim_admin(nominee)` | Nominee | Claim admin rights after nomination. | `ContractPaused`, `no pending admin nomination`, `unauthorized` |
| `get_pending_admin()` → `Option<Address>` | Anyone | Return the pending admin nominee, if any. | — |
| `set_proof_cooldown(admin, ledgers)` | Admin | Set minimum ledger gap between proof resubmissions on the same milestone. Works even when paused. | `contract not initialised`, `unauthorized` |

Additional admin functions (documented elsewhere): `init`, `renounce_admin`, `set_ledgers_per_day`, `set_max_retention_days`, `set_max_milestones`, `set_inactivity_timeout_ledgers`, `set_storage_ttl_extend_to`, `set_confirm_window`, `set_dispute_window`, `set_max_proof_hash_length`, `set_arbiter_fee`, `set_amendment_ttl`, `set_upgrade_lock_duration`, `propose_upgrade`, `add_allowed_token`, `remove_allowed_token`, `set_token_allowlist_enabled`.

### Engagement Lifecycle
`create_engagement`, `unlock_milestone`, `notify_milestone_due_soon`, `submit_proof`, `confirm_milestone`, `batch_confirm_milestones`, `force_confirm_milestone`, `raise_dispute`, `cast_arbiter_vote`, `request_replacement`, `cancel_engagement`, `top_up_escrow`, `request_early_exit`, `accept_early_exit`, `reject_early_exit`, `expire_engagement`

### Per-Engagement Pause (Quarantine)

Beyond the contract-wide `pause` / `unpause`, an admin can freeze a **single**
engagement with `pause_engagement(admin, engagement_id)`. This lets an admin
quarantine one problematic engagement without halting every other engagement in
the contract.

While an engagement is quarantined, every lifecycle call for that ID is rejected
with `EngagementPaused`: milestone unlock, proof submission, confirmation
(including `batch_confirm_milestones` and `force_confirm_milestone`), disputes
and arbiter voting, escalation, replacements, amendments, milestone extensions,
role transfers, escrow top-ups, early exit, cancellation, and expiry.

Deliberately still permitted:

- **Read-only queries** — quarantine must not blind indexers or the parties to
  the engagement's own state.
- **`admin_replace_arbiter`** — an engagement is frequently quarantined *because*
  its arbiter panel is the problem, so the admin keeps the tool to fix it.
- **`pause_engagement` / `unpause_engagement`** themselves, so the admin can
  always lift the freeze — including while the contract is globally paused,
  which is exactly when triage is needed.

The two pause mechanisms are orthogonal and both must pass for a call to
proceed: an engagement can be quarantined while the contract runs normally, and
calling `unpause` on the contract does **not** lift a per-engagement quarantine.

#### Interaction with the global pause

The two mechanisms are tracked in **separate storage flags** and enforced by two
independent guards — `assert_not_paused` for the contract-wide pause and
`assert_engagement_not_paused` for the quarantine (see `lib.rs`). A
state-changing call must clear **both** guards to proceed; they are an **AND**,
not an OR.

Because they are independent, neither pause clears the other:

- `unpause(admin)` — the contract-wide resume **does not** lift a per-engagement
  quarantine. An engagement quarantined before the contract was paused stays
  quarantined after the contract is resumed; its lifecycle calls keep failing
  with `EngagementPaused` until `unpause_engagement` is called for that ID.
- `unpause_engagement(admin, engagement_id)` — lifting a single engagement's
  quarantine **does not** resume a globally paused contract. While the whole
  contract is paused, that engagement's calls still fail with `ContractPaused`
  regardless of its own quarantine state.

The combined effect for any given engagement:

| Global pause | Engagement quarantine | State-changing call |
|---|---|---|
| off | off | proceeds |
| off | on  | rejected — `EngagementPaused` |
| on  | off | rejected — `ContractPaused` |
| on  | on  | rejected — `ContractPaused` (global guard checked first) |

**Example.** An admin quarantines `ENG-42` with `pause_engagement` while the
contract is running, then later pauses the whole contract with `pause` for
maintenance:

1. `pause_engagement(admin, "ENG-42")` → `is_engagement_paused("ENG-42")` is
   `true`, while `is_paused()` is still `false`.
2. `pause(admin)` → `is_paused()` becomes `true`; `ENG-42` remains quarantined.
3. A lifecycle call on `ENG-42` now fails with `ContractPaused` (the global guard
   is checked before the engagement guard).
4. `unpause(admin)` → `is_paused()` is `false` again, **but** `ENG-42` is still
   quarantined, so its calls still fail with `EngagementPaused`.
5. `unpause_engagement(admin, "ENG-42")` → quarantine lifted; lifecycle calls on
   `ENG-42` proceed normally.

To check ahead of a write, query both `is_paused()` and
`is_engagement_paused(engagement_id)` — a call is only accepted when **both**
return `false`.

### Milestone Due-Soon Notifications

Soroban contracts cannot schedule their own work, so the due-soon signal follows
the same permissionless-keeper shape as `unlock_milestone` and
`escalate_dispute`. Once a `Locked` retention milestone is within
`get_due_soon_window()` ledgers of its `valid_after_ledger`, anyone may call
`notify_milestone_due_soon(engagement_id, milestone_index)` to emit a
`milestone_due_soon` event. Off-chain notification services subscribe to that
single event stream instead of polling `ledgers_until_unlock` for every locked
milestone.

The event fires **at most once per deadline** — a second call panics with
`DueSoonAlreadyNotified`. The record is cleared whenever the deadline itself
moves (an accepted milestone extension, or a replacement restarting the
retention timer) or the milestone unlocks, so a rescheduled milestone is
announced again for its new deadline. Use `is_milestone_due_soon` to test the
window condition and `is_milestone_due_soon_notified` to skip milestones that
have already been announced.

The call deliberately does **not** touch `last_activity_ledger`: a notification
is not engagement activity, and bumping it would let a keeper postpone
`expire_engagement` indefinitely.

### Engagement Tags

Engagements can be labelled with free-form string tags so off-chain tooling can
group and filter them (e.g. by client, role, or campaign). Tags are managed by
the engagement's **company** (or its registered co-signer) and are only ever read
by the tag-based queries — they do not affect escrow, payments, or lifecycle.

**Limits** (enforced when the engagement is created via `config.tags`):

- **Max tags per engagement: 10** — creating with more panics `TooManyTags`.
- **Max tag length: 32 characters** — a longer tag panics `TagTooLong`; a
  zero-length tag panics `TagEmpty`.
- Tags are de-duplicated at creation, so a repeated tag lists the engagement once
  in its index.

These bounds exist so storage stays predictable regardless of caller input. The
cap is checked at creation time; `add_engagement_tag` appends to a tag's
engagement index without re-checking the per-engagement cap.

| Function | Caller | Purpose | Panics |
|---|---|---|---|
| `add_engagement_tag(company, engagement_id, tag)` | Company (or co-signer) | Add `engagement_id` to a tag's index. Duplicate tags are silently ignored. | `unauthorized`, `engagement not found` |
| `remove_engagement_tag(company, engagement_id, tag)` | Company (or co-signer) | Remove `engagement_id` from a tag's index. No-op if the tag was not present. | `unauthorized`, `engagement not found` |
| `get_engagements_by_tag(tag, page, page_size)` → `Vec<String>` | Anyone | Paginated engagement IDs tagged with `tag`. Out-of-range pages return an empty vec. | — |
| `get_engagement_tag_count(tag)` → `u32` | Anyone | Total number of engagements tagged with `tag`. Returns `0` for a tag no engagement has. | — |

`get_engagement_tag_count` is the companion to `get_engagements_by_tag` for
sizing pagination: pass the same `tag` to both to know how many pages to fetch.

### Amendments
`propose_amendment`, `accept_amendment`, `reject_amendment`, `withdraw_amendment_proposal`

### Arbiter Succession
`nominate_arbiter_successor`, `claim_arbiter`
### Arbiter Voting & Succession

#### Arbiter Voting

HireSettle uses a **multi-arbiter M-of-N voting model** to resolve disputes. When a company raises a dispute on a milestone, each assigned arbiter may cast a single vote using `cast_arbiter_vote()`.

Votes are tracked until one of the following conditions is met:

- **Approval quorum reached** (`approve_votes >= quorum`)
  - The dispute is resolved in favour of the recruiter.
  - The milestone payment is released from escrow.
- **Rejection quorum reached** (`reject_votes > arbiters.len() - quorum`)
  - The submitted proof is rejected.
  - The milestone returns to the `Pending` state, allowing the recruiter to submit new proof.

Each arbiter may vote only once for a dispute. Duplicate votes are rejected by the contract.

#### Viewing Vote Progress

Applications can retrieve the current dispute vote tally using:

```text
get_arbiter_votes()
```

This read-only function returns the current approval and rejection vote counts without exposing the voter list, allowing dashboards to display dispute progress while preserving voter privacy.

#### Arbiter Succession

To support long-running engagements, HireSettle allows arbiters to transfer their responsibilities to a successor without modifying the engagement itself.

The succession process consists of two steps:

1. The current arbiter nominates a successor using:

```text
nominate_arbiter_successor()
```

2. The nominated address accepts the role by calling:

```text
claim_arbiter()
```

Only the nominated address can complete the claim. Once claimed, the successor assumes the arbiter's position for future dispute voting while preserving the integrity of the arbitration panel.

#### Dispute Escalation to Super Arbiter

If a dispute's arbiter votes remain split — neither the approval quorum nor the
rejection threshold reached — past the dispute window, it can be escalated to a
single configured Super Arbiter for a tie-breaking resolution.

| Function | Access | Purpose |
|---|---|---|
| `set_super_arbiter(admin, super_arbiter)` | Admin | Configure (or replace) the Super Arbiter address used to resolve escalated disputes. |
| `get_super_arbiter()` → `Option<Address>` | Public | Return the currently configured Super Arbiter, or `None` if none has been set. |
| `is_dispute_escalated(engagement_id, milestone_index)` → `bool` | Public | Return whether the dispute on that milestone is currently escalated. |
| `escalate_dispute(engagement_id, milestone_index)` | Permissionless | Escalate an eligible, still-hung dispute to the configured Super Arbiter. |
| `super_arbiter_resolve(super_arbiter, engagement_id, milestone_index, approve)` | Super Arbiter | Directly approve or reject an escalated dispute, bypassing the normal M-of-N vote. |

**Escalation eligibility.** A dispute becomes eligible for `escalate_dispute`
only when **all** of the following hold:

1. The contract and the engagement are not paused.
2. The engagement is `Active`.
3. The milestone is `Disputed`.
4. The dispute window has elapsed (measured from when `raise_dispute` was called).
5. Neither approval nor rejection has reached its quorum (the vote is still hung).
6. A Super Arbiter has been configured.

**Anyone can call `escalate_dispute` once those conditions are satisfied.** No
client, freelancer, arbiter, or admin authorization is required — escalation
follows the same permissionless-keeper pattern as `unlock_milestone`.

**Normal arbiter voting vs. Super Arbiter resolution.** The Super Arbiter
resolves an escalated dispute directly, without the M-of-N threshold that normal
arbiter voting requires:

| | Normal arbiter voting | Super Arbiter resolution |
|---|---|---|
| Trigger | `cast_arbiter_vote` | `super_arbiter_resolve` |
| Decision makers | Multiple arbiters | Configured Super Arbiter |
| Threshold | M-of-N quorum | No vote threshold |
| Authorization | Individual arbiter | Configured Super Arbiter |
| Approval | Quorum resolves dispute | Direct approval resolves dispute |
| Rejection | Quorum rejection returns dispute to `Pending` | Direct rejection returns dispute to `Pending` |
| Arbiter fee | Deciding arbiter | Super Arbiter |

**Timeout.** If the Super Arbiter does not call `super_arbiter_resolve` within
the configured response window (default `super_arbiter_response_window`, ~3
days) after escalation, the permissionless `resolve_escalation_timeout` may be
invoked to auto-resolve the escalated dispute in the recruiter's favor — no
arbiter fee is deducted since the Super Arbiter did not act. The admin can tune
the window via `set_super_arbiter_deadline(admin, ledgers)` and read it with
`get_super_arbiter_deadline()`; `get_super_arbiter_resolutions()` reports the
total number of disputes concluded through the escalation path.

### Early Exit

The early-exit protocol allows a **recruiter** to request a graceful exit from an engagement before all milestones are completed. It is a three-step flow involving both the recruiter and the company.

#### Step 1 -- Recruiter requests early exit

```text
request_early_exit(recruiter, engagement_id)
```

**Caller:** Recruiter (must match the engagement's recruiter and sign the transaction).

The engagement must be in `Active` status. On success, the engagement transitions to `ExitRequested`. No funds are moved at this stage.

#### Step 2a -- Company accepts

```text
accept_early_exit(company, engagement_id)
```

**Caller:** Company (must match the engagement's company and sign the transaction).

On acceptance:

- The recruiter **keeps** all payments already released for confirmed or resolved milestones (`released_amount`).
- The remaining escrow balance (`total_amount - released_amount`) is **refunded** to the company.
- The engagement status moves to `Cancelled` (terminal).

#### Step 2b -- Company rejects

```text
reject_early_exit(company, engagement_id)
```

**Caller:** Company.

On rejection, the engagement returns to `Active` and the recruiter must continue working through the remaining milestones. No funds are moved.

#### Payment split summary

| Scenario | Recruiter receives | Company receives |
|---|---|---|
| Accepted (partial progress) | `released_amount` (already paid) | `total_amount - released_amount` (refund) |
| Accepted (no milestones done) | Nothing | Full `total_amount` refund |
| Rejected | N/A -- engagement continues | N/A -- engagement continues |

### Read-Only Queries

- **`get_config_snapshot()`**: Fetches all active contract configuration tunables and parameters in a single read-only call, eliminating the need for indexers to make multiple round-trips.


All read-only functions are permissionless and require no authentication.

#### Which query do I call?

A quick lookup for common frontend needs. See the subsections below for full
argument and return-type details.

| Frontend need | Function |
|---|---|
| Show a company their active engagements | `get_engagements_by_company` + `get_company_active_count` |
| Show all engagements (admin / explorer view) | `get_engagement_ids_by_status` + `get_engagement_count` |
| Show one engagement's full detail | `get_engagement` / `get_engagement_summary` |
| Show an engagement's milestone list + status | `get_all_milestone_statuses` |
| Show a single milestone's detail | `get_milestone` |
| Check if a milestone is unlockable now | `is_milestone_unlockable` |
| Show how long until a milestone unlocks | `ledgers_until_unlock` / `get_estimated_unlock_seconds` |
| Warn a user a milestone is due soon | `is_milestone_due_soon` |
| Show how much has been released / remains in escrow | `get_total_released` / `get_escrow_balance` |
| Check if an engagement is finished | `get_is_engagement_complete` |
| Show milestone unlock progress for a bar | `get_unlock_progress` |
| Show a recruiter's / company's star rating | `get_recruiter_rating` / `get_company_rating` |
| Check whether I already rated someone | `is_recruiter_rated` / `is_company_rated` |
| Show pending milestone amendment / its TTL | `get_pending_amendment` / `get_amendment_ttl` |
| Show amendment history for a milestone | `get_amendment_log` |
| Show arbiter vote tally on a dispute | `get_arbiter_votes` |
| Show why a milestone is in dispute | `get_dispute_reason` |
| Show replacement history / reason | `get_replacement_count` / `get_replacement_reason` |
| Fetch the contract PDF / metadata hash | `get_contract_pdf_hash` / `get_metadata_hash` |
| Is the contract (or one engagement) paused? | `is_paused` / `is_engagement_paused` |
| Load current config in one call (indexers) | `get_config_snapshot` |
| Fetch a single config value | `get_version`, `get_min_amount`, `get_platform_fee`, `get_allowed_tokens`, … |
| Show open dispute count on an engagement | `get_active_dispute_count` |

#### Engagement Queries

| Function | Arguments | Return Type |
|---|---|---|
| `get_engagement` | `engagement_id: String` | `Engagement` |
| `get_engagement_summary` | `engagement_id: String` | `EngagementSummary` |
| `get_is_engagement_complete` | `engagement_id: String` | `bool` |
| `get_active_dispute_count` | `engagement_id: String` | `u32` |
| `get_unlock_progress` | `engagement_id: String` | `(u32, u32)` |
| `get_metadata_hash` | `engagement_id: String` | `Option<String>` |
| `get_contract_pdf_hash` | `engagement_id: String` | `Option<String>` |
| `get_total_released` | `engagement_id: String` | `i128` |
| `get_escrow_balance` | `engagement_id: String` | `i128` |

#### Milestone Queries

| Function | Arguments | Return Type |
|---|---|---|
| `get_milestone` | `engagement_id: String`, `milestone_index: u32` | `Milestone` |
| `get_all_milestone_statuses` | `engagement_id: String` | `Vec<MilestoneStatus>` |
| `is_milestone_unlockable` | `engagement_id: String`, `milestone_index: u32` | `bool` |
| `ledgers_until_unlock` | `engagement_id: String`, `milestone_index: u32` | `u32` |
| `get_estimated_unlock_seconds` | `engagement_id: String`, `milestone_index: u32` | `u64` |
| `is_milestone_due_soon` | `engagement_id: String`, `milestone_index: u32` | `bool` |
| `is_milestone_due_soon_notified` | `engagement_id: String`, `milestone_index: u32` | `bool` |
| `get_arbiter_votes` | `engagement_id: String`, `milestone_index: u32` | `ArbiterVoteCounts` |
| `get_dispute_reason` | `engagement_id: String`, `milestone_index: u32` | `Option<String>` |

#### Engagement Listing & Counting

| Function | Arguments | Return Type |
|---|---|---|
| `get_engagement_count` | — | `u64` |
| `get_company_engagement_count` | `company: Address` | `u32` |
| `get_engagements_by_company` | `company: Address`, `page: u32`, `page_size: u32` | `Vec<String>` |
| `get_company_active_count` | `company: Address` | `u32` |
| `get_engagement_ids_by_status` | `status: EngagementStatus`, `page: u32`, `page_size: u32` | `Vec<String>` |
| `get_engagement_count_by_status` | `status: EngagementStatus` | `u32` |

`get_engagement_ids_by_status` paginates the **filtered** result, so page 0 always
holds the first `page_size` matches regardless of how many non-matching
engagements they are interleaved with. It scans every engagement record, since
status lives on the engagement rather than in a per-status index — prefer the
per-company / per-recruiter queries when the party is already known. Its backing
index is populated at `create_engagement`, so engagements created before this
function was deployed are not enumerated (their records stay readable via
`get_engagement`).

`page` is 0-indexed and `status` accepts any
[`EngagementStatus`](#engagementstatus) variant, so
`get_engagement_ids_by_status(EngagementStatus::Active, 0, 50)` returns the
first 50 IDs currently in `Active` status. A `page_size` of `0` returns an
empty vec rather than erroring or falling back to a default size; an unset
caller-side page size therefore silently yields nothing. Requesting a page
past the last match also returns an empty vec, so iterate until a page comes
back empty instead of precomputing a page count
(`get_engagement_count_by_status` can size one, but its count shifts as
engagements change status between calls). Results reflect each engagement's
status at call time, and index entries whose engagement record has expired
from storage are skipped rather than returned stale.

#### Amendment Queries

| Function | Arguments | Return Type |
|---|---|---|
| `get_amendment_log` | `engagement_id: String`, `milestone_index: u32` | `Vec<AmendmentEntry>` |
| `get_pending_amendment` | `engagement_id: String`, `milestone_index: u32` | `Option<AmendmentProposal>` |
| `get_amendment_ttl` | — | `u32` |

#### Feedback Rating Queries

| Function | Arguments | Return Type |
|---|---|---|
| `get_recruiter_rating` | `recruiter: Address` | `RatingSummary` |
| `get_company_rating` | `company: Address` | `RatingSummary` |
| `is_recruiter_rated` | `engagement_id: String` | `bool` |
| `is_company_rated` | `engagement_id: String` | `bool` |

An address that has never been rated returns a zeroed summary rather than
panicking, so new recruiters and companies render without a prior existence
check. Always check `count` before presenting `average_x100` — a `0` average
means "no ratings yet", not "rated zero".

#### Replacement Queries

| Function | Arguments | Return Type |
|---|---|---|
| `get_replacement_reason` | `engagement_id: String`, `replacement_index: u32` | `Option<String>` |
| `get_replacement_count` | `engagement_id: String` | `u32` |

#### Contract Config Getters

| Function | Arguments | Return Type |
|---|---|---|
| `get_version` | — | `String` |
| `get_min_amount` | — | `i128` |
| `get_platform_fee` | — | `(u32, Address)` |
| `get_fee_tiers` | — | `Vec<FeeTier>` |
| `get_ledgers_per_day` | — | `u32` |
| `get_max_retention_days` | — | `u32` |
| `get_max_milestones` | — | `u32` |
| `get_max_replacements` | — | `u32` |
| `get_max_active_per_company` | — | `u32` |
| `get_inactivity_timeout_ledgers` | — | `u32` |
| `get_storage_ttl_extend_to` | — | `u32` |
| `get_confirm_window` | — | `u32` |
| `get_dispute_window` | — | `u32` |
| `get_max_proof_hash_length` | — | `u32` |
| `get_arbiter_fee` | — | `u32` |
| `get_upgrade_lock_duration` | — | `u32` |
| `get_due_soon_window` | — | `u32` |
| `get_allowed_tokens` | — | `Vec<Address>` |
| `get_config_snapshot` | — | `ConfigSnapshot` |

`get_config_snapshot` returns every admin-configurable parameter above in a
single read-only call, so off-chain indexers do not need ~20 separate
round-trips to reconstruct the current configuration. Each field is sourced from
the same accessor its dedicated getter uses — including the defaults applied to
parameters that were never explicitly set — and because the whole struct is read
within one invocation the values are mutually consistent, with no window for an
admin transaction to land between two reads and yield a torn view.

#### Admin & Contract State

| Function | Arguments | Return Type |
|---|---|---|
| `is_paused` | — | `bool` |
| `is_engagement_paused` | `engagement_id: String` | `bool` |
| `get_admin` | — | `Address` |
| `get_pending_admin` | — | `Option<Address>` |

### Milestone Confirmation Functions

HireSettle provides three milestone confirmation paths with different authorization, preconditions, and semantics. The canonical single-milestone `confirm_milestone` is documented here as a reference point so the two batch / force variants can be contrasted against it.

---

#### `confirm_milestone` — Single Milestone (Reference)

Contract function reference: [confirm_milestone](file:///C:/Users/Shepherd/projects/hiresettle-contract/contracts/hiresettle/src/lib.rs#L1169-L1286)

**Caller**: The engagement's `company` address. Requires authentication (`company.require_auth()`).

**Preconditions**:
- Engagement status is `Active`.
- Milestone status is `ProofSubmitted` (recruiter has already submitted a proof hash).
- **Sequential confirmation** (Issue #67): every milestone with a lower index must already be in `Confirmed` or `Resolved` status. A later milestone cannot leapfrog an earlier unfinished one.
- For `Retention` milestones: `env.ledger().sequence() >= milestone.valid_after_ledger`. The retention time-gate is re-verified at confirmation time even if `unlock_milestone` was already called, preventing a company from accidentally confirming before the window truly elapses.
- Contract is not paused.

**Payment / Side effects**:
- The gross share is `engagement.total_amount × milestone.payment_percent ÷ 100`. From this, `milestone.replacement_paid_out` is subtracted (Issue #183) so escrow topped up *after* a replacement reset still reaches the recruiter rather than getting permanently stuck in the contract.
- Platform fee (bps × gross share ÷ 10 000) is transferred to the treasury. See [Platform Fee](#platform-fee) for how `bps` is configured and its 5% cap.
- The net remainder is distributed to the recruiter (and co-recruiter, if configured, per `recruiter_split_bps`).
- Milestone moves to `Confirmed`; engagement moves to `Completed` if this was the last outstanding milestone.
- Emits `milestone_confirmed` with `(milestone_index, payment)`.

---

#### `batch_confirm_milestones` — Atomic Multi-Milestone Confirmation

Contract function reference: [batch_confirm_milestones](file:///C:/Users/Shepherd/projects/hiresettle-contract/contracts/hiresettle/src/lib.rs#L3195-L3332)

Confirms several milestones in one transaction with a single company signature and all-or-nothing semantics. Useful when a batch of milestones (e.g. placement + 30-day retention) have proof submitted and the company wants to release them together.

**Caller**: The engagement's `company` address. Requires authentication (`company.require_auth()`).

**Arguments**:
- `milestone_indices: Vec<u32>` — ordered list of milestone indices to confirm. Must be non-empty (panics with `EmptyIndices` otherwise).

**Preconditions** (validated for every index in the batch *before* any state mutation or transfer):
- Engagement status is `Active`.
- Each target milestone is in `ProofSubmitted` status.
- For each `Retention` milestone in the batch: `current_ledger >= valid_after_ledger`.
- **Sequential confirmation (Issue #67 / #184)**: For every index `idx` in the batch, *all* milestones with index `< idx` must be either (a) already `Confirmed` or `Resolved` on-chain, *or* (b) present somewhere in `milestone_indices` itself. This allows a single batch to close a contiguous gap of unfinished milestones — e.g. confirming indices `[0, 1, 2]` atomically even if none of them were previously confirmed — while still forbidding index `[2]` without index `[1]`.

**Differences from single `confirm_milestone`**:

| Aspect | `confirm_milestone` | `batch_confirm_milestones` |
|---|---|---|
| Scope / signature | One `milestone_index` per tx. | `milestone_indices: Vec<u32>` — arbitrary count per tx. |
| Atomicity | Single call per milestone. tx failure leaves prior milestones confirmed. | **Validate-all first, then mutate-all.** Any failing precondition in the batch rejects the entire transaction — no partial confirmations and no stuck half-paid batches. |
| Sequential rule | Only prior on-chain `Confirmed`/`Resolved` milestones count. | Prior milestones may be either on-chain done **or** another entry in the same batch. |
| Replacement paid-out handling | Deducts `milestone.replacement_paid_out` from the gross share (Issue #183). | **Does not deduct** `replacement_paid_out` — always pays the full `total_amount × payment_percent ÷ 100` gross share. |
| Completion check | Checked once per call. | Checked once *after* the loop, so a batch that closes the final milestone(s) transitions the engagement to `Completed` and emits exactly one `engagement_completed`. |
| Events per milestone | One `milestone_confirmed` event. | One `milestone_confirmed` event **per index** in the batch. |

**Payment / Side effects**:
- Identical per-milestone payout path (platform fee → treasury, net payout → recruiter(s), `released_amount` += gross) but **without** the `replacement_paid_out` subtraction (see above).
- Milestones move to `Confirmed` in batch order; if the batch exhausts the engagement, engagement → `Completed`.

---

#### `force_confirm_milestone` — Confirm-Window Timeout Override

Contract function reference: [force_confirm_milestone](file:///C:/Users/Shepherd/projects/hiresettle-contract/contracts/hiresettle/src/lib.rs#L3404-L3519)

A permissionless keeper function that force-confirms a milestone whose proof was submitted but the company never acted on it within the configured confirm window. This is the "auto-confirm" safety net advertised in the feature table: it guarantees the recruiter eventually gets paid if the deliverable is not disputed, even if the company goes silent.

**Caller**: **Any** address. `caller.require_auth()` is checked (so a real signature is required) but no role-based restriction is applied. The company, recruiter, an arbiter, or an unaffiliated keeper bot may all invoke it.

**Preconditions**:
- Engagement status is `Active`.
- Milestone status is exactly `ProofSubmitted`.
- **Confirm window has elapsed** — the timeout gate. See "Confirm Window" subsection below.
- Contract is not paused.

**Preconditions that `confirm_milestone` enforces but `force_confirm_milestone` intentionally skips**:
- **No sequential-confirmation check**: A later milestone whose own confirm window has elapsed can be force-confirmed even if earlier milestones are still open. This keeps the timeout path usable even if an earlier milestone is stuck in a dispute.
- **No retention time-gate re-check**: `force_confirm_milestone` does not re-verify `valid_after_ledger`. A `Retention` milestone in `ProofSubmitted` status has, by definition, already been unlocked (either by `unlock_milestone` or because it was a `Placement`), so the time-gate is not re-tested.
- **No replacement paid-out deduction**: Always pays the full gross share, matching `batch_confirm_milestones` and contrasting with single `confirm_milestone`.

**Confirm Window (Timeout Condition)**

The confirm window is a global admin-configured ledger delta stored under `DataKey::ConfirmWindow`. It is managed by two admin/read functions:

- [set_confirm_window](file:///C:/Users/Shepherd/projects/hiresettle-contract/contracts/hiresettle/src/lib.rs#L3355-L3362) — `set_confirm_window(env, admin, ledgers)`. Admin only. Sets the confirm-window ledger delta and emits `confirm_window_set`.
- [get_confirm_window](file:///C:/Users/Shepherd/projects/hiresettle-contract/contracts/hiresettle/src/lib.rs#L3365-L3370) — `get_confirm_window(env) -> u32`. Read-only. Returns the active window; defaults to `DEFAULT_CONFIRM_WINDOW_LEDGERS = 86_400` (≈ 5 calendar days at 5 s / ledger) if admin has never set one.

The force-confirm gate inside `force_confirm_milestone` is the **strict greater-than** inequality:

```
current_ledger  >  milestone.proof_submitted_at  +  confirm_window
```

If this does not hold, the call panics with `ConfirmWindowNotElapsed`. In particular, equality (`==`) is **not** sufficient: callers must wait one additional ledger past the deadline.

`proof_submitted_at` is populated by `submit_proof` at the moment the recruiter's proof hash is accepted, and is part of the on-chain `Milestone` struct.

**Differences from single `confirm_milestone` (summary)**:

| Aspect | `confirm_milestone` | `force_confirm_milestone` |
|---|---|---|
| Caller restriction | `company` only. | **Any** authenticated address (permissionless). |
| Gate | Company signature. | Confirm-window elapsed since `proof_submitted_at`. |
| Sequential rule | Enforced. | Skipped. |
| Retention time-gate re-checked | Yes. | No. |
| Replacement paid-out handling | Deducted from gross share. | Not deducted. |
| Authorization path | `company.require_auth()`. | `caller.require_auth()` with no role check. |
| Emitted event | `milestone_confirmed`. | **`milestone_force_confirmed`** (distinct symbol for off-chain indexers to distinguish timeout-driven payouts from genuine company approvals). |

**Payment / Side effects**:
- Identical transfer mechanics to `confirm_milestone` (gross share, platform fee deduction, recruiter / co-recruiter distribution) except without the `replacement_paid_out` subtraction.
- Milestone → `Confirmed`; engagement → `Completed` if this closes the last outstanding milestone.
- Emits `milestone_force_confirmed` with `(milestone_index, payment)`.

---

## Events

The contract emits Soroban events for all state transitions. Events are grouped by feature area below. Each event's first topic is its symbol name; `engagement_id` appears as a second topic for engagement-scoped events.

### Admin & Contract Config

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `platform_fee_set` | — | `(bps, treasury)` | `set_platform_fee` |
| `fee_tiers_set` | — | `tiers.len()` | `set_fee_tiers` |
| `version_set` | — | `version` | `set_version` |
| `min_amount_set` | — | `amount` | `set_min_amount` |
| `paused` | — | `admin` | `pause` |
| `unpaused` | — | `admin` | `unpause` |
| `admin_nominated` | — | `new_admin` | `nominate_admin` |
| `admin_claimed` | — | `nominee` | `claim_admin` |
| `admin_renounced` | — | `final_ledger` | `renounce_admin` |
| `max_replacements_set` | — | `count` | `set_max_replacements` |
| `ledgers_per_day_set` | — | `value` | `set_ledgers_per_day` |
| `max_retention_days_set` | — | `days` | `set_max_retention_days` |
| `max_milestones_set` | — | `count` | `set_max_milestones` |
| `max_active_per_company_set` | — | `count` | `set_max_active_per_company` |
| `inactivity_timeout_set` | — | `ledgers` | `set_inactivity_timeout_ledgers` |
| `storage_ttl_extend_to_set` | — | `ledgers` | `set_storage_ttl_extend_to` |
| `confirm_window_set` | — | `ledgers` | `set_confirm_window` |
| `dispute_window_set` | — | `ledgers` | `set_dispute_window` |
| `upgrade_lock_duration_set` | — | `ledgers` | `set_upgrade_lock_duration` |
| `max_proof_hash_length_set` | — | `len` | `set_max_proof_hash_length` |
| `arbiter_fee_set` | — | `bps` | `set_arbiter_fee` |
| `due_soon_window_set` | — | `ledgers` | `set_due_soon_window` |
| `engagement_paused` | `engagement_id` | `admin` | `pause_engagement` |
| `engagement_unpaused` | `engagement_id` | `admin` | `unpause_engagement` |
| `token_allowlisted` | — | `token` | `add_allowed_token` |
| `token_removed` | — | `token` | `remove_allowed_token` |
| `allowlist_enabled_set` | — | `enabled` | `set_token_allowlist_enabled` |
| `super_arbiter_set` | — | `super_arbiter` | `set_super_arbiter` |
| `super_arbiter_deadline_set` | — | `ledgers` | `set_super_arbiter_deadline` |

### Upgrade

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `upgrade_proposed` | — | `(new_wasm_hash, execute_after_ledger)` | `propose_upgrade` |
| `upgrade_executed` | — | `new_wasm_hash` | `execute_upgrade` (permissionless after lock) |

### Engagement Lifecycle

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `engagement_created` | `engagement_id` | `engagement_id` | `create_engagement` |
| `engagement_cancelled` | `engagement_id` | `refund` | `cancel_engagement` (company + recruiter) |
| `engagement_expired` | `engagement_id` | `refund` | `expire_engagement` (permissionless keeper) |
| `engagement_completed` | `engagement_id` | `(engagement_id, released_amount, ledger)` | Final milestone confirmed |
| `escrow_topped_up` | `engagement_id` | `(amount, total_amount)` | `top_up_escrow` |
| `company_transferred` | `engagement_id` | `(old_company, new_company)` | `transfer_company_role` |
| `recruiter_transferred` | `engagement_id` | `(old_recruiter, new_recruiter)` | `accept_recruiter_transfer` |
| `status_changed` | `engagement_id` | `(old_status, new_status)` | Any engagement status transition |

### Milestone

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `milestone_unlocked` | `engagement_id` | `(milestone_index, valid_after_ledger, unlocked_at_ledger)` | `unlock_milestone` |
| `milestone_due_soon` | `engagement_id` | `(milestone_index, valid_after_ledger, ledgers_remaining, window)` | `notify_milestone_due_soon` (permissionless keeper) |
| `proof_submitted` | `engagement_id` | `milestone_index` | `submit_proof` (first submission) |
| `proof_resubmitted` | `engagement_id` | `(milestone_index, old_hash, proof_hash)` | `submit_proof` (resubmission) |
| `milestone_confirmed` | `engagement_id` | `(milestone_index, payment)` | `confirm_milestone` / `batch_confirm_milestones` |
| `milestone_force_confirmed` | `engagement_id` | `(milestone_index, payment)` | `force_confirm_milestone` (permissionless after window) |
| `milestone_status_changed` | `engagement_id` | `(milestone_index, old_status, new_status)` | Any milestone status transition |
| `platform_fee_collected` | `engagement_id` | `(milestone_index, fee_amount, treasury)` | Milestone confirmation when fee > 0 |

### Dispute

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `dispute_raised` | `engagement_id` | `(milestone_index, reason)` | `raise_dispute` |
| `arbiter_voted` | `engagement_id` | `(milestone_index, approve)` | `cast_arbiter_vote` |
| `dispute_resolved` | `engagement_id` | `(milestone_index, approved)` | Quorum reached in `cast_arbiter_vote` |
| `dispute_escalated` | `engagement_id` | `(milestone_index, super_arbiter)` | `escalate_dispute` |
| `dispute_escalation_resolved` | `engagement_id` | `(milestone_index, super_arbiter, approved)` | `super_arbiter_resolve` |
| `super_arbiter_timeout_resolved` | `engagement_id` | `milestone_index` | `resolve_escalation_timeout` (auto-favor recruiter) |

#### Dispute-to-Super-Arbiter Escalation Path

Disputes normally resolve through M-of-N arbiter voting. If no quorum is reached
before the dispute window elapses, the dispute can be escalated to a configured
super arbiter for a tie-breaking resolution (issue #246):

```
   Company            Arbiter(s)           HireSettle           Super-Arbiter
      │                    │                    │                     │
      │ raise_dispute(engagement_id, milestone_index, reason)         │
      │────────────────────────────────────────▶│                     │
      │                    │                    │                     │
      │                    │                    │ milestone: ProofSubmitted ▶ Disputed
      │                    │                    │                     │
      │                    │ cast_arbiter_vote(approve = true/false)  │
      │                    │───────────────────▶│                     │
      │                    │                    │                     │
      │                    │                    │ tallies M-of-N votes│
      │                    │                    │                     │
      │                    │                    │ approve_votes >= quorum ▶ Confirmed (payment released)
      │                    │                    │ reject_votes > N - quorum ▶ Pending (proof cleared)
      │                    │                    │                     │
      │                    │ no quorum reached  │                     │
      │                    │ dispute window elapses                   │
      │                    │                    │                     │
      │ escalate_dispute(engagement_id, milestone_index) [any keeper] │
      │────────────────────────────────────────▶│                     │
      │                    │                    │                     │
      │                    │                    │  dispute_escalated  │
      │                    │                    │────────────────────▶│
      │                    │                    │                     │
      │                    │                    │ super_arbiter_resolve(engagement_id, milestone_index, approve)
      │                    │                    │◀────────────────────│
      │                    │                    │                     │
      │                    │                    │ approve  ▶ Resolved (payment released)
      │                    │                    │ reject   ▶ Pending (proof cleared)
      │                    │                    │                     │
```

### Amendment

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `amendment_proposed` | `engagement_id` | `(milestone_index, proposer, new_payment_percent, expires_at_ledger)` | `propose_amendment` |
| `amendment_accepted` | `engagement_id` | `(milestone_index, acceptor, old_payment_percent, new_payment_percent)` | `accept_amendment` |
| `amendment_rejected` | `engagement_id` | `(milestone_index, rejector, reason)` | `reject_amendment` or expired proposal on `accept_amendment` |
| `amendment_withdrawn` | `engagement_id` | `(milestone_index, proposer, new_payment_percent)` | `withdraw_amendment_proposal` |

### Feedback Ratings

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `recruiter_rated` | `engagement_id` | `(recruiter, rating, new_count)` | `submit_recruiter_rating` |
| `company_rated` | `engagement_id` | `(company, rating, new_count)` | `submit_company_rating` |

### Arbiter Succession

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `arbiter_nominated` | `engagement_id` | `successor` | `nominate_arbiter_successor` |
| `arbiter_claimed` | `engagement_id` | `nominee` | `claim_arbiter` |

### Early Exit

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `early_exit_requested` | `engagement_id` | `recruiter` | `request_early_exit` |
| `early_exit_accepted` | `engagement_id` | `refund` | `accept_early_exit` |
| `early_exit_rejected` | `engagement_id` | `company` | `reject_early_exit` |

### Replacement

| Event | Topics | Payload | Trigger |
|---|---|---|---|
| `replacement_requested` | `engagement_id` | `(replacement_index, reason)` | `request_replacement` |

---

## Project Structure

```
hiresettle-contract-1/
├── Cargo.toml              # Workspace root
├── contracts/
│   └── hiresettle/
│       ├── Cargo.toml      # Contract crate config
│       ├── Makefile        # Build helpers
│       └── src/
│           ├── lib.rs      # Main contract logic (~3500 lines)
│           └── test.rs     # Unit tests (~2000 lines)
├── TODO.md                 # Task tracking
└── README.md               # This file
```

---

## Testing

Run the full test suite from the contract directory:

```bash
cd contracts/hiresettle && cargo test
```

Tests cover creation, proof submission, confirmation, disputes, arbiter voting, replacement flow, early exit, amendments, batch confirmations, auto-confirm, expiry, admin configuration, and edge cases for all validation rules.

---

## Usage Example

```rust
// 1. Init contract
HireSettleContract::init(env, admin);

// 2. Create engagement
let config = EngagementConfig {
    metadata_hash: Some(String::from_str(&env, "Qm...")),
    co_recruiter: None,
    recruiter_split_bps: 10_000,
    contract_pdf_hash: Some(String::from_str(&env, "sha256:abc123...")),
};
HireSettleContract::create_engagement(
    env, "engagement-1", company, recruiter,
    arbiter_setup, token, 100_000_000, "Senior Engineer",
    milestones, retention_days, config
);

// 3. Recruiter unlocks & submits proof
HireSettleContract::unlock_milestone(env, "engagement-1", 1);
HireSettleContract::submit_proof(env, recruiter, "engagement-1", 1, "ipfs://QmProof...");

// 4. Company confirms → payment released
HireSettleContract::confirm_milestone(env, company, "engagement-1", 1);
```

### Create a test engagement via CLI

```bash
stellar contract invoke \
  --id <CONTRACT_ID> \
  --source my-account \
  --network testnet \
  -- create_engagement \
  --engagement_id "ENG-TEST-001" \
  --company <COMPANY_ADDRESS> \
  --recruiter <RECRUITER_ADDRESS> \
  --arbiter_setup '{"arbiters":["<ARBITER_ADDRESS>"],"quorum":1}' \
  --token <USDC_SAC_ADDRESS> \
  --total_amount 5000000000 \
  --job_title "Senior Engineer" \
  --milestones '[...]' \
  --retention_days '[30, 90]'
```

> USDC SAC on Testnet: `CBIELTK6YBZJU5UP2WWQEUCYKLPU6AUNZ2BQ4WWFEIE3USCIHMXQDAMA`

> **Note:** the payout amounts in the examples above assume the default 0 bps platform fee. If `set_platform_fee` has been called, the recruiter (and co-recruiter, if configured) receives `gross_share − platform_fee`, not the full gross share. See [Platform Fee](#platform-fee).

---

## Security Considerations

- **Authorization**: Every state-changing function calls `require_auth()`. Recruiters cannot confirm their own milestones. Companies cannot cast arbiter votes.
- **No party/arbiter address overlap**: `create_engagement` rejects `company == recruiter`, `company` appearing in the arbiter set, or `recruiter` appearing in the arbiter set. Without this check a company could name itself (or a colluding address) as arbiter and vote on its own disputes, or name itself as recruiter to self-confirm milestones.
- **Multi-arbiter quorum**: Disputes require M-of-N arbiter votes to resolve. A single arbiter cannot unilaterally release or withhold payment — both approval and rejection require a configurable quorum. Duplicate votes from the same arbiter are rejected on-chain.
- **Token amounts are raw integer units, not decimal-aware**: The token allowlist accepts any allowlisted SAC, not just USDC. `total_amount`, `MinEngagementAmount`, and all milestone payout math (`amount * payment_percent / 100`) operate on raw integer units of whichever token is used — the contract never reads a token's `decimals()`. Percentage splits are exact regardless of precision, but a single admin-wide minimum amount (`set_min_amount`) will represent a different real-world value across tokens of different precision (e.g. 7-decimal vs. 18-decimal tokens). Integrators are responsible for only allowlisting tokens of comparable precision, or adjusting the minimum accordingly, when using a token other than the reference 7-decimal USDC used throughout this README's examples.
- **Arbiter fee cap**: The arbiter fee is capped at 200 bps (2%) to prevent excessive deduction from recruiter payouts on dispute approval.
- **Retention double-check**: `confirm_milestone()` re-verifies `valid_after_ledger` even if `unlock_milestone()` was called, preventing a company from confirming a retention milestone before the window truly ends.
- **Replacement fee fairness**: The Placement tranche paid to the recruiter is non-refundable. Only unreleased amounts are frozen. This is explicit in the contract and documented clearly so both parties understand the terms at engagement creation.
- **Ledger drift**: The 5s/ledger assumption is approximate. Stellar's actual ledger time may vary slightly. The contract uses ledger sequence numbers — not timestamps — so the unlock is purely count-based. Production deployments should account for ~±5% drift in real-world retention windows.
- **Upgrade time-lock**: Contract upgrades require a configurable time-lock (default 17,280 ledgers ≈ 1 day) between proposal and execution, giving stakeholders time to review before changes take effect.

---

## Roadmap

- [x] Core escrow + milestone logic
- [x] Time-gated retention milestones (ledger-based unlock)
- [x] Replacement clause with clock reset (`request_replacement`: company-only, requires confirmed Placement; resets the Placement milestone, restarts retention clocks, panics on bad preconditions, and emits the `replacement_requested` event)
- [x] Dispute resolution via arbiter
- [x] Flexible milestone structure (2-milestone 50/50, 3-milestone, custom)
- [x] Unit tests — 282 `#[test]` functions (see `cargo test` output for current count)
- [ ] Multi-candidate engagements (multiple positions, one company-recruiter pair)
- [ ] Partial payout on replacement (configurable replacement fee)
- [x] Contract upgrade mechanism
- [ ] Mainnet deployment

---

## License

MIT

---
