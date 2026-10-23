//! # HireSettle Smart Contract
//!
//! ## Overview
//! The HireSettle smart contract provides escrow and settlement mechanisms for decentralized work
//! engagements. It manages the full lifecycle of milestone-based agreements, security retentions,
//! disputes, and protocol fee distributions.
//!
//! ## Major Subsystems
//! - **Escrow & Funding**: Handles locking user funds in contract storage during active engagements.
//! - **Milestones**: Tracks deliverable checkpoints, approvals, and payout releases.
//! - **Disputes & Arbitration**: Manages escalation paths, resolution voting, super-arbiters, and quorums.
//! - **Amendments**: Facilitates proposed modifications to live contract parameters and agreements.
//! - **Tags**: Enables metadata classification and custom key-value attributes for engagements.
//! - **Fees & Retention**: Calculates protocol commissions, retention holds, and fee distributions.
//!
//! ## Section Navigation
//! - `Data Types & Storage`: Core structs (`Engagement`, `Milestone`, `Dispute`), state keys, and enums.
//! - `Contract Initialization`: Setup administrative defaults, fee structures, and protocol parameters.
//! - `Core Lifecycle Functions`: Initializing engagements, funding escrow, approving, and releasing milestones.
//! - `Dispute Resolution`: Escalating deadlocks, casting arbiter votes, and executing resolutions.
//! - `Batch Disputes`: Raising disputes for many milestones in a single call via `batch_raise_dispute`.
//! - `Admin & Configuration`: Protocol parameter updates, fee withdrawals, and arbiter management.
//!
//! ## Batch Disputes (`batch_raise_dispute`)
//!
//! `batch_raise_dispute` raises a dispute for several milestones of one engagement in a single
//! transaction. It is the batch counterpart to the single-milestone dispute entry point and shares
//! the same per-milestone dispute cycle accounting.
//!
//! ### All-or-nothing semantics
//! The batch is atomic: if **any** supplied milestone index is invalid (out of range for the
//! engagement, or otherwise not eligible to be disputed), the entire call reverts and **no**
//! dispute is raised for any milestone. There is no partial success — callers must ensure every
//! index in the batch is valid before submitting.
//!
//! ### Shared reason string
//! A single `reason` string is applied to every milestone in the batch. All disputes created by
//! one call therefore carry the same reason; to record different reasons, issue separate calls.
//!
//! ### Event emission
//! A dispute event is emitted **per milestone** in the batch, so indexers observe one event for
//! each milestone that entered the dispute cycle — not a single aggregate event.
//!
//! ### Dispute cycle accounting
//! Each milestone in the batch advances its own dispute cycle counter exactly as the
//! single-milestone path would, keeping per-milestone cycle limits and escalation rules intact.
//!
//! ### Resource guidance
//! Because the whole batch executes in one transaction, keep batches modest: every additional
//! index adds CPU/memory instructions and event bytes, and an oversized batch can exceed the
//! transaction resource limits and revert. Prefer splitting very large sets into multiple calls.

#![no_std]
// `#[contractimpl]` expands `create_engagement`'s many required fields into a
// flat parameter list on the generated contract, client, and args types;
// bundling them into a struct would break the deployed ABI, so the lint is
// disabled crate-wide for the macro-generated bindings it triggers on.
#![allow(clippy::too_many_arguments)]

use soroban_sdk::contract;

mod constants;
mod errors;
mod types;
mod enums;
mod admin;
mod engagement;
mod milestones;
mod disputes;
mod transfers;
mod queries;
mod helpers;
mod arbiter_pool;
mod payout;
mod ratings;
mod bonds;
mod features;
mod timeline;

pub(crate) use constants::*;
pub(crate) use errors::*;
pub use types::*;
pub use enums::*;
pub use payout::{SwapAdapter, SwapAdapterClient};

// ============================================================
// CONTRACT
// ============================================================

/// Milestone-based recruiter fee escrow contract.
#[contract]
pub struct HireSettleContract;

mod test;
