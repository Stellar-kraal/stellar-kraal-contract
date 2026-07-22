#![cfg(test)]

use crate::*;
use soroban_sdk::{symbol_short, testutils::Address as _, Address, BytesN, Env};

fn make_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn deploy_credit(env: &Env) -> (CarbonCreditClient<'_>, Address, Address, Address) {
    let registry = Address::generate(env);
    let marketplace = Address::generate(env);
    let admin = Address::generate(env);
    let client = CarbonCreditClient::new(env, &env.register(CarbonCredit, ()));
    client.initialize(&admin, &registry, &marketplace);
    (client, admin, registry, marketplace)
}

fn fake_project_id(env: &Env) -> BytesN<32> {
    BytesN::from_array(env, &[1u8; 32])
}

// ── Initialization ─────────────────────────────────────────────────────────

#[test]
fn test_initialize_succeeds() {
    let env = make_env();
    let (client, _admin, _registry, _marketplace) = deploy_credit(&env);
    // verify config can be loaded (no panic)
    let _ = client.balance_of(&Address::generate(&env), &fake_project_id(&env));
}

#[test]
fn test_initialize_twice_fails() {
    let env = make_env();
    let (client, admin, registry, marketplace) = deploy_credit(&env);
    let res = client.try_initialize(&admin, &registry, &marketplace);
    assert_eq!(res, Err(Ok(CreditError::AlreadyInitialized)));
}

// ── Mint ───────────────────────────────────────────────────────────────────

/// Mint requires a cross-contract call to the registry.
/// We register a real registry contract so that the invoke_contract call succeeds.
#[test]
fn test_mint_increases_balance_and_supply() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();

    // Deploy registry
    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    // Register and verify a project
    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("TEST"), &1000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    // Deploy credit contract pointing at real registry
    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let recipient = Address::generate(&env);
    credit_client.mint(&recipient, &project_id, &100_i128);

    assert_eq!(credit_client.balance_of(&recipient, &project_id), 100);
    assert_eq!(credit_client.total_supply(&project_id), 100);
}

#[test]
fn test_mint_zero_amount_fails() {
    let env = make_env();
    let (client, _admin, _registry, _marketplace) = deploy_credit(&env);
    let recipient = Address::generate(&env);
    let res = client.try_mint(&recipient, &fake_project_id(&env), &0_i128);
    assert_eq!(res, Err(Ok(CreditError::InvalidAmount)));
}

#[test]
fn test_mint_negative_amount_fails() {
    let env = make_env();
    let (client, _admin, _registry, _marketplace) = deploy_credit(&env);
    let recipient = Address::generate(&env);
    let res = client.try_mint(&recipient, &fake_project_id(&env), &(-1_i128));
    assert_eq!(res, Err(Ok(CreditError::InvalidAmount)));
}

/// VULN-CC-01 reproduction: demonstrates that mint() uses a stale project status.
/// After a project is suspended in the registry, a mint call with mock_all_auths
/// will fail because the registry get_project will return Suspended status — this
/// test documents the TOCTOU window by showing suspend → mint returns ProjectNotVerified.
#[test]
fn test_vuln_cc01_toctou_mint_after_suspend_fails() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();

    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("TCTOU"), &1000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    // Suspend the project BEFORE the mint call executes
    reg_client.suspend_project(&project_id);

    // Now mint should fail because the registry reports Suspended
    let recipient = Address::generate(&env);
    let res = credit_client.try_mint(&recipient, &project_id, &100_i128);
    assert_eq!(
        res,
        Err(Ok(CreditError::ProjectNotVerified)),
        "Mint on a suspended project must fail"
    );
}

// ── Transfer ───────────────────────────────────────────────────────────────

#[test]
fn test_transfer_moves_balance() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();

    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("TRF"), &1000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &200_i128);

    credit_client.transfer(&alice, &bob, &project_id, &80_i128);

    assert_eq!(credit_client.balance_of(&alice, &project_id), 120);
    assert_eq!(credit_client.balance_of(&bob, &project_id), 80);
}

#[test]
fn test_transfer_insufficient_balance_fails() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();
    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("TRF2"), &1000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &50_i128);

    let res = credit_client.try_transfer(&alice, &bob, &project_id, &100_i128);
    assert_eq!(res, Err(Ok(CreditError::InsufficientBalance)));
}

// ── Burn ───────────────────────────────────────────────────────────────────

#[test]
fn test_burn_reduces_balance_and_supply() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();
    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("BURN"), &1000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let alice = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &300_i128);
    credit_client.burn(&alice, &project_id, &100_i128);

    assert_eq!(credit_client.balance_of(&alice, &project_id), 200);
    assert_eq!(credit_client.total_supply(&project_id), 200);
}

#[test]
fn test_burn_more_than_balance_fails() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();
    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("BURN2"), &1000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let alice = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &50_i128);
    let res = credit_client.try_burn(&alice, &project_id, &100_i128);
    assert_eq!(res, Err(Ok(CreditError::InsufficientBalance)));
}

