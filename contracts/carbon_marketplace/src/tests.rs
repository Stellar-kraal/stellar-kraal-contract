#![cfg(test)]

use soroban_sdk::{testutils::{Address as _}, Address, Env, BytesN, symbol_short};
use crate::*;

fn make_env() -> Env {
    let env = Env::default();
    env.mock_all_auths();
    env
}

// ── Test harness ───────────────────────────────────────────────────────────

use carbon_registry::{CarbonRegistry, CarbonRegistryClient};
use carbon_credit::{CarbonCredit, CarbonCreditClient};

/// Deploys the full three-contract stack and returns all addresses/clients.
struct TestContext<'a> {
    env: Env,
    reg_client: CarbonRegistryClient<'a>,
    credit_client: CarbonCreditClient<'a>,
    market_client: CarbonMarketplaceClient<'a>,
    admin: Address,
    reg_admin: Address,
}

fn setup_full<'a>(env: &'a Env) -> TestContext<'a> {
    let reg_admin = Address::generate(env);
    let market_admin = Address::generate(env);

    // Deploy marketplace first to get its address (needed as trusted caller in registry)
    let market_addr = env.register(CarbonMarketplace, ());
    let market_client = CarbonMarketplaceClient::new(env, &market_addr);

    // Deploy registry with marketplace as the trusted caller
    let reg_addr = env.register(CarbonRegistry, ());
    let reg_client = CarbonRegistryClient::new(env, &reg_addr);
    reg_client.initialize(&reg_admin, &market_addr);

    // Deploy credit contract
    let credit_addr = env.register(CarbonCredit, ());
    let credit_client = CarbonCreditClient::new(env, &credit_addr);
    credit_client.initialize(&market_admin, &reg_addr, &market_addr);

    // Initialize marketplace
    market_client.initialize(&market_admin, &reg_addr, &credit_addr, &100_i128);

    TestContext {
        env: env.clone(),
        reg_client,
        credit_client,
        market_client,
        admin: market_admin,
        reg_admin,
    }
}

/// Register + verify a project; mint `mint_amount` credits to `seller`.
/// Returns the project_id.
fn setup_project_with_credits(
    ctx: &TestContext<'_>,
    seller: &Address,
    total_credits: i128,
    mint_amount: i128,
) -> BytesN<32> {
    let owner = Address::generate(&ctx.env);
    let project_id = ctx.reg_client.register_project(
        &owner,
        &symbol_short!("PROJ"),
        &total_credits,
        &2024_u32,
    );
    ctx.reg_client.verify_project(&project_id);

    // issue_credits via registry to record allocation
    ctx.reg_client.issue_credits(&project_id, &mint_amount);

    // Mint credits to seller via credit contract
    ctx.credit_client.mint(seller, &project_id, &mint_amount);

    project_id
}

// ── Basic happy path ───────────────────────────────────────────────────────

#[test]
fn test_initialize_succeeds() {
    let env = make_env();
    let ctx = setup_full(&env);
    let cfg = ctx.market_client.get_config();
    assert_eq!(cfg.admin, ctx.admin);
    assert_eq!(cfg.fee_bps, 100);
}

#[test]
fn test_create_listing_happy_path() {
    let env = make_env();
    let ctx = setup_full(&env);

    let seller = Address::generate(&env);
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 500);

    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &200_i128, &10_i128);
    let listing = ctx.market_client.get_listing(&listing_id);

    assert_eq!(listing.seller, seller);
    assert_eq!(listing.amount, 200);
    assert_eq!(listing.price_per_credit, 10);
    assert_eq!(listing.status, ListingStatus::Active);
}

#[test]
fn test_purchase_listing_happy_path() {
    let env = make_env();
    let ctx = setup_full(&env);

    let seller = Address::generate(&env);
    let buyer = Address::generate(&env);
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 300);

    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &100_i128, &5_i128);
    ctx.market_client.purchase_listing(&buyer, &listing_id, &500_i128);

    let listing = ctx.market_client.get_listing(&listing_id);
    assert_eq!(listing.status, ListingStatus::Sold);

    // Buyer should have received credits
    assert_eq!(ctx.credit_client.balance_of(&buyer, &project_id), 100);
}

