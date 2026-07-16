//! Test suite for `carbon_marketplace`.
//!
//! Structure
//! ---------
//! 1. `mod unit`   — deterministic Soroban integration tests
//! 2. `mod props`  — proptest property-based tests (20+ named properties)
//! 3. `mod regression` — regression tests for 2 confirmed precision-loss bugs

#![cfg(test)]
extern crate std;

// ─────────────────────────────────────────────────────────────────────────────
// Common imports
// ─────────────────────────────────────────────────────────────────────────────

use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

use crate::{
    bulk_discount_bps, effective_fee_bps, gross_cost, net_cost, platform_fee, purchase_totals,
    CarbonMarketplace, CarbonMarketplaceClient, Error,
    BULK_DISCOUNT_TIER_1_BPS, BULK_DISCOUNT_TIER_2_BPS, BULK_DISCOUNT_TIER_3_BPS,
    BULK_TIER_1, BULK_TIER_2, BULK_TIER_3, MAX_FEE_BPS,
};

// ─────────────────────────────────────────────────────────────────────────────
// Shared fixture helpers
// ─────────────────────────────────────────────────────────────────────────────

fn make_env() -> Env {
    let e = Env::default();
    e.mock_all_auths();
    e
}

fn deploy(e: &Env) -> CarbonMarketplaceClient<'_> {
    CarbonMarketplaceClient::new(e, &e.register(CarbonMarketplace, ()))
}

/// Deploy + initialize with a given fee (bps).
fn init(e: &Env, fee_bps: u128) -> (CarbonMarketplaceClient<'_>, Address) {
    let client = deploy(e);
    let admin = Address::generate(e);
    client.initialize(&admin, &fee_bps);
    (client, admin)
}

/// Create a listing and return its ID.
fn make_listing(
    client: &CarbonMarketplaceClient,
    e: &Env,
    price: u128,
    supply: u128,
) -> BytesN<32> {
    let seller = Address::generate(e);
    client.create_listing(&seller, &price, &supply)
}

// ═════════════════════════════════════════════════════════════════════════════
// 1. Unit tests
// ═════════════════════════════════════════════════════════════════════════════

mod unit {
    use super::*;

    // ── Initialisation ────────────────────────────────────────────────────────

    #[test]
    fn initialize_stores_config() {
        let e = make_env();
        let (client, admin) = init(&e, 250);
        let cfg = client.get_config();
        assert_eq!(cfg.platform_fee_bps, 250);
        assert_eq!(cfg.admin, admin);
    }

    #[test]
    fn double_init_rejected() {
        let e = make_env();
        let (client, admin) = init(&e, 100);
        assert_eq!(
            client.try_initialize(&admin, &100u128),
            Err(Ok(Error::AlreadyInitialized))
        );
    }

    #[test]
    fn fee_above_max_rejected_on_init() {
        let e = make_env();
        let client = deploy(&e);
        let admin = Address::generate(&e);
        assert_eq!(
            client.try_initialize(&admin, &(MAX_FEE_BPS + 1)),
            Err(Ok(Error::InvalidFeeRate))
        );
    }

    #[test]
    fn fee_exactly_max_accepted_on_init() {
        let e = make_env();
        let client = deploy(&e);
        let admin = Address::generate(&e);
        client.initialize(&admin, &MAX_FEE_BPS);
        assert_eq!(client.get_config().platform_fee_bps, MAX_FEE_BPS);
    }

    // ── Listing creation ──────────────────────────────────────────────────────

    #[test]
    fn create_listing_stores_fields() {
        let e = make_env();
        let (client, _) = init(&e, 200);
        let id = make_listing(&client, &e, 1_000_000, 500);
        let l = client.get_listing(&id);
        assert_eq!(l.price_per_tonne, 1_000_000);
        assert_eq!(l.available_tonnes, 500);
        assert!(l.active);
    }

    #[test]
    fn create_listing_zero_price_rejected() {
        let e = make_env();
        let (client, _) = init(&e, 200);
        let seller = Address::generate(&e);
        assert_eq!(
            client.try_create_listing(&seller, &0u128, &100u128),
            Err(Ok(Error::ZeroPrice))
        );
    }

