# Soroban Storage Footprint Optimization and TTL/Archival Safety Audit

## Summary

Audited contracts: `carbon_registry`, `carbon_credit`, `carbon_marketplace`, and `carbon_oracle`.

All contracts now bump instance TTL whenever configuration is loaded and bump persistent TTL for entries that are read or written. Missing entries continue to return explicit `*NotFound` errors instead of silently creating replacement state. This is the intended archival-safety behavior when an entry is expired/archived and omitted from the transaction footprint.

## Footprint optimizations implemented

| Contract | Change | Before | After | Reduction |
| --- | --- | --- | --- | --- |
| `carbon_credit` | Remove zero-value balance rows after full transfer/burn/retire instead of storing `0` | one persistent `BAL` entry remains per emptied holder/project | no `BAL` entry remains | 1 persistent entry per fully emptied balance |
| `carbon_credit` | Remove per-project `TSUP` row when live supply reaches zero instead of storing `0` | one persistent `TSUP` entry remains | no `TSUP` entry remains | 1 persistent entry per fully burned/retired project supply |
| `carbon_oracle` | Cap stored aggregation source rows at 10 and reject larger submissions | unbounded caller-provided vector stored in `AGGFEED` | bounded maximum of 10 source values | prevents unbounded value growth |

These are footprint measurements at the storage-entry level because the repository does not include Soroban fee-estimation tooling and fee display is out of scope for issue #59.

## TTL policy

- Instance configuration: bumped from every config load with `INSTANCE_TTL_THRESHOLD = 30 days` and `INSTANCE_TTL_EXTEND_TO = 120 days`.
- Persistent business records: bumped after successful read/write with `PERSISTENT_TTL_THRESHOLD = 30 days` and `PERSISTENT_TTL_EXTEND_TO = 180 days`.
- Missing persistent records are not recreated implicitly. They return typed errors (`ProjectNotFound`, `ListingNotFound`, `FeedNotFound`, or `CommitmentNotFound`) so callers cannot confuse archival/missing state with zero or active state.

## Per-contract analysis

### `carbon_registry`

Persistent footprint is one project entry per registered project. The project record stores owner, short symbol name, total/issued credits, status, and vintage. No unbounded maps or vectors are stored inside a project. The project entry is required as source-of-truth state and is bumped on registration, lifecycle updates, issuance, and project reads.

### `carbon_credit`

Persistent footprint is one balance entry per non-zero `(owner, project)` balance, one live supply entry per non-zero project supply, and one retired-supply entry per project with retired credits. Full-balance transfer, burn, and retire now remove emptied balance/supply entries, preventing accumulation of zero-value rows.

### `carbon_marketplace`

Persistent footprint is one listing entry per listing. Listings remain queryable after purchase/cancel to preserve the public API, and every listing read or terminal status update bumps the listing TTL. Missing or archived listing rows return `ListingNotFound`.

### `carbon_oracle`

Persistent footprint is one price row per feed, one commitment row per feed with a commitment, and one aggregated-feed row per feed. Aggregated submissions now reject more than 10 source values to prevent unbounded value size. All feed and commitment entries are TTL-bumped on successful read/write.

## Archival edge cases tested

The test suite covers missing/removed entries as a proxy for expired or archived ledger entries not restored into the transaction footprint:

1. `carbon_registry`: reading a missing project returns `ProjectNotFound`.
2. `carbon_marketplace`: missing listing rows return `ListingNotFound` instead of being recreated.
3. `carbon_oracle`: reading missing feeds/commitments returns `FeedNotFound`/`CommitmentNotFound`.
4. `carbon_credit`: full transfer/burn/retire removes zero rows while public balance/supply queries still safely return `0`.
