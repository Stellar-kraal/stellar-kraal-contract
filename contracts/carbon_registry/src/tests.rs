#![cfg(test)]

use crate::*;
use soroban_sdk::{symbol_short, testutils::Address as _, Address, BytesN, Env};

fn make_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

fn deploy(env: &Env) -> CarbonRegistryClient<'_> {
    CarbonRegistryClient::new(env, &env.register(CarbonRegistry, ()))
}

fn setup(env: &Env) -> (CarbonRegistryClient<'_>, Address, Address) {
    let client = deploy(env);
    let admin = Address::generate(env);
    let marketplace = Address::generate(env);
    client.initialize(&admin, &marketplace);
    (client, admin, marketplace)
}

/// Helper: register and verify a project, returning its ID.
fn register_and_verify(
    client: &CarbonRegistryClient<'_>,
    env: &Env,
    _admin: &Address,
    total_credits: i128,
) -> BytesN<32> {
    let owner = Address::generate(env);
    let id = client.register_project(&owner, &symbol_short!("TEST"), &total_credits, &2024_u32);
    client.verify_project(&id);
    id
}

// ── Initialization ─────────────────────────────────────────────────────────

#[test]
fn test_initialize_succeeds() {
    let env = make_env();
    let (client, admin, marketplace) = setup(&env);
    let cfg = client.get_config();
    assert_eq!(cfg.admin, admin);
    assert_eq!(cfg.marketplace, marketplace);
}

#[test]
fn test_initialize_twice_fails() {
    let env = make_env();
    let (client, admin, marketplace) = setup(&env);
    let res = client.try_initialize(&admin, &marketplace);
    assert_eq!(res, Err(Ok(RegistryError::AlreadyInitialized)));
}

// ── Project registration ───────────────────────────────────────────────────

#[test]
fn test_register_project_creates_pending_project() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let owner = Address::generate(&env);
    let id = client.register_project(&owner, &symbol_short!("PROJ"), &1000_i128, &2024_u32);
    let project = client.get_project(&id);
    assert_eq!(project.status, ProjectStatus::Pending);
    assert_eq!(project.owner, owner);
    assert_eq!(project.total_credits, 1000);
    assert_eq!(project.issued_credits, 0);
    assert_eq!(project.vintage_year, 2024);
}

#[test]
fn test_register_project_zero_credits_fails() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let owner = Address::generate(&env);
    let res = client.try_register_project(&owner, &symbol_short!("PROJ"), &0_i128, &2024_u32);
    assert_eq!(res, Err(Ok(RegistryError::InvalidAmount)));
}

#[test]
fn test_register_project_negative_credits_fails() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let owner = Address::generate(&env);
    let res = client.try_register_project(&owner, &symbol_short!("PROJ"), &(-1_i128), &2024_u32);
    assert_eq!(res, Err(Ok(RegistryError::InvalidAmount)));
}

// ── Project lifecycle ──────────────────────────────────────────────────────

#[test]
fn test_verify_project_changes_status() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 1000);
    let project = client.get_project(&id);
    assert_eq!(project.status, ProjectStatus::Verified);
}

#[test]
fn test_suspend_project_changes_status() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 1000);
    client.suspend_project(&id);
    let project = client.get_project(&id);
    assert_eq!(project.status, ProjectStatus::Suspended);
}

#[test]
fn test_retire_project_changes_status() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 1000);
    client.retire_project(&id);
    let project = client.get_project(&id);
    assert_eq!(project.status, ProjectStatus::Retired);
}

#[test]
fn test_get_project_not_found_fails() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let fake_id: BytesN<32> = BytesN::from_array(&env, &[0u8; 32]);
    let res = client.try_get_project(&fake_id);
    // try_ returns Err(Ok(error)) for contract errors
    assert!(
        res.is_err(),
        "get_project on unknown id must return an error"
    );
}

// ── Issue credits ──────────────────────────────────────────────────────────

#[test]
fn test_issue_credits_verified_project_succeeds() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 1000);
    client.issue_credits(&id, &500_i128);
    let project = client.get_project(&id);
    assert_eq!(project.issued_credits, 500);
}

#[test]
fn test_issue_credits_exceeding_total_fails() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 100);
    let res = client.try_issue_credits(&id, &200_i128);
    assert_eq!(res, Err(Ok(RegistryError::InsufficientCredits)));
}

#[test]
fn test_issue_credits_suspended_project_fails() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 1000);
    client.suspend_project(&id);
    let res = client.try_issue_credits(&id, &100_i128);
    assert_eq!(res, Err(Ok(RegistryError::ProjectSuspended)));
}

#[test]
fn test_issue_credits_pending_project_fails() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let owner = Address::generate(&env);
    let id = client.register_project(&owner, &symbol_short!("PEND"), &1000_i128, &2024_u32);
    let res = client.try_issue_credits(&id, &100_i128);
    assert_eq!(res, Err(Ok(RegistryError::ProjectNotVerified)));
}

#[test]
fn test_issue_credits_zero_amount_fails() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 1000);
    let res = client.try_issue_credits(&id, &0_i128);
    assert_eq!(res, Err(Ok(RegistryError::InvalidAmount)));
}

#[test]
fn test_issue_credits_accumulates_correctly() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let id = register_and_verify(&client, &env, &_admin, 1000);
    client.issue_credits(&id, &300_i128);
    client.issue_credits(&id, &300_i128);
    let project = client.get_project(&id);
    assert_eq!(project.issued_credits, 600);
}

// ── Property-based style tests ─────────────────────────────────────────────

/// Property: issued_credits never exceeds total_credits.
#[test]
fn test_prop_issued_never_exceeds_total() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    let total: i128 = 500;
    let id = register_and_verify(&client, &env, &_admin, total);

    // Issue up to the limit
    client.issue_credits(&id, &250_i128);
    client.issue_credits(&id, &250_i128);
    let project = client.get_project(&id);
    assert_eq!(project.issued_credits, total);

    // One more should fail
    let res = client.try_issue_credits(&id, &1_i128);
    assert_eq!(res, Err(Ok(RegistryError::InsufficientCredits)));

    // Invariant holds
    let project = client.get_project(&id);
    assert!(project.issued_credits <= project.total_credits);
}

/// Property: each new project starts in Pending status.
#[test]
fn test_prop_new_project_always_pending() {
    let env = make_env();
    let (client, _admin, _marketplace) = setup(&env);
    for i in 0u32..5 {
        let owner = Address::generate(&env);
        let id = client.register_project(&owner, &symbol_short!("PROJ"), &1000_i128, &(2020 + i));
        let project = client.get_project(&id);
        assert_eq!(
            project.status,
            ProjectStatus::Pending,
            "Project {} should start in Pending",
            i
        );
    }
}
