# Replay Attack Surface Analysis

**System:** StellarKraal Carbon Credit Contracts  
**Issue:** #58  
**Analysis Date:** 2026-07-18  
**Analyst:** Kiro AI  
**Status:** All exploitable surfaces fixed and verified

---

## 1. Executive Summary

This document analyses the full transaction lifecycle of `carbon_marketplace::purchase_listing` and `carbon_credit::retire` for replay attack surfaces. Replay attacks occur when a valid signed transaction (or a structurally identical unsigned call) can be resubmitted — either by the original signer or by an observer — to produce duplicate on-chain effects.

Four replay surfaces were identified. Two are mitigated at the Stellar protocol layer and require no application-level countermeasure. Two are application-level gaps that were not covered by any prior guard and have been fixed in this work.

| ID | Surface | Layer | Exploitable | Status |
|----|---------|-------|-------------|--------|
| RS-01 | `retire()` — no idempotency key | Application | **Yes** | **Fixed** |
| RS-02 | `create_listing()` — deterministic listing ID | Application | **Yes** | **Fixed** |
| RS-03 | `purchase_listing()` — no max-ledger enforcement | Application | Conditional | **Fixed** |
| RS-04 | Stellar protocol sequence numbers | Protocol | No | Mitigated at protocol layer |

---

## 2. Scope

| Contract | File | Role |
|----------|------|------|
| `carbon_credit` | `contracts/carbon_credit/src/lib.rs` | Balance ledger; `retire()` is the primary irreversible value-destruction operation |
| `carbon_marketplace` | `contracts/carbon_marketplace/src/lib.rs` | Orchestrator; `purchase_listing()` transfers credits between parties |

Out of scope: replay attacks at the Stellar base-layer (sequence number enforcement), backend HTTP replay (covered in `docs/backend/idempotency.md`), and oracle bridge operations.

---

## 3. Background: What Soroban Provides

### 3.1 Protocol-Layer Mitigations (RS-04)

Every Stellar transaction carries:

- **Account sequence number** — monotonically increasing per source account; the network rejects any transaction whose sequence number is not `current + 1`. This prevents any verbatim transaction replay at the XDR layer.
- **Ledger bounds** (`min_ledger_sequence` / `max_ledger_sequence`) — optional transaction-level fields that restrict the ledger range in which the transaction is valid. A transaction with `max_ledger_sequence = N` is permanently invalid after ledger `N` closes.
- **Time bounds** (`min_time` / `max_time`) — analogous time-based validity window.

These protocol controls mean that a **signed XDR transaction cannot be replayed verbatim** because the sequence number will be exhausted after the first acceptance.

**What protocol controls do NOT prevent:**

1. A caller constructing a new transaction with a fresh sequence number that carries an identical contract invocation payload. The contract sees the same arguments, the same caller address, and the same ledger — and if the contract stores no operation-level deduplication state, the contract will execute the operation again.
2. An off-chain relay or backend that retries a contract call on a transient RPC failure, submitting the same logical operation multiple times with different sequence numbers.
3. A malicious caller who observes a successful operation and manually reissues an identical call with a fresh sequence number.

Application-level mitigations are therefore necessary for any operation where duplicate execution causes harm.

---

## 4. Identified Replay Surfaces

### RS-01 — `retire()`: No Idempotency Key (HIGH)

**Contract:** `carbon_credit`  
**Function:** `retire(from, project_id, amount)`  
**Location:** `contracts/carbon_credit/src/lib.rs`

#### Description

`retire()` is the permanent, irreversible destruction of carbon credits — it moves credits from a holder's active balance into the `RSUP` (retired supply) pool. Retired credits represent exercised environmental offsets and **cannot be un-retired**.

Before this fix, `retire()` accepted calls identified only by `(from, project_id, amount)`. No operation identifier, nonce, or deduplication key was stored. Any caller holding enough balance could submit an identical call multiple times — each invocation would pass all checks and retire another `amount` credits from the caller's balance.

#### Exploitation Path

```
Precondition: Alice holds 1000 credits for project P.

t0: Alice calls retire(Alice, P, 100)  →  balance: 900,  retired: 100  ✓
t1: Alice (accidentally, or an attacker replaying a captured intent)
    calls retire(Alice, P, 100) again  →  balance: 800,  retired: 200  ✓
    ... repeatable until balance == 0
```

**Accident scenario:** Alice's wallet or backend retries a retire call on a transient RPC failure. Both the original and the retry land, retiring 200 credits instead of 100. Alice has no recourse because retirement is irreversible.

**Malicious scenario (off-chain authorization):** In some designs, Alice signs an off-chain "retire intent" (e.g., a certificate request) that is later submitted on-chain by an intermediary. If the intermediary holds the signed intent payload, they can submit it multiple times, draining Alice's balance.

#### Impact

- Credits over-retired — environmental offsets are claimed that were never intended.
- Holder's balance drained beyond their intention.
- Because retirement is irreversible, there is no recovery path once the credits are retired.

**Severity: High**

#### Mitigation Applied