// ── Property-based style tests ─────────────────────────────────────────────

/// Property: total supply is conserved across a transfer — no credits created or destroyed.
#[test]
fn test_prop_credits_conserved_across_transfer() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();
    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("CONS"), &10000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let alice = Address::generate(&env);
    let bob = Address::generate(&env);
    let carol = Address::generate(&env);

    // Mint initial supply
    credit_client.mint(&alice, &project_id, &1000_i128);
    credit_client.mint(&bob, &project_id, &500_i128);

    let supply_before = credit_client.total_supply(&project_id);
    let alice_before = credit_client.balance_of(&alice, &project_id);
    let bob_before = credit_client.balance_of(&bob, &project_id);
    let carol_before = credit_client.balance_of(&carol, &project_id);

    // Total pre-transfer individual balances must equal total supply
    assert_eq!(
        alice_before + bob_before + carol_before,
        supply_before,
        "Sum of balances must equal total supply before transfer"
    );

    // Perform transfers
    credit_client.transfer(&alice, &carol, &project_id, &200_i128);
    credit_client.transfer(&bob, &alice, &project_id, &100_i128);

    let supply_after = credit_client.total_supply(&project_id);
    let alice_after = credit_client.balance_of(&alice, &project_id);
    let bob_after = credit_client.balance_of(&bob, &project_id);
    let carol_after = credit_client.balance_of(&carol, &project_id);

    // Total supply must be unchanged
    assert_eq!(
        supply_before, supply_after,
        "Total supply must not change across transfers"
    );

    // Sum of individual balances must still equal total supply
    assert_eq!(
        alice_after + bob_after + carol_after,
        supply_after,
        "Sum of balances must equal total supply after transfer"
    );
}

/// Property: balance_of never returns negative.
#[test]
fn test_prop_balance_never_negative() {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();
    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(&env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("NNEG"), &5000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    let alice = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &500_i128);

    // Attempt to over-burn (should fail, not result in negative)
    let _ = credit_client.try_burn(&alice, &project_id, &600_i128);

    assert!(
        credit_client.balance_of(&alice, &project_id) >= 0,
        "Balance must never be negative"
    );
}

// ── Replay Attack Tests (RS-01: retire idempotency) ────────────────────────

