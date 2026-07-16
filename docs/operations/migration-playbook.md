# Carbon Registry Migration Playbook

## Overview

This document describes the zero-data-loss state migration process for upgrading the `carbon_registry` contract from v1 to v2+ schema versions.

**Key Properties**:
- ✅ Zero data loss: all v1 state preserved during migration
- ✅ Reversible: migration state tracked with rollback checkpoint
- ✅ Auditable: MigrationState provides complete provenance
- ✅ Safe: schema compatibility validated before any data modifications

## Schema Versions

### V1 (Legacy)
- `CreditV1`: Basic credit record (id, owner, amount, issued_at, credit_type)
- `HolderAccountV1`: Holder balance (holder, balance, last_updated)
- No registry tracking or metadata hashing

### V2 (Current)
- `CreditV2`: Enhanced credit with registry tracking and metadata hashing
  - ✨ New: `registry_id` — identifies which registry issued the credit
  - ✨ New: `metadata_hash` — SHA-256 of credit metadata
- `HolderAccountV2`: Accounts now track schema version
  - ✨ New: `schema_version` — enables future migrations
- `AggregationMetadata`: Provenance tracking for audits

## Pre-Migration Checklist

- [ ] Backup all persistent storage (ledger snapshots)
- [ ] Verify all v1 credits are valid via `validate_credit_migration()`
- [ ] Verify all v1 accounts are valid via `validate_account_migration()`
- [ ] Generate registry_id and metadata_hash for all credits
- [ ] Prepare migration fixture with complete v1 snapshot
- [ ] Run integration test suite against test fixture
- [ ] Obtain admin approval and multi-sig authorization

## Migration Steps

### Phase 1: Preparation

```bash
# 1. Export v1 state snapshot
soroban-cli export-contract-state \
  --contract carbon_registry \
  --network testnet \
  --output v1-snapshot.json

# 2. Generate registry mappings (off-chain)
python scripts/generate_registry_mappings.py \
  --input v1-snapshot.json \
  --output registry-mappings.json

# 3. Validate migration compatibility
python scripts/validate_migration.py \
  --v1-snapshot v1-snapshot.json \
  --registry-mappings registry-mappings.json
```

### Phase 2: Begin Migration (On-Chain)

```bash
# Call admin-only entry point to create migration checkpoint
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --source admin-key \
  --method begin_migration \
  --arg admin:address={ADMIN_ADDRESS}
```

This creates a `MigrationState` entry:
- `from_version: 1`
- `to_version: 2`
- `status: "in_progress"`
- `started_at: <current_ledger>`

### Phase 3: Migrate Data

**For each v1 credit:**

```bash
# 1. Read v1 credit from storage
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --method get_price \
  --arg feed_id:bytes={CREDIT_ID}

# 2. Migrate to v2 (off-chain)
credit_v2 = migrate_credit_v1_to_v2(
    credit_v1,
    registry_id,
    metadata_hash
)

# 3. Write v2 credit (overwrites v1 in persistent storage)
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --source admin-key \
  --method register_credit \
  --arg admin:address={ADMIN_ADDRESS} \
  --arg id:bytes={CREDIT_ID} \
  --arg owner:address={OWNER} \
  --arg amount:i128={AMOUNT} \
  --arg credit_type:symbol={TYPE} \
  --arg registry_id:bytes={REGISTRY_ID} \
  --arg metadata_hash:bytes={METADATA_HASH}
```

**For each v1 account:**

```bash
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --source admin-key \
  --method update_balance \
  --arg admin:address={ADMIN_ADDRESS} \
  --arg holder:address={HOLDER} \
  --arg new_balance:i128={NEW_BALANCE}
```

### Phase 4: Finalize Migration (On-Chain)

```bash
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --source admin-key \
  --method finalize_migration \
  --arg admin:address={ADMIN_ADDRESS}
```

This:
- Updates `Config.schema_version` to 2
- Marks `MigrationState.status` as "completed"
- Records `completed_at` timestamp

