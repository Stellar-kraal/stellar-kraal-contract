# Authorization Audit Checklist

**System:** StellarKraal Carbon Credit Contracts  
**Issue:** #56 — Authorization Model Audit: Admin Key Compromise and Privilege Escalation Paths  
**Audit Date:** 2026-07-17  
**Auditor:** Kiro AI  
**Scope:** All four contracts — `carbon_registry`, `carbon_marketplace`, `carbon_credit`, `carbon_oracle`  
**Status:** Complete

---

## 1. Audit Methodology

For each `require_auth` call site the following questions were evaluated:

| Check | Description |
|-------|-------------|
| **A1** | Is the auth check the first statement after config load? |
| **A2** | Does the checked address match the intended principal for this operation? |
| **A3** | Is the checked address bound to a specific function argument (require_auth_for_args)? |
| **A4** | Would a compromised key executing this function cause irreversible harm? |
| **A5** | Is a timelock or multi-sig required based on the blast radius? |
| **A6** | Are there any code paths that skip the auth check? |

Legend: ✅ Pass · ⚠️ Warning · ❌ Fail · N/A Not applicable

---

## 2. carbon_registry

**File:** `contracts/carbon_registry/src/lib.rs`  
**Admin address:** `RegistryConfig.admin`  
**Secondary principals:** `RegistryConfig.marketplace` (for `issue_credits`)

### 2.1 Call Site Inventory

| # | Function | Checked Address | Line (approx.) | A1 | A2 | A3 | A4 | A5 | A6 | Notes |
|---|----------|----------------|----------------|----|----|----|----|----|----|-------|
| REG-01 | `register_project` | `owner` (caller-supplied) | ~113 | ✅ | ✅ | N/A | ❌ | N/A | ✅ | Owner registers their own project. Auth is first call after precondition check. Correct. |
| REG-02 | `verify_project` | `cfg.admin` | ~139 | ✅ | ✅ | N/A | ✅ | ⚠️ | ✅ | Admin-only. Marks a project Verified, enabling credit issuance. High-value, no timelock. |
| REG-03 | `suspend_project` | `cfg.admin` | ~160 | ✅ | ✅ | N/A | ✅ | ⚠️ | ✅ | Admin-only. Halts trading and minting for a project. Reversible but disruptive. No timelock. |
| REG-04 | `retire_project` | `cfg.admin` | ~181 | ✅ | ✅ | N/A | ✅ | ✅ | ✅ | Admin-only. **Irreversible.** Permanently closes a project. Highest-risk registry operation. **Requires timelock.** |
| REG-05 | `issue_credits` | `cfg.marketplace` | ~207 | ⚠️ | ⚠️ | N/A | ✅ | ⚠️ | ✅ | Only marketplace can call. Comment in source claims "marketplace OR admin" but code only calls `cfg.marketplace.require_auth()`. Admin cannot directly issue credits — this is an undocumented restriction. See AUDIT-REG-01. |

### 2.2 Findings

#### AUDIT-REG-01 — `issue_credits` Only Authorizes Marketplace (Not Admin)
- **Severity:** Low / Informational
- **Location:** `carbon_registry::issue_credits()` (~line 207)
- **Description:** The function comment states *"Either the marketplace or the admin must authorize this call"*, and the code contains dead variables `caller_is_marketplace` and `caller_is_admin`, but the actual auth check is `cfg.marketplace.require_auth()` only. The admin has no direct path to issue credits without going through the marketplace contract.
- **Risk:** Admin key compromise does not grant direct `issue_credits` capability, which is beneficial for containment. However, the misleading comment may cause future developers to introduce an incorrect dual-auth pattern.
- **Recommendation:** Either add `cfg.admin.require_auth()` as an alternative (requiring one of the two to be present), or remove the misleading dead code and comment. Soroban does not natively support OR-auth; the pattern requires checking both and catching the auth error.

#### AUDIT-REG-02 — No `require_auth_for_args` Usage
- **Severity:** Informational
- **Description:** None of the registry functions use `require_auth_for_args`. For `verify_project`, `suspend_project`, and `retire_project`, the project ID is not bound to the auth context — an admin can call these functions with any `project_id` they choose. This is intentional (admin is fully trusted for these operations) but should be documented.
- **Recommendation:** Accept as-is; document in security policy that admin key compromise grants project lifecycle control over all projects.

### 2.3 Summary Table

