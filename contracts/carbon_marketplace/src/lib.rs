//! # carbon_marketplace
//!
//! Orchestrator contract that manages listings and purchases of carbon credits.
//! Makes cross-contract calls to both `carbon_registry` and `carbon_credit`.
//!
//! ## ⚠️ DELIBERATE SECURITY VULNERABILITIES (for audit purposes)
//!
//! ### VULN-MP-01: TOCTOU in `create_listing()` — CWE-367 (HIGH)
//! Project status and seller balance are checked independently via two cross-contract
//! calls, then the listing is created Active. The checks are NOT atomic with the
//! listing creation: between the status check and the listing write, the project can be
//! suspended, or the seller can transfer all their credits to another address.
//!
//! ### VULN-MP-02: Check-Effects-Interactions violation in `purchase_listing()` — CWE-362 (HIGH)
//! State is updated (listing → Sold) AFTER all cross-contract interactions complete.
//! The canonical secure pattern (CEI) requires updating state BEFORE making external calls.
//! Because the listing remains Active during all cross-contract calls, a re-entrant or
//! concurrent caller can purchase the same listing multiple times.
//!
//! **Exploitation path (same-ledger double-purchase):**
//! 1. Buyer A and Buyer B both call `purchase_listing` for the same listing in the same ledger.
//! 2. Both read listing.status = Active.
//! 3. Both proceed to cross-contract calls (burn seller, mint buyer).
//! 4. Both succeed: seller's balance is burned twice, buyer receives credits twice.
//! 5. Both eventually write listing.status = Sold (last writer wins — only one write lands).
//!
//! ### VULN-MP-03: Auth-after-effect in `mint_project_credits()` — CWE-284 (MEDIUM)
//! `admin.require_auth()` is called AFTER the first cross-contract call
//! (`registry.issue_credits`). Any caller can trigger the registry call before
//! the auth check fails. The auth check itself will eventually reject unauthorized callers,
//! but the registry state may already have been mutated by the time the auth check fires
//! (depending on how the auth framework evaluates — in practice Soroban auth is
//! pre-validated, but the structural pattern is still wrong and misleading to auditors
//! and maintainers, and in non-Soroban systems this pattern is actively exploitable).

#![no_std]
#![allow(clippy::too_many_arguments)]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    IntoVal, Symbol, Val,
};

// ── Storage keys ──────────────────────────────────────────────────────────────

const CONFIG: Symbol = symbol_short!("CONFIG");
const CIRCUIT: Symbol = symbol_short!("CIRCUIT");

/// Per-listing storage key: ("LST", listing_id)
fn listing_key(e: &Env, id: &BytesN<32>) -> Val {
    (symbol_short!("LST"), id.clone()).into_val(e)
}

// ── Data types ────────────────────────────────────────────────────────────────

/// Marketplace-level configuration.
#[contracttype]
#[derive(Clone)]
pub struct MarketConfig {
    pub admin: Address,
    /// Address of the carbon_registry contract.
    pub registry: Address,
    /// Address of the carbon_credit contract.
    pub credit_contract: Address,
    /// Fee in basis points charged on each sale.
    pub fee_bps: i128,
    /// Optional oracle contract address used for staleness gating.
    pub oracle: Option<Address>,
    /// Feed checked before price-dependent marketplace operations.
    pub feed_id: Option<BytesN<32>>,
    /// Maximum acceptable price age in seconds.
    pub max_price_age_seconds: i64,
}

/// Status of a credit listing on the marketplace.
///
/// AUDIT NOTE: The Active → Sold transition in purchase_listing happens AFTER
/// all cross-contract calls, violating check-effects-interactions (CEI).
#[contracttype]
#[derive(Clone, PartialEq, Debug)]
#[repr(u32)]
pub enum CircuitBreakerState {
    Active = 0,
    Tripped = 1,
    AdminPaused = 2,
}

#[contracttype]
#[derive(Clone, PartialEq, Debug)]
#[repr(u32)]
pub enum ListingStatus {
    Active = 0,
    Sold = 1,
    Cancelled = 2,
}