## Rollback Procedure

If issues are detected during migration:

```bash
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --source admin-key \
  --method rollback_migration \
  --arg admin:address={ADMIN_ADDRESS}
```

This:
- Marks `MigrationState.status` as "rolled_back"
- Does **NOT** delete any data
- Allows retry or alternate migration path
- Maintains complete audit trail

**Recovery:**
1. Analyze the rollback reason
2. Fix data issues if needed
3. Call `begin_migration()` again to restart
4. Re-run migration steps for remaining data

## Monitoring Migration Progress

```bash
# Check migration state
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --method get_migration_state

# Check schema version
soroban contract invoke \
  --network testnet \
  --contract carbon_registry \
  --method get_schema_version
```

Expected output after successful migration:
```json
{
  "schema_version": 2,
  "migration_state": {
    "from_version": 1,
    "to_version": 2,
    "migrated_count": <total_credits>,
    "failed_count": 0,
    "status": "completed",
    "completed_at": <ledger_sequence>
  }
}
```

## Post-Migration Verification

- [ ] Verify `get_schema_version()` returns 2
- [ ] Sample v2 credits with `get_credit(id)` — confirm registry_id and metadata_hash present
- [ ] Sample v2 accounts with `get_balance(holder)` — balances match v1
- [ ] Attempt to register new credit — must work with v2 schema
- [ ] Verify v1 entry points reject with InvalidSchemaVersion

## Troubleshooting

### Issue: Migration fails midway

**Symptom**: Some credits migrated, some not; `status: "in_progress"`

**Resolution**:
1. Call `rollback_migration()` to mark state
2. Identify which credits failed (check on-chain)
3. Fix off-chain migration logic if needed
4. Call `begin_migration()` again
5. Resume from first failed credit

### Issue: Schema version mismatch

**Symptom**: Contract rejects new operations with `InvalidSchemaVersion`

**Resolution**:
1. Check `get_schema_version()` — should be 2
2. If still 1, migration was not finalized
3. Call `finalize_migration()` to update version

### Issue: Data integrity concern

**Symptom**: Off-chain validation detected anomalies

**Resolution**:
1. Do not proceed to `finalize_migration()`
2. Call `rollback_migration()` immediately
3. Audit v1 and migrated data
4. Fix root cause
5. Start over from `begin_migration()`

## Automatic CI Testing

A CI pipeline test validates migrations pre-deployment:

```bash
# In .github/workflows/migration-test.yml
- name: Run migration integration test
  run: |
    cargo test --release \
      --package carbon_registry \
      --test migration_integration \
      --features testutils
```

This test:
1. Deploys v2 contract
2. Loads v1 fixture snapshot
3. Runs full migration pipeline
4. Verifies all data preserved
5. Confirms schema version updated

## Key Invariants

1. **All v1 data readable throughout migration**: No destructive deletes
2. **Schema version guards entry points**: V1-only operations rejected after migration
3. **Migration state immutable once started**: Prevents concurrent migrations
4. **Audit trail complete**: Every action logged in MigrationState
5. **Rollback always possible**: Until finalize_migration() succeeds

## Design Rationale

### Why envelope pattern?

The `Config` struct includes `schema_version` so every entry point can validate compatibility. This is more explicit than separate version storage and easier to reason about.

### Why MigrationState?

Provides:
- Rollback checkpoint (resume from failure)
- Audit trail (when/who/what migrated)
- Monitoring (progress tracking)
- Prevents concurrent migrations

### Why separate storage keys (FEED vs AGGFEED)?

Allows both single-source and aggregated feeds coexist during transition. Gradual migration reduces risk.

## Future Migrations (V2 → V3+)

Follow the same pattern:
1. Add new storage keys for v3+ data
2. Implement new `migrate_*_v2_to_v3()` functions
3. Extend validation logic
4. Update CI tests

The framework is designed to be reusable for all future schema versions.
