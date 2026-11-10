# StellarKraal Protocol Glossary

A comprehensive reference of domain-specific terminology, financial mechanics, decentralized finance (DeFi) invariants, and Stellar/Soroban platform primitives used across the StellarKraal livestock-backed lending protocol.

---

## Table of Contents

1. [DeFi & Lending Mechanics](#1-defi--lending-mechanics)
   - [LTV (Loan-to-Value)](#ltv-loan-to-value)
   - [Health Factor (HF)](#health-factor-hf)
   - [Liquidation](#liquidation)
   - [Liquidation Threshold](#liquidation-threshold)
   - [Liquidation Bonus / Penalty](#liquidation-bonus--penalty)
   - [Collateral](#collateral)
   - [Collateral Valuation](#collateral-valuation)
   - [Basis Points (bps)](#basis-points-bps)
   - [Loan Principal](#loan-principal)
   - [Accrued Interest](#accrued-interest)
   - [Close Factor](#close-factor)
2. [Stellar & Soroban Blockchain Primitives](#2-stellar--soroban-blockchain-primitives)
   - [Soroban](#soroban)
   - [Stroops](#stroops)
   - [Persistent Storage](#persistent-storage)
   - [Instance Storage](#instance-storage)
   - [Temporary Storage](#temporary-storage)
   - [Time-To-Live (TTL)](#time-to-live-ttl)
   - [Contract Authorization (require_auth)](#contract-authorization-require_auth)
3. [Oracle & Market Infrastructure](#3-oracle--market-infrastructure)
   - [Oracle](#oracle)
   - [Median Aggregation](#median-aggregation)
   - [Heartbeat & Staleness Window](#heartbeat--staleness-window)
   - [Livestock Appraisal Registry](#livestock-appraisal-registry)

---

## 1. DeFi & Lending Mechanics

### LTV (Loan-to-Value)
**Definition**: The ratio of the total borrowed debt value relative to the appraised market value of the pledged livestock collateral, expressed in basis points or as a percentage. In StellarKraal, the maximum origination LTV determines the credit ceiling borrowable against verified livestock, ensuring a conservative buffer against asset market volatility. If price fluctuations or accumulated interest cause the position to exceed safe ratios, risk mitigation controls engage automatically.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [`docs/security/COLLATERAL_VALUATION.md`](security/COLLATERAL_VALUATION.md)

### Health Factor (HF)
**Definition**: A dynamic numerical safety metric representing the solvency condition of an active borrower position. The health factor is computed by multiplying the total collateral value by the liquidation threshold and dividing by total outstanding debt. Positions with an HF strictly greater than 1.0 (or 10,000 bps) are solvent; any drop below 1.0 marks the position as eligible for partial or full liquidation to protect protocol solvency.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [`docs/security/CIRCUIT_BREAKER.md`](security/CIRCUIT_BREAKER.md)

### Liquidation
**Definition**: The automated or permissionless process where an undercollateralized borrow position (Health Factor < 1.0) is partially or fully repaid by third-party liquidators in exchange for seized livestock collateral. Liquidators repay the underlying loan asset and receive collateral at a discount, re-stabilizing the protocol's systemic solvency. This mechanism prevents the protocol from accumulating bad debt during sharp downturns in livestock market valuation.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [`docs/security/COLLATERAL_VALUATION.md`](security/COLLATERAL_VALUATION.md)

### Liquidation Threshold
**Definition**: The predetermined maximum LTV percentage beyond which a loan position is categorized as dangerously undercollateralized and becomes open to liquidation. Typically configured several percentage points above the initial loan origination LTV, this buffer gives borrowers adequate time to top up livestock collateral or repay principal before incurring liquidation penalties.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

### Liquidation Bonus / Penalty
**Definition**: An economic incentive awarded to liquidators who service distressed loans by purchasing liquidated livestock collateral below fair market valuation. The bonus is financed by deducting a penalty fee from the liquidated borrower's residual collateral equity. This fee structure guarantees that liquidators remain economically motivated to execute timely interventions even during adverse network conditions.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

### Collateral
**Definition**: Verifiable on-chain assets or tokenized representations of physical livestock pledged by a borrower to secure a debt obligation. In StellarKraal, livestock credentials record biological markers, ownership history, veterinary inspection logs, and verified appraisal records on the Stellar network. Pledged collateral is locked in Soroban smart contract custody until the corresponding debt is settled or liquidated.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [`docs/security/COLLATERAL_VALUATION.md`](security/COLLATERAL_VALUATION.md)

### Collateral Valuation
**Definition**: The process of determining the real-time monetary value of pledged livestock collateral by querying off-chain appraisal data and on-chain price feeds. Appraisals consider breed standards, live weight, age, health certifications, and prevailing local agricultural market prices. Validated valuations are committed on-chain through authorized oracle reports before any lending or liquidation calculation executes.  
**Related Code & Docs**: [`docs/security/COLLATERAL_VALUATION.md`](security/COLLATERAL_VALUATION.md) · [`contracts/carbon_oracle`](../contracts/carbon_oracle)

### Basis Points (bps)
**Definition**: A standard unit of measure for percentages in financial contracts, where 1 basis point equals one-hundredth of a percentage point (0.01% or 0.0001). Using basis points allows Soroban smart contracts to perform high-precision interest rate calculations, fee deductions, and LTV boundaries using fixed-point integer arithmetic without floating-point rounding errors. For example, an 80% LTV is represented on-chain as 8,000 bps.  
**Related Code & Docs**: [`contracts/arithmetic`](../contracts/arithmetic) · [`contracts/stellarkraal`](../contracts/stellarkraal)

### Loan Principal
**Definition**: The baseline nominal amount of stablecoins or native Stellar currency transferred to a borrower upon loan initialization. Repayment of the loan principal decreases total protocol debt exposure and restores borrowing capacity for the associated livestock collateral.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

### Accrued Interest
**Definition**: The incremental financing charge calculated continuously or per ledger block based on the active borrowing rate and elapsed duration since loan origination. Accrued interest is compounded into total outstanding debt and must be paid alongside the principal to fully release pledged collateral.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [`contracts/arithmetic`](../contracts/arithmetic)

### Close Factor
**Definition**: The maximum fraction of an undercollateralized loan's debt that a liquidator is permitted to service within a single transaction. Enforcing a close factor prevents over-liquidation of borrower positions, preserving as much residual borrower livestock equity as possible while restoring position solvency.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

---

## 2. Stellar & Soroban Blockchain Primitives

### Soroban
**Definition**: The native WebAssembly (WASM)-based smart contracts platform integrated directly into the Stellar blockchain. Soroban provides Turing-complete smart contract capabilities, deterministic state execution, sandboxed memory boundaries, and low transaction fees. StellarKraal contracts leverage Soroban to implement decentralized lending rules, automated liquidations, and oracle price aggregation.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [Soroban Documentation](https://soroban.stellar.org)

### Stroops
**Definition**: The smallest fractional unit of the native Stellar Lumens (XLM) asset, equal to one ten-millionth of an XLM (0.0000001 XLM or 10⁻⁷ XLM). All financial transfers, transaction fees, and ledger calculations within Stellar protocol layers operate in integer stroops to eliminate floating-point precision hazards.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

### Persistent Storage
**Definition**: A Soroban storage tier designed for durable contract state—such as active loan records, user collateral balances, and historical ownership registries—that remains retrievable indefinitely provided its Time-To-Live (TTL) is regularly extended. In StellarKraal, all mission-critical debt state and registered collateral metadata reside in persistent storage to prevent state expiration.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [State Archival Docs](https://soroban.stellar.org/docs/fundamentals-and-concepts/state-archival)

### Instance Storage
**Definition**: A Soroban storage domain that shares its Time-To-Live directly with the deployed smart contract instance itself. Instance storage is ideal for protocol-wide configuration parameters, global admin keys, oracle router addresses, and baseline interest rate constants that are always accessed when calling contract entrypoints.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

### Temporary Storage
**Definition**: A cost-efficient Soroban storage tier suitable for ephemeral data that does not need to persist across long time horizons, such as one-time reentrancy flags, transient calculation caches, or short-lived challenge commitments. Temporary storage does not survive archival once its ledger TTL expires, substantially reducing long-term state rent fees.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

### Time-To-Live (TTL)
**Definition**: A ledger-enforced lifetime counter measured in ledger sequences that dictates how long stored contract entries remain live in active memory before being archived. StellarKraal functions implement automated TTL bumping (`extend_ttl`) to ensure borrower loans, collateral registries, and configuration keys remain permanently active and accessible without risk of sudden archival.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal) · [State Archival Concepts](https://soroban.stellar.org/docs/fundamentals-and-concepts/state-archival)

### Contract Authorization (require_auth)
**Definition**: Soroban's cryptographic authorization framework allowing accounts and smart contracts to verify caller permissions explicitly before executing privileged state mutations. In StellarKraal, functions such as loan disbursement, collateral withdrawal, and administrative interest adjustments mandate cryptographic caller signatures via `require_auth` to guard against unauthorized access.  
**Related Code & Docs**: [`contracts/stellarkraal`](../contracts/stellarkraal)

---

## 3. Oracle & Market Infrastructure

### Oracle
**Definition**: An off-chain to on-chain data bridge that feeds external real-world market prices and certified livestock appraisal valuations into Soroban smart contracts. StellarKraal oracles verify digital signatures from accredited agricultural appraisers and price reporting nodes before committing valuation snapshots to contract storage.  
**Related Code & Docs**: [`contracts/carbon_oracle`](../contracts/carbon_oracle) · [`docs/oracle/DATA_FLOW.md`](oracle/DATA_FLOW.md)

### Median Aggregation
**Definition**: A mathematical consensus technique that sorts multiple independent price reports submitted by oracle nodes and selects the middle value as the protocol reference price. Median aggregation renders the valuation feed resilient against extreme outlier reports, malfunctioning telemetry nodes, and adversarial price manipulation.  
**Related Code & Docs**: [`contracts/carbon_oracle`](../contracts/carbon_oracle) · [`contracts/arithmetic`](../contracts/arithmetic)

### Heartbeat & Staleness Window
**Definition**: The maximum permitted elapsed time between consecutive oracle price updates before on-chain feeds are flagged as stale. If an oracle feed exceeds the staleness window without publishing a fresh heartbeat, StellarKraal halts new loan disbursements and enters safe liquidation modes to prevent pricing on out-of-date market information.  
**Related Code & Docs**: [`contracts/carbon_oracle`](../contracts/carbon_oracle) · [`docs/security/CIRCUIT_BREAKER.md`](security/CIRCUIT_BREAKER.md)

### Livestock Appraisal Registry
**Definition**: The verified on-chain data registry that tracks livestock certification credentials, national ear-tag identification numbers, veterinary health attestations, and certified appraisal timestamps. This registry serves as the single source of truth for authenticating physical collateral assets backing StellarKraal loans.  
**Related Code & Docs**: [`contracts/carbon_registry`](../contracts/carbon_registry) · [`docs/security/COLLATERAL_VALUATION.md`](security/COLLATERAL_VALUATION.md)
