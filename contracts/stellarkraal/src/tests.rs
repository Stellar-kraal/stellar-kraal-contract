#![cfg(test)]

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Address, Env,
};

use crate::{Error, StellarKraal, StellarKraalClient};

fn make_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

fn deploy(env: &Env) -> StellarKraalClient<'_> {
    StellarKraalClient::new(env, &env.register(StellarKraal, ()))
}

fn init<'a>(env: &Env, client: &StellarKraalClient<'a>) -> (Address, Address, Address, Address) {
    let admin = Address::generate(env);
    let o1 = Address::generate(env);
    let o2 = Address::generate(env);
    let o3 = Address::generate(env);
    client.initialize(
        &admin, &o1, &o2, &o3, &7_000, &8_500, &500, &1_000, &0, &100,
    );
    (admin, o1, o2, o3)
}

fn push_prices(client: &StellarKraalClient, o1: &Address, o2: &Address, o3: &Address, price: i128) {
    client.submit_price(o1, &price);
    client.submit_price(o2, &price);
    client.submit_price(o3, &price);
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[test]
fn test_initialize_twice_fails() {
    let env = make_env();
    let client = deploy(&env);
    let (admin, o1, o2, o3) = init(&env, &client);
    let res = client.try_initialize(
        &admin, &o1, &o2, &o3, &7_000, &8_500, &500, &1_000, &0, &100,
    );
    assert_eq!(res, Err(Ok(Error::AlreadyInitialized)));
}

#[test]
fn test_ltv_boundary_exact_allowed() {
    let env = make_env();
    let client = deploy(&env);
    let _ = init(&env, &client);
    let owner = Address::generate(&env);
    let asset_id = client.register_asset(
        &owner,
        &symbol_short!("CATTLE"),
        &symbol_short!("T1"),
        &1_000_000,
    );
    let loan_id = client.open_loan(&owner, &asset_id, &700_000);
    let loan = client.get_loan(&loan_id);
    assert_eq!(loan.principal, 700_000);
}

#[test]
fn test_ltv_boundary_over_fails() {
    let env = make_env();
    let client = deploy(&env);
    let _ = init(&env, &client);
    let owner = Address::generate(&env);
    let asset_id = client.register_asset(
        &owner,
        &symbol_short!("CATTLE"),
        &symbol_short!("T2"),
        &1_000_000,
    );
    let res = client.try_open_loan(&owner, &asset_id, &700_001);
    assert_eq!(res, Err(Ok(Error::PrincipalExceedsLtv)));
}

#[test]
fn test_health_factor_safe() {
    let env = make_env();
    let client = deploy(&env);
    let _ = init(&env, &client);
    let owner = Address::generate(&env);
    // appraised=1_000_000, borrow=500_000
    // HF = (1_000_000 * 8500 * 100) / (500_000 * 10_000)
    //    = 850_000_000_000 / 5_000_000_000 = 170
    let asset_id = client.register_asset(
        &owner,
        &symbol_short!("CATTLE"),
        &symbol_short!("T3"),
        &1_000_000,
    );
    let loan_id = client.open_loan(&owner, &asset_id, &500_000);
    let hf = client.health_factor(&loan_id);
    assert_eq!(hf, 170);
}

#[test]
fn test_liquidation_safe_loan_fails() {
    let env = make_env();
    let client = deploy(&env);
    let _ = init(&env, &client);
    let owner = Address::generate(&env);
    // appraised=1_000_000, borrow=500_000 → HF=170 ≥ 120 → safe
    let asset_id = client.register_asset(
        &owner,
        &symbol_short!("CATTLE"),
        &symbol_short!("T4"),
        &1_000_000,
    );
    let loan_id = client.open_loan(&owner, &asset_id, &500_000);
    let liquidator = Address::generate(&env);
    let res = client.try_liquidate(&liquidator, &loan_id);
    assert_eq!(res, Err(Ok(Error::HealthFactorSafe)));
}

#[test]
fn test_liquidation_boundary() {
    let env = make_env();
    // Bump TTLs before creating any state so entries live long enough
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 10_000_000;
        li.max_entry_ttl = 10_000_001;
    });
    let client = deploy(&env);
    // LTV=90% max (collateral_ratio_bps=9000). liquidation_threshold_bps=9500
    // (> collateral) so a max loan at 90% of appraised has
    // HF = 1_000_000 * 9500 * 100 / (900_000 * 10_000) = 105 < 120 → liquidatable.
    let admin = Address::generate(&env);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    client.initialize(
        &admin, &o1, &o2, &o3, &9_000, &9_500, &500, &1_000, &0, &100,
    );

    let owner = Address::generate(&env);
    // appraised=1_000_000, LTV=90% → max borrow=900_000
    // HF = (1_000_000 * 8500 * 100) / (900_000 * 10_000) = 94 (< 120) → liquidatable
    let asset_id = client.register_asset(
        &owner,
        &symbol_short!("CATTLE"),
        &symbol_short!("T5"),
        &1_000_000,
    );
    let loan_id = client.open_loan(&owner, &asset_id, &900_000);
    let hf = client.health_factor(&loan_id);
    assert!(hf < 120, "expected HF < 120 but got {hf}");

    let liquidator = Address::generate(&env);
    let seized = client.liquidate(&liquidator, &loan_id);
    // seized = 900_000 * 10_500 / 10_000 = 945_000
    assert_eq!(seized, 945_000);
}