/// A posted offer to sell carbon credits.
#[contracttype]
#[derive(Clone)]
pub struct Listing {
    pub seller: Address,
    pub project_id: BytesN<32>,
    pub amount: i128,
    pub price_per_credit: i128,
    pub status: ListingStatus,
}

/// Mirror of registry's CarbonProject for cross-contract decoding.
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
#[derive(Clone, PartialEq, Debug)]
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
pub enum MarketError {
    AlreadyInitialized = 1,
    NotInitialized = 2,
    Unauthorized = 3,
    ListingNotFound = 4,
    ListingNotActive = 5,
    InsufficientFunds = 6,
    InvalidAmount = 7,
    ProjectNotVerified = 8,
    RegistryError = 9,
    CreditError = 10,
    StalePriceFeed = 11,
    CircuitBreakerOpen = 12,
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct CarbonMarketplace;

#[contractimpl]
impl CarbonMarketplace {
    // ── Initialization ──────────────────────────────────────────────────────

    pub fn initialize(
        e: Env,
        admin: Address,
        registry: Address,
        credit_contract: Address,
        fee_bps: i128,
    ) -> Result<(), MarketError> {
        if e.storage().instance().has(&CONFIG) {
            return Err(MarketError::AlreadyInitialized);
        }
        let cfg = MarketConfig {
            admin,
            registry,
            credit_contract,
            fee_bps,
            oracle: None,
            feed_id: None,
            max_price_age_seconds: 0,
        };
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    /// Configure oracle freshness checks for price-dependent operations.
    pub fn configure_oracle(
        e: Env,
        admin: Address,
        oracle: Address,
        feed_id: BytesN<32>,
        max_price_age_seconds: i64,
    ) -> Result<(), MarketError> {
        let mut cfg = Self::load_config(&e)?;
        admin.require_auth();
        if admin != cfg.admin {
            return Err(MarketError::Unauthorized);
        }
        cfg.oracle = Some(oracle);
        cfg.feed_id = Some(feed_id);
        cfg.max_price_age_seconds = max_price_age_seconds;
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    pub fn set_circuit_breaker_state(
        e: Env,
        admin: Address,
        state: CircuitBreakerState,
    ) -> Result<(), MarketError> {
        let cfg = Self::load_config(&e)?;
        admin.require_auth();
        if admin != cfg.admin {
            return Err(MarketError::Unauthorized);
        }
        e.storage().instance().set(&CIRCUIT, &state);
        Ok(())
    }

    pub fn get_circuit_breaker_state(e: Env) -> Result<CircuitBreakerState, MarketError> {
        Self::load_config(&e)?;
        Ok(Self::circuit_state(&e))
    }

    // ── Listings ────────────────────────────────────────────────────────────

    /// Post a new listing to sell `amount` credits from `project_id` at `price_per_credit`.
    ///
    /// ## ⚠️ VULN-MP-01: TOCTOU — stale state at listing creation (HIGH)
    ///
    /// The function performs two cross-contract reads then writes the listing:
    ///   1. [CROSS-CONTRACT READ] registry.get_project() → check Verified
    ///   2. [CROSS-CONTRACT READ] credit.balance_of(seller) → check sufficient
    ///   3. [LOCAL WRITE] Store listing as Active
    ///
    /// Between steps 1-3 the following races are possible:
    ///   - Admin suspends the project after step 1 but before step 3 → listing created for suspended project
    ///   - Seller transfers credits after step 2 but before step 3 → listing created with inflated balance
    ///   - A concurrent purchase_listing drains seller credits between check and settlement
    ///
    /// None of these are caught at settlement time because purchase_listing does not
    /// re-verify seller balance before burning (VULN-MP-02 compounds this).
    pub fn create_listing(
        e: Env,
        seller: Address,
        project_id: BytesN<32>,
        amount: i128,
        price_per_credit: i128,
    ) -> Result<BytesN<32>, MarketError> {
        seller.require_auth();
        let cfg = Self::load_config(&e)?;
        Self::require_market_active(&e)?;
        Self::require_fresh_price(&e, &cfg)?;

        if amount <= 0 || price_per_credit <= 0 {
            return Err(MarketError::InvalidAmount);
        }

        // ── VULN-MP-01 BEGINS ──────────────────────────────────────────────
        // STEP 1: Read project status (cross-contract, no lock held).
        let project: CarbonProject = e.invoke_contract(
            &cfg.registry,
            &Symbol::new(&e, "get_project"),
            soroban_sdk::vec![&e, project_id.clone().into_val(&e)],
        );

        // Status check on stale data — project may be suspended after this point.
        if project.status != ProjectStatus::Verified {
            return Err(MarketError::ProjectNotVerified);
        }

        // STEP 2: Read seller balance (cross-contract, no lock held).
        // The balance could decrease (via transfer) before this listing is purchased.
        let seller_balance: i128 = e.invoke_contract(
            &cfg.credit_contract,
            &Symbol::new(&e, "balance_of"),
            soroban_sdk::vec![
                &e,
                seller.clone().into_val(&e),
                project_id.clone().into_val(&e)
            ],
        );

        if seller_balance < amount {
            return Err(MarketError::InsufficientFunds);
        }
        // ── RACE WINDOW: project can be suspended, seller can transfer credits ──

        // STEP 3: Create the listing as Active (stale checks are now "locked in").
        let listing_id_input: soroban_sdk::Val = (
            seller.clone(),
            project_id.clone(),
            amount,
            e.ledger().sequence(),
        )
            .into_val(&e);
        let listing_id_bytes: soroban_sdk::Bytes =
            <soroban_sdk::Bytes as soroban_sdk::TryFromVal<Env, soroban_sdk::Val>>::try_from_val(
                &e,
                &listing_id_input,
            )
            .unwrap_or_else(|_| soroban_sdk::Bytes::new(&e));
        let listing_id: BytesN<32> = e.crypto().sha256(&listing_id_bytes).into();

        let listing = Listing {
            seller,
            project_id,
            amount,
            price_per_credit,
            status: ListingStatus::Active,
        };
        e.storage()
            .persistent()
            .set(&listing_key(&e, &listing_id), &listing);
        // ── VULN-MP-01 ENDS ────────────────────────────────────────────────

        Ok(listing_id)
    }

    /// Purchase a listing: transfer credits from seller to buyer.
    ///
    /// ## ⚠️ VULN-MP-02: Check-Effects-Interactions violation (HIGH)
    ///
    /// Correct CEI pattern requires:
    ///   1. CHECK  — verify preconditions
    ///   2. EFFECT — update state (listing → Sold) ← MUST HAPPEN BEFORE INTERACTIONS
    ///   3. INTERACT — call external contracts
    ///
    /// This implementation does:
    ///   1. CHECK   — read listing, verify Active
    ///   2. INTERACT — three cross-contract calls (registry read, burn, mint)  ← WRONG ORDER
    ///   3. EFFECT  — write listing → Sold  ← TOO LATE
    ///
    /// Because the listing stays Active during all cross-contract calls, a concurrent
    /// caller (or re-entrant path via another contract) can execute purchase_listing
    /// on the same listing_id. Both callers will read status = Active, both will
    /// succeed through the cross-contract calls, and one will overwrite the other's
    /// Sold write — but both will have burned/minted credits.
    pub fn purchase_listing(
        e: Env,
        buyer: Address,
        listing_id: BytesN<32>,
        payment_amount: i128,
    ) -> Result<(), MarketError> {
        buyer.require_auth();
        let cfg = Self::load_config(&e)?;
        Self::require_market_active(&e)?;
        Self::require_fresh_price(&e, &cfg)?;

        // ── VULN-MP-02 BEGINS ──────────────────────────────────────────────
        // STEP 1 (CHECK): Read listing. Listing is still Active at this point.
        let lkey = listing_key(&e, &listing_id);
        let listing: Listing = e
            .storage()
            .persistent()
            .get(&lkey)
            .ok_or(MarketError::ListingNotFound)?;

        if listing.status != ListingStatus::Active {
            return Err(MarketError::ListingNotActive);
        }

        let total_price = listing
            .price_per_credit
            .checked_mul(listing.amount)
            .ok_or(MarketError::InvalidAmount)?;
        if payment_amount < total_price {
            return Err(MarketError::InsufficientFunds);
        }

        // ── FIX (CC-002): Check-Effects-Interactions pattern applied ──────────
        // EFFECT: Write listing → Sold BEFORE any cross-contract calls.
        // This ensures that any concurrent or re-entrant attempt to purchase the
        // same listing will read status = Sold and fail immediately, preventing
        // double-spend regardless of execution ordering within the ledger.
        let mut sold_listing = listing.clone();
        sold_listing.status = ListingStatus::Sold;
        e.storage().persistent().set(&lkey, &sold_listing);

        // ── FIX (CC-001 / TOCTOU): Re-verify project status at purchase time ──
        // The listing only stores project_id. Re-check current registry status
        // here so that a listing created while the project was Verified cannot
        // be settled after the project has been suspended/retired.
        // INTERACT-A: Read current project status (cross-contract call #1).
        let project: CarbonProject = e.invoke_contract(
            &cfg.registry,
            &Symbol::new(&e, "get_project"),
            soroban_sdk::vec![&e, listing.project_id.clone().into_val(&e)],
        );
        if project.status != ProjectStatus::Verified {
            return Err(MarketError::ProjectNotVerified);
        }

        // INTERACT-B: Burn credits from seller (cross-contract call #2).
        let _: () = e.invoke_contract(
            &cfg.credit_contract,
            &symbol_short!("burn"),
            soroban_sdk::vec![
                &e,
                listing.seller.clone().into_val(&e),
                listing.project_id.clone().into_val(&e),
                listing.amount.into_val(&e)
            ],
        );

        // INTERACT-C: Mint credits to buyer (cross-contract call #3).
        let _: () = e.invoke_contract(
            &cfg.credit_contract,
            &symbol_short!("mint"),
            soroban_sdk::vec![
                &e,
                buyer.clone().into_val(&e),
                listing.project_id.clone().into_val(&e),
                listing.amount.into_val(&e)
            ],
        );

        Ok(())
    }

    /// Cancel an Active listing. Only the original seller may cancel.
    pub fn cancel_listing(
        e: Env,
        seller: Address,
        listing_id: BytesN<32>,
    ) -> Result<(), MarketError> {
        seller.require_auth();
        let _ = Self::load_config(&e)?;
        Self::require_market_active(&e)?;

        let lkey = listing_key(&e, &listing_id);
        let mut listing: Listing = e
            .storage()
            .persistent()
            .get(&lkey)
            .ok_or(MarketError::ListingNotFound)?;

        if listing.status != ListingStatus::Active {
            return Err(MarketError::ListingNotActive);
        }

        if listing.seller != seller {
            return Err(MarketError::Unauthorized);
        }

        listing.status = ListingStatus::Cancelled;
        e.storage().persistent().set(&lkey, &listing);
        Ok(())
    }

    /// Issue new credits for a project (calls registry + credit contract).
    ///
    /// ## ⚠️ VULN-MP-03: Auth-after-effect — wrong placement of require_auth() (MEDIUM)
    ///
    /// The admin auth check (`cfg.admin.require_auth()`) is called AFTER the first
    /// cross-contract call to `registry.issue_credits()`. This means:
    ///
    ///   1. `registry.issue_credits()` is invoked (the registry state changes).
    ///   2. THEN `cfg.admin.require_auth()` is evaluated.
    ///
    /// In Soroban's auth model, `require_auth()` is pre-validated before the transaction
    /// executes, so in practice an unauthorized caller will be rejected at invocation time.
    /// However the structural anti-pattern is dangerous because:
    ///   - It misleads code reviewers into thinking auth is checked "somewhere below"
    ///   - In non-Soroban or upgraded environments it becomes directly exploitable
    ///   - It violates the principle that authorization MUST precede any state mutation
    ///
    /// The correct placement is: `cfg.admin.require_auth()` as the FIRST statement,
    /// before any cross-contract calls or state reads.
    pub fn mint_project_credits(
        e: Env,
        project_id: BytesN<32>,
        amount: i128,
    ) -> Result<(), MarketError> {
        let cfg = Self::load_config(&e)?;
        Self::require_market_active(&e)?;

        if amount <= 0 {
            return Err(MarketError::InvalidAmount);
        }

        // ── FIX (CC-003): Auth check BEFORE any cross-contract calls or state reads ──
        // Authorization must always precede all state mutations and external interactions.
        // Placing require_auth() here ensures no external call is made for unauthorized callers.
        cfg.admin.require_auth();

        // INTERACT-1: Call registry to record issuance.
        let _: () = e.invoke_contract(
            &cfg.registry,
            &Symbol::new(&e, "issue_credits"),
            soroban_sdk::vec![&e, project_id.clone().into_val(&e), amount.into_val(&e)],
        );

        // Fetch the project to get the owner for minting.
        let project: CarbonProject = e.invoke_contract(
            &cfg.registry,
            &Symbol::new(&e, "get_project"),
            soroban_sdk::vec![&e, project_id.clone().into_val(&e)],
        );

        // INTERACT-2: Mint credits to project owner.
        let _: () = e.invoke_contract(
            &cfg.credit_contract,
            &symbol_short!("mint"),
            soroban_sdk::vec![
                &e,
                project.owner.clone().into_val(&e),
                project_id.clone().into_val(&e),
                amount.into_val(&e)
            ],
        );

        Ok(())
    }

    // ── Read-only queries ───────────────────────────────────────────────────

    /// Return the full listing record.
    pub fn get_listing(e: Env, listing_id: BytesN<32>) -> Result<Listing, MarketError> {
        e.storage()
            .persistent()
            .get(&listing_key(&e, &listing_id))
            .ok_or(MarketError::ListingNotFound)
    }

    /// Return the marketplace configuration.
    pub fn get_config(e: Env) -> Result<MarketConfig, MarketError> {
        Self::load_config(&e)
    }

    // ── Internal helpers ────────────────────────────────────────────────────

    fn circuit_state(e: &Env) -> CircuitBreakerState {
        e.storage()
            .instance()
            .get(&CIRCUIT)
            .unwrap_or(CircuitBreakerState::Active)
    }

    fn require_market_active(e: &Env) -> Result<(), MarketError> {
        if Self::circuit_state(e) != CircuitBreakerState::Active {
            return Err(MarketError::CircuitBreakerOpen);
        }
        Ok(())
    }

    fn require_fresh_price(e: &Env, cfg: &MarketConfig) -> Result<(), MarketError> {
        if let (Some(oracle), Some(feed_id)) = (cfg.oracle.clone(), cfg.feed_id.clone()) {
            let oracle_state: CircuitBreakerState = e.invoke_contract(
                &oracle,
                &Symbol::new(e, "get_circuit_breaker_state"),
                soroban_sdk::vec![e],
            );
            if oracle_state != CircuitBreakerState::Active {
                return Err(MarketError::CircuitBreakerOpen);
            }

            let stale: bool = e.invoke_contract(
                &oracle,
                &Symbol::new(e, "is_price_stale"),
                soroban_sdk::vec![
                    e,
                    feed_id.into_val(e),
                    cfg.max_price_age_seconds.into_val(e)
                ],
            );
            if stale {
                return Err(MarketError::StalePriceFeed);
            }
        }
        Ok(())
    }

    fn load_config(e: &Env) -> Result<MarketConfig, MarketError> {
        e.storage()
            .instance()
            .get(&CONFIG)
            .ok_or(MarketError::NotInitialized)
    }
}

mod tests;
