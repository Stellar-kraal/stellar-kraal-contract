//! # carbon_credit
//!
//! Token-like contract that tracks carbon credit balances tied to specific projects.
//! Makes cross-contract calls to `carbon_registry` to verify project status.
//!
//! ## ⚠️ DELIBERATE SECURITY VULNERABILITIES (for audit purposes)
//!
//! ### VULN-CC-01: TOCTOU in `mint()` — CWE-367
//! `mint()` reads project status from the registry, verifies it is `Verified`,
//! and THEN updates balances. Between the `get_project` cross-contract call and
//! the storage write, another transaction can call `registry.suspend_project()`,
//! causing credits to be minted for a suspended project.
//!
//! **Exploitation path:**
//! 1. Attacker observes a `mint` call in the mempool.
//! 2. Attacker submits `suspend_project` in the same ledger (different operation ordering).
//! 3. `mint` reads status = Verified (stale), passes the check.
//! 4. Registry state changes to Suspended.
//! 5. `mint` writes the new balance — credits now exist for a suspended project.
//!
//! **Fix:** Move status check and balance update into the same atomic ledger operation,
//! or use a registry-side callback that holds the lock during minting.

#![no_std]
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    IntoVal, Symbol, Val,
};

// ── Storage keys ──────────────────────────────────────────────────────────────

const CONFIG: Symbol = symbol_short!("CONFIG");

/// Per-(owner, project) balance key in persistent storage.
/// Composite: ("BAL", owner_address, project_id)
fn balance_key(e: &Env, owner: &Address, project_id: &BytesN<32>) -> Val {
    (symbol_short!("BAL"), owner.clone(), project_id.clone()).into_val(e)
}

/// Per-project total supply key in persistent storage.
/// Composite: ("TSUP", project_id)
fn supply_key(e: &Env, project_id: &BytesN<32>) -> Val {
    (symbol_short!("TSUP"), project_id.clone()).into_val(e)
}

/// Per-project retired supply key in persistent storage.
/// Composite: ("RSUP", project_id)
fn retired_supply_key(e: &Env, project_id: &BytesN<32>) -> Val {
    (symbol_short!("RSUP"), project_id.clone()).into_val(e)
}

// ── Data types ────────────────────────────────────────────────────────────────

/// Credit contract configuration stored in instance storage.
#[contracttype]
#[derive(Clone)]
pub struct CreditConfig {
    pub admin: Address,
    /// Address of the carbon_registry contract.
    pub registry: Address,
    /// Address of the carbon_marketplace contract (sole authorized minter/burner).
    pub marketplace: Address,
}

/// Inline balance record (used internally; balance_of returns i128 directly).
#[contracttype]
#[derive(Clone)]
pub struct CreditBalance {
    pub amount: i128,
    pub project_id: BytesN<32>,
}

/// Mirror of registry's CarbonProject — needed to decode cross-contract return values.
/// Must exactly match the field order and types declared in carbon_registry.
#[contracttype]
#[derive(Clone)]
pub struct CarbonProject {
    pub owner: Address,
    pub name: Symbol,
    pub total_credits: i128,
    pub issued_credits: i128,
    pub status: ProjectStatus,
    pub vintage_year: u32,
}

/// Mirror of registry's ProjectStatus enum.
#[contracttype]
#[derive(Clone, PartialEq)]
#[repr(u32)]
pub enum ProjectStatus {
    Pending = 0,
    Verified = 1,
    Suspended = 2,
    Retired = 3,
}

// ── Error codes ───────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u32)]
pub enum CreditError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    InsufficientBalance = 4,
    ProjectNotVerified = 5,
    InvalidAmount = 6,
    RegistryError = 7,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct CarbonCredit;

#[contractimpl]
impl CarbonCredit {
    // ── Initialization ──────────────────────────────────────────────────────