#[test]
fn test_oracle_staleness_rejected() {
    let env = make_env();
    let client = deploy(&env);
    let (_, o1, o2, o3) = init(&env, &client);
    push_prices(&client, &o1, &o2, &o3, 100_000);
    // Advance past max_price_age (100 ledgers)
    env.ledger().with_mut(|li| li.sequence_number = 200);
    let res = client.try_get_price();
    assert_eq!(res, Err(Ok(Error::StalePrices)));
}

#[test]
fn test_oracle_median() {
    let env = make_env();
    let client = deploy(&env);
    let (_, o1, o2, o3) = init(&env, &client);
    client.submit_price(&o1, &100);
    client.submit_price(&o2, &200);
    client.submit_price(&o3, &150);
    let price = client.get_price();
    assert_eq!(price, 150); // median of [100, 150, 200]
}

#[test]
fn test_interest_accrual() {
    let env = make_env();
    // Bump TTLs before creating state so entries survive the ledger jump
    env.ledger().with_mut(|li| {
        li.min_persistent_entry_ttl = 10_000_000;
        li.max_entry_ttl = 10_000_001;
    });
    let client = deploy(&env);
    let _ = init(&env, &client);
    let owner = Address::generate(&env);
    let asset_id = client.register_asset(
        &owner,
        &symbol_short!("CATTLE"),
        &symbol_short!("T6"),
        &1_000_000,
    );
    let loan_id = client.open_loan(&owner, &asset_id, &100_000);
    // Advance exactly 1 year of ledgers
    // interest = 100_000 * 1000 * 6_307_200 / (10_000 * 6_307_200) = 10_000
    env.ledger().with_mut(|li| li.sequence_number = 6_307_200);
    let remaining = client.repay_loan(&owner, &loan_id, &0);
    assert_eq!(remaining, 110_000);
}

#[test]
fn test_full_repayment_frees_asset() {
    let env = make_env();
    let client = deploy(&env);
    let _ = init(&env, &client);
    let owner = Address::generate(&env);
    let asset_id = client.register_asset(
        &owner,
        &symbol_short!("CATTLE"),
        &symbol_short!("T7"),
        &1_000_000,
    );
    let loan_id = client.open_loan(&owner, &asset_id, &100_000);
    let remaining = client.repay_loan(&owner, &loan_id, &200_000);
    assert_eq!(remaining, 0);
    let asset = client.get_asset(&asset_id);
    assert!(!asset.on_loan);
}

#[test]
fn test_unauthorized_oracle_rejected() {
    let env = make_env();
    let client = deploy(&env);
    let _ = init(&env, &client);
    let rogue = Address::generate(&env);
    let res = client.try_submit_price(&rogue, &999);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
}

// ── initialize() config validation ─────────────────────────────────────────

#[test]
fn test_initialize_zero_collateral_rejected() {
    let env = make_env();
    let client = deploy(&env);
    let admin = Address::generate(&env);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    let res = client.try_initialize(
        &admin, &o1, &o2, &o3, &0, &8_500, &500, &1_000, &0, &100,
    );
    assert_eq!(res, Err(Ok(Error::InvalidConfig)));
}

#[test]
fn test_initialize_negative_collateral_rejected() {
    let env = make_env();
    let client = deploy(&env);
    let admin = Address::generate(&env);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    let res = client.try_initialize(
        &admin, &o1, &o2, &o3, &-1, &8_500, &500, &1_000, &0, &100,
    );
    assert_eq!(res, Err(Ok(Error::InvalidConfig)));
}

#[test]
fn test_initialize_threshold_equal_collateral_rejected() {
    let env = make_env();
    let client = deploy(&env);
    let admin = Address::generate(&env);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    // liquidation_threshold_bps == collateral_ratio_bps is not allowed
    let res = client.try_initialize(
        &admin, &o1, &o2, &o3, &7_000, &7_000, &500, &1_000, &0, &100,
    );
    assert_eq!(res, Err(Ok(Error::InvalidConfig)));
}

#[test]
fn test_initialize_threshold_below_collateral_rejected() {
    let env = make_env();
    let client = deploy(&env);
    let admin = Address::generate(&env);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    // liquidation_threshold_bps < collateral_ratio_bps is not allowed
    let res = client.try_initialize(
        &admin, &o1, &o2, &o3, &9_000, &8_500, &500, &1_000, &0, &100,
    );
    assert_eq!(res, Err(Ok(Error::InvalidConfig)));
}

#[test]
fn test_initialize_threshold_above_100pct_rejected() {
    let env = make_env();
    let client = deploy(&env);
    let admin = Address::generate(&env);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    // liquidation_threshold_bps > 10_000 (>100%) would make all loans
    // permanently liquidatable
    let res = client.try_initialize(
        &admin, &o1, &o2, &o3, &7_000, &10_500, &500, &1_000, &0, &100,
    );
    assert_eq!(res, Err(Ok(Error::InvalidConfig)));
}

#[test]
fn test_initialize_valid_config_succeeds() {
    let env = make_env();
    let client = deploy(&env);
    let admin = Address::generate(&env);
    let o1 = Address::generate(&env);
    let o2 = Address::generate(&env);
    let o3 = Address::generate(&env);
    // collateral 7000, threshold 8500 (threshold > collateral, both <= 10_000)
    let res = client.try_initialize(
        &admin, &o1, &o2, &o3, &7_000, &8_500, &500, &1_000, &0, &100,
    );
    assert_eq!(res, Ok(Ok(())));
}