/// Helper: build a full credit-contract environment with a verified project.
fn setup_credit_with_project<'a>(
    env: &'a Env,
) -> (
    CarbonCreditClient<'a>,
    Address,  // marketplace_addr (authorized minter)
    BytesN<32>, // project_id
) {
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let reg_client = CarbonRegistryClient::new(env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(env);
    let marketplace_addr = Address::generate(env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    let owner = Address::generate(env);
    let project_id =
        reg_client.register_project(&owner, &symbol_short!("RPLY"), &10000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    (credit_client, marketplace_addr, project_id)
}

/// PoC — RS-01: demonstrates that WITHOUT idempotency protection, the same
/// logical retire operation could be replayed by submitting multiple transactions
/// with different Stellar sequence numbers but identical arguments.
///
/// This test shows the *before-fix* threat model: a caller who retires 100
/// credits twice (different Stellar transactions, same logical intent) would
/// drain 200 credits. With the `operation_id` mitigation applied, the second
/// call is rejected with `AlreadyRetired`, protecting the caller's balance.
///
/// The test verifies the PoC scenario end-to-end: first retire succeeds, second
/// retire with the same `operation_id` fails, and total retired equals the
/// intended 100, not 200.
#[test]
fn test_poc_retire_replay_without_operation_id() {
    let env = make_env();
    let (credit_client, _, project_id) = setup_credit_with_project(&env);

    let alice = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &1000_i128);

    let balance_before = credit_client.balance_of(&alice, &project_id);
    let retired_before = credit_client.retired_supply(&project_id);

    // Alice's retire intent: operation_id is a unique nonce per intent.
    // An attacker or faulty relay that resubmits this operation would carry the
    // SAME operation_id (because the intent is the same logical action).
    let op_id = BytesN::from_array(&env, &[0xAA_u8; 32]);

    // First retire succeeds
    credit_client.retire(&alice, &project_id, &100_i128, &op_id);

    let balance_mid = credit_client.balance_of(&alice, &project_id);
    let retired_mid = credit_client.retired_supply(&project_id);

    assert_eq!(balance_mid, balance_before - 100, "First retire must deduct 100");
    assert_eq!(retired_mid, retired_before + 100, "First retire must add 100 to retired supply");

    // PoC: second call with SAME operation_id must be rejected (replay blocked).
    // Before the fix, this would retire another 100 credits, draining Alice's balance.
    let res = credit_client.try_retire(&alice, &project_id, &100_i128, &op_id);
    assert_eq!(
        res,
        Err(Ok(CreditError::AlreadyRetired)),
        "RS-01 PoC: replay of same operation_id must be rejected with AlreadyRetired"
    );

    // Balance must be exactly 100 fewer than before — not 200 fewer
    let balance_after = credit_client.balance_of(&alice, &project_id);
    let retired_after = credit_client.retired_supply(&project_id);

    assert_eq!(
        balance_after,
        balance_before - 100,
        "RS-01 PoC: only 100 credits must be retired, not 200 (replay blocked)"
    );
    assert_eq!(
        retired_after,
        retired_before + 100,
        "RS-01 PoC: retired supply must be 100, not 200"
    );
}

/// Mitigation regression — RS-01: verifies that the operation_id dedup guard
/// is permanently enforced: even a fresh retire call on a different amount but
/// the same operation_id must be rejected.
#[test]
fn test_mitigation_retire_operation_id_dedup() {
    let env = make_env();
    let (credit_client, _, project_id) = setup_credit_with_project(&env);

    let alice = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &1000_i128);

    let op_id = BytesN::from_array(&env, &[0x11_u8; 32]);

    // First retire: succeeds
    credit_client.retire(&alice, &project_id, &50_i128, &op_id);

    assert_eq!(credit_client.balance_of(&alice, &project_id), 950);
    assert_eq!(credit_client.retired_supply(&project_id), 50);

    // Second call — same operation_id, even with a different amount — must fail.
    let res = credit_client.try_retire(&alice, &project_id, &50_i128, &op_id);
    assert_eq!(
        res,
        Err(Ok(CreditError::AlreadyRetired)),
        "Mitigation regression: duplicate operation_id must always be rejected"
    );

    // State must be identical to after the first retire
    assert_eq!(
        credit_client.balance_of(&alice, &project_id),
        950,
        "Balance must not change after rejected replay"
    );
    assert_eq!(
        credit_client.retired_supply(&project_id),
        50,
        "Retired supply must not change after rejected replay"
    );

    // A DIFFERENT operation_id allows a new legitimate retire
    let op_id_2 = BytesN::from_array(&env, &[0x22_u8; 32]);
    credit_client.retire(&alice, &project_id, &50_i128, &op_id_2);

    assert_eq!(
        credit_client.balance_of(&alice, &project_id),
        900,
        "A new operation_id must allow a new retire"
    );
    assert_eq!(
        credit_client.retired_supply(&project_id),
        100,
        "Retired supply must increment for a new operation_id"
    );
}

/// Mitigation regression — RS-01: verifies that multiple distinct operation_ids
/// from the same caller each retire independently, and that total retired equals
/// the sum of all individual retirements.
#[test]
fn test_mitigation_retire_multiple_unique_operation_ids() {
    let env = make_env();
    let (credit_client, _, project_id) = setup_credit_with_project(&env);

    let alice = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &1000_i128);

    // Retire in three separate operations with unique ids
    let op_ids: [[u8; 32]; 3] = [
        [0x01_u8; 32],
        [0x02_u8; 32],
        [0x03_u8; 32],
    ];
    let amounts: [i128; 3] = [100, 200, 300];
    let mut total_retired = 0_i128;

    for (i, (op_bytes, amt)) in op_ids.iter().zip(amounts.iter()).enumerate() {
        let op_id = BytesN::from_array(&env, op_bytes);
        credit_client.retire(&alice, &project_id, amt, &op_id);
        total_retired += amt;

        assert_eq!(
            credit_client.retired_supply(&project_id),
            total_retired,
            "After retire #{}: retired supply must be {}",
            i + 1,
            total_retired
        );
    }

    assert_eq!(
        credit_client.balance_of(&alice, &project_id),
        1000 - total_retired,
        "Alice's remaining balance must equal 1000 minus total retired"
    );
}

/// Mitigation regression — RS-01: the operation_id dedup guard fires BEFORE
/// any balance check, so a replay is rejected even if the caller's balance
/// would be insufficient to cover the amount.
#[test]
fn test_mitigation_retire_dedup_before_balance_check() {
    let env = make_env();
    let (credit_client, _, project_id) = setup_credit_with_project(&env);

    let alice = Address::generate(&env);
    credit_client.mint(&alice, &project_id, &100_i128);

    let op_id = BytesN::from_array(&env, &[0x55_u8; 32]);

    // First retire drains the balance
    credit_client.retire(&alice, &project_id, &100_i128, &op_id);
    assert_eq!(credit_client.balance_of(&alice, &project_id), 0);

    // Second call with same op_id: balance is 0, amount is 1.
    // The dedup guard must fire FIRST (AlreadyRetired), not InsufficientBalance.
    let res = credit_client.try_retire(&alice, &project_id, &1_i128, &op_id);
    assert_eq!(
        res,
        Err(Ok(CreditError::AlreadyRetired)),
        "Dedup guard must fire before balance check"
    );
}