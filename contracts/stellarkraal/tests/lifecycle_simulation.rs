//! Deterministic simulation test suite for the full loan lifecycle end-to-end.
//!
//! Each scenario exercises the complete StellarKraal lifecycle — contract
//! initialization, oracle price feeds, asset (collateral) registration, loan
//! issuance, repayment, and liquidation — as a single coordinated scenario
//! with multiple parties (admin, three oracle feeders, farmers/borrowers, and
//! a third-party liquidator).
//!
//! Determinism: every scenario pins the ledger sequence and timestamp up
//! front and advances them explicitly. No wall-clock time or randomness is
//! involved, so runs are bit-for-bit reproducible (the SDK's test snapshots
//! under `test_snapshots/` guard against drift).
//!
//! Scenario variants:
//! 1. `scenario_happy_path_full_lifecycle` — happy path across every module
//! 2. `scenario_partial_repayment_then_full` — partial fills of the balance
//! 3. `scenario_stale_oracle_feed_expires_and_recovers` — feed expiry
//! 4. `scenario_rejected_requests_leave_state_untouched` — rejected actions
//! 5. `scenario_forced_liquidation_after_interest_accrual` — forced close

#![cfg(test)]

use soroban_sdk::{
    symbol_short,
    testutils::{Address as _, Ledger},
    Address, Env,
};
use stellarkraal::{Error, StellarKraal, StellarKraalClient};

// ── Simulation constants ──────────────────────────────────────────────────────

const GENESIS_SEQUENCE: u32 = 100;
const GENESIS_TIMESTAMP: u64 = 1_700_000_000;
/// Must match `LEDGERS_PER_YEAR` in the contract (~1 ledger / 5 s).
const LEDGERS_PER_YEAR: u32 = 6_307_200;

const COLLATERAL_RATIO_BPS: i128 = 7_000; // 70% LTV
const LIQ_THRESHOLD_BPS: i128 = 8_500;
const LIQ_BONUS_BPS: i128 = 500;
const INTEREST_RATE_BPS: i128 = 1_000; // 10% APR
const CREATION_FEE: i128 = 1_000;
const MAX_PRICE_AGE: u32 = 100;

// ── Harness ───────────────────────────────────────────────────────────────────

/// All parties participating in a simulation scenario.
struct Actors {
    admin: Address,
    oracle_1: Address,
    oracle_2: Address,
    oracle_3: Address,
    farmer_a: Address,
    farmer_b: Address,
    liquidator: Address,
}

struct Sim<'a> {
    env: Env,
    client: StellarKraalClient<'a>,
    actors: Actors,
}

impl Sim<'_> {
    /// Deploy and initialize the contract on a pinned, deterministic ledger.
    fn boot() -> Self {
        let env = Env::default();
        env.mock_all_auths();
        env.ledger().with_mut(|li| {
            li.sequence_number = GENESIS_SEQUENCE;
            li.timestamp = GENESIS_TIMESTAMP;
            // Long TTLs so persistent entries survive multi-year ledger jumps.
            li.min_persistent_entry_ttl = 4 * LEDGERS_PER_YEAR;
            li.max_entry_ttl = 4 * LEDGERS_PER_YEAR + 1;
        });

        let client = StellarKraalClient::new(&env, &env.register(StellarKraal, ()));
        let actors = Actors {
            admin: Address::generate(&env),
            oracle_1: Address::generate(&env),
            oracle_2: Address::generate(&env),
            oracle_3: Address::generate(&env),
            farmer_a: Address::generate(&env),
            farmer_b: Address::generate(&env),
            liquidator: Address::generate(&env),
        };

        client.initialize(
            &actors.admin,
            &actors.oracle_1,
            &actors.oracle_2,
            &actors.oracle_3,
            &COLLATERAL_RATIO_BPS,
            &LIQ_THRESHOLD_BPS,
            &LIQ_BONUS_BPS,
            &INTEREST_RATE_BPS,
            &CREATION_FEE,
            &MAX_PRICE_AGE,
        );

        Sim {
            env,
            client,
            actors,
        }
    }

    /// Advance the simulated chain by `ledgers`, keeping time consistent
    /// with Stellar's ~5 s ledger close time.
    fn advance(&self, ledgers: u32) {
        self.env.ledger().with_mut(|li| {
            li.sequence_number += ledgers;
            li.timestamp += u64::from(ledgers) * 5;
        });
    }

    /// All three oracle feeders submit (possibly diverging) prices.
    fn feed_prices(&self, p1: i128, p2: i128, p3: i128) {
        self.client.submit_price(&self.actors.oracle_1, &p1);
        self.client.submit_price(&self.actors.oracle_2, &p2);
        self.client.submit_price(&self.actors.oracle_3, &p3);
    }
}

