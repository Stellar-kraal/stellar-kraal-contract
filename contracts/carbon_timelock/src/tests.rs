//! Unit tests for the `carbon_timelock` contract.
//!
//! ## Coverage
//!
//! ### Happy-path tests
//! - `test_initialize_succeeds` — contract stores admin and guardian correctly.
//! - `test_propose_operation_succeeds` — operation queued with correct fields.
//! - `test_execute_operation_succeeds` — execution after delay is allowed.
//! - `test_cancel_by_admin_succeeds` — admin can cancel a pending operation.
//! - `test_cancel_by_guardian_succeeds` — guardian can cancel a pending operation.
//! - `test_is_ready_before_and_after_delay` — is_ready returns correct boolean.
//! - `test_get_minimum_delay_values` — minimum delays match constants.
//!
//! ### Bypass prevention tests (acceptance criteria)
//! - `test_delay_not_elapsed_rejects_execution` — cannot execute before delay elapses.
//! - `test_params_mismatch_rejects_execution` — different params_hash is rejected.
//! - `test_non_proposer_cannot_execute` — only original proposer may execute.
//! - `test_non_admin_cannot_propose` — only admin may propose.
//! - `test_non_admin_non_guardian_cannot_cancel` — unauthorized cancel is rejected.
//! - `test_delay_too_short_rejected` — delay below minimum is rejected per op type.
//! - `test_duplicate_op_id_rejected` — same op_id cannot be proposed twice.
//! - `test_executed_operation_cannot_be_executed_again` — no double execution.
//! - `test_executed_operation_cannot_be_cancelled` — no cancel after execute.
//! - `test_cancelled_operation_cannot_be_executed` — no execute after cancel.
//! - `test_cancelled_operation_cannot_be_cancelled_again` — no double cancel.
//!
//! ### Delay enforcement tests (acceptance criteria — "delay period is enforced")
//! - `test_rotate_oracle_key_enforces_24h_delay` — RotateOracleKey requires ≥17280 ledgers.
//! - `test_force_retire_enforces_24h_delay` — ForceRetireProject requires ≥17280 ledgers.
//! - `test_pause_marketplace_enforces_12h_delay` — PauseMarketplace requires ≥8640 ledgers.
//! - `test_execute_exactly_at_delay_boundary` — execution succeeds at exactly execute_after.
//! - `test_execute_one_ledger_before_boundary_fails` — off-by-one: execute_after - 1 rejected.

#[cfg(test)]
mod timelock_tests {
    use soroban_sdk::{
        testutils::{Address as _, Ledger},
        Address, Bytes, BytesN, Env,
    };

    use crate::{
        CarbonTimelock, CarbonTimelockClient, OperationStatus, OperationType, TimelockError,
        ABSOLUTE_MIN_DELAY, MIN_DELAY_PAUSE, MIN_DELAY_ROTATE_KEY,
    };

    // ── Test helpers ──────────────────────────────────────────────────────────

    /// Build a deterministic 32-byte operation ID from a u64 seed.
    fn make_op_id(e: &Env, seed: u64) -> BytesN<32> {
        let mut b = Bytes::new(e);
        for byte in seed.to_be_bytes() {
            b.push_back(byte);
        }
        // Pad to 32 bytes
        for _ in 0..24 {
            b.push_back(0u8);
        }
        e.crypto().sha256(&b).into()
    }

    /// Build a deterministic 32-byte params hash from a u64 seed.
    fn make_params_hash(e: &Env, seed: u64) -> BytesN<32> {
        let mut b = Bytes::new(e);
        for byte in seed.to_be_bytes() {
            b.push_back(byte);
        }
        for _ in 0..24 {
            b.push_back(0xff_u8);
        }
        e.crypto().sha256(&b).into()
    }

    /// Set up a fresh environment with the contract deployed and initialized.
    /// Returns (env, client, admin, guardian).
    fn setup() -> (Env, CarbonTimelockClient<'static>, Address, Address) {
        let e = Env::default();
        e.mock_all_auths();
        // Set high TTL limits so that advancing the ledger by up to 34,560 ledgers
        // (the timelock's maximum delay window) does not archive instance or persistent
        // storage entries. The default test env uses min_persistent_entry_ttl = 4096,
        // which is smaller than MIN_DELAY_ROTATE_KEY = 17,280.
        e.ledger().with_mut(|l| {
            l.min_persistent_entry_ttl = 100_000;
            l.max_entry_ttl = 100_001;
        });
        let contract_id = e.register(CarbonTimelock, ());
        let client = CarbonTimelockClient::new(&e, &contract_id);

        let admin = Address::generate(&e);
        let guardian = Address::generate(&e);
        client.initialize(&admin, &guardian);

        (e, client, admin, guardian)
    }

