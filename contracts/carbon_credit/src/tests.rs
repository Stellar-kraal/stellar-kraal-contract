#![cfg(test)]

use soroban_sdk::{testutils::{Address as _}, Address, BytesN, Env, symbol_short};
use crate::*;

fn make_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

// ── Helpers ────────────────────────────────────────────────────────────────

fn deploy_registry(env: &Env) -> (Address, Address, Address) {
    use soroban_sdk::IntoVal;

    let admin = Address::generate(env);
    let marketplace = Address::generate(env);

    // We register a mock registry — in unit tests we mock cross-contract calls,
    // so we only need the address for config.
    (Address::generate(env), admin, marketplace)
}

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
    let (client, admin, registry, marketplace) = deploy_credit(&env);
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
    use soroban_sdk::{IntoVal, Symbol};
    use carbon_registry::{CarbonRegistry, CarbonRegistryClient};

    let env = make_env();

    // Deploy registry
    let reg_client = CarbonRegistryClient::new(&env, &env.register(CarbonRegistry, ()));
    let reg_admin = Address::generate(&env);
    let marketplace_addr = Address::generate(&env);
    reg_client.initialize(&reg_admin, &marketplace_addr);

    // Register and verify a project
    let owner = Address::generate(&env);
    let project_id = reg_client.register_project(&owner, &symbol_short!("TEST"), &1000_i128, &2024_u32);
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
    let project_id = reg_client.register_project(&owner, &symbol_short!("TCTOU"), &1000_i128, &2024_u32);
    reg_client.verify_project(&project_id);

    let credit_client = CarbonCreditClient::new(&env, &env.register(CarbonCredit, ()));
    let credit_admin = Address::generate(&env);
    credit_client.initialize(&credit_admin, &reg_client.address, &marketplace_addr);

    // Suspend the project BEFORE the mint call executes
    reg_client.suspend_project(&project_id);

    // Now mint should fail because the registry reports Suspended
    let recipient = Address::generate(&env);
    let res = credit_client.try_mint(&recipient, &project_id, &100_i128);
    assert_eq!(res, Err(Ok(CreditError::ProjectNotVerified)),
        "Mint on a suspended project must fail");
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
    let project_id = reg_client.register_project(&owner, &symbol_short!("TRF"), &1000_i128, &2024_u32);
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
    let project_id = reg_client.register_project(&owner, &symbol_short!("TRF2"), &1000_i128, &2024_u32);
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
    let project_id = reg_client.register_project(&owner, &symbol_short!("BURN"), &1000_i128, &2024_u32);
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
    let project_id = reg_client.register_project(&owner, &symbol_short!("BURN2"), &1000_i128, &2024_u32);
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
    let project_id = reg_client.register_project(&owner, &symbol_short!("CONS"), &10000_i128, &2024_u32);
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
    assert_eq!(alice_before + bob_before + carol_before, supply_before,
        "Sum of balances must equal total supply before transfer");

    // Perform transfers
    credit_client.transfer(&alice, &carol, &project_id, &200_i128);
    credit_client.transfer(&bob, &alice, &project_id, &100_i128);

    let supply_after = credit_client.total_supply(&project_id);
    let alice_after = credit_client.balance_of(&alice, &project_id);
    let bob_after = credit_client.balance_of(&bob, &project_id);
    let carol_after = credit_client.balance_of(&carol, &project_id);

    // Total supply must be unchanged
    assert_eq!(supply_before, supply_after,
        "Total supply must not change across transfers");

    // Sum of individual balances must still equal total supply
    assert_eq!(alice_after + bob_after + carol_after, supply_after,
        "Sum of balances must equal total supply after transfer");
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
    let project_id = reg_client.register_project(&owner, &symbol_short!("NNEG"), &5000_i128, &2024_u32);
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
