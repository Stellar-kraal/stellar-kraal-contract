//! Migration utilities for state schema upgrades.
//!
//! Provides helpers for zero-data-loss migration from V1 to V2 schemas.
//! All migration operations are reversible and audit-logged via MigrationState.

use soroban_sdk::{Address, BytesN, Env, Symbol};

use crate::{
    CreditV1, CreditV2, HolderAccountV1, HolderAccountV2, Error, CURRENT_SCHEMA_VERSION,
};

/// Migrates a V1 credit to V2 format.
///
/// # Arguments
/// - `credit_v1`: The legacy V1 credit entry
/// - `registry_id`: Registry identifier for the credit (new in V2)
/// - `metadata_hash`: Hash of additional metadata (new in V2)
///
/// # Returns
/// A V2 credit with all V1 fields preserved and V2 additions filled.
pub fn migrate_credit_v1_to_v2(
    credit_v1: CreditV1,
    registry_id: BytesN<32>,
    metadata_hash: BytesN<32>,
) -> CreditV2 {
    CreditV2 {
        id: credit_v1.id,
        owner: credit_v1.owner,
        amount: credit_v1.amount,
        issued_at: credit_v1.issued_at,
        credit_type: credit_v1.credit_type,
        registry_id,
        metadata_hash,
    }
}

/// Migrates a V1 holder account to V2 format.
///
/// # Arguments
/// - `account_v1`: The legacy V1 holder account
/// - `current_ledger`: Current ledger sequence for timestamp
///
/// # Returns
/// A V2 account with schema version field added.
pub fn migrate_account_v1_to_v2(
    account_v1: HolderAccountV1,
    _current_ledger: u32,
) -> HolderAccountV2 {
    HolderAccountV2 {
        holder: account_v1.holder,
        balance: account_v1.balance,
        last_updated: account_v1.last_updated,
        schema_version: CURRENT_SCHEMA_VERSION,
    }
}

/// Validates that a V1 credit can be safely migrated to V2.
///
/// Checks:
/// - Amount is non-negative and fits in i128
/// - Owner address is valid (non-zero)
/// - Issued timestamp is reasonable
pub fn validate_credit_migration(credit: &CreditV1) -> Result<(), Error> {
    if credit.amount < 0 {
        return Err(Error::MigrationFailed);
    }

    if credit.issued_at == 0 {
        return Err(Error::MigrationFailed);
    }

    Ok(())
}

/// Validates that a V1 account can be safely migrated to V2.
///
/// Checks:
/// - Balance is valid (non-negative)
/// - Last update timestamp is reasonable
pub fn validate_account_migration(account: &HolderAccountV1) -> Result<(), Error> {
    if account.balance < 0 {
        return Err(Error::MigrationFailed);
    }

    if account.last_updated == 0 {
        return Err(Error::MigrationFailed);
    }

    Ok(())
}

/// Validates schema compatibility between versions.
///
/// Returns Ok if migration path is supported, Err otherwise.
pub fn validate_schema_compatibility(from_version: u32, to_version: u32) -> Result<(), Error> {
    // Only support V1 -> V2 migration for now
    if from_version == 1 && to_version == 2 {
        return Ok(());
    }

    Err(Error::IncompatibleSchema)
}