// ── Scenario 1: happy path ────────────────────────────────────────────────────

/// Full coordinated lifecycle: admin initializes, three oracle feeders push
/// diverging prices, two farmers register collateral, one opens a loan,
/// interest accrues over half a year, and the loan is repaid in full.
/// Final-state assertions cover every storage domain the contract owns:
/// config, oracle price feed, assets, and loans.
#[test]
fn scenario_happy_path_full_lifecycle() {
    let sim = Sim::boot();
    let (client, actors) = (&sim.client, &sim.actors);

    // Oracle round: median of diverging submissions must win.
    sim.feed_prices(95_000, 100_000, 104_000);
    assert_eq!(client.get_price(), 100_000);

    // Registration round: two independent farmers register collateral.
    let cow_a = client.register_asset(
        &actors.farmer_a,
        &symbol_short!("CATTLE"),
        &symbol_short!("KR001"),
        &2_000_000,
    );
    sim.advance(1); // asset ids are seeded from the ledger sequence
    let goat_b = client.register_asset(
        &actors.farmer_b,
        &symbol_short!("GOAT"),
        &symbol_short!("KR002"),
        &500_000,
    );
    assert_ne!(cow_a, goat_b);

    // Issuance round: farmer A borrows 50% of appraised value.
    let loan_id = client.open_loan(&actors.farmer_a, &cow_a, &1_000_000);
    let loan = client.get_loan(&loan_id);
    assert_eq!(loan.principal, 1_000_000);
    assert_eq!(loan.balance, 1_000_000 + CREATION_FEE); // fee rolled in
    assert!(loan.active);
    assert!(client.get_asset(&cow_a).on_loan);

    // Half a year passes; oracles keep the feed alive.
    sim.advance(LEDGERS_PER_YEAR / 2);
    sim.feed_prices(101_000, 102_000, 103_000);

    // Settlement round: 10% APR for half a year on 1_001_000 = 50_050.
    let remaining = client.repay_loan(&actors.farmer_a, &loan_id, &2_000_000);
    assert_eq!(remaining, 0);

    // ── Final on-chain state across all storage domains ──
    // Config: untouched by the whole scenario.
    let cfg = client.get_config();
    assert_eq!(cfg.admin, actors.admin);
    assert_eq!(cfg.interest_rate_bps, INTEREST_RATE_BPS);
    // Oracle feed: fresh and still the median.
    assert_eq!(client.get_price(), 102_000);
    // Assets: A's collateral is released, B's was never encumbered.
    let asset_a = client.get_asset(&cow_a);
    assert_eq!(asset_a.owner, actors.farmer_a);
    assert!(!asset_a.on_loan);
    let asset_b = client.get_asset(&goat_b);
    assert_eq!(asset_b.owner, actors.farmer_b);
    assert!(!asset_b.on_loan);
    // Loan: settled, zero balance, principal preserved for audit.
    let loan = client.get_loan(&loan_id);
    assert!(!loan.active);
    assert_eq!(loan.balance, 0);
    assert_eq!(loan.principal, 1_000_000);
    assert_eq!(client.health_factor(&loan_id), i128::MAX);
}