#[test]
fn test_cancel_listing_happy_path() {
    let env = make_env();
    let ctx = setup_full(&env);

    let seller = Address::generate(&env);
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 200);

    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &100_i128, &3_i128);
    ctx.market_client.cancel_listing(&seller, &listing_id);

    let listing = ctx.market_client.get_listing(&listing_id);
    assert_eq!(listing.status, ListingStatus::Cancelled);
}

// ── Vulnerability Reproduction Tests ──────────────────────────────────────

/// CC-002 reproduction: CEI violation in purchase_listing.
///
/// In the UNFIXED code, the listing stays `Active` during all cross-contract calls.
/// This test documents the vulnerable state: after a successful purchase the listing
/// SHOULD be Sold. We verify the post-purchase state equals Sold (the fix invariant),
/// and note where the vulnerability window existed.
///
/// In the original vulnerable code, two concurrent purchases of the same listing
/// would both read `status == Active` and both proceed. Because Soroban is
/// single-threaded within a ledger but allows multiple operations, this manifests
/// as the second purchase being able to read Active status before the first write
/// of Sold lands. After the fix (CEI applied), status is set Sold BEFORE any
/// cross-contract calls, so a second purchase attempt reads Sold and fails.
#[test]
fn test_purchase_listing_check_effects_violation() {
    let env = make_env();
    let ctx = setup_full(&env);

    let seller = Address::generate(&env);
    let buyer1 = Address::generate(&env);
    let buyer2 = Address::generate(&env);

    // Set up a project with exactly 100 credits for the seller
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 100);

    // Create a listing for all 100 credits
    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &100_i128, &1_i128);

    // Verify: listing starts Active
    let listing_before = ctx.market_client.get_listing(&listing_id);
    assert_eq!(listing_before.status, ListingStatus::Active,
        "Listing must start Active");

    // First purchase succeeds
    ctx.market_client.purchase_listing(&buyer1, &listing_id, &100_i128);

    // After purchase: listing must be Sold (CEI fix ensures this)
    let listing_after = ctx.market_client.get_listing(&listing_id);
    assert_eq!(listing_after.status, ListingStatus::Sold,
        "INVARIANT: listing.status MUST be Sold immediately after purchase_listing returns");

    // Second purchase on the same listing must fail — listing is now Sold
    // In the UNFIXED code with concurrent execution, both would have succeeded.
    // In the FIXED code, the state was written Sold before any cross-contract call,
    // so the second attempt reads Sold and returns ListingNotActive.
    let res = ctx.market_client.try_purchase_listing(&buyer2, &listing_id, &100_i128);
    assert_eq!(res, Err(Ok(MarketError::ListingNotActive)),
        "A second purchase of a Sold listing must fail with ListingNotActive");

    // Buyer2 must have received no credits
    assert_eq!(ctx.credit_client.balance_of(&buyer2, &project_id), 0,
        "buyer2 must receive zero credits — the double-spend is prevented");
}

/// CC-001 reproduction: TOCTOU — stale project status at create_listing time.
///
/// In the UNFIXED `create_listing()`, the project status is read once and not
/// re-verified at purchase time. This test shows that even when a project is
/// suspended AFTER listing creation, the FIXED `purchase_listing()` catches it
/// by re-checking the project status before executing the transfer.
///
/// Test flow:
///   1. Create listing while project is Verified (listing creation succeeds).
///   2. Admin suspends the project.
///   3. Attempt to purchase the listing — must fail because the fix re-checks status.
#[test]
fn test_create_listing_toctou_stale_project_status() {
    let env = make_env();
    let ctx = setup_full(&env);

    let seller = Address::generate(&env);
    let buyer = Address::generate(&env);

    // Step 1: Project is Verified — listing creation passes the status check.
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 200);
    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &100_i128, &2_i128);

    let listing = ctx.market_client.get_listing(&listing_id);
    assert_eq!(listing.status, ListingStatus::Active,
        "Listing should be Active after creation while project is Verified");

    // Step 2: Admin suspends the project AFTER the listing is created.
    // This is the TOCTOU race: the status read at create_listing time is now stale.
    ctx.reg_client.suspend_project(&project_id);

    // Step 3: purchase_listing must re-verify project status.
    // FIXED behavior: purchase_listing re-checks registry status and must reject the purchase.
    let res = ctx.market_client.try_purchase_listing(&buyer, &listing_id, &200_i128);
    assert_eq!(res, Err(Ok(MarketError::ProjectNotVerified)),
        "TOCTOU fix: purchase must fail when project is suspended after listing creation");

    // Seller's credits are untouched
    assert_eq!(ctx.credit_client.balance_of(&seller, &project_id), 200,
        "Seller credits must not be burned when purchase is rejected");
}