    pub fn initialize(
        e: Env,
        admin: Address,
        registry: Address,
        marketplace: Address,
    ) -> Result<(), CreditError> {
        if e.storage().instance().has(&CONFIG) {
            return Err(CreditError::AlreadyInitialized);
        }
        let cfg = CreditConfig { admin, registry, marketplace };
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    // ── Token operations ────────────────────────────────────────────────────

    /// Mint `amount` credits to `to` for the given project.
    ///
    /// Only the marketplace may call this function.
    ///
    /// ## ⚠️ VULN-CC-01: TOCTOU — check-then-act across cross-contract boundary
    ///
    /// The sequence is:
    ///   1. [CROSS-CONTRACT READ] Call registry.get_project() → capture status
    ///   2. [LOCAL CHECK] Verify status == Verified
    ///   3. [LOCAL WRITE] Update balance and total supply  ← RACE WINDOW ENDS HERE
    ///
    /// Between steps 1 and 3 the registry may have changed the project status.
    /// In Soroban's single-ledger execution model this race occurs when multiple
    /// operations in the same transaction set interact, or when a second concurrent
    /// transaction in the same ledger closes interleaved operations.
    pub fn mint(
        e: Env,
        to: Address,
        project_id: BytesN<32>,
        amount: i128,
    ) -> Result<(), CreditError> {
        let cfg = Self::load_config(&e)?;
        cfg.marketplace.require_auth();

        if amount <= 0 {
            return Err(CreditError::InvalidAmount);
        }

        // ── VULN-CC-01 BEGINS ──────────────────────────────────────────────
        // STEP 1: Read project state from registry (cross-contract call).
        // This snapshot is immediately stale — no lock is held on the registry.
        let project: CarbonProject = e.invoke_contract(
            &cfg.registry,
            &Symbol::new(&e, "get_project"),
            soroban_sdk::vec![&e, project_id.clone().into_val(&e)],
        );

        // STEP 2: Act on the stale snapshot.
        // By the time we reach STEP 3, registry.status may be Suspended or Retired.
        if project.status != ProjectStatus::Verified {
            return Err(CreditError::ProjectNotVerified);
        }
        // ── RACE WINDOW: registry status can change here ───────────────────

        // STEP 3: Write new balance (operates on stale verification result).
        let bkey = balance_key(&e, &to, &project_id);
        let current: i128 = e.storage().persistent().get(&bkey).unwrap_or(0);
        let new_balance = current.checked_add(amount).ok_or(CreditError::InvalidAmount)?;
        e.storage().persistent().set(&bkey, &new_balance);

        // Update total supply
        let skey = supply_key(&e, &project_id);
        let current_supply: i128 = e.storage().persistent().get(&skey).unwrap_or(0);
        let new_supply = current_supply
            .checked_add(amount)
            .ok_or(CreditError::InvalidAmount)?;
        e.storage().persistent().set(&skey, &new_supply);
        // ── VULN-CC-01 ENDS ────────────────────────────────────────────────

        Ok(())
    }

    /// Transfer credits from `from` to `to` within the same project.
    /// The sender must authorize this call.
    pub fn transfer(
        e: Env,
        from: Address,
        to: Address,
        project_id: BytesN<32>,
        amount: i128,
    ) -> Result<(), CreditError> {
        from.require_auth();
        let _ = Self::load_config(&e)?;

        if amount <= 0 {
            return Err(CreditError::InvalidAmount);
        }

        if from == to {
            return Ok(());
        }

        let from_key = balance_key(&e, &from, &project_id);
        let from_bal: i128 = e.storage().persistent().get(&from_key).unwrap_or(0);
        if from_bal < amount {
            return Err(CreditError::InsufficientBalance);
        }

        let to_key = balance_key(&e, &to, &project_id);
        let to_bal: i128 = e.storage().persistent().get(&to_key).unwrap_or(0);

        e.storage()
            .persistent()
            .set(&from_key, &(from_bal - amount));
        e.storage()
            .persistent()
            .set(&to_key, &(to_bal.checked_add(amount).ok_or(CreditError::InvalidAmount)?));

        Ok(())
    }

    /// Burn `amount` credits from `from` for a project.
    /// Only the marketplace may call this.
    pub fn burn(
        e: Env,
        from: Address,
        project_id: BytesN<32>,
        amount: i128,
    ) -> Result<(), CreditError> {
        let cfg = Self::load_config(&e)?;
        cfg.marketplace.require_auth();

        if amount <= 0 {
            return Err(CreditError::InvalidAmount);
        }

        let bkey = balance_key(&e, &from, &project_id);
        let current: i128 = e.storage().persistent().get(&bkey).unwrap_or(0);
        if current < amount {
            return Err(CreditError::InsufficientBalance);
        }
        e.storage().persistent().set(&bkey, &(current - amount));

        // Reduce total supply
        let skey = supply_key(&e, &project_id);
        let current_supply: i128 = e.storage().persistent().get(&skey).unwrap_or(0);
        let new_supply = current_supply.saturating_sub(amount);
        e.storage().persistent().set(&skey, &new_supply);

        Ok(())
    }

    /// Retire `amount` credits from `from` for a project.
    /// The credits are moved to a retired pool.
    pub fn retire(
        e: Env,
        from: Address,
        project_id: BytesN<32>,
        amount: i128,
    ) -> Result<(), CreditError> {
        from.require_auth();
        let _ = Self::load_config(&e)?;

        if amount <= 0 {
            return Err(CreditError::InvalidAmount);
        }

        let bkey = balance_key(&e, &from, &project_id);
        let current: i128 = e.storage().persistent().get(&bkey).unwrap_or(0);
        if current < amount {
            return Err(CreditError::InsufficientBalance);
        }
        e.storage().persistent().set(&bkey, &(current - amount));

        // Update total supply and retired supply
        let skey = supply_key(&e, &project_id);
        let current_supply: i128 = e.storage().persistent().get(&skey).unwrap_or(0);
        let new_supply = current_supply.saturating_sub(amount);
        e.storage().persistent().set(&skey, &new_supply);

        let rkey = retired_supply_key(&e, &project_id);
        let current_retired: i128 = e.storage().persistent().get(&rkey).unwrap_or(0);
        let new_retired = current_retired.checked_add(amount).ok_or(CreditError::InvalidAmount)?;
        e.storage().persistent().set(&rkey, &new_retired);

        Ok(())
    }

    /// Batch transfer credits.
    pub fn batch_transfer(
        e: Env,
        from: Address,
        transfers: soroban_sdk::Vec<(Address, BytesN<32>, i128)>,
    ) -> Result<(), CreditError> {
        from.require_auth();
        for i in 0..transfers.len() {
            let transfer = transfers.get(i).unwrap();
            Self::transfer(e.clone(), from.clone(), transfer.0, transfer.1, transfer.2)?;
        }
        Ok(())
    }

    // ── Read-only queries ───────────────────────────────────────────────────

    /// Return the credit balance of `owner` for a specific project.
    pub fn balance_of(e: Env, owner: Address, project_id: BytesN<32>) -> i128 {
        e.storage()
            .persistent()
            .get(&balance_key(&e, &owner, &project_id))
            .unwrap_or(0)
    }

    /// Return the total supply of credits for a project.
    pub fn total_supply(e: Env, project_id: BytesN<32>) -> i128 {
        e.storage()
            .persistent()
            .get(&supply_key(&e, &project_id))
            .unwrap_or(0)
    }

    /// Return the retired supply of credits for a project.
    pub fn retired_supply(e: Env, project_id: BytesN<32>) -> i128 {
        e.storage()
            .persistent()
            .get(&retired_supply_key(&e, &project_id))
            .unwrap_or(0)
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    fn load_config(e: &Env) -> Result<CreditConfig, CreditError> {
        e.storage()
            .instance()
            .get(&CONFIG)
            .ok_or(CreditError::NotInitialized)
    }
}

mod tests;