// ── Scenario 2: partial fill ──────────────────────────────────────────────────

/// The borrower settles the balance in three partial repayments within the
/// same ledger (zero interest between fills), then any further repayment is
/// rejected because the loan is closed.
#[test]
fn scenario_partial_repayment_then_full() {
    let sim = Sim::boot();
    let (client, actors) = (&sim.client, &sim.actors);

    let asset_id = client.register_asset(
        &actors.farmer_a,
        &symbol_short!("CATTLE"),
        &symbol_short!("KR003"),
        &1_000_000,
    );
    let loan_id = client.open_loan(&actors.farmer_a, &asset_id, &500_000);
    let opening_balance = 500_000 + CREATION_FEE;

    // First fill: balance drops, collateral stays locked.
    let after_first = client.repay_loan(&actors.farmer_a, &loan_id, &200_000);
    assert_eq!(after_first, opening_balance - 200_000);
    assert!(client.get_loan(&loan_id).active);
    assert!(client.get_asset(&asset_id).on_loan);

    // Second fill.
    let after_second = client.repay_loan(&actors.farmer_a, &loan_id, &150_000);
    assert_eq!(after_second, opening_balance - 350_000);
    assert!(client.get_asset(&asset_id).on_loan);

    // Final fill overshoots the residual; contract clamps to zero and
    // releases the collateral.
    let after_final = client.repay_loan(&actors.farmer_a, &loan_id, &200_000);
    assert_eq!(after_final, 0);
    let loan = client.get_loan(&loan_id);
    assert!(!loan.active);
    assert_eq!(loan.balance, 0);
    assert!(!client.get_asset(&asset_id).on_loan);

    // A closed loan accepts no further repayments.
    let res = client.try_repay_loan(&actors.farmer_a, &loan_id, &1);
    assert_eq!(res, Err(Ok(Error::LoanNotActive)));
}

// ── Scenario 3: expired feed ──────────────────────────────────────────────────

/// The oracle feed expires after `max_price_age_ledgers` and consumers are
/// rejected until the feeders come back; a fresh round fully recovers the
/// feed. Also covers a partially-submitted round (only 2 of 3 feeders).
#[test]
fn scenario_stale_oracle_feed_expires_and_recovers() {
    let sim = Sim::boot();
    let (client, actors) = (&sim.client, &sim.actors);

    sim.feed_prices(100_000, 100_500, 99_500);
    assert_eq!(client.get_price(), 100_000);

    // Still fresh exactly at the age limit.
    sim.advance(MAX_PRICE_AGE);
    assert_eq!(client.get_price(), 100_000);

    // One ledger past the limit: the feed is stale.
    sim.advance(1);
    assert_eq!(client.try_get_price(), Err(Ok(Error::StalePrices)));

    // A partial round (2 of 3 feeders) does not resurrect the feed.
    client.submit_price(&actors.oracle_1, &110_000);
    client.submit_price(&actors.oracle_2, &111_000);
    assert_eq!(client.try_get_price(), Err(Ok(Error::StalePrices)));

    // Third feeder completes the round: feed recovers with the new median.
    client.submit_price(&actors.oracle_3, &112_000);
    assert_eq!(client.get_price(), 111_000);
}

// ── Scenario 4: rejected requests ─────────────────────────────────────────────