| Function | Auth Type | Correct | Timelock Needed |
|----------|-----------|---------|-----------------|
| `initialize` | None (one-time, no auth) | ✅ | No |
| `register_project` | owner | ✅ | No |
| `verify_project` | admin | ✅ | No (reversible) |
| `suspend_project` | admin | ✅ | No (reversible) |
| `retire_project` | admin | ✅ | **Yes (irreversible)** |
| `issue_credits` | marketplace only | ⚠️ | No |
| `get_project` | none (read-only) | ✅ | N/A |
| `get_config` | none (read-only) | ✅ | N/A |

---

## 3. carbon_marketplace

**File:** `contracts/carbon_marketplace/src/lib.rs`  
**Admin address:** `MarketConfig.admin`  
**Secondary principals:** `seller` (listing owner), `buyer` (purchaser)

### 3.1 Call Site Inventory

| # | Function | Checked Address | Line (approx.) | A1 | A2 | A3 | A4 | A5 | A6 | Notes |
|---|----------|----------------|----------------|----|----|----|----|----|----|-------|
| MP-01 | `create_listing` | `seller` (caller-supplied) | ~215 | ✅ | ✅ | N/A | ❌ | N/A | ✅ | Seller authorizes their own listing. First statement. Correct. |
| MP-02 | `purchase_listing` | `buyer` (caller-supplied) | ~273 | ✅ | ✅ | N/A | ❌ | N/A | ✅ | Buyer authorizes their own purchase. First statement. Correct. |
| MP-03 | `cancel_listing` | `seller` (caller-supplied) | ~337 | ✅ | ✅ | N/A | ❌ | N/A | ✅ | Seller-only cancel. Additionally checks `listing.seller == seller`. Correct. |
| MP-04 | `mint_project_credits` | `cfg.admin` | ~370 | ✅* | ✅ | N/A | ✅ | ✅ | ✅ | Fixed (CC-003): auth now precedes all cross-contract calls. Requires timelock. |

*Fixed in CC-003 — previously auth was placed after first cross-contract call.

### 3.2 Findings

#### AUDIT-MP-01 — No Pause/Circuit-Breaker Mechanism
- **Severity:** High (Gap)
- **Description:** The marketplace has no `pause` function. A compromised admin key cannot halt trading, but neither can a legitimate admin respond to an emergency. In the event of a discovered vulnerability, there is no on-chain mechanism to stop all marketplace operations.
- **Risk:** All ongoing listings remain purchasable with no admin intervention possible. A security incident cannot be contained on-chain.
- **Recommendation:** Implement a `pause_marketplace` admin function with a timelock. All state-modifying marketplace functions should check a `paused` flag and return early if set. See Section 6 (Timelock Implementation) for the guarded `pause_marketplace` function.

#### AUDIT-MP-02 — `mint_project_credits` Was Auth-After-Effect (Fixed)
- **Severity:** Medium (Fixed — CC-003)
- **Description:** Previously, `cfg.admin.require_auth()` appeared after `registry.issue_credits()`. Now fixed.
- **Status:** ✅ Fixed

#### AUDIT-MP-03 — TOCTOU in `create_listing` (Known — VULN-MP-01)
- **Severity:** High (Informational — documented)
- **Description:** Project status and seller balance are checked independently before listing creation, with no atomic lock. A suspension between check and write creates an active listing for a suspended project.
- **Auth Impact:** None — this is a logic race, not an auth bypass.
- **Status:** Documented as VULN-MP-01; partially mitigated by re-verification at purchase time.

### 3.3 Summary Table

| Function | Auth Type | Correct | Timelock Needed |
|----------|-----------|---------|-----------------|
| `initialize` | None (one-time) | ✅ | No |
| `create_listing` | seller | ✅ | No |
| `purchase_listing` | buyer | ✅ | No |
| `cancel_listing` | seller | ✅ | No |
| `mint_project_credits` | admin | ✅ (fixed) | **Yes** |
| `pause_marketplace` | admin | ❌ Missing | **Yes (new function)** |
| `get_listing` | none (read-only) | ✅ | N/A |
| `get_config` | none (read-only) | ✅ | N/A |

---

## 4. carbon_credit

**File:** `contracts/carbon_credit/src/lib.rs`  
**Admin address:** `CreditConfig.admin` (stored but not currently used in any write operation)  
**Primary principals:** `CreditConfig.marketplace` (minter/burner), `from` address (transfer/retire/batch_transfer)

### 4.1 Call Site Inventory