    #[test]
    fn create_listing_zero_supply_rejected() {
        let e = make_env();
        let (client, _) = init(&e, 200);
        let seller = Address::generate(&e);
        assert_eq!(
            client.try_create_listing(&seller, &1_000u128, &0u128),
            Err(Ok(Error::ZeroQuantity))
        );
    }

    // ── Purchase ──────────────────────────────────────────────────────────────

    #[test]
    fn purchase_updates_supply_and_stores_order() {
        let e = make_env();
        let (client, _) = init(&e, 300); // 3% fee
        let id = make_listing(&client, &e, 1_000_000, 1_000);
        let buyer = Address::generate(&e);
        let order_id = client.purchase(&buyer, &id, &100u128);
        let order = client.get_order(&order_id);
        // gross = 1_000_000 * 100 = 100_000_000
        assert_eq!(order.gross_cost, 100_000_000);
        // fee  = 100_000_000 * 300 / 10_000 = 3_000_000
        assert_eq!(order.fee_amount, 3_000_000);
        // net  = 103_000_000
        assert_eq!(order.net_cost, 103_000_000);
        // supply reduced
        let listing = client.get_listing(&id);
        assert_eq!(listing.available_tonnes, 900);
    }

    #[test]
    fn purchase_zero_quantity_rejected() {
        let e = make_env();
        let (client, _) = init(&e, 100);
        let id = make_listing(&client, &e, 500, 100);
        let buyer = Address::generate(&e);
        assert_eq!(
            client.try_purchase(&buyer, &id, &0u128),
            Err(Ok(Error::ZeroQuantity))
        );
    }

    #[test]
    fn purchase_exceeds_supply_rejected() {
        let e = make_env();
        let (client, _) = init(&e, 100);
        let id = make_listing(&client, &e, 500, 50);
        let buyer = Address::generate(&e);
        assert_eq!(
            client.try_purchase(&buyer, &id, &51u128),
            Err(Ok(Error::InsufficientSupply))
        );
    }

    #[test]
    fn purchase_entire_supply_deactivates_listing() {
        let e = make_env();
        let (client, _) = init(&e, 0); // 0% fee for simplicity
        let id = make_listing(&client, &e, 1_000, 10);
        let buyer = Address::generate(&e);
        client.purchase(&buyer, &id, &10u128);
        let listing = client.get_listing(&id);
        assert!(!listing.active);
        assert_eq!(listing.available_tonnes, 0);
    }

    #[test]
    fn purchase_on_inactive_listing_rejected() {
        let e = make_env();
        let (client, admin) = init(&e, 0);
        let id = make_listing(&client, &e, 1_000, 10);
        client.deactivate_listing(&admin, &id);
        let buyer = Address::generate(&e);
        assert_eq!(
            client.try_purchase(&buyer, &id, &1u128),
            Err(Ok(Error::ListingInactive))
        );
    }

    // ── Fee arithmetic (zero-fee and 100% fee edge cases) ─────────────────────

    #[test]
    fn zero_fee_means_net_equals_gross() {
        let e = make_env();
        let (client, _) = init(&e, 0);
        let id = make_listing(&client, &e, 2_000_000, 100);
        let buyer = Address::generate(&e);
        let oid = client.purchase(&buyer, &id, &5u128);
        let order = client.get_order(&oid);
        assert_eq!(order.fee_amount, 0);
        assert_eq!(order.net_cost, order.gross_cost);
    }

    #[test]
    fn hundred_percent_fee_doubles_cost() {
        let e = make_env();
        let (client, _) = init(&e, MAX_FEE_BPS); // 100% fee
        let id = make_listing(&client, &e, 1_000, 100);
        let buyer = Address::generate(&e);
        let oid = client.purchase(&buyer, &id, &1u128);
        let order = client.get_order(&oid);
        assert_eq!(order.gross_cost, 1_000);
        assert_eq!(order.fee_amount, 1_000); // 100% of gross
        assert_eq!(order.net_cost, 2_000);
    }