A per-operation idempotency key (`operation_id: BytesN<32>`) was added to `retire()`. The contract stores a boolean flag at the key `("RETOP", operation_id)` in persistent storage before executing the retirement. Any duplicate call with the same `operation_id` is rejected with `CreditError::AlreadyRetired` before any balance change occurs.

The caller (wallet, backend, or relay) is responsible for generating a unique `operation_id` per intent — typically a random 32-byte nonce or a hash of `(caller, project, amount, timestamp, nonce)`. Because the key is stored in persistent ledger storage, deduplication is permanent.

---

### RS-02 — `create_listing()`: Deterministic Listing ID (MEDIUM)

**Contract:** `carbon_marketplace`  
**Function:** `create_listing(seller, project_id, amount, price_per_credit)`  
**Location:** `contracts/carbon_marketplace/src/lib.rs`

#### Description

Before this fix, the `listing_id` was derived as:

```rust
let listing_id_input = (seller, project_id, amount, e.ledger().sequence());
let listing_id: BytesN<32> = e.crypto().sha256(&encode(listing_id_input)).into();
```

The only entropy source in the derivation was `e.ledger().sequence()` (the current ledger number, a public value). If a seller submitted two identical create-listing calls in the same ledger — or if a relay retried a failed create-listing in the same ledger — both calls would compute the same `listing_id` and the second call would silently overwrite the first.

#### Exploitation Path

```
Ledger 100:
  t0: Seller calls create_listing(S, P, 100 credits, 5/credit)
      → listing_id = sha256(S, P, 100, 100) = 0xABCD...
      → stores Listing { amount: 100, price: 5, status: Active }

  t1 (same ledger, retry): Relay retries because it received a transient RPC timeout.
      → same args, same ledger → same listing_id = 0xABCD...
      → overwrites the existing listing with the same data (no harm in this case)

Ledger 100 (malicious):
  t0: Seller calls create_listing(S, P, 100 credits, 5/credit) → listing_id 0xABCD
  t1: Seller calls create_listing(S, P, 100 credits, 1/credit)  ← same args except price!
      → same listing_id 0xABCD (price_per_credit is NOT part of the hash input!)
      → overwrites with price: 1 — seller's listing is front-run to a lower price
```

Note: `price_per_credit` was not part of the hash input in the original code, making this overwrite trivially achievable by the seller themselves or by an observer who predicted the `listing_id`.

#### Impact

- A listing can be silently overwritten by a duplicate call in the same ledger.
- Buyers holding a `listing_id` obtained from an earlier transaction may find a different price or amount than expected.
- A griefing attack can repeatedly overwrite a seller's listing to prevent them from maintaining a stable offer.

**Severity: Medium**

#### Mitigation Applied

A monotonic per-seller counter (`"SCTR"` key in instance storage) is used as an additional entropy component. The listing ID is now derived as:

```rust
let seller_nonce: u64 = e.storage().instance().get(&("SCTR", &seller)).unwrap_or(0);
let listing_id = sha256(seller, project_id, amount, price_per_credit, ledger_sequence, seller_nonce);
e.storage().instance().set(&("SCTR", &seller), &(seller_nonce + 1));
```

Because `seller_nonce` is strictly increasing and includes the seller's address, no two calls from the same seller (even in the same ledger) can produce the same `listing_id`. The nonce also now incorporates `price_per_credit`, closing the overwrite-by-price-change vector.

---

### RS-03 — `purchase_listing()`: No Application-Level Ledger-Bound Enforcement (MEDIUM)

**Contract:** `carbon_marketplace`  
**Function:** `purchase_listing(buyer, listing_id, payment_amount)`  
**Location:** `contracts/carbon_marketplace/src/lib.rs`

#### Description

Soroban transactions may carry `max_ledger_sequence` at the XDR envelope level. However, the contract itself does not enforce any caller-supplied expiry on the purchase operation. A buyer who signs an off-chain purchase intent (e.g., for a marketplace UI) cannot express an on-chain deadline after which the intent is invalid. If the signed transaction is held by a relay or broker and submitted significantly later, the buyer may receive credits at a price that no longer reflects the intended transaction context.

Additionally, without a contract-level ledger-bound check, a purchase intent cannot be made "atomic with a ledger window" — meaning the buyer cannot say "only execute this purchase in ledger 1000–1010, after which I may have changed my mind."

#### Impact

- A delayed or withheld purchase intent can be submitted by a broker after market conditions change.
- No application-level expiry audit trail: there is no on-chain record of an intended expiry window.

**Severity: Medium** (depends on relayer trust model; lower in fully trustless UX flows)

#### Mitigation Applied

A `max_ledger` parameter was added to `purchase_listing()`. The contract asserts:

```rust
if e.ledger().sequence() > max_ledger {
    return Err(MarketError::TransactionExpired);
}
```

This check runs immediately after auth verification, before any state read. Passing `max_ledger = u32::MAX` preserves the original behaviour for callers that do not require expiry enforcement.

---

### RS-04 — Stellar Protocol Sequence Numbers (MITIGATED AT PROTOCOL LAYER)

**Layer:** Stellar protocol  
**Mitigated by:** Account sequence number uniqueness enforced by the network validators

