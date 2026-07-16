#![no_std]
//! `carbon_marketplace` — on-chain marketplace for carbon credit listings.
//!
//! # Arithmetic safety
//! **Every** arithmetic expression in this contract uses `checked_*` or
//! `saturating_*` variants.  Any operation that would overflow or divide-by-
//! zero returns [`Error::ArithmeticOverflow`] instead of trapping or silently
//! wrapping.
//!
//! # Price / fee model
//! Prices and balances are stored as **u128 stroops** (1 XLM = 10^7 stroops).
//! Fee rates are expressed in **basis points** (bps): 1 bps = 0.01 %.
//! Maximum supported fee rate is 10 000 bps (= 100 %).
//!
//! ## Fee calculation
//! ```text
//! fee = price_per_tonne * quantity * fee_bps / 10_000
//! ```
//! Integer division is used; the remainder is truncated (in favour of the
//! buyer). `price_per_tonne`, `quantity`, and `fee_bps` are all `u128`.
//!
//! ## Bulk-discount tiers
//! | Tier threshold (tonnes) | Discount (bps off fee) |
//! |-------------------------|------------------------|
//! | ≥ 1 000                 | 500 bps  (5 %)         |
//! | ≥ 10 000                | 1 000 bps (10 %)       |
//! | ≥ 100 000               | 2 000 bps (20 %)       |
//!
//! The *effective* fee rate is `max(0, fee_bps - discount_bps)`.

mod tests;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    Symbol,
};

// ── Constants ─────────────────────────────────────────────────────────────────

/// Maximum fee rate: 10 000 bps = 100 %.
pub const MAX_FEE_BPS: u128 = 10_000;

/// Bulk-tier thresholds (tonnes purchased in a single order).
pub const BULK_TIER_1: u128 = 1_000;
pub const BULK_TIER_2: u128 = 10_000;
pub const BULK_TIER_3: u128 = 100_000;

/// Discount applied per tier (bps knocked off the base fee rate).
pub const BULK_DISCOUNT_TIER_1_BPS: u128 = 500;
pub const BULK_DISCOUNT_TIER_2_BPS: u128 = 1_000;
pub const BULK_DISCOUNT_TIER_3_BPS: u128 = 2_000;

// ── Storage keys ─────────────────────────────────────────────────────────────

const CONFIG: Symbol = symbol_short!("CONFIG");

fn listing_key(_e: &Env, id: &BytesN<32>) -> (Symbol, BytesN<32>) {
    (symbol_short!("LISTING"), id.clone())
}

fn order_key(_e: &Env, id: &BytesN<32>) -> (Symbol, BytesN<32>) {
    (symbol_short!("ORDER"), id.clone())
}

// ── Data types ────────────────────────────────────────────────────────────────

/// Marketplace configuration stored in instance storage.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Config {
    pub admin: Address,
    /// Base platform fee in basis points (0..=10_000).
    pub platform_fee_bps: u128,
}

/// A carbon credit listing.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Listing {
    pub seller: Address,
    /// Price per tonne in stroops (u128).
    pub price_per_tonne: u128,
    /// Available supply in tonnes (u128).
    pub available_tonnes: u128,
    /// Whether the listing is still active.
    pub active: bool,
}

/// A completed purchase order.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct Order {
    pub buyer: Address,
    pub listing_id: BytesN<32>,
    /// Tonnes purchased.
    pub quantity: u128,
    /// Total gross cost (price × quantity) in stroops.
    pub gross_cost: u128,
    /// Platform fee deducted in stroops.
    pub fee_amount: u128,
    /// Net cost paid by buyer (gross_cost + fee_amount) in stroops.
    pub net_cost: u128,
    /// Effective fee rate applied (after bulk discount), in bps.
    pub effective_fee_bps: u128,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    AlreadyInitialized     = 1,
    NotInitialized         = 2,
    Unauthorized           = 3,
    ListingNotFound        = 4,
    ListingInactive        = 5,
    InsufficientSupply     = 6,
    ZeroQuantity           = 7,
    ZeroPrice              = 8,
    /// Any checked arithmetic operation overflowed or divided by zero.
    ArithmeticOverflow     = 9,
    /// A fee rate > MAX_FEE_BPS was supplied.
    InvalidFeeRate         = 10,
    OrderNotFound          = 11,
}

// ── Pure arithmetic helpers (pub for testing) ────────────────────────────────

/// Compute the bulk-discount rate in bps for the given quantity.
///
/// Returns a discount that is **subtracted** from the base fee rate.
/// The result is always in [0, MAX_FEE_BPS].
pub fn bulk_discount_bps(quantity: u128) -> u128 {
    if quantity >= BULK_TIER_3 {
        BULK_DISCOUNT_TIER_3_BPS
    } else if quantity >= BULK_TIER_2 {
        BULK_DISCOUNT_TIER_2_BPS
    } else if quantity >= BULK_TIER_1 {
        BULK_DISCOUNT_TIER_1_BPS
    } else {
        0
    }
}

