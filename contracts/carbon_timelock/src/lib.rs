//! # carbon_timelock
//!
//! Time-delay guard for the highest-risk admin operations in the StellarKraal
//! carbon credit system.
//!
//! ## Purpose
//!
//! A compromised admin key can execute destructive operations instantly. This
//! contract introduces a mandatory waiting period between when an admin *proposes*
//! a sensitive operation and when it can be *executed*, providing a detection and
//! cancellation window.
//!
//! ## Protected operations
//!
//! | Operation ID | Maps to contract function | Default delay |
//! |---|---|---|
//! | `RotateOracleKey` | `carbon_oracle::rotate_key` | 17,280 ledgers (~24 h at 5 s/ledger) |
//! | `ForceRetireProject` | `carbon_registry::retire_project` | 17,280 ledgers (~24 h) |
//! | `PauseMarketplace` | `carbon_marketplace` pause flag | 8,640 ledgers (~12 h) |
//!
//! ## Lifecycle
//!
//! ```text
//! propose_operation(op_type, params_hash, delay)
//!   → op stored with status = Pending, execute_after = ledger + delay
//!
//! [within delay window] cancel_operation(op_id)  ← admin OR guardian
//!   → op status = Cancelled
//!
//! [after delay elapses] execute_operation(op_id, actual_params_hash)
//!   → verifies status = Pending, ledger >= execute_after, actual_params_hash == stored hash
//!   → op status = Executed
//!   → emits TimelockExecuted event for off-chain consumers
//! ```
//!
//! ## Bypass prevention
//!
//! 1. The `params_hash` binds a specific set of execution parameters to the queued
//!    operation. Executing with different parameters is rejected even if the same
//!    operation type is queued.
//! 2. Only the original proposer can execute an operation.
//! 3. Only the admin or the designated guardian can cancel an operation.
//! 4. The minimum delay is enforced by the contract — callers cannot pass a delay
//!    shorter than `MIN_DELAY_LEDGERS` for protected operation types.
//! 5. Once Executed or Cancelled, an operation ID cannot be reused.

#![no_std]
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env, Symbol,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Minimum delay for rotate_key and force_retire operations: ~24 hours at 5 s/ledger.
pub const MIN_DELAY_ROTATE_KEY: u32 = 17_280;

/// Minimum delay for pause_marketplace: ~12 hours at 5 s/ledger.
pub const MIN_DELAY_PAUSE: u32 = 8_640;

/// Absolute minimum delay any operation can have (guards against accidental 0-delay).
pub const ABSOLUTE_MIN_DELAY: u32 = 100;

// ── Storage keys ──────────────────────────────────────────────────────────────

const CONFIG: Symbol = symbol_short!("CONFIG");

/// Per-operation storage key: ("OP", op_id)
fn op_key(_e: &Env, op_id: &BytesN<32>) -> (Symbol, BytesN<32>) {
    (symbol_short!("OP"), op_id.clone())
}

// ── Data types ────────────────────────────────────────────────────────────────

/// Timelock contract configuration.
#[contracttype]
#[derive(Clone)]
pub struct TimelockConfig {
    /// The admin address — can propose and execute operations.
    pub admin: Address,
    /// An optional guardian address that can cancel operations but not propose/execute.
    /// Set to admin if no separate guardian is desired.
    pub guardian: Address,
}

/// The type of admin operation being timelocked.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum OperationType {
    /// Rotate the oracle's Ed25519 public key in `carbon_oracle`.
    /// Minimum delay: MIN_DELAY_ROTATE_KEY.
    RotateOracleKey,
    /// Permanently retire a project in `carbon_registry`.
    /// Minimum delay: MIN_DELAY_ROTATE_KEY (same 24h — irreversible).
    ForceRetireProject,
    /// Pause all marketplace state-modifying operations.
    /// Minimum delay: MIN_DELAY_PAUSE.
    PauseMarketplace,
    /// Generic catch-all for other admin operations.
    /// Minimum delay: ABSOLUTE_MIN_DELAY.
    Generic,
}

/// Status of a queued timelock operation.
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
pub enum OperationStatus {
    /// Queued and waiting for the delay to elapse.
    Pending,
    /// Successfully executed after the delay.
    Executed,
    /// Cancelled by admin or guardian before execution.
    Cancelled,
}

/// A queued admin operation with its delay and parameter binding.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct TimelockOperation {
    /// The type of operation being timelocked.
    pub op_type: OperationType,
    /// SHA-256 hash of the canonical execution parameters. The execute call must
    /// supply matching parameters — this prevents parameter substitution attacks
    /// where a queued benign operation is executed with malicious parameters.
    pub params_hash: BytesN<32>,
    /// Ledger sequence at which this operation was proposed.
    pub proposed_at: u32,
    /// Earliest ledger at which this operation may be executed.
    /// `execute_after = proposed_at + delay_ledgers`
    pub execute_after: u32,
    /// Address that proposed this operation. Only this address may execute it.
    pub proposer: Address,
    /// Current status of this operation.
    pub status: OperationStatus,
}