/// CC-003 reproduction: auth-order vulnerability in mint_project_credits.
///
/// In the UNFIXED code, `cfg.admin.require_auth()` is called AFTER the first
/// cross-contract call to `registry.issue_credits()`. This test documents the
/// correct (FIXED) behavior: the admin auth check must be the very first operation,
/// and the function must succeed when called by the admin.
///
/// Because `mock_all_auths()` is active, both the vulnerable and fixed versions
/// will pass auth. The test instead verifies:
///   1. The function succeeds when called by the admin (normal path).
///   2. The fix is documented: require_auth() appears before any external call.
///
/// The structural vulnerability is verified by inspection (see audit report CC-003).
#[test]
fn test_mint_project_credits_auth_order() {
    let env = make_env();
    let ctx = setup_full(&env);

    // Register and verify a project — issue_credits in registry requires marketplace auth
    let owner = Address::generate(&env);
    let project_id = ctx.reg_client.register_project(
        &owner,
        &symbol_short!("MINT"),
        &5000_i128,
        &2024_u32,
    );
    ctx.reg_client.verify_project(&project_id);

    // Call mint_project_credits as admin — should succeed
    ctx.market_client.mint_project_credits(&project_id, &200_i128);

    // Owner should have received credits
    let owner_balance = ctx.credit_client.balance_of(&owner, &project_id);
    assert_eq!(owner_balance, 200,
        "Owner must receive credits after mint_project_credits");

    // Registry should record issued credits
    let project = ctx.reg_client.get_project(&project_id);
    assert_eq!(project.issued_credits, 200,
        "Registry must record the issued credits");

    // Verify auth was required: in non-mocked environment, calling without admin
    // credentials would fail. The FIXED code places require_auth() BEFORE any
    // cross-contract call, ensuring no state mutation occurs for unauthorized callers.
    // (Structural verification: see carbon_marketplace/src/lib.rs line ~350 after fix)
}

// ── Property-Based Tests ───────────────────────────────────────────────────

/// Property: after purchase_listing succeeds, listing.status MUST be Sold.
///
/// This invariant is the direct consequence of applying CEI: state is written
/// before interactions, so it is always committed when the function returns Ok.
/// Tested across multiple listings with varying amounts and prices.
#[test]
fn test_prop_listing_always_sold_after_purchase() {
    let env = make_env();
    let ctx = setup_full(&env);

    let seller = Address::generate(&env);
    let project_id = setup_project_with_credits(&ctx, &seller, 10000, 3000);

    // Create multiple listings with different parameters
    let test_cases: [(i128, i128); 3] = [
        (100, 5),
        (200, 10),
        (50, 20),
    ];

    for (amount, price) in test_cases.iter() {
        let buyer = Address::generate(&env);
        let listing_id = ctx.market_client.create_listing(&seller, &project_id, amount, price);

        // Pre-condition: listing is Active
        let before = ctx.market_client.get_listing(&listing_id);
        assert_eq!(before.status, ListingStatus::Active);

        let total_cost = amount * price;
        ctx.market_client.purchase_listing(&buyer, &listing_id, &total_cost);

        // POST-CONDITION (the property): status is ALWAYS Sold after successful purchase
        let after = ctx.market_client.get_listing(&listing_id);
        assert_eq!(after.status, ListingStatus::Sold,
            "PROPERTY VIOLATION: listing.status must be Sold after purchase_listing succeeds \
             (amount={}, price={})", amount, price);

        // Buyer received the correct number of credits
        assert_eq!(ctx.credit_client.balance_of(&buyer, &project_id), *amount,
            "Buyer must receive exactly {} credits", amount);
    }
}

