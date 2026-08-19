#![no_std]
#![allow(clippy::too_many_arguments)]

mod tests;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    IntoVal, Symbol, Val,
};

// ── Storage keys ─────────────────────────────────────────────────────────────

const CONFIG: Symbol = symbol_short!("CONFIG");

fn asset_key(e: &Env, id: &BytesN<32>) -> Val {
    (symbol_short!("ASSET"), id.clone()).into_val(e)
}
fn loan_key(e: &Env, id: &BytesN<32>) -> Val {
    (symbol_short!("LOAN"), id.clone()).into_val(e)
}
fn price_key(e: &Env, oracle: &Address) -> Val {
    (symbol_short!("PRICE"), oracle.clone()).into_val(e)
}

// ── Data types ────────────────────────────────────────────────────────────────

#[contracttype]
#[derive(Clone)]
pub struct Config {
    pub admin: Address,
    pub oracle_1: Address,
    pub oracle_2: Address,
    pub oracle_3: Address,
    pub collateral_ratio_bps: i128,
    pub liquidation_threshold_bps: i128,
    pub liquidation_bonus_bps: i128,
    pub interest_rate_bps: i128,
    pub creation_fee_stroops: i128,
    pub max_price_age_ledgers: u32,
}

#[contracttype]
#[derive(Clone)]
pub struct Asset {
    pub owner: Address,
    pub animal_type: Symbol,
    pub tag_id: Symbol,
    pub appraised_value_xlm: i128,
    pub on_loan: bool,
}

#[contracttype]
#[derive(Clone)]
pub struct Loan {
    pub borrower: Address,
    pub asset_id: BytesN<32>,
    pub principal: i128,
    pub balance: i128,
    pub opened_at: u32,
    pub updated_at: u32,
    pub active: bool,
}

#[contracttype]
#[derive(Clone)]
pub struct PriceEntry {
    pub price: i128,
    pub submitted_at: u32,
}

// ── Errors ───────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    AssetNotFound = 4,
    LoanNotFound = 5,
    AssetAlreadyOnLoan = 6,
    PrincipalExceedsLtv = 7,
    LoanNotActive = 8,
    HealthFactorSafe = 9,
    InsufficientPrice = 10,
    ArithmeticError = 11,
    StalePrices = 12,
    InvalidConfig = 13,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn median3(a: i128, b: i128, c: i128) -> i128 {
    let mut arr = [a, b, c];
    if arr[0] > arr[1] {
        arr.swap(0, 1);
    }
    if arr[1] > arr[2] {
        arr.swap(1, 2);
    }
    if arr[0] > arr[1] {
        arr.swap(0, 1);
    }
    arr[1]
}

/// Stellar: ~1 ledger per 5 s → 6_307_200 ledgers/year
const LEDGERS_PER_YEAR: i128 = 6_307_200;

fn accrue(balance: i128, rate_bps: i128, ledgers: i128) -> Result<i128, Error> {
    let interest = balance
        .checked_mul(rate_bps)
        .ok_or(Error::ArithmeticError)?
        .checked_mul(ledgers)
        .ok_or(Error::ArithmeticError)?
        .checked_div(
            10_000_i128
                .checked_mul(LEDGERS_PER_YEAR)
                .ok_or(Error::ArithmeticError)?,
        )
        .ok_or(Error::ArithmeticError)?;
    balance.checked_add(interest).ok_or(Error::ArithmeticError)
}

fn require_config(e: &Env) -> Result<Config, Error> {
    e.storage()
        .instance()
        .get(&CONFIG)
        .ok_or(Error::NotInitialized)
}