/// Every rejected interaction — over-LTV borrowing, double-collateralizing,
/// borrowing against a phantom asset, and an unauthorized price feeder —
/// must fail with the right error and leave on-chain state untouched.
#[test]
fn scenario_rejected_requests_leave_state_untouched() {
    let sim = Sim::boot();
    let (client, actors) = (&sim.client, &sim.actors);

    sim.feed_prices(100_000, 100_000, 100_000);
    let asset_id = client.register_asset(
        &actors.farmer_a,
        &symbol_short!("CATTLE"),
        &symbol_short!("KR004"),
        &1_000_000,
    );

    // Over-LTV request (70% of 1_000_000 = 700_000 max) is rejected and the
    // collateral stays free.
    let res = client.try_open_loan(&actors.farmer_a, &asset_id, &700_001);
    assert_eq!(res, Err(Ok(Error::PrincipalExceedsLtv)));
    assert!(!client.get_asset(&asset_id).on_loan);

    // A valid loan at the exact LTV boundary then succeeds.
    let loan_id = client.open_loan(&actors.farmer_a, &asset_id, &700_000);
    assert!(client.get_loan(&loan_id).active);

    // The same collateral cannot back a second loan.
    let res = client.try_open_loan(&actors.farmer_a, &asset_id, &100_000);
    assert_eq!(res, Err(Ok(Error::AssetAlreadyOnLoan)));

    // Borrowing against an unregistered asset is rejected.
    let phantom = soroban_sdk::BytesN::from_array(&sim.env, &[7u8; 32]);
    let res = client.try_open_loan(&actors.farmer_b, &phantom, &1);
    assert_eq!(res, Err(Ok(Error::AssetNotFound)));

    // A rogue feeder cannot poison the price feed.
    let res = client.try_submit_price(&actors.liquidator, &1);
    assert_eq!(res, Err(Ok(Error::Unauthorized)));
    assert_eq!(client.get_price(), 100_000);

    // A liquidation attempt on the healthy loan is rejected.
    let res = client.try_liquidate(&actors.liquidator, &loan_id);
    assert_eq!(res, Err(Ok(Error::HealthFactorSafe)));
    let loan = client.get_loan(&loan_id);
    assert!(loan.active);
    assert_eq!(loan.balance, 700_000 + CREATION_FEE);
}

// ── Scenario 5: forced liquidation ────────────────────────────────────────────

/// A loan opened at the LTV boundary decays below the liquidation threshold
/// purely through interest accrual; a third-party liquidator then force-closes
/// it and receives the bonus-adjusted seizure amount.
#[test]
fn scenario_forced_liquidation_after_interest_accrual() {
    let sim = Sim::boot();
    let (client, actors) = (&sim.client, &sim.actors);

    sim.feed_prices(100_000, 100_000, 100_000);
    let asset_id = client.register_asset(
        &actors.farmer_b,
        &symbol_short!("CATTLE"),
        &symbol_short!("KR005"),
        &1_000_000,
    );
    // Max borrow at 70% LTV. Opening balance = 700_000 + 1_000 fee.
    // HF = (1_000_000 * 8_500 * 100) / (701_000 * 10_000) = 121 → safe.
    let loan_id = client.open_loan(&actors.farmer_b, &asset_id, &700_000);
    assert!(client.health_factor(&loan_id) >= 120);
    assert_eq!(
        client.try_liquidate(&actors.liquidator, &loan_id),
        Err(Ok(Error::HealthFactorSafe))
    );

    // One year of 10% APR: balance = 701_000 + 70_100 = 771_100.
    // HF = (1_000_000 * 8_500 * 100) / (771_100 * 10_000) = 110 → liquidatable.
    sim.advance(LEDGERS_PER_YEAR);

    let seized = client.liquidate(&actors.liquidator, &loan_id);
    // seized = 771_100 * (10_000 + 500) / 10_000 = 809_655.
    assert_eq!(seized, 809_655);

    // Final state: loan force-closed, collateral released for the farmer.
    let loan = client.get_loan(&loan_id);
    assert!(!loan.active);
    assert_eq!(loan.balance, 0);
    assert!(!client.get_asset(&asset_id).on_loan);

    // A second liquidation of the same loan is rejected.
    assert_eq!(
        client.try_liquidate(&actors.liquidator, &loan_id),
        Err(Ok(Error::LoanNotActive))
    );
}