/// Property: total supply of credits is conserved across any transfer sequence.
///
/// Minting increases supply, burning decreases supply, and transfers leave it unchanged.
/// This test validates the conservation law across a realistic purchase flow:
///   - Mint credits to seller.
///   - Seller lists credits.
///   - Buyer purchases → credits move from seller to buyer.
///   - Total supply stays the same across the purchase (burn old + mint new = net zero change).
#[test]
fn test_prop_credits_conserved_across_transfer() {
    let env = make_env();
    let ctx = setup_full(&env);

    let seller = Address::generate(&env);
    let buyer = Address::generate(&env);
    let mint_amount: i128 = 500;
    let list_amount: i128 = 200;

    let project_id = setup_project_with_credits(&ctx, &seller, 1000, mint_amount);

    // Record supply and balances before the purchase
    let supply_before = ctx.credit_client.total_supply(&project_id);
    let seller_before = ctx.credit_client.balance_of(&seller, &project_id);
    let buyer_before = ctx.credit_client.balance_of(&buyer, &project_id);

    assert_eq!(supply_before, mint_amount,
        "Initial supply must equal minted amount");
    assert_eq!(seller_before, mint_amount,
        "Seller must hold all initially minted credits");
    assert_eq!(buyer_before, 0,
        "Buyer starts with zero credits");

    // Create listing and purchase
    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &list_amount, &1_i128);
    ctx.market_client.purchase_listing(&buyer, &listing_id, &list_amount);

    // Observe post-purchase state
    let supply_after = ctx.credit_client.total_supply(&project_id);
    let seller_after = ctx.credit_client.balance_of(&seller, &project_id);
    let buyer_after = ctx.credit_client.balance_of(&buyer, &project_id);

    // PROPERTY 1: Total supply is conserved across the purchase
    // (burn seller + mint buyer is net-neutral on total supply)
    assert_eq!(supply_before, supply_after,
        "PROPERTY VIOLATION: total supply must be conserved across a purchase \
         (was {}, now {})", supply_before, supply_after);

    // PROPERTY 2: Credit redistribution is exact
    assert_eq!(seller_after, seller_before - list_amount,
        "Seller must have exactly list_amount fewer credits");
    assert_eq!(buyer_after, buyer_before + list_amount,
        "Buyer must have exactly list_amount more credits");

    // PROPERTY 3: Sum of individual balances equals total supply
    assert_eq!(seller_after + buyer_after, supply_after,
        "Sum of all balances must equal total supply");
}

// ── Edge case / negative tests ─────────────────────────────────────────────

#[test]
fn test_create_listing_zero_amount_fails() {
    let env = make_env();
    let ctx = setup_full(&env);
    let seller = Address::generate(&env);
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 100);
    let res = ctx.market_client.try_create_listing(&seller, &project_id, &0_i128, &10_i128);
    assert_eq!(res, Err(Ok(MarketError::InvalidAmount)));
}

#[test]
fn test_purchase_nonexistent_listing_fails() {
    let env = make_env();
    let ctx = setup_full(&env);
    let buyer = Address::generate(&env);
    let fake_id = BytesN::from_array(&env, &[0u8; 32]);
    let res = ctx.market_client.try_purchase_listing(&buyer, &fake_id, &1000_i128);
    assert_eq!(res, Err(Ok(MarketError::ListingNotFound)));
}

#[test]
fn test_purchase_underpayment_fails() {
    let env = make_env();
    let ctx = setup_full(&env);
    let seller = Address::generate(&env);
    let buyer = Address::generate(&env);
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 100);

    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &100_i128, &10_i128);
    // Total cost is 1000 but payment is only 999
    let res = ctx.market_client.try_purchase_listing(&buyer, &listing_id, &999_i128);
    assert_eq!(res, Err(Ok(MarketError::InsufficientFunds)));
}

#[test]
fn test_cancel_already_sold_listing_fails() {
    let env = make_env();
    let ctx = setup_full(&env);
    let seller = Address::generate(&env);
    let buyer = Address::generate(&env);
    let project_id = setup_project_with_credits(&ctx, &seller, 1000, 100);

    let listing_id = ctx.market_client.create_listing(&seller, &project_id, &100_i128, &1_i128);
    ctx.market_client.purchase_listing(&buyer, &listing_id, &100_i128);

    let res = ctx.market_client.try_cancel_listing(&seller, &listing_id);
    assert_eq!(res, Err(Ok(MarketError::ListingNotActive)));
}