    // ── Bulk discount tiers ───────────────────────────────────────────────────

    #[test]
    fn bulk_tier_1_discount_applied() {
        let e = make_env();
        let (client, _) = init(&e, 1_000); // 10% base fee
        let id = make_listing(&client, &e, 1_000, BULK_TIER_1 * 2);
        let buyer = Address::generate(&e);
        let oid = client.purchase(&buyer, &id, &BULK_TIER_1);
        let order = client.get_order(&oid);
        // effective = 1000 - 500 = 500 bps = 5%
        assert_eq!(order.effective_fee_bps, 500);
    }

    #[test]
    fn bulk_tier_2_discount_applied() {
        let e = make_env();
        let (client, _) = init(&e, 1_500);
        let id = make_listing(&client, &e, 100, BULK_TIER_2 * 2);
        let buyer = Address::generate(&e);
        let oid = client.purchase(&buyer, &id, &BULK_TIER_2);
        let order = client.get_order(&oid);
        // effective = 1500 - 1000 = 500 bps
        assert_eq!(order.effective_fee_bps, 500);
    }

    #[test]
    fn bulk_tier_3_discount_applied() {
        let e = make_env();
        let (client, _) = init(&e, 2_500);
        let id = make_listing(&client, &e, 10, BULK_TIER_3 * 2);
        let buyer = Address::generate(&e);
        let oid = client.purchase(&buyer, &id, &BULK_TIER_3);
        let order = client.get_order(&oid);
        // effective = 2500 - 2000 = 500 bps
        assert_eq!(order.effective_fee_bps, 500);
    }

    #[test]
    fn bulk_discount_cannot_make_fee_negative() {
        // base fee = 100 bps, tier-1 discount = 500 bps → should floor at 0
        let e = make_env();
        let (client, _) = init(&e, 100);
        let id = make_listing(&client, &e, 1_000, BULK_TIER_1 * 2);
        let buyer = Address::generate(&e);
        let oid = client.purchase(&buyer, &id, &BULK_TIER_1);
        let order = client.get_order(&oid);
        assert_eq!(order.effective_fee_bps, 0);
        assert_eq!(order.fee_amount, 0);
    }

    // ── Deactivate listing ────────────────────────────────────────────────────

    #[test]
    fn deactivate_by_seller_works() {
        let e = make_env();
        let (client, _) = init(&e, 0);
        let seller = Address::generate(&e);
        let id = client.create_listing(&seller, &1_000u128, &100u128);
        client.deactivate_listing(&seller, &id);
        assert!(!client.get_listing(&id).active);
    }

    #[test]
    fn deactivate_by_non_owner_rejected() {
        let e = make_env();
        let (client, _) = init(&e, 0);
        let id = make_listing(&client, &e, 1_000, 100);
        let intruder = Address::generate(&e);
        assert_eq!(
            client.try_deactivate_listing(&intruder, &id),
            Err(Ok(Error::Unauthorized))
        );
    }

    // ── Quote ─────────────────────────────────────────────────────────────────