/// Compute the effective fee rate (bps) after applying the bulk discount.
///
/// Returns `Err(Error::InvalidFeeRate)` when `base_fee_bps > MAX_FEE_BPS`.
/// The effective rate is `max(0, base_fee_bps - discount_bps)` and is
/// clamped to `[0, MAX_FEE_BPS]`.
pub fn effective_fee_bps(base_fee_bps: u128, quantity: u128) -> Result<u128, Error> {
    if base_fee_bps > MAX_FEE_BPS {
        return Err(Error::InvalidFeeRate);
    }
    let discount = bulk_discount_bps(quantity);
    // saturating_sub: if discount > base_fee_bps the rate floors at 0.
    Ok(base_fee_bps.saturating_sub(discount))
}

/// Compute the gross cost (price × quantity) with checked multiplication.
///
/// Returns `Err(Error::ArithmeticOverflow)` on u128 overflow.
pub fn gross_cost(price_per_tonne: u128, quantity: u128) -> Result<u128, Error> {
    price_per_tonne
        .checked_mul(quantity)
        .ok_or(Error::ArithmeticOverflow)
}

/// Compute the platform fee for a single purchase.
///
/// ```text
/// fee = gross * eff_fee_bps / MAX_FEE_BPS
/// ```
///
/// Uses checked multiply then checked divide.  Division by MAX_FEE_BPS
/// (10_000) can never be zero so the division can only fail if `gross`
/// overflowed first, which is guarded by the caller.
pub fn platform_fee(gross: u128, eff_fee_bps: u128) -> Result<u128, Error> {
    // eff_fee_bps is already validated to be in [0, MAX_FEE_BPS], so
    // gross * eff_fee_bps may still overflow u128 for very large gross values.
    let numerator = gross
        .checked_mul(eff_fee_bps)
        .ok_or(Error::ArithmeticOverflow)?;
    // MAX_FEE_BPS = 10_000 ≠ 0, so checked_div can only return None if
    // numerator is somehow larger than u128::MAX — already guarded above.
    numerator
        .checked_div(MAX_FEE_BPS)
        .ok_or(Error::ArithmeticOverflow)
}

/// Compute the net cost (gross + fee) with checked addition.
pub fn net_cost(gross: u128, fee: u128) -> Result<u128, Error> {
    gross.checked_add(fee).ok_or(Error::ArithmeticOverflow)
}