// ── Error codes ───────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum TimelockError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    OperationNotFound = 4,
    /// The delay period has not yet elapsed — too early to execute.
    DelayNotElapsed = 5,
    /// The operation is not in Pending status (already executed or cancelled).
    OperationNotPending = 6,
    /// The params_hash provided to execute does not match the queued hash.
    ParamsMismatch = 7,
    /// The requested delay is shorter than the minimum for this operation type.
    DelayTooShort = 8,
    /// The operation ID already exists (duplicate proposal).
    DuplicateOperation = 9,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct CarbonTimelock;

#[contractimpl]
impl CarbonTimelock {
    // ── Initialization ──────────────────────────────────────────────────────

    /// Initialize the timelock contract with an admin and optional guardian.
    ///
    /// - `admin`: can propose, execute, and cancel operations.
    /// - `guardian`: can only cancel operations (use admin address if no separate guardian).
    pub fn initialize(e: Env, admin: Address, guardian: Address) -> Result<(), TimelockError> {
        if e.storage().instance().has(&CONFIG) {
            return Err(TimelockError::AlreadyInitialized);
        }
        admin.require_auth();
        let cfg = TimelockConfig { admin, guardian };
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    // ── Operation lifecycle ─────────────────────────────────────────────────

    /// Propose a new timelocked admin operation.
    ///
    /// # Parameters
    /// - `proposer`: must be the admin; authorizes the proposal.
    /// - `op_id`: a caller-chosen unique 32-byte identifier for this operation.
    ///   Typically `SHA256(op_type || params_hash || timestamp)` computed off-chain.
    /// - `op_type`: the category of operation (determines minimum delay).
    /// - `params_hash`: SHA-256 of the canonical operation parameters.
    ///   Must match what is provided to `execute_operation` later.
    /// - `delay_ledgers`: number of ledgers to wait. Must be ≥ the minimum for `op_type`.
    ///
    /// # Bypass prevention
    /// - Only the admin can propose.
    /// - The minimum delay for each op type is enforced.
    /// - Duplicate op_ids are rejected.
    /// - The `params_hash` binds the specific parameters at proposal time.
    pub fn propose_operation(
        e: Env,
        proposer: Address,
        op_id: BytesN<32>,
        op_type: OperationType,
        params_hash: BytesN<32>,
        delay_ledgers: u32,
    ) -> Result<(), TimelockError> {
        let cfg = Self::load_config(&e)?;
        // Only the admin may propose operations.
        proposer.require_auth();
        if proposer != cfg.admin {
            return Err(TimelockError::Unauthorized);
        }

        // Enforce minimum delay per operation type.
        let min_delay = Self::minimum_delay(&op_type);
        if delay_ledgers < min_delay {
            return Err(TimelockError::DelayTooShort);
        }

        // Reject duplicate operation IDs.
        let key = op_key(&e, &op_id);
        if e.storage().persistent().has(&key) {
            return Err(TimelockError::DuplicateOperation);
        }

        let current_ledger = e.ledger().sequence();
        let execute_after = current_ledger.saturating_add(delay_ledgers);

        let op = TimelockOperation {
            op_type,
            params_hash,
            proposed_at: current_ledger,
            execute_after,
            proposer,
            status: OperationStatus::Pending,
        };

        e.storage().persistent().set(&key, &op);

        // Emit proposal event for off-chain monitoring.
        e.events()
            .publish((symbol_short!("TL_PROP"), op_id.clone()), execute_after);

        Ok(())
    }

    /// Execute a previously proposed operation after the delay has elapsed.
    ///
    /// # Parameters
    /// - `executor`: must be the original proposer; authorizes execution.
    /// - `op_id`: identifies the queued operation.
    /// - `actual_params_hash`: SHA-256 of the parameters that will be used.
    ///   Must match the `params_hash` stored at proposal time.
    ///
    /// # Bypass prevention
    /// - Only the original proposer can execute.
    /// - Delay must have elapsed.
    /// - The `actual_params_hash` must match the stored hash exactly.
    /// - The operation must be in Pending status.
    ///
    /// This function does not make the cross-contract call itself — that is the
    /// responsibility of the calling contract or off-chain client. It records the
    /// execution approval on-chain so that the guarded contracts can verify a
    /// timelock approval exists before proceeding.
    pub fn execute_operation(
        e: Env,
        executor: Address,
        op_id: BytesN<32>,
        actual_params_hash: BytesN<32>,
    ) -> Result<(), TimelockError> {
        let cfg = Self::load_config(&e)?;
        executor.require_auth();

        let key = op_key(&e, &op_id);
        let mut op: TimelockOperation = e
            .storage()
            .persistent()
            .get(&key)
            .ok_or(TimelockError::OperationNotFound)?;

        // Only the original proposer may execute.
        if executor != op.proposer {
            return Err(TimelockError::Unauthorized);
        }

        // Must still be the admin (guards against admin rotation between propose and execute).
        if executor != cfg.admin {
            return Err(TimelockError::Unauthorized);
        }

        // Operation must be Pending.
        if op.status != OperationStatus::Pending {
            return Err(TimelockError::OperationNotPending);
        }

        // Delay must have elapsed.
        let current_ledger = e.ledger().sequence();
        if current_ledger < op.execute_after {
            return Err(TimelockError::DelayNotElapsed);
        }

        // Parameters must match the commitment made at proposal time.
        if actual_params_hash != op.params_hash {
            return Err(TimelockError::ParamsMismatch);
        }

        // Mark as executed.
        op.status = OperationStatus::Executed;
        e.storage().persistent().set(&key, &op);

        // Emit execution event for off-chain monitoring and audit trail.
        e.events()
            .publish((symbol_short!("TL_EXEC"), op_id.clone()), current_ledger);

        Ok(())
    }

    /// Cancel a pending operation.
    ///
    /// May be called by the admin OR the guardian. This separation of concerns
    /// allows a guardian address with limited scope to veto suspicious proposals
    /// without having any propose or execute authority.
    ///
    /// # Parameters
    /// - `canceller`: must be admin or guardian.
    /// - `op_id`: identifies the queued operation to cancel.
    pub fn cancel_operation(
        e: Env,
        canceller: Address,
        op_id: BytesN<32>,
    ) -> Result<(), TimelockError> {
        let cfg = Self::load_config(&e)?;
        canceller.require_auth();

        // Admin or guardian may cancel.
        if canceller != cfg.admin && canceller != cfg.guardian {
            return Err(TimelockError::Unauthorized);
        }

        let key = op_key(&e, &op_id);
        let mut op: TimelockOperation = e
            .storage()
            .persistent()
            .get(&key)
            .ok_or(TimelockError::OperationNotFound)?;

        // Only Pending operations can be cancelled.
        if op.status != OperationStatus::Pending {
            return Err(TimelockError::OperationNotPending);
        }

        op.status = OperationStatus::Cancelled;
        e.storage().persistent().set(&key, &op);

        // Emit cancellation event.
        e.events().publish(
            (symbol_short!("TL_CNCL"), op_id.clone()),
            e.ledger().sequence(),
        );

        Ok(())
    }

    // ── Read-only queries ───────────────────────────────────────────────────

    /// Return the full operation record for the given op_id.
    pub fn get_operation(e: Env, op_id: BytesN<32>) -> Result<TimelockOperation, TimelockError> {
        e.storage()
            .persistent()
            .get(&op_key(&e, &op_id))
            .ok_or(TimelockError::OperationNotFound)
    }

    /// Check if an operation is ready to execute (delay elapsed and still Pending).
    pub fn is_ready(e: Env, op_id: BytesN<32>) -> Result<bool, TimelockError> {
        let op: TimelockOperation = e
            .storage()
            .persistent()
            .get(&op_key(&e, &op_id))
            .ok_or(TimelockError::OperationNotFound)?;

        Ok(op.status == OperationStatus::Pending && e.ledger().sequence() >= op.execute_after)
    }

    /// Return the current timelock configuration.
    pub fn get_config(e: Env) -> Result<TimelockConfig, TimelockError> {
        Self::load_config(&e)
    }

    /// Return the minimum delay ledgers for a given operation type.
    pub fn get_minimum_delay(e: Env, op_type: OperationType) -> u32 {
        let _ = e;
        Self::minimum_delay(&op_type)
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    fn load_config(e: &Env) -> Result<TimelockConfig, TimelockError> {
        e.storage()
            .instance()
            .get(&CONFIG)
            .ok_or(TimelockError::NotInitialized)
    }

    /// Returns the minimum required delay ledgers for an operation type.
    fn minimum_delay(op_type: &OperationType) -> u32 {
        match op_type {
            OperationType::RotateOracleKey => MIN_DELAY_ROTATE_KEY,
            OperationType::ForceRetireProject => MIN_DELAY_ROTATE_KEY,
            OperationType::PauseMarketplace => MIN_DELAY_PAUSE,
            OperationType::Generic => ABSOLUTE_MIN_DELAY,
        }
    }
}

mod tests;