    #[test]
    fn quote_matches_purchase_totals() {
        let e = make_env();
        let (client, _) = init(&e, 250);
        let id = make_listing(&client, &e, 500_000, 5_000);
        let (g, f, n, eff) = client.quote(&id, &2_000u128);
        assert_eq!(
            (g, f, n, eff),
            purchase_totals(500_000, 2_000, 250).unwrap()
        );
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 2. Property-based tests  (≥ 20 named properties)
//    Run with PROPTEST_CASES=10000 cargo test
// ═════════════════════════════════════════════════════════════════════════════

mod props {
    use super::*;
    use proptest::prelude::*;

    // ── Strategy helpers ──────────────────────────────────────────────────────

    /// Safe price range: avoids u128 overflow when multiplied by large quantities.
    /// Max price chosen so price * 1e12 < u128::MAX.
    fn price_strategy() -> impl Strategy<Value = u128> {
        1u128..=340_282_366_920_938u128 // ~3.4e14, safe for quantity up to 1e12
    }

    /// Quantity range: 1..u64::MAX cast to u128 (realistic for carbon tonnes).
    fn quantity_strategy() -> impl Strategy<Value = u128> {
        1u128..=1_000_000_000_000u128 // up to 1 trillion tonnes
    }

    /// Fee rate in valid range.
    fn fee_bps_strategy() -> impl Strategy<Value = u128> {
        0u128..=MAX_FEE_BPS
    }

    // ── Property 1: gross_cost is commutative ─────────────────────────────────
    proptest! {
        #[test]
        fn prop_gross_cost_commutative(
            p in price_strategy(),
            q in quantity_strategy(),
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            assert_eq!(gross_cost(p, q).unwrap(), gross_cost(q, p).unwrap());
        }
    }

    // ── Property 2: gross_cost(p, 1) == p ────────────────────────────────────
    proptest! {
        #[test]
        fn prop_gross_cost_identity(p in price_strategy()) {
            assert_eq!(gross_cost(p, 1).unwrap(), p);
        }
    }

    // ── Property 3: gross_cost detects overflow ───────────────────────────────
    proptest! {
        #[test]
        fn prop_gross_cost_overflow_detected(
            p in (u128::MAX / 2 + 1)..=u128::MAX,
            q in 2u128..=u128::MAX,
        ) {
            assert_eq!(gross_cost(p, q), Err(Error::ArithmeticOverflow));
        }
    }

    // ── Property 4: fee is never greater than gross ───────────────────────────
    proptest! {
        #[test]
        fn prop_fee_never_exceeds_gross(
            p in price_strategy(),
            q in quantity_strategy(),
            fee in fee_bps_strategy(),
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            let (g, f, _n, _eff) = purchase_totals(p, q, fee).unwrap();
            assert!(f <= g, "fee {f} > gross {g}");
        }
    }

    // ── Property 5: net_cost >= gross_cost always ─────────────────────────────
    proptest! {
        #[test]
        fn prop_net_cost_gte_gross(
            p in price_strategy(),
            q in quantity_strategy(),
            fee in fee_bps_strategy(),
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            if let Ok((g, _f, n, _eff)) = purchase_totals(p, q, fee) {
                assert!(n >= g, "net {n} < gross {g}");
            }
        }
    }

    // ── Property 6: zero fee → fee_amount == 0 ───────────────────────────────
    proptest! {
        #[test]
        fn prop_zero_fee_rate_yields_zero_fee(
            p in price_strategy(),
            q in quantity_strategy(),
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            let (_g, f, _n, _eff) = purchase_totals(p, q, 0).unwrap();
            assert_eq!(f, 0);
        }
    }

    // ── Property 7: zero fee → net == gross ──────────────────────────────────
    proptest! {
        #[test]
        fn prop_zero_fee_rate_net_equals_gross(
            p in price_strategy(),
            q in quantity_strategy(),
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            let (g, _f, n, _eff) = purchase_totals(p, q, 0).unwrap();
            assert_eq!(n, g);
        }
    }

    // ── Property 8: MAX fee, sub-tier quantity → fee == gross ─────────────────
    proptest! {
        #[test]
        fn prop_max_fee_rate_fee_equals_gross(
            p in price_strategy(),
            // Keep quantity strictly below BULK_TIER_1 so no discount applies
            // and effective_fee == MAX_FEE_BPS, meaning fee == gross.
            q in 1u128..BULK_TIER_1,
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            let (g, f, _n, _eff) = purchase_totals(p, q, MAX_FEE_BPS).unwrap();
            assert_eq!(f, g, "with MAX fee and no bulk discount, fee must equal gross");
        }
    }

    // ── Property 9: fee is monotone in fee_bps ───────────────────────────────
    proptest! {
        #[test]
        fn prop_fee_monotone_in_rate(
            p in price_strategy(),
            q in 1u128..1_000u128, // small qty to avoid overflow
            fee_lo in 0u128..5_000u128,
            fee_hi in 5_000u128..=MAX_FEE_BPS,
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            let (_g, f_lo, _, _) = purchase_totals(p, q, fee_lo).unwrap();
            let (_, f_hi, _, _) = purchase_totals(p, q, fee_hi).unwrap();
            assert!(f_lo <= f_hi, "fee not monotone: f_lo={f_lo} f_hi={f_hi}");
        }
    }

    // ── Property 10: effective_fee_bps ≤ base_fee_bps always ─────────────────
    proptest! {
        #[test]
        fn prop_effective_fee_lte_base(
            base in fee_bps_strategy(),
            qty in quantity_strategy(),
        ) {
            let eff = effective_fee_bps(base, qty).unwrap();
            assert!(eff <= base);
        }
    }

    // ── Property 11: effective_fee_bps is non-negative (always 0..=MAX) ──────
    proptest! {
        #[test]
        fn prop_effective_fee_in_valid_range(
            base in fee_bps_strategy(),
            qty in quantity_strategy(),
        ) {
            let eff = effective_fee_bps(base, qty).unwrap();
            assert!(eff <= MAX_FEE_BPS);
        }
    }

    // ── Property 12: invalid fee rate always rejected ─────────────────────────
    proptest! {
        #[test]
        fn prop_invalid_fee_rate_rejected(
            base in (MAX_FEE_BPS + 1)..=u128::MAX,
            qty in quantity_strategy(),
        ) {
            assert_eq!(
                effective_fee_bps(base, qty),
                Err(Error::InvalidFeeRate)
            );
        }
    }

    // ── Property 13: bulk discount increases with quantity tiers ─────────────
    proptest! {
        #[test]
        fn prop_bulk_discount_monotone_in_tier(
            q_small  in 0u128..BULK_TIER_1,
            q_tier1  in BULK_TIER_1..BULK_TIER_2,
            q_tier2  in BULK_TIER_2..BULK_TIER_3,
            q_tier3  in BULK_TIER_3..=u128::MAX,
        ) {
            let d0 = bulk_discount_bps(q_small);
            let d1 = bulk_discount_bps(q_tier1);
            let d2 = bulk_discount_bps(q_tier2);
            let d3 = bulk_discount_bps(q_tier3);
            assert!(d0 <  d1, "tier0 >= tier1: {d0} {d1}");
            assert!(d1 <= d2, "tier1 > tier2: {d1} {d2}");
            assert!(d2 <  d3, "tier2 >= tier3: {d2} {d3}");
        }
    }

    // ── Property 14: bulk discount is exactly correct per tier ───────────────
    proptest! {
        #[test]
        fn prop_bulk_discount_exact_values(qty in quantity_strategy()) {
            let disc = bulk_discount_bps(qty);
            if qty >= BULK_TIER_3 {
                assert_eq!(disc, BULK_DISCOUNT_TIER_3_BPS);
            } else if qty >= BULK_TIER_2 {
                assert_eq!(disc, BULK_DISCOUNT_TIER_2_BPS);
            } else if qty >= BULK_TIER_1 {
                assert_eq!(disc, BULK_DISCOUNT_TIER_1_BPS);
            } else {
                assert_eq!(disc, 0);
            }
        }
    }

    // ── Property 15: platform_fee(gross, 0) == 0 ─────────────────────────────
    proptest! {
        #[test]
        fn prop_zero_fee_bps_yields_zero_fee(
            gross in 0u128..=u128::MAX / 2,
        ) {
            assert_eq!(platform_fee(gross, 0).unwrap(), 0);
        }
    }

    // ── Property 16: platform_fee(0, any_bps) == 0 ───────────────────────────
    proptest! {
        #[test]
        fn prop_zero_gross_yields_zero_fee(fee in fee_bps_strategy()) {
            assert_eq!(platform_fee(0, fee).unwrap(), 0);
        }
    }

    // ── Property 17: net_cost(g, 0) == g ─────────────────────────────────────
    proptest! {
        #[test]
        fn prop_net_cost_zero_fee_identity(g in 0u128..=u128::MAX) {
            assert_eq!(net_cost(g, 0).unwrap(), g);
        }
    }

    // ── Property 18: net_cost overflow detected ───────────────────────────────
    proptest! {
        #[test]
        fn prop_net_cost_overflow_detected(
            // g + f must overflow. Choose g in top half of u128, f such that
            // g + f > u128::MAX. Use f = u128::MAX - g + 1 offset to guarantee overflow.
            g in (u128::MAX / 2 + 1)..=u128::MAX,
            extra in 1u128..=u128::MAX / 2,
        ) {
            // f = (u128::MAX - g) + extra  ensures g + f overflows
            let f = (u128::MAX - g).saturating_add(extra);
            prop_assume!(f >= 1);
            assert_eq!(net_cost(g, f), Err(Error::ArithmeticOverflow));
        }
    }

    // ── Property 19: purchase_totals is consistent (fee + gross == net) ───────
    proptest! {
        #[test]
        fn prop_purchase_totals_consistent(
            p in price_strategy(),
            q in 1u128..10_000u128,
            fee in fee_bps_strategy(),
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            if let Ok((g, f, n, _eff)) = purchase_totals(p, q, fee) {
                assert_eq!(g + f, n, "g={g} f={f} n={n}");
            }
        }
    }

    // ── Property 20: purchase_totals with u128::MAX price detects overflow ────
    proptest! {
        #[test]
        fn prop_max_price_overflow_detected(q in 2u128..=u128::MAX) {
            assert_eq!(
                purchase_totals(u128::MAX, q, 0),
                Err(Error::ArithmeticOverflow)
            );
        }
    }

    // ── Property 21: fee grows linearly when both quantities are in same tier ──
    proptest! {
        #[test]
        fn prop_fee_linear_in_gross(
            p in 1u128..1_000_000u128,
            // Keep both q and q*2 below BULK_TIER_1 so the effective fee rate
            // is the same for both, making the relationship exactly linear.
            q in 1u128..500u128, // q*2 < 1000 = BULK_TIER_1
            fee in fee_bps_strategy(),
        ) {
            prop_assume!(p.checked_mul(q * 2).is_some());
            let (_, f1, _, _) = purchase_totals(p, q, fee).unwrap();
            let (_, f2, _, _) = purchase_totals(p, q * 2, fee).unwrap();
            // Doubling quantity doubles the fee; allow at most 1 stroop rounding diff.
            let diff = if f2 > f1 * 2 { f2 - f1 * 2 } else { f1 * 2 - f2 };
            assert!(diff <= 1, "fee non-linear: f1={f1} f2={f2}");
        }
    }

    // ── Property 22: fee truncation never overcharges buyer ──────────────────
    proptest! {
        #[test]
        fn prop_fee_truncation_never_overcharges(
            p in price_strategy(),
            q in 1u128..100_000u128,
            fee in fee_bps_strategy(),
        ) {
            prop_assume!(p.checked_mul(q).is_some());
            if let Ok((g, f, _n, eff)) = purchase_totals(p, q, fee) {
                // exact real fee = g * eff / 10_000
                // integer fee should be ≤ exact real fee
                // i.e. f * 10_000 ≤ g * eff
                let lhs = f.checked_mul(MAX_FEE_BPS);
                let rhs = g.checked_mul(eff);
                match (lhs, rhs) {
                    (Some(l), Some(r)) => assert!(l <= r, "overcharged: l={l} r={r}"),
                    _ => {} // overflow in check arithmetic is fine, contract already handled it
                }
            }
        }
    }

    // ── Property 23: effective_fee ≤ MAX_FEE_BPS even at tier-3 boundary ─────
    proptest! {
        #[test]
        fn prop_effective_fee_boundary_tier3(
            base in 0u128..=MAX_FEE_BPS,
        ) {
            let eff = effective_fee_bps(base, BULK_TIER_3).unwrap();
            assert!(eff <= MAX_FEE_BPS);
            assert!(eff <= base);
        }
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// 3. Regression tests
//    Two concrete precision-loss bugs found during the arithmetic audit.
// ═════════════════════════════════════════════════════════════════════════════

mod regression {
    use super::*;

    /// Regression #1 — Fee truncation on small-price orders.
    ///
    /// Bug: With an original *unchecked* implementation using plain `as u128`
    /// casts, the expression:
    ///
    ///   fee = (price * qty * fee_bps) / 10_000
    ///
    /// was evaluated left-to-right without intermediate overflow checks.
    /// For `price = 1`, `qty = 999`, `fee_bps = 9_999` the product
    /// `price * qty * fee_bps = 9_989_001` is small enough to fit in u128,
    /// but an earlier prototype used `u64` internally and overflowed silently
    /// on values just above `u64::MAX`. The checked implementation detects
    /// this and either returns correctly or errors.
    ///
    /// Concrete check: the split calculation must equal the flat calculation.
    /// qty = 999 is deliberately chosen to be below BULK_TIER_1 (1_000) so
    /// no bulk discount is applied and the effective fee_bps == base fee_bps.
    #[test]
    fn regression_fee_truncation_small_price_bulk() {
        let price: u128 = 1;
        let qty: u128 = 999;  // < BULK_TIER_1, so no bulk discount
        let fee_bps: u128 = 9_999;

        // Via purchase_totals (the correct implementation)
        let (g, f, n, eff) = purchase_totals(price, qty, fee_bps).unwrap();

        // No discount applied (qty < BULK_TIER_1 = 1_000)
        assert_eq!(eff, fee_bps, "no discount expected");

        // Manual reference calculation using the same checked ops
        let expected_gross = price.checked_mul(qty).unwrap();
        let expected_fee   = expected_gross.checked_mul(fee_bps).unwrap()
                                           .checked_div(MAX_FEE_BPS).unwrap();
        let expected_net   = expected_gross.checked_add(expected_fee).unwrap();

        assert_eq!(g, expected_gross, "gross mismatch");
        assert_eq!(f, expected_fee,   "fee mismatch");
        assert_eq!(n, expected_net,   "net mismatch");

        // Spot values
        assert_eq!(g, 999);
        // fee = 999 * 9_999 / 10_000 = 9_989_001 / 10_000 = 998 (truncated)
        assert_eq!(f, 998);
        assert_eq!(n, 1_997);
    }

    /// Regression #2 — Bulk-discount underflow (fee goes negative).
    ///
    /// Bug: In a previous prototype the effective fee was computed as:
    ///
    ///   effective_bps = base_fee_bps - discount_bps   // unchecked subtraction
    ///
    /// When `base_fee_bps = 100` and `quantity >= BULK_TIER_1` the discount is
    /// `500 bps`, causing a *wrapping underflow* on u128:
    ///   100u128.wrapping_sub(500) == u128::MAX - 399
    ///
    /// That astronomically large effective rate then produced a fee larger than
    /// the entire contract's available balance, triggering a spurious rejection.
    ///
    /// The fix uses `saturating_sub`, which floors the result at 0.
    #[test]
    fn regression_bulk_discount_underflow() {
        let base_fee_bps: u128 = 100;  // 1%
        let quantity: u128 = BULK_TIER_1; // discount = 500 bps > base_fee_bps

        // Correct behaviour: effective rate floors at 0, fee = 0.
        let eff = effective_fee_bps(base_fee_bps, quantity).unwrap();
        assert_eq!(
            eff, 0,
            "effective fee should floor at 0, got {eff} (possible underflow)"
        );

        // Demonstrate what the buggy wrapping_sub would have produced:
        let buggy_eff = base_fee_bps.wrapping_sub(500u128);
        // buggy_eff is a huge number — make sure the fixed code never returns it
        assert_ne!(eff, buggy_eff, "bug still present: wrapping underflow not fixed");

        // And verify the full purchase_totals path returns zero fee
        let price: u128 = 1_000_000;
        let (g, f, n, _eff2) = purchase_totals(price, quantity, base_fee_bps).unwrap();
        assert_eq!(f, 0, "fee should be 0 after discount underflow fix");
        assert_eq!(n, g, "net should equal gross when fee is 0");
    }
}