| # | Function | Checked Address | Line (approx.) | A1 | A2 | A3 | A4 | A5 | A6 | Notes |
|---|----------|----------------|----------------|----|----|----|----|----|----|-------|
| CC-01 | `mint` | `cfg.marketplace` | ~190 | ✅ | ✅ | N/A | ✅ | N/A | ✅ | Marketplace-only minting. Auth first after config load. Correct. |
| CC-02 | `transfer` | `from` (caller-supplied) | ~226 | ✅ | ✅ | N/A | ❌ | N/A | ✅ | Sender authorizes transfer. Correct. |
| CC-03 | `burn` | `cfg.marketplace` | ~264 | ✅ | ✅ | N/A | ✅ | N/A | ✅ | Marketplace-only burn. Auth first. Correct. |
| CC-04 | `retire` | `from` (caller-supplied) | ~292 | ✅ | ✅ | N/A | ⚠️ | N/A | ✅ | Any holder can self-retire credits. Retirement is irreversible but is the intended final state. No admin auth needed. |
| CC-05 | `batch_transfer` | `from` (caller-supplied) | ~322 | ✅ | ✅ | N/A | ❌ | N/A | ✅ | `from.require_auth()` once before loop. All transfers use the same sender. Correct. |

### 4.2 Findings

#### AUDIT-CC-01 — Admin Address Stored but Never Used
- **Severity:** Informational
- **Description:** `CreditConfig.admin` is stored at initialization but there is no admin-only function in `carbon_credit`. The admin cannot directly mint, burn, pause, or modify any state. This is actually a positive containment property — admin key compromise does not directly affect the credit ledger.
- **Recommendation:** Document this as intentional. The credit contract is designed to be controlled exclusively through the marketplace contract. The admin field may be reserved for future upgrade governance.

#### AUDIT-CC-02 — TOCTOU in `mint()` (Known — VULN-CC-01)
- **Severity:** Medium (Documented)
- **Description:** `mint()` calls `registry.get_project()` to verify status, but between that cross-contract read and the balance write, the project status could change to Suspended. The marketplace's `purchase_listing()` fix (CC-002) re-verifies status before calling `mint`, providing partial mitigation.
- **Auth Impact:** None — this is a logic race at the cross-contract boundary.
- **Status:** Documented as VULN-CC-01; partially mitigated by upstream re-verification.

### 4.3 Summary Table

| Function | Auth Type | Correct | Timelock Needed |
|----------|-----------|---------|-----------------|
| `initialize` | None (one-time) | ✅ | No |
| `mint` | marketplace | ✅ | No |
| `transfer` | from | ✅ | No |
| `burn` | marketplace | ✅ | No |
| `retire` | from | ✅ | No |
| `batch_transfer` | from | ✅ | No |
| `balance_of` | none (read-only) | ✅ | N/A |
| `total_supply` | none (read-only) | ✅ | N/A |
| `retired_supply` | none (read-only) | ✅ | N/A |

---

## 5. carbon_oracle

**File:** `contracts/carbon_oracle/src/lib.rs`  
**Admin address:** `Config.admin`  
**Oracle principal:** oracle operator address (must be authorized per submission)

### 5.1 Call Site Inventory

| # | Function | Checked Address | Line (approx.) | A1 | A2 | A3 | A4 | A5 | A6 | Notes |
|---|----------|----------------|----------------|----|----|----|----|----|----|-------|
| OR-01 | `initialize` | `admin` (caller-supplied) | ~290 | ✅ | ✅ | N/A | ✅ | N/A | ✅ | Admin requires auth at initialization. Correct. |
| OR-02 | `submit_price` | `oracle` (caller-supplied) | ~330 | ✅ | ✅ | N/A | ✅ | N/A | ✅ | Oracle operator auth. Ed25519 attestation provides second factor. Dual-factor auth. |
| OR-03 | `set_challenge_window` | `admin` (caller-supplied) | ~382 | ✅* | ✅ | N/A | ✅ | ⚠️ | ✅ | Admin-only. Additionally checks `admin == cfg.admin`. Auth first. *Minor: admin is passed as argument and checked against stored value — redundant but not harmful. |
| OR-04 | `commit_price` | `oracle` (caller-supplied) | ~402 | ✅ | ✅ | N/A | ✅ | N/A | ✅ | Oracle operator auth for commit. Correct. |
| OR-05 | `challenge_price` | `challenger` (caller-supplied) | ~427 | ✅ | ✅ | N/A | ❌ | N/A | ✅ | Any address can challenge within window. Correct — open challenge model. |
| OR-06 | `reveal_price` | `oracle` (caller-supplied) | ~456 | ✅ | ✅ | N/A | ✅ | N/A | ✅ | Oracle operator auth for reveal. Verified against commitment hash and Ed25519 sig. |
| OR-07 | `rotate_key` | `admin` (caller-supplied) | ~510 | ✅ | ✅ | N/A | ✅ | ✅ | ✅ | Admin-only. Replaces the oracle's Ed25519 public key. **Highest-risk oracle operation.** Requires timelock. |
| OR-08 | `submit_aggregated_price` | `oracle` (caller-supplied) | ~537 | ✅ | ✅ | N/A | ✅ | N/A | ✅ | Oracle operator auth. Signature over aggregation payload verified. Correct. |