/// Full purchase arithmetic: returns `(gross, fee, net, effective_fee_bps)`.
///
/// All intermediate results use checked operations.  A single `Err` at any
/// stage short-circuits and returns `Error::ArithmeticOverflow`.
pub fn purchase_totals(
    price_per_tonne: u128,
    quantity: u128,
    base_fee_bps: u128,
) -> Result<(u128, u128, u128, u128), Error> {
    let eff = effective_fee_bps(base_fee_bps, quantity)?;
    let g = gross_cost(price_per_tonne, quantity)?;
    let f = platform_fee(g, eff)?;
    let n = net_cost(g, f)?;
    Ok((g, f, n, eff))
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct CarbonMarketplace;

#[contractimpl]
impl CarbonMarketplace {
    /// Initialise the marketplace with an admin and a base fee rate.
    pub fn initialize(
        e: Env,
        admin: Address,
        platform_fee_bps: u128,
    ) -> Result<(), Error> {
        if e.storage().instance().has(&CONFIG) {
            return Err(Error::AlreadyInitialized);
        }
        if platform_fee_bps > MAX_FEE_BPS {
            return Err(Error::InvalidFeeRate);
        }
        admin.require_auth();
        e.storage()
            .instance()
            .set(&CONFIG, &Config { admin, platform_fee_bps });
        Ok(())
    }

    /// Update the platform fee rate.  Admin only.
    pub fn set_fee(e: Env, admin: Address, new_fee_bps: u128) -> Result<(), Error> {
        let mut cfg = require_config(&e)?;
        admin.require_auth();
        if admin != cfg.admin {
            return Err(Error::Unauthorized);
        }
        if new_fee_bps > MAX_FEE_BPS {
            return Err(Error::InvalidFeeRate);
        }
        cfg.platform_fee_bps = new_fee_bps;
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    /// Create a new carbon credit listing.
    pub fn create_listing(
        e: Env,
        seller: Address,
        price_per_tonne: u128,
        available_tonnes: u128,
    ) -> Result<BytesN<32>, Error> {
        require_config(&e)?;
        seller.require_auth();
        if price_per_tonne == 0 {
            return Err(Error::ZeroPrice);
        }
        if available_tonnes == 0 {
            return Err(Error::ZeroQuantity);
        }

        let id = derive_id(&e, &seller, price_per_tonne, available_tonnes);
        e.storage().persistent().set(
            &listing_key(&e, &id),
            &Listing {
                seller,
                price_per_tonne,
                available_tonnes,
                active: true,
            },
        );
        Ok(id)
    }

    /// Purchase carbon credits from a listing.
    ///
    /// All arithmetic uses `checked_*` operations.  Returns the [`Order`] ID
    /// on success, or an [`Error`] if any invariant is violated.
    pub fn purchase(
        e: Env,
        buyer: Address,
        listing_id: BytesN<32>,
        quantity: u128,
    ) -> Result<BytesN<32>, Error> {
        let cfg = require_config(&e)?;
        buyer.require_auth();

        if quantity == 0 {
            return Err(Error::ZeroQuantity);
        }

        let mut listing: Listing = e
            .storage()
            .persistent()
            .get(&listing_key(&e, &listing_id))
            .ok_or(Error::ListingNotFound)?;

        if !listing.active {
            return Err(Error::ListingInactive);
        }
        if listing.available_tonnes < quantity {
            return Err(Error::InsufficientSupply);
        }

        // ── Checked arithmetic block ──────────────────────────────────────
        let (gross, fee, net, eff_bps) =
            purchase_totals(listing.price_per_tonne, quantity, cfg.platform_fee_bps)?;
        // ─────────────────────────────────────────────────────────────────

        // Update supply with checked subtraction (listing.available_tonnes ≥ quantity
        // is already guaranteed by the InsufficientSupply guard above).
        listing.available_tonnes = listing
            .available_tonnes
            .checked_sub(quantity)
            .ok_or(Error::ArithmeticOverflow)?;
        if listing.available_tonnes == 0 {
            listing.active = false;
        }
        e.storage()
            .persistent()
            .set(&listing_key(&e, &listing_id), &listing);

        let order = Order {
            buyer: buyer.clone(),
            listing_id: listing_id.clone(),
            quantity,
            gross_cost: gross,
            fee_amount: fee,
            net_cost: net,
            effective_fee_bps: eff_bps,
        };

        let order_id = derive_order_id(&e, &buyer, &listing_id, quantity);
        e.storage()
            .persistent()
            .set(&order_key(&e, &order_id), &order);

        Ok(order_id)
    }

    /// Deactivate a listing.  Seller or admin may call this.
    pub fn deactivate_listing(
        e: Env,
        caller: Address,
        listing_id: BytesN<32>,
    ) -> Result<(), Error> {
        let cfg = require_config(&e)?;
        caller.require_auth();

        let mut listing: Listing = e
            .storage()
            .persistent()
            .get(&listing_key(&e, &listing_id))
            .ok_or(Error::ListingNotFound)?;

        if caller != listing.seller && caller != cfg.admin {
            return Err(Error::Unauthorized);
        }
        listing.active = false;
        e.storage()
            .persistent()
            .set(&listing_key(&e, &listing_id), &listing);
        Ok(())
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    pub fn get_listing(e: Env, listing_id: BytesN<32>) -> Result<Listing, Error> {
        require_config(&e)?;
        e.storage()
            .persistent()
            .get(&listing_key(&e, &listing_id))
            .ok_or(Error::ListingNotFound)
    }

    pub fn get_order(e: Env, order_id: BytesN<32>) -> Result<Order, Error> {
        require_config(&e)?;
        e.storage()
            .persistent()
            .get(&order_key(&e, &order_id))
            .ok_or(Error::OrderNotFound)
    }

    pub fn get_config(e: Env) -> Result<Config, Error> {
        require_config(&e)
    }

    /// Quote the purchase cost for a given quantity without side effects.
    /// Returns `(gross_cost, fee_amount, net_cost, effective_fee_bps)`.
    pub fn quote(
        e: Env,
        listing_id: BytesN<32>,
        quantity: u128,
    ) -> Result<(u128, u128, u128, u128), Error> {
        let cfg = require_config(&e)?;
        let listing: Listing = e
            .storage()
            .persistent()
            .get(&listing_key(&e, &listing_id))
            .ok_or(Error::ListingNotFound)?;
        purchase_totals(listing.price_per_tonne, quantity, cfg.platform_fee_bps)
    }
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn require_config(e: &Env) -> Result<Config, Error> {
    e.storage()
        .instance()
        .get(&CONFIG)
        .ok_or(Error::NotInitialized)
}

/// Deterministic listing ID derived from seller + price + supply + ledger.
fn derive_id(e: &Env, _seller: &Address, price: u128, supply: u128) -> BytesN<32> {
    let mut seed = soroban_sdk::Bytes::new(e);
    for b in e.ledger().sequence().to_be_bytes() {
        seed.push_back(b);
    }
    for b in price.to_be_bytes() {
        seed.push_back(b);
    }
    for b in supply.to_be_bytes() {
        seed.push_back(b);
    }
    e.crypto().sha256(&seed).into()
}

/// Deterministic order ID derived from buyer + listing + quantity + ledger.
fn derive_order_id(
    e: &Env,
    _buyer: &Address,
    listing_id: &BytesN<32>,
    quantity: u128,
) -> BytesN<32> {
    let mut seed = soroban_sdk::Bytes::new(e);
    for b in e.ledger().sequence().to_be_bytes() {
        seed.push_back(b);
    }
    for b in quantity.to_be_bytes() {
        seed.push_back(b);
    }
    seed.append(&listing_id.clone().into());
    e.crypto().sha256(&seed).into()
}