    // ── Happy-path tests ──────────────────────────────────────────────────────

    #[test]
    fn test_initialize_succeeds() {
        let (e, client, admin, guardian) = setup();
        let _ = e;
        let cfg = client.get_config();
        assert_eq!(cfg.admin, admin);
        assert_eq!(cfg.guardian, guardian);
    }

    #[test]
    fn test_initialize_twice_fails() {
        let (e, client, admin, guardian) = setup();
        let _ = e;
        let result = client.try_initialize(&admin, &guardian);
        assert_eq!(result, Err(Ok(TimelockError::AlreadyInitialized)));
    }

    #[test]
    fn test_propose_operation_succeeds() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 1);
        let params_hash = make_params_hash(&e, 42);

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::RotateOracleKey,
            &params_hash,
            &MIN_DELAY_ROTATE_KEY,
        );

        let op = client.get_operation(&op_id);
        assert_eq!(op.op_type, OperationType::RotateOracleKey);
        assert_eq!(op.params_hash, params_hash);
        assert_eq!(op.proposer, admin);
        assert_eq!(op.status, OperationStatus::Pending);
        assert_eq!(op.proposed_at, e.ledger().sequence());
        assert_eq!(
            op.execute_after,
            e.ledger().sequence() + MIN_DELAY_ROTATE_KEY
        );
    }

    #[test]
    fn test_execute_operation_succeeds() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 2);
        let params_hash = make_params_hash(&e, 99);
        let delay = MIN_DELAY_ROTATE_KEY;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::RotateOracleKey,
            &params_hash,
            &delay,
        );

        // Advance ledger past the delay.
        e.ledger().with_mut(|l| {
            l.sequence_number += delay;
        });

        // Execution should succeed.
        client.execute_operation(&admin, &op_id, &params_hash);

        let op = client.get_operation(&op_id);
        assert_eq!(op.status, OperationStatus::Executed);
    }

    #[test]
    fn test_cancel_by_admin_succeeds() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 3);
        let params_hash = make_params_hash(&e, 10);

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::ForceRetireProject,
            &params_hash,
            &MIN_DELAY_ROTATE_KEY,
        );

        client.cancel_operation(&admin, &op_id);

        let op = client.get_operation(&op_id);
        assert_eq!(op.status, OperationStatus::Cancelled);
    }

    #[test]
    fn test_cancel_by_guardian_succeeds() {
        let (e, client, admin, guardian) = setup();
        let op_id = make_op_id(&e, 4);
        let params_hash = make_params_hash(&e, 11);

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::ForceRetireProject,
            &params_hash,
            &MIN_DELAY_ROTATE_KEY,
        );

        // Guardian (not admin) cancels the suspicious operation.
        client.cancel_operation(&guardian, &op_id);

        let op = client.get_operation(&op_id);
        assert_eq!(op.status, OperationStatus::Cancelled);
    }

    #[test]
    fn test_is_ready_before_and_after_delay() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 5);
        let params_hash = make_params_hash(&e, 77);
        let delay = MIN_DELAY_PAUSE;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::PauseMarketplace,
            &params_hash,
            &delay,
        );

        // Before delay: not ready.
        assert!(!client.is_ready(&op_id));

        // Advance exactly to execute_after - 1: still not ready.
        e.ledger().with_mut(|l| {
            l.sequence_number += delay - 1;
        });
        assert!(!client.is_ready(&op_id));

        // Advance one more ledger: now ready.
        e.ledger().with_mut(|l| {
            l.sequence_number += 1;
        });
        assert!(client.is_ready(&op_id));
    }

    #[test]
    fn test_get_minimum_delay_values() {
        let (e, client, _admin, _guardian) = setup();
        let _ = e;
        assert_eq!(
            client.get_minimum_delay(&OperationType::RotateOracleKey),
            MIN_DELAY_ROTATE_KEY
        );
        assert_eq!(
            client.get_minimum_delay(&OperationType::ForceRetireProject),
            MIN_DELAY_ROTATE_KEY
        );
        assert_eq!(
            client.get_minimum_delay(&OperationType::PauseMarketplace),
            MIN_DELAY_PAUSE
        );
        assert_eq!(
            client.get_minimum_delay(&OperationType::Generic),
            ABSOLUTE_MIN_DELAY
        );
    }

    // ── Bypass prevention tests ───────────────────────────────────────────────

    /// BYPASS-01: Timelock cannot be bypassed by executing before delay elapses.
    #[test]
    fn test_delay_not_elapsed_rejects_execution() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 10);
        let params_hash = make_params_hash(&e, 1);
        let delay = MIN_DELAY_ROTATE_KEY;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::RotateOracleKey,
            &params_hash,
            &delay,
        );

        // Advance less than the full delay.
        e.ledger().with_mut(|l| {
            l.sequence_number += delay - 1;
        });

        let result = client.try_execute_operation(&admin, &op_id, &params_hash);
        assert_eq!(result, Err(Ok(TimelockError::DelayNotElapsed)));

        // Status remains Pending — no state change.
        let op = client.get_operation(&op_id);
        assert_eq!(op.status, OperationStatus::Pending);
    }

    /// BYPASS-02: Execution with wrong params_hash is rejected — prevents parameter substitution.
    #[test]
    fn test_params_mismatch_rejects_execution() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 11);
        let real_params_hash = make_params_hash(&e, 200);
        let fake_params_hash = make_params_hash(&e, 201); // Different!
        let delay = MIN_DELAY_ROTATE_KEY;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::RotateOracleKey,
            &real_params_hash,
            &delay,
        );

        e.ledger().with_mut(|l| {
            l.sequence_number += delay;
        });

        // Attempt to execute with different params.
        let result = client.try_execute_operation(&admin, &op_id, &fake_params_hash);
        assert_eq!(result, Err(Ok(TimelockError::ParamsMismatch)));

        // Status remains Pending — no state change.
        let op = client.get_operation(&op_id);
        assert_eq!(op.status, OperationStatus::Pending);
    }

    /// BYPASS-03: A different address (not the original proposer) cannot execute.
    #[test]
    fn test_non_proposer_cannot_execute() {
        let (e, client, admin, _guardian) = setup();
        let attacker = Address::generate(&e);
        let op_id = make_op_id(&e, 12);
        let params_hash = make_params_hash(&e, 5);
        let delay = MIN_DELAY_ROTATE_KEY;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::RotateOracleKey,
            &params_hash,
            &delay,
        );

        e.ledger().with_mut(|l| {
            l.sequence_number += delay;
        });

        // Attacker tries to execute — rejected because attacker != proposer AND attacker != admin.
        let result = client.try_execute_operation(&attacker, &op_id, &params_hash);
        assert_eq!(result, Err(Ok(TimelockError::Unauthorized)));
    }

    /// BYPASS-04: Only admin can propose operations.
    #[test]
    fn test_non_admin_cannot_propose() {
        let (e, client, _admin, _guardian) = setup();
        let attacker = Address::generate(&e);
        let op_id = make_op_id(&e, 13);
        let params_hash = make_params_hash(&e, 6);

        let result = client.try_propose_operation(
            &attacker,
            &op_id,
            &OperationType::RotateOracleKey,
            &params_hash,
            &MIN_DELAY_ROTATE_KEY,
        );
        assert_eq!(result, Err(Ok(TimelockError::Unauthorized)));
    }

    /// BYPASS-05: Neither a random address nor the proposer (when not guardian) can cancel.
    #[test]
    fn test_non_admin_non_guardian_cannot_cancel() {
        let (e, client, admin, _guardian) = setup();
        let random = Address::generate(&e);
        let op_id = make_op_id(&e, 14);
        let params_hash = make_params_hash(&e, 7);

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::ForceRetireProject,
            &params_hash,
            &MIN_DELAY_ROTATE_KEY,
        );

        let result = client.try_cancel_operation(&random, &op_id);
        assert_eq!(result, Err(Ok(TimelockError::Unauthorized)));

        // Still Pending.
        let op = client.get_operation(&op_id);
        assert_eq!(op.status, OperationStatus::Pending);
    }

    /// BYPASS-06: Delay below the minimum for the operation type is rejected at proposal time.
    #[test]
    fn test_delay_too_short_rejected() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 15);
        let params_hash = make_params_hash(&e, 8);

        // Try to propose RotateOracleKey with delay = MIN_DELAY_ROTATE_KEY - 1.
        let result = client.try_propose_operation(
            &admin,
            &op_id,
            &OperationType::RotateOracleKey,
            &params_hash,
            &(MIN_DELAY_ROTATE_KEY - 1),
        );
        assert_eq!(result, Err(Ok(TimelockError::DelayTooShort)));
    }

    /// BYPASS-07: Duplicate operation ID is rejected.
    #[test]
    fn test_duplicate_op_id_rejected() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 16);
        let params_hash = make_params_hash(&e, 9);

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::Generic,
            &params_hash,
            &ABSOLUTE_MIN_DELAY,
        );

        // Second proposal with same op_id.
        let result = client.try_propose_operation(
            &admin,
            &op_id,
            &OperationType::Generic,
            &params_hash,
            &ABSOLUTE_MIN_DELAY,
        );
        assert_eq!(result, Err(Ok(TimelockError::DuplicateOperation)));
    }

    /// BYPASS-08: An already-executed operation cannot be executed again.
    #[test]
    fn test_executed_operation_cannot_be_executed_again() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 17);
        let params_hash = make_params_hash(&e, 50);
        let delay = MIN_DELAY_PAUSE;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::PauseMarketplace,
            &params_hash,
            &delay,
        );

        e.ledger().with_mut(|l| {
            l.sequence_number += delay;
        });

        // First execution succeeds.
        client.execute_operation(&admin, &op_id, &params_hash);

        // Second execution on the same op_id is rejected.
        let result = client.try_execute_operation(&admin, &op_id, &params_hash);
        assert_eq!(result, Err(Ok(TimelockError::OperationNotPending)));
    }

    /// BYPASS-09: An executed operation cannot be cancelled.
    #[test]
    fn test_executed_operation_cannot_be_cancelled() {
        let (e, client, admin, guardian) = setup();
        let op_id = make_op_id(&e, 18);
        let params_hash = make_params_hash(&e, 51);
        let delay = MIN_DELAY_PAUSE;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::PauseMarketplace,
            &params_hash,
            &delay,
        );

        e.ledger().with_mut(|l| {
            l.sequence_number += delay;
        });

        client.execute_operation(&admin, &op_id, &params_hash);

        // Cancel attempt fails — operation is no longer Pending.
        let result = client.try_cancel_operation(&guardian, &op_id);
        assert_eq!(result, Err(Ok(TimelockError::OperationNotPending)));
    }

    /// BYPASS-10: A cancelled operation cannot be executed.
    #[test]
    fn test_cancelled_operation_cannot_be_executed() {
        let (e, client, admin, guardian) = setup();
        let op_id = make_op_id(&e, 19);
        let params_hash = make_params_hash(&e, 52);
        let delay = MIN_DELAY_ROTATE_KEY;

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::RotateOracleKey,
            &params_hash,
            &delay,
        );

        // Guardian cancels during the window.
        client.cancel_operation(&guardian, &op_id);

        // Advance past the delay.
        e.ledger().with_mut(|l| {
            l.sequence_number += delay;
        });

        // Execute attempt fails — operation was cancelled.
        let result = client.try_execute_operation(&admin, &op_id, &params_hash);
        assert_eq!(result, Err(Ok(TimelockError::OperationNotPending)));
    }

    /// BYPASS-11: A cancelled operation cannot be cancelled again.
    #[test]
    fn test_cancelled_operation_cannot_be_cancelled_again() {
        let (e, client, admin, guardian) = setup();
        let op_id = make_op_id(&e, 20);
        let params_hash = make_params_hash(&e, 53);

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::Generic,
            &params_hash,
            &ABSOLUTE_MIN_DELAY,
        );

        client.cancel_operation(&admin, &op_id);

        let result = client.try_cancel_operation(&guardian, &op_id);
        assert_eq!(result, Err(Ok(TimelockError::OperationNotPending)));
    }

    // ── Delay enforcement tests ───────────────────────────────────────────────

    /// DELAY-01: RotateOracleKey enforces minimum 24h (17,280 ledger) delay.
    #[test]
    fn test_rotate_oracle_key_enforces_24h_delay() {
        let (e, client, admin, _guardian) = setup();

        // Below minimum — rejected.
        let below_min = MIN_DELAY_ROTATE_KEY - 1;
        let op_id1 = make_op_id(&e, 30);
        let ph1 = make_params_hash(&e, 30);
        let result = client.try_propose_operation(
            &admin,
            &op_id1,
            &OperationType::RotateOracleKey,
            &ph1,
            &below_min,
        );
        assert_eq!(result, Err(Ok(TimelockError::DelayTooShort)));

        // Exactly at minimum — accepted.
        let op_id2 = make_op_id(&e, 31);
        let ph2 = make_params_hash(&e, 31);
        client.propose_operation(
            &admin,
            &op_id2,
            &OperationType::RotateOracleKey,
            &ph2,
            &MIN_DELAY_ROTATE_KEY,
        );
        let op = client.get_operation(&op_id2);
        assert_eq!(op.status, OperationStatus::Pending);
        assert_eq!(
            op.execute_after,
            e.ledger().sequence() + MIN_DELAY_ROTATE_KEY
        );
    }

    /// DELAY-02: ForceRetireProject enforces minimum 24h delay.
    #[test]
    fn test_force_retire_enforces_24h_delay() {
        let (e, client, admin, _guardian) = setup();

        let op_id = make_op_id(&e, 32);
        let ph = make_params_hash(&e, 32);

        // Below minimum — rejected.
        let result = client.try_propose_operation(
            &admin,
            &op_id,
            &OperationType::ForceRetireProject,
            &ph,
            &(MIN_DELAY_ROTATE_KEY - 1),
        );
        assert_eq!(result, Err(Ok(TimelockError::DelayTooShort)));

        // At minimum — accepted.
        let op_id2 = make_op_id(&e, 33);
        client.propose_operation(
            &admin,
            &op_id2,
            &OperationType::ForceRetireProject,
            &ph,
            &MIN_DELAY_ROTATE_KEY,
        );
        let op = client.get_operation(&op_id2);
        assert_eq!(op.status, OperationStatus::Pending);
    }

    /// DELAY-03: PauseMarketplace enforces minimum 12h (8,640 ledger) delay.
    #[test]
    fn test_pause_marketplace_enforces_12h_delay() {
        let (e, client, admin, _guardian) = setup();

        let op_id = make_op_id(&e, 34);
        let ph = make_params_hash(&e, 34);

        // Below minimum — rejected.
        let result = client.try_propose_operation(
            &admin,
            &op_id,
            &OperationType::PauseMarketplace,
            &ph,
            &(MIN_DELAY_PAUSE - 1),
        );
        assert_eq!(result, Err(Ok(TimelockError::DelayTooShort)));

        // At minimum — accepted.
        let op_id2 = make_op_id(&e, 35);
        client.propose_operation(
            &admin,
            &op_id2,
            &OperationType::PauseMarketplace,
            &ph,
            &MIN_DELAY_PAUSE,
        );
        let op = client.get_operation(&op_id2);
        assert_eq!(op.status, OperationStatus::Pending);
    }

    /// DELAY-04: Execution succeeds exactly at the execute_after boundary.
    #[test]
    fn test_execute_exactly_at_delay_boundary() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 40);
        let params_hash = make_params_hash(&e, 40);
        let delay = MIN_DELAY_PAUSE;
        let proposed_at = e.ledger().sequence();

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::PauseMarketplace,
            &params_hash,
            &delay,
        );

        // Advance to exactly execute_after (proposed_at + delay).
        e.ledger().with_mut(|l| {
            l.sequence_number = proposed_at + delay;
        });

        // Should succeed.
        client.execute_operation(&admin, &op_id, &params_hash);

        let op = client.get_operation(&op_id);
        assert_eq!(op.status, OperationStatus::Executed);
    }

    /// DELAY-05: Execution fails at exactly execute_after - 1 (off-by-one boundary).
    #[test]
    fn test_execute_one_ledger_before_boundary_fails() {
        let (e, client, admin, _guardian) = setup();
        let op_id = make_op_id(&e, 41);
        let params_hash = make_params_hash(&e, 41);
        let delay = MIN_DELAY_PAUSE;
        let proposed_at = e.ledger().sequence();

        client.propose_operation(
            &admin,
            &op_id,
            &OperationType::PauseMarketplace,
            &params_hash,
            &delay,
        );

        // Advance to exactly execute_after - 1 (one ledger too early).
        e.ledger().with_mut(|l| {
            l.sequence_number = proposed_at + delay - 1;
        });

        let result = client.try_execute_operation(&admin, &op_id, &params_hash);
        assert_eq!(result, Err(Ok(TimelockError::DelayNotElapsed)));
    }

    /// DELAY-06: Operations not found return correct error.
    #[test]
    fn test_get_operation_not_found() {
        let (e, client, _admin, _guardian) = setup();
        let op_id = make_op_id(&e, 99);
        let result = client.try_get_operation(&op_id);
        assert_eq!(result, Err(Ok(TimelockError::OperationNotFound)));
    }

    /// DELAY-07: is_ready returns error for non-existent operation.
    #[test]
    fn test_is_ready_not_found() {
        let (e, client, _admin, _guardian) = setup();
        let op_id = make_op_id(&e, 100);
        let result = client.try_is_ready(&op_id);
        assert_eq!(result, Err(Ok(TimelockError::OperationNotFound)));
    }
}