Every Stellar transaction carries a per-account, monotonically increasing sequence number. A transaction with `sequence = N` is valid only if the source account's current on-chain sequence equals `N - 1`. Once the transaction is included in a ledger, the account's sequence advances to `N`, permanently invalidating any replay of the exact same XDR envelope.

**This surface requires no application-level mitigation.** The Stellar protocol unconditionally prevents verbatim XDR replay. However, as noted in Section 3.1, this does not prevent a caller from constructing a new transaction carrying an identical logical operation — which is the class of attacks addressed by RS-01, RS-02, and RS-03.

---

## 5. Protocol vs. Application Layer: Summary Table

| Surface | Protocol Mitigation | Application Mitigation | Notes |
|---------|--------------------|-----------------------|-------|
| Verbatim XDR replay | ✅ Sequence numbers | Not needed | Stellar guarantees uniqueness |
| Transaction time expiry | ✅ Time bounds (optional) | Partial (RS-03 adds ledger bounds) | Protocol time bounds are per-envelope; contract ledger bounds are per-intent |
| Duplicate retire with fresh seq# | ❌ None | ✅ RS-01: operation_id dedup | Caller constructs new tx, same logical op |
| Listing ID collision / overwrite | ❌ None | ✅ RS-02: seller nonce | Same-ledger deterministic ID |
| Stale purchase intent submission | ❌ None | ✅ RS-03: max_ledger param | Off-chain relay withholding attack |

---

## 6. Mitigations Implementation Summary

### 6.1 `carbon_credit::retire()` — Operation-ID Idempotency (RS-01)

**New signature:**
```rust
pub fn retire(
    e: Env,
    from: Address,
    project_id: BytesN<32>,
    amount: i128,
    operation_id: BytesN<32>,   // ← NEW
) -> Result<(), CreditError>
```

**New error code:**
```rust
CreditError::AlreadyRetired = 8,
```

**New storage key:**
```rust
fn retire_op_key(e: &Env, op_id: &BytesN<32>) -> Val {
    (symbol_short!("RETOP"), op_id.clone()).into_val(e)
}
```

**Guard logic (runs before any balance change):**
```rust
let opkey = retire_op_key(&e, &operation_id);
if e.storage().persistent().has(&opkey) {
    return Err(CreditError::AlreadyRetired);
}
e.storage().persistent().set(&opkey, &true);
```

### 6.2 `carbon_marketplace::create_listing()` — Seller Nonce (RS-02)

**New storage key:**
```rust
fn seller_nonce_key(e: &Env, seller: &Address) -> Val {
    (symbol_short!("SCTR"), seller.clone()).into_val(e)
}
```

**Listing ID derivation (replaces old derivation):**
```rust
let seller_nonce: u64 = e.storage().instance()
    .get(&seller_nonce_key(&e, &seller))
    .unwrap_or(0u64);
e.storage().instance().set(&seller_nonce_key(&e, &seller), &(seller_nonce + 1));

let listing_id_input = (
    seller.clone(),
    project_id.clone(),
    amount,
    price_per_credit,         // ← now included
    e.ledger().sequence(),
    seller_nonce,             // ← NEW
).into_val(&e);
let listing_id: BytesN<32> = e.crypto().sha256(&encode(listing_id_input)).into();
```

### 6.3 `carbon_marketplace::purchase_listing()` — Max Ledger Enforcement (RS-03)

**New signature:**
```rust
pub fn purchase_listing(
    e: Env,
    buyer: Address,
    listing_id: BytesN<32>,
    payment_amount: i128,
    max_ledger: u32,           // ← NEW
) -> Result<(), MarketError>
```

**New error code:**
```rust
MarketError::TransactionExpired = 11,
```

**Guard logic (runs immediately after auth):**
```rust
if e.ledger().sequence() > max_ledger {
    return Err(MarketError::TransactionExpired);
}
```

---

## 7. Test Coverage

Three PoC exploit tests and three regression tests are provided. See the test files for full details.

| Test | File | Purpose |
|------|------|---------|
| `test_poc_retire_replay_without_operation_id` | `carbon_credit/src/tests.rs` | PoC: demonstrates retire replay drains balance |
| `test_poc_create_listing_id_collision_same_ledger` | `carbon_marketplace/src/tests.rs` | PoC: demonstrates listing ID collision overwrites |
| `test_poc_purchase_listing_no_expiry` | `carbon_marketplace/src/tests.rs` | PoC: demonstrates purchase with no ledger bound |
| `test_mitigation_retire_operation_id_dedup` | `carbon_credit/src/tests.rs` | Regression: duplicate retire rejected |
| `test_mitigation_create_listing_unique_ids_same_ledger` | `carbon_marketplace/src/tests.rs` | Regression: same-ledger listings get unique IDs |
| `test_mitigation_purchase_listing_expired_rejected` | `carbon_marketplace/src/tests.rs` | Regression: expired purchase rejected |

---

## 8. Sign-off

**Analyst:** Kiro AI  
**Date:** 2026-07-18  
**All exploitable surfaces:** Fixed and verified  
**Test suite:** All tests passing (0 failures)

Closes #58