### 5.2 Findings

#### AUDIT-OR-01 — `rotate_key` Allows Instant Oracle Takeover
- **Severity:** Critical (Design Gap)
- **Description:** `rotate_key` replaces `Config.oracle_pubkey` immediately with no delay. A compromised admin can instantly redirect all oracle price submissions to an attacker-controlled key, enabling arbitrary price manipulation without any detection window.
- **Exploitation path:** Attacker compromises admin key → calls `rotate_key(attacker_pubkey)` → immediately starts submitting fake prices signed with attacker key → manipulates carbon credit valuations → all downstream contracts accept fraudulent prices.
- **Recommendation:** Apply timelock to `rotate_key` with minimum 24-hour delay (≈17,280 ledgers at 5s/ledger). Implement via `carbon_timelock` contract. See Section 7.

#### AUDIT-OR-02 — `set_challenge_window` Argument Pattern
- **Severity:** Informational
- **Description:** `set_challenge_window(e, admin, duration)` takes `admin` as an explicit argument and then checks `admin != cfg.admin`. This is redundant — `admin.require_auth()` already ensures the signer is `admin`; the explicit equality check is duplicate verification that cannot fail if `require_auth` passes (assuming the transaction was authorized by `cfg.admin`). However, it also means any address can pass itself as `admin` and have `require_auth()` succeed if they signed the transaction — the stored `cfg.admin` check is the real guard.
- **Risk:** Low. The stored `cfg.admin` check is the correct guard. `require_auth()` ensures the transaction was signed by the argument address; the equality check ensures that address is the stored admin.
- **Recommendation:** The pattern is correct but unusual. Consider using `require_config(e)?.admin.require_auth()` instead to avoid the redundant argument.

#### AUDIT-OR-03 — No `require_auth_for_args` on Price Submissions
- **Severity:** Informational  
- **Description:** `submit_price` and `submit_aggregated_price` do not use `require_auth_for_args`. The oracle operator's signature is validated via Ed25519 (application-level), which provides equivalent binding. The Soroban-level `require_auth` only verifies that the `oracle` address signed the transaction, not that it signed the specific price payload.
- **Risk:** Low — Ed25519 verification over the full 113-byte canonical payload provides strong binding between the authorized caller and the specific values submitted. This is stronger than `require_auth_for_args` for this use case.
- **Recommendation:** Accept as-is. Document that Ed25519 attestation serves as the argument-binding mechanism.

### 5.3 Summary Table

| Function | Auth Type | Correct | Timelock Needed |
|----------|-----------|---------|-----------------|
| `initialize` | admin | ✅ | No |
| `submit_price` | oracle + Ed25519 | ✅ | No |
| `set_challenge_window` | admin (stored check) | ✅ | No |
| `commit_price` | oracle | ✅ | No |
| `challenge_price` | challenger (any) | ✅ | No |
| `reveal_price` | oracle + Ed25519 + hash | ✅ | No |
| `rotate_key` | admin | ✅ | **Yes (critical)** |
| `submit_aggregated_price` | oracle + Ed25519 | ✅ | No |
| `get_price` | none (read-only) | ✅ | N/A |
| `get_commitment` | none (read-only) | ✅ | N/A |
| `get_aggregated_price` | none (read-only) | ✅ | N/A |
| `get_config` | none (read-only) | ✅ | N/A |

---

## 6. `require_auth_for_args` Coverage

`require_auth_for_args` binds authorization to specific argument values, preventing authorized callers from using a valid signature for one operation to authorize a different operation with different parameters. The following table assesses whether each admin operation should use it:

| Contract | Function | Should Use require_auth_for_args? | Rationale |
|----------|----------|-----------------------------------|-----------|
| `carbon_registry` | `verify_project` | No | Admin is fully trusted for all projects; no per-project binding needed. |
| `carbon_registry` | `suspend_project` | No | Same rationale. |
| `carbon_registry` | `retire_project` | **Yes (recommended)** | Irreversible — binding the admin signature to the specific `project_id` prevents replay of a retirement authorization against a different project. |
| `carbon_marketplace` | `mint_project_credits` | **Yes (recommended)** | Admin signature should be bound to the specific `(project_id, amount)` to prevent replay. |
| `carbon_marketplace` | `pause_marketplace` | No | Boolean state; no arguments to bind. |
| `carbon_oracle` | `rotate_key` | **Yes (required via timelock)** | The new pubkey should be explicitly bound in the timelock queue entry. |
| `carbon_oracle` | `set_challenge_window` | No | Low-impact configuration parameter. |

**Current state:** No contract uses `require_auth_for_args`. The highest-risk operations (`retire_project`, `mint_project_credits`, `rotate_key`) would benefit from argument binding.

---

## 7. Top-3 Highest-Risk Admin Operations Requiring Timelock

Based on irreversibility, blast radius, and financial impact:

| Rank | Contract | Function | Risk | Timelock Delay |
|------|----------|----------|------|----------------|
| 1 | `carbon_oracle` | `rotate_key` | Instant oracle takeover, arbitrary price manipulation | 24 hours (~17,280 ledgers) |
| 2 | `carbon_registry` | `retire_project` | Permanent irreversible project closure, all trading halted | 24 hours (~17,280 ledgers) |
| 3 | `carbon_marketplace` | `mint_project_credits` / `pause_marketplace` | Credit inflation or trading halt | 12 hours (~8,640 ledgers) |

See `docs/security/blast-radius-analysis.md` for full impact modeling.  
See `contracts/carbon_timelock/` for the timelock guard implementation.

---

## 8. Complete Auth Call Site Summary

| ID | Contract | Function | Checked Address | Placement | Correct | Timelock |
|----|----------|----------|----------------|-----------|---------|----------|
| REG-01 | carbon_registry | register_project | owner | First | ✅ | No |
| REG-02 | carbon_registry | verify_project | admin | First | ✅ | No |
| REG-03 | carbon_registry | suspend_project | admin | First | ✅ | No |
| REG-04 | carbon_registry | retire_project | admin | First | ✅ | **Yes** |
| REG-05 | carbon_registry | issue_credits | marketplace only | First | ⚠️ | No |
| MP-01 | carbon_marketplace | create_listing | seller | First | ✅ | No |
| MP-02 | carbon_marketplace | purchase_listing | buyer | First | ✅ | No |
| MP-03 | carbon_marketplace | cancel_listing | seller | First | ✅ | No |
| MP-04 | carbon_marketplace | mint_project_credits | admin | First (fixed) | ✅ | **Yes** |
| CC-01 | carbon_credit | mint | marketplace | First | ✅ | No |
| CC-02 | carbon_credit | transfer | from | First | ✅ | No |
| CC-03 | carbon_credit | burn | marketplace | First | ✅ | No |
| CC-04 | carbon_credit | retire | from | First | ✅ | No |
| CC-05 | carbon_credit | batch_transfer | from | First | ✅ | No |
| OR-01 | carbon_oracle | initialize | admin | First | ✅ | No |
| OR-02 | carbon_oracle | submit_price | oracle | First | ✅ | No |
| OR-03 | carbon_oracle | set_challenge_window | admin (stored) | First | ✅ | No |
| OR-04 | carbon_oracle | commit_price | oracle | First | ✅ | No |
| OR-05 | carbon_oracle | challenge_price | challenger | First | ✅ | No |
| OR-06 | carbon_oracle | reveal_price | oracle | First | ✅ | No |
| OR-07 | carbon_oracle | rotate_key | admin (stored) | First | ✅ | **Yes (critical)** |
| OR-08 | carbon_oracle | submit_aggregated_price | oracle | First | ✅ | No |

**Total call sites audited:** 22  
**Correct:** 21 (95%)  
**Warnings:** 1 (REG-05 — misleading comment/dead code, marketplace-only not admin)  
**Failures:** 0  
**Timelocks required:** 3 (REG-04, MP-04, OR-07) — implemented in `contracts/carbon_timelock/`

---

## 9. Sign-off

| Item | Status |
|------|--------|
| All require_auth call sites catalogued | ✅ |
| All require_auth_for_args call sites catalogued (none exist) | ✅ |
| Privilege escalation paths identified | ✅ |
| Top-3 admin operations identified for timelock | ✅ |
| Timelock implementation committed | ✅ |
| Blast-radius analysis committed | ✅ |

**Related documents:**  
- `docs/security/blast-radius-analysis.md` — full admin key compromise scenario  
- `contracts/carbon_timelock/src/lib.rs` — timelock guard implementation  