fn get_median_price(e: &Env, cfg: &Config) -> Result<i128, Error> {
    let now = e.ledger().sequence();
    let p1: PriceEntry = e
        .storage()
        .instance()
        .get(&price_key(e, &cfg.oracle_1))
        .ok_or(Error::StalePrices)?;
    let p2: PriceEntry = e
        .storage()
        .instance()
        .get(&price_key(e, &cfg.oracle_2))
        .ok_or(Error::StalePrices)?;
    let p3: PriceEntry = e
        .storage()
        .instance()
        .get(&price_key(e, &cfg.oracle_3))
        .ok_or(Error::StalePrices)?;

    if now.saturating_sub(p1.submitted_at) > cfg.max_price_age_ledgers
        || now.saturating_sub(p2.submitted_at) > cfg.max_price_age_ledgers
        || now.saturating_sub(p3.submitted_at) > cfg.max_price_age_ledgers
    {
        return Err(Error::StalePrices);
    }
    Ok(median3(p1.price, p2.price, p3.price))
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct StellarKraal;

#[contractimpl]
impl StellarKraal {
    #[allow(clippy::too_many_arguments)]
    pub fn initialize(
        e: Env,
        admin: Address,
        oracle_1: Address,
        oracle_2: Address,
        oracle_3: Address,
        collateral_ratio_bps: i128,
        liquidation_threshold_bps: i128,
        liquidation_bonus_bps: i128,
        interest_rate_bps: i128,
        creation_fee_stroops: i128,
        max_price_age_ledgers: u32,
    ) -> Result<(), Error> {
        if e.storage().instance().has(&CONFIG) {
            return Err(Error::AlreadyInitialized);
        }

        // Guard against misconfiguration that would make the protocol unusable:
        //  - collateral_ratio_bps must be a positive bps value in 1..=10_000
        //    (0 or negative makes opening any loan impossible).
        //  - liquidation_threshold_bps must exceed collateral_ratio_bps and must
        //    not exceed 10_000 (>100% would make every loan permanently
        //    liquidatable).
        if collateral_ratio_bps <= 0 || collateral_ratio_bps > 10_000 {
            return Err(Error::InvalidConfig);
        }
        if liquidation_threshold_bps <= collateral_ratio_bps
            || liquidation_threshold_bps > 10_000
        {
            return Err(Error::InvalidConfig);
        }

        admin.require_auth();
        e.storage().instance().set(
            &CONFIG,
            &Config {
                admin,
                oracle_1,
                oracle_2,
                oracle_3,
                collateral_ratio_bps,
                liquidation_threshold_bps,
                liquidation_bonus_bps,
                interest_rate_bps,
                creation_fee_stroops,
                max_price_age_ledgers,
            },
        );
        Ok(())
    }

    pub fn register_asset(
        e: Env,
        owner: Address,
        animal_type: Symbol,
        tag_id: Symbol,
        appraised_value_xlm: i128,
    ) -> Result<BytesN<32>, Error> {
        require_config(&e)?;
        owner.require_auth();

        let mut seed = soroban_sdk::Bytes::new(&e);
        for b in e.ledger().sequence().to_be_bytes() {
            seed.push_back(b);
        }
        let id: BytesN<32> = e.crypto().sha256(&seed).into();

        e.storage().persistent().set(
            &asset_key(&e, &id),
            &Asset {
                owner,
                animal_type,
                tag_id,
                appraised_value_xlm,
                on_loan: false,
            },
        );
        Ok(id)
    }

    pub fn open_loan(
        e: Env,
        borrower: Address,
        asset_id: BytesN<32>,
        principal_stroops: i128,
    ) -> Result<BytesN<32>, Error> {
        let cfg = require_config(&e)?;
        borrower.require_auth();

        let mut asset: Asset = e
            .storage()
            .persistent()
            .get(&asset_key(&e, &asset_id))
            .ok_or(Error::AssetNotFound)?;

        if asset.on_loan {
            return Err(Error::AssetAlreadyOnLoan);
        }

        let max_principal = asset
            .appraised_value_xlm
            .checked_mul(cfg.collateral_ratio_bps)
            .ok_or(Error::ArithmeticError)?
            .checked_div(10_000)
            .ok_or(Error::ArithmeticError)?;

        if principal_stroops > max_principal {
            return Err(Error::PrincipalExceedsLtv);
        }

        asset.on_loan = true;
        e.storage()
            .persistent()
            .set(&asset_key(&e, &asset_id), &asset);

        let now = e.ledger().sequence();
        let balance = principal_stroops
            .checked_add(cfg.creation_fee_stroops)
            .ok_or(Error::ArithmeticError)?;

        let mut seed = soroban_sdk::Bytes::new(&e);
        for b in now.to_be_bytes() {
            seed.push_back(b);
        }
        // mix in asset_id bytes for uniqueness
        for b in asset_id.to_array() {
            seed.push_back(b);
        }
        let loan_id: BytesN<32> = e.crypto().sha256(&seed).into();

        e.storage().persistent().set(
            &loan_key(&e, &loan_id),
            &Loan {
                borrower,
                asset_id,
                principal: principal_stroops,
                balance,
                opened_at: now,
                updated_at: now,
                active: true,
            },
        );
        Ok(loan_id)
    }

    pub fn repay_loan(
        e: Env,
        caller: Address,
        loan_id: BytesN<32>,
        repayment_stroops: i128,
    ) -> Result<i128, Error> {
        let cfg = require_config(&e)?;
        caller.require_auth();

        let mut loan: Loan = e
            .storage()
            .persistent()
            .get(&loan_key(&e, &loan_id))
            .ok_or(Error::LoanNotFound)?;

        if !loan.active {
            return Err(Error::LoanNotActive);
        }

        let now = e.ledger().sequence();
        let elapsed = now.saturating_sub(loan.updated_at) as i128;
        loan.balance = accrue(loan.balance, cfg.interest_rate_bps, elapsed)?;
        loan.updated_at = now;

        let new_balance = loan.balance.saturating_sub(repayment_stroops);
        if new_balance == 0 || repayment_stroops >= loan.balance {
            loan.balance = 0;
            loan.active = false;
            let mut asset: Asset = e
                .storage()
                .persistent()
                .get(&asset_key(&e, &loan.asset_id))
                .ok_or(Error::AssetNotFound)?;
            asset.on_loan = false;
            e.storage()
                .persistent()
                .set(&asset_key(&e, &loan.asset_id), &asset);
        } else {
            loan.balance = new_balance;
        }

        e.storage().persistent().set(&loan_key(&e, &loan_id), &loan);
        Ok(loan.balance)
    }

    pub fn liquidate(e: Env, liquidator: Address, loan_id: BytesN<32>) -> Result<i128, Error> {
        let cfg = require_config(&e)?;
        liquidator.require_auth();

        let mut loan: Loan = e
            .storage()
            .persistent()
            .get(&loan_key(&e, &loan_id))
            .ok_or(Error::LoanNotFound)?;

        if !loan.active {
            return Err(Error::LoanNotActive);
        }

        let now = e.ledger().sequence();
        let elapsed = now.saturating_sub(loan.updated_at) as i128;
        loan.balance = accrue(loan.balance, cfg.interest_rate_bps, elapsed)?;
        loan.updated_at = now;

        let hf = Self::health_factor_inner(&e, &cfg, &loan)?;
        if hf >= 120 {
            return Err(Error::HealthFactorSafe);
        }

        let seized = loan
            .balance
            .checked_mul(
                10_000_i128
                    .checked_add(cfg.liquidation_bonus_bps)
                    .ok_or(Error::ArithmeticError)?,
            )
            .ok_or(Error::ArithmeticError)?
            .checked_div(10_000)
            .ok_or(Error::ArithmeticError)?;

        loan.active = false;
        loan.balance = 0;

        let mut asset: Asset = e
            .storage()
            .persistent()
            .get(&asset_key(&e, &loan.asset_id))
            .ok_or(Error::AssetNotFound)?;
        asset.on_loan = false;
        e.storage()
            .persistent()
            .set(&asset_key(&e, &loan.asset_id), &asset);
        e.storage().persistent().set(&loan_key(&e, &loan_id), &loan);

        Ok(seized)
    }

    pub fn health_factor(e: Env, loan_id: BytesN<32>) -> Result<i128, Error> {
        let cfg = require_config(&e)?;
        let loan: Loan = e
            .storage()
            .persistent()
            .get(&loan_key(&e, &loan_id))
            .ok_or(Error::LoanNotFound)?;
        Self::health_factor_inner(&e, &cfg, &loan)
    }

    fn health_factor_inner(e: &Env, cfg: &Config, loan: &Loan) -> Result<i128, Error> {
        if loan.balance == 0 {
            return Ok(i128::MAX);
        }
        let asset: Asset = e
            .storage()
            .persistent()
            .get(&asset_key(e, &loan.asset_id))
            .ok_or(Error::AssetNotFound)?;

        // HF = (asset_value * liquidation_threshold_bps * 100) / (balance * 10_000)
        // Scaled ×100 so HF=120 → 12000
        let numerator = asset
            .appraised_value_xlm
            .checked_mul(cfg.liquidation_threshold_bps)
            .ok_or(Error::ArithmeticError)?
            .checked_mul(100)
            .ok_or(Error::ArithmeticError)?;
        let denominator = loan
            .balance
            .checked_mul(10_000)
            .ok_or(Error::ArithmeticError)?;

        numerator
            .checked_div(denominator)
            .ok_or(Error::ArithmeticError)
    }

    pub fn submit_price(e: Env, oracle: Address, price: i128) -> Result<(), Error> {
        let cfg = require_config(&e)?;
        oracle.require_auth();

        if oracle != cfg.oracle_1 && oracle != cfg.oracle_2 && oracle != cfg.oracle_3 {
            return Err(Error::Unauthorized);
        }

        e.storage().instance().set(
            &price_key(&e, &oracle),
            &PriceEntry {
                price,
                submitted_at: e.ledger().sequence(),
            },
        );
        Ok(())
    }

    pub fn get_asset(e: Env, asset_id: BytesN<32>) -> Result<Asset, Error> {
        require_config(&e)?;
        e.storage()
            .persistent()
            .get(&asset_key(&e, &asset_id))
            .ok_or(Error::AssetNotFound)
    }

    pub fn get_loan(e: Env, loan_id: BytesN<32>) -> Result<Loan, Error> {
        require_config(&e)?;
        e.storage()
            .persistent()
            .get(&loan_key(&e, &loan_id))
            .ok_or(Error::LoanNotFound)
    }

    pub fn get_config(e: Env) -> Result<Config, Error> {
        require_config(&e)
    }

    pub fn get_price(e: Env) -> Result<i128, Error> {
        let cfg = require_config(&e)?;
        get_median_price(&e, &cfg)
    }
}
