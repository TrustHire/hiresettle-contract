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
//! - `Admin & Configuration`: Protocol parameter updates, fee withdrawals, and arbiter management.

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
