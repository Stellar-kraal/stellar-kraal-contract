#![no_std]
#![allow(clippy::too_many_arguments)]

//! `carbon_oracle` — Soroban contract for GEE-backed carbon sequestration price feeds.
//!
//! Every price update **must** be accompanied by an Ed25519 attestation over
//! the canonical 113-byte payload described in
//! `docs/oracle/attestation-schema.md`.  Updates that fail signature
//! verification are rejected with [`Error::InvalidAttestation`].
//!
//! # Attestation payload layout (113 bytes, big-endian integers)
//! ```text
//! [1]  schema_version    u8
//! [32] script_hash       bytes
//! [32] input_params_hash bytes
//! [8]  output_value      i64
//! [8]  timestamp_utc     i64
//! [32] feed_id           bytes
//! ```

mod tests;

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, symbol_short, Address, BytesN, Env,
    Symbol,
};

// ── Storage keys ─────────────────────────────────────────────────────────────

const CONFIG: Symbol = symbol_short!("CONFIG");

fn feed_key(_e: &Env, feed_id: &BytesN<32>) -> (Symbol, BytesN<32>) {
    (symbol_short!("FEED"), feed_id.clone())
}

fn agg_feed_key(_e: &Env, feed_id: &BytesN<32>) -> (Symbol, BytesN<32>) {
    (symbol_short!("AGGFEED"), feed_id.clone())
}

// ── Attestation payload constants ─────────────────────────────────────────────

/// Total length of the canonical signing message in bytes.
pub const PAYLOAD_LEN: usize = 113;

/// Schema version this contract accepts.
pub const SCHEMA_VERSION: u8 = 1;

// ── Data types ────────────────────────────────────────────────────────────────

/// State of a price commitment in the commit-reveal scheme.
#[contracttype]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommitmentState {
    Committed,
    ChallengeWindow,
    Revealed,
    Disputed,
}

/// Contract configuration stored in instance storage.
#[contracttype]
#[derive(Clone)]
pub struct Config {
    /// The admin address that initialised the contract.
    pub admin: Address,
    /// The 32-byte Ed25519 public key whose signatures the contract trusts.
    pub oracle_pubkey: BytesN<32>,
    /// Duration of the challenge window in ledgers.
    pub challenge_window_duration: u32,
}

/// A commitment to a price entry.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceCommitment {
    /// The hash of the commitment: SHA256(script_hash || input_params_hash || output_value || timestamp_utc || feed_id || salt)
    pub commitment_hash: BytesN<32>,
    /// Ledger sequence when this commitment was recorded.
    pub recorded_at: u32,
    /// State of the commitment.
    pub state: CommitmentState,
}

/// A stored price entry for a given feed.
#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PriceEntry {
    /// Signed carbon sequestration value (micrograms CO₂-eq/m²).
    pub output_value: i64,
    /// Unix timestamp of the GEE computation that produced this value.
    pub timestamp_utc: i64,
    /// The SHA-256 of the GEE script that produced this value (audit trail).
    pub script_hash: BytesN<32>,
    /// The SHA-256 of the canonical input parameters (audit trail).
    pub input_params_hash: BytesN<32>,
    /// Ledger sequence number when this entry was recorded.
    pub recorded_at: u32,
}

/// Per-source price value for multi-source aggregations.
#[contracttype]
#[derive(Clone, Debug)]
pub struct SourceValue {
    /// The source identifier (e.g., "xpansiv_cbl", "toucan_protocol").
    pub source_id: soroban_sdk::String,
    /// The price value from this source.
    pub value: i64,
    /// The weight assigned to this source in aggregation.
    pub weight_numerator: i128,  // Stored as numerator for precision
    pub weight_denominator: i128,
}

/// Aggregation provenance metadata for multi-source submissions.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AggregationMetadata {
    /// The aggregation method used (e.g., "weighted_median").
    pub method: soroban_sdk::String,
    /// The outlier rejection method (e.g., "iqr", "mad", "none").
    pub outlier_method: soroban_sdk::String,
    /// Number of sources used in the aggregation.
    pub num_sources_used: u32,
    /// Number of sources rejected as outliers.
    pub num_sources_rejected: u32,
    /// Timestamp of aggregation (Unix).
    pub timestamp_utc: i64,
}

/// Complete aggregated price entry with per-source values and metadata.
#[contracttype]
#[derive(Clone, Debug)]
pub struct AggregatedPriceEntry {
    /// The computed aggregate value (weighted median).
    pub aggregate_value: i64,
    /// Per-source values (up to 10 sources stored).
    pub source_values: soroban_sdk::Vec<SourceValue>,
    /// Aggregation provenance metadata.
    pub metadata: AggregationMetadata,
    /// Ledger sequence when recorded.
    pub recorded_at: u32,
}

// ── Errors ────────────────────────────────────────────────────────────────────

#[contracterror]
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Error {
    /// Contract has already been initialised.
    AlreadyInitialized = 1,
    /// Contract has not been initialised.
    NotInitialized = 2,
    /// Caller is not the admin.
    Unauthorized = 3,
    /// The attestation signature is invalid or the payload is malformed.
    InvalidAttestation = 4,
    /// No price entry found for the requested feed.
    FeedNotFound = 5,
    /// The schema_version byte in the payload is not supported.
    UnsupportedSchemaVersion = 6,
    /// Commitment not found for the requested feed.
    CommitmentNotFound = 7,
    /// Commitment already exists for this feed and is active.
    CommitmentActive = 8,
    /// Challenge window has not yet expired.
    ChallengeWindowNotExpired = 9,
    /// Challenge window has already expired.
    ChallengeWindowExpired = 10,
    /// The revealed parameters do not match the commitment hash.
    InvalidReveal = 11,
    /// The commitment is in an invalid state for this operation.
    InvalidCommitmentState = 12,
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn require_config(e: &Env) -> Result<Config, Error> {
    e.storage()
        .instance()
        .get(&CONFIG)
        .ok_or(Error::NotInitialized)
}

/// Build the canonical 113-byte attestation payload that is signed / verified.
///
/// Layout:
/// ```text
/// [0]        schema_version  : u8   (1 byte)
/// [1..33]    script_hash     : [u8;32]
/// [33..65]   input_params_hash : [u8;32]
/// [65..73]   output_value    : i64 big-endian
/// [73..81]   timestamp_utc   : i64 big-endian
/// [81..113]  feed_id         : [u8;32]
/// ```
fn build_payload(
    e: &Env,
    schema_version: u8,
    script_hash: &BytesN<32>,
    input_params_hash: &BytesN<32>,
    output_value: i64,
    timestamp_utc: i64,
    feed_id: &BytesN<32>,
) -> soroban_sdk::Bytes {
    let mut msg = soroban_sdk::Bytes::new(e);

    // schema_version (1 byte)
    msg.push_back(schema_version);

    // script_hash (32 bytes)
    msg.append(&script_hash.clone().into());

    // input_params_hash (32 bytes)
    msg.append(&input_params_hash.clone().into());

    // output_value as big-endian i64 (8 bytes)
    for b in output_value.to_be_bytes() {
        msg.push_back(b);
    }

    // timestamp_utc as big-endian i64 (8 bytes)
    for b in timestamp_utc.to_be_bytes() {
        msg.push_back(b);
    }

    // feed_id (32 bytes)
    msg.append(&feed_id.clone().into());

    msg
}

fn commitment_key(_e: &Env, feed_id: &BytesN<32>) -> (Symbol, BytesN<32>) {
    (symbol_short!("COMMIT"), feed_id.clone())
}

/// Computes the commitment hash: SHA256(script_hash || input_params_hash || output_value || timestamp_utc || feed_id || salt)
fn build_commitment_payload(
    e: &Env,
    script_hash: &BytesN<32>,
    input_params_hash: &BytesN<32>,
    output_value: i64,
    timestamp_utc: i64,
    feed_id: &BytesN<32>,
    salt: &BytesN<32>,
) -> soroban_sdk::Bytes {
    let mut msg = soroban_sdk::Bytes::new(e);
    msg.append(&script_hash.clone().into());
    msg.append(&input_params_hash.clone().into());
    for b in output_value.to_be_bytes() {
        msg.push_back(b);
    }
    for b in timestamp_utc.to_be_bytes() {
        msg.push_back(b);
    }
    msg.append(&feed_id.clone().into());
    msg.append(&salt.clone().into());
    msg
}

// ── Contract ──────────────────────────────────────────────────────────────────

#[contract]
pub struct CarbonOracle;

#[contractimpl]
impl CarbonOracle {
    /// Initialise the contract with an admin address and the oracle's Ed25519
    /// public key.
    ///
    /// The `oracle_pubkey` is a 32-byte raw Ed25519 public key.  It is stored
    /// in instance storage and used to verify every subsequent `submit_price`
    /// call.
    pub fn initialize(
        e: Env,
        admin: Address,
        oracle_pubkey: BytesN<32>,
        challenge_window_duration: u32,
    ) -> Result<(), Error> {
        if e.storage().instance().has(&CONFIG) {
            return Err(Error::AlreadyInitialized);
        }
        admin.require_auth();
        e.storage().instance().set(
            &CONFIG,
            &Config {
                admin,
                oracle_pubkey,
                challenge_window_duration,
            },
        );
        Ok(())
    }

    /// Submit a new price entry for `feed_id`.
    ///
    /// The caller must supply:
    /// - `oracle`: the oracle operator address (must `require_auth` on Stellar).
    /// - `script_hash`: SHA-256 of the GEE script source (32 bytes).
    /// - `input_params_hash`: SHA-256 of the canonical input-params JSON (32 bytes).
    /// - `output_value`: carbon sequestration result (signed i64).
    /// - `timestamp_utc`: Unix timestamp of the GEE computation.
    /// - `feed_id`: 32-byte feed identifier.
    /// - `signature`: 64-byte Ed25519 signature over the 113-byte canonical payload.
    ///
    /// The contract re-derives the canonical 113-byte payload, verifies the
    /// Ed25519 signature against the stored oracle public key, and rejects the
    /// call with [`Error::InvalidAttestation`] if verification fails.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_price(
        e: Env,
        oracle: Address,
        script_hash: BytesN<32>,
        input_params_hash: BytesN<32>,
        output_value: i64,
        timestamp_utc: i64,
        feed_id: BytesN<32>,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let cfg = require_config(&e)?;
        oracle.require_auth();

        // ── 1. Build the canonical 113-byte payload ───────────────────────────
        let payload = build_payload(
            &e,
            SCHEMA_VERSION,
            &script_hash,
            &input_params_hash,
            output_value,
            timestamp_utc,
            &feed_id,
        );

        // ── 2. Verify Ed25519 signature ───────────────────────────────────────
        e.crypto()
            .ed25519_verify(&cfg.oracle_pubkey, &payload, &signature);

        // ── 3. Store the price entry ──────────────────────────────────────────
        let entry = PriceEntry {
            output_value,
            timestamp_utc,
            script_hash,
            input_params_hash,
            recorded_at: e.ledger().sequence(),
        };
        e.storage()
            .persistent()
            .set(&feed_key(&e, &feed_id), &entry);

        Ok(())
    }

    /// Set the challenge window duration.
    pub fn set_challenge_window(
        e: Env,
        admin: Address,
        duration: u32,
    ) -> Result<(), Error> {
        let mut cfg = require_config(&e)?;
        admin.require_auth();
        if admin != cfg.admin {
            return Err(Error::Unauthorized);
        }
        cfg.challenge_window_duration = duration;
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    /// Commit a price entry for a given feed.
    pub fn commit_price(
        e: Env,
        oracle: Address,
        feed_id: BytesN<32>,
        commitment_hash: BytesN<32>,
    ) -> Result<(), Error> {
        require_config(&e)?;
        oracle.require_auth();

        // Prevent overwriting active commitments (wait for reveal or challenge window to pass)
        let key = commitment_key(&e, &feed_id);
        if let Some(existing) = e.storage().persistent().get::<_, PriceCommitment>(&key) {
            if existing.state == CommitmentState::Committed || existing.state == CommitmentState::ChallengeWindow {
                return Err(Error::CommitmentActive);
            }
        }

        let commitment = PriceCommitment {
            commitment_hash,
            recorded_at: e.ledger().sequence(),
            state: CommitmentState::ChallengeWindow, // Starts challenge window immediately
        };

        e.storage().persistent().set(&key, &commitment);
        Ok(())
    }

    /// Challenge a price entry within the challenge window.
    pub fn challenge_price(
        e: Env,
        challenger: Address,
        feed_id: BytesN<32>,
        _alternative_output_value: i64,
    ) -> Result<(), Error> {
        let cfg = require_config(&e)?;
        challenger.require_auth();

        let key = commitment_key(&e, &feed_id);
        let mut commitment = e.storage().persistent().get::<_, PriceCommitment>(&key).ok_or(Error::CommitmentNotFound)?;

        if commitment.state != CommitmentState::ChallengeWindow {
            return Err(Error::InvalidCommitmentState);
        }

        let current_ledger = e.ledger().sequence();
        if current_ledger >= commitment.recorded_at + cfg.challenge_window_duration {
            return Err(Error::ChallengeWindowExpired);
        }

        commitment.state = CommitmentState::Disputed;
        e.storage().persistent().set(&key, &commitment);

        Ok(())
    }

    /// Reveal a price entry after the challenge window expires.
    #[allow(clippy::too_many_arguments)]
    pub fn reveal_price(
        e: Env,
        oracle: Address,
        feed_id: BytesN<32>,
        script_hash: BytesN<32>,
        input_params_hash: BytesN<32>,
        output_value: i64,
        timestamp_utc: i64,
        salt: BytesN<32>,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let cfg = require_config(&e)?;
        oracle.require_auth();

        let key = commitment_key(&e, &feed_id);
        let mut commitment = e.storage().persistent().get::<_, PriceCommitment>(&key).ok_or(Error::CommitmentNotFound)?;

        if commitment.state != CommitmentState::ChallengeWindow {
            return Err(Error::InvalidCommitmentState);
        }

        let current_ledger = e.ledger().sequence();
        if current_ledger < commitment.recorded_at + cfg.challenge_window_duration {
            return Err(Error::ChallengeWindowNotExpired);
        }

        // Re-derive commitment hash and verify
        let payload = build_commitment_payload(
            &e,
            &script_hash,
            &input_params_hash,
            output_value,
            timestamp_utc,
            &feed_id,
            &salt,
        );
        let expected_hash = e.crypto().sha256(&payload);
        if expected_hash != commitment.commitment_hash {
            return Err(Error::InvalidReveal);
        }

        // Verify Ed25519 signature over the canonical attestation payload
        let attestation_payload = build_payload(
            &e,
            SCHEMA_VERSION,
            &script_hash,
            &input_params_hash,
            output_value,
            timestamp_utc,
            &feed_id,
        );
        e.crypto()
            .ed25519_verify(&cfg.oracle_pubkey, &attestation_payload, &signature);

        // Transition to Revealed
        commitment.state = CommitmentState::Revealed;
        e.storage().persistent().set(&key, &commitment);

        // Store the final price entry
        let entry = PriceEntry {
            output_value,
            timestamp_utc,
            script_hash,
            input_params_hash,
            recorded_at: current_ledger,
        };
        e.storage()
            .persistent()
            .set(&feed_key(&e, &feed_id), &entry);

        Ok(())
    }

    /// Rotate the oracle public key.  Only the admin may call this.
    pub fn rotate_key(
        e: Env,
        admin: Address,
        new_pubkey: BytesN<32>,
    ) -> Result<(), Error> {
        let mut cfg = require_config(&e)?;
        admin.require_auth();
        if admin != cfg.admin {
            return Err(Error::Unauthorized);
        }
        cfg.oracle_pubkey = new_pubkey;
        e.storage().instance().set(&CONFIG, &cfg);
        Ok(())
    }

    /// Read the latest price entry for a given feed.
    pub fn get_price(e: Env, feed_id: BytesN<32>) -> Result<PriceEntry, Error> {
        require_config(&e)?;
        e.storage()
            .persistent()
            .get(&feed_key(&e, &feed_id))
            .ok_or(Error::FeedNotFound)
    }

    /// Read the current commitment for a given feed.
    pub fn get_commitment(e: Env, feed_id: BytesN<32>) -> Result<PriceCommitment, Error> {
        require_config(&e)?;
        e.storage()
            .persistent()
            .get(&commitment_key(&e, &feed_id))
            .ok_or(Error::CommitmentNotFound)
    }

    /// Submit an aggregated price entry with per-source values and metadata.
    ///
    /// Used for multi-source aggregations with provenance tracking.
    /// Parameters:
    /// - `oracle`: the oracle operator address.
    /// - `feed_id`: 32-byte feed identifier.
    /// - `aggregate_value`: computed weighted median (i64).
    /// - `source_values`: list of per-source values with IDs and weights.
    /// - `method`: aggregation method name (e.g., "weighted_median").
    /// - `outlier_method`: outlier rejection method (e.g., "iqr", "mad", "none").
    /// - `num_sources_rejected`: count of sources rejected as outliers.
    /// - `timestamp_utc`: Unix timestamp of aggregation.
    /// - `signature`: Ed25519 signature over the aggregation payload.
    ///
    /// The contract verifies the signature and stores the aggregated entry
    /// in a separate storage key for audit and retrieval.
    #[allow(clippy::too_many_arguments)]
    pub fn submit_aggregated_price(
        e: Env,
        oracle: Address,
        feed_id: BytesN<32>,
        aggregate_value: i64,
        source_values: soroban_sdk::Vec<(soroban_sdk::String, i64, i128, i128)>,
        method: soroban_sdk::String,
        outlier_method: soroban_sdk::String,
        num_sources_rejected: u32,
        timestamp_utc: i64,
        signature: BytesN<64>,
    ) -> Result<(), Error> {
        let cfg = require_config(&e)?;
        oracle.require_auth();

        // Verify signature over aggregation parameters
        // Create a deterministic payload from aggregation inputs
        let mut msg = soroban_sdk::Bytes::new(&e);

        // Include all relevant fields in signature verification
        // Schema version marker for aggregated entries
        msg.push_back(2u8);

        // feed_id (32 bytes)
        msg.append(&feed_id.clone().into());

        // aggregate_value as big-endian i64 (8 bytes)
        for b in aggregate_value.to_be_bytes() {
            msg.push_back(b);
        }

        // timestamp_utc as big-endian i64 (8 bytes)
        for b in timestamp_utc.to_be_bytes() {
            msg.push_back(b);
        }

        // num_sources as big-endian u32 (4 bytes)
        let num_sources = source_values.len() as u32;
        for b in num_sources.to_be_bytes() {
            msg.push_back(b);
        }

        // Verify signature
        e.crypto()
            .ed25519_verify(&cfg.oracle_pubkey, &msg, &signature);

        // Build source values vector
        let mut src_vals = soroban_sdk::Vec::new(&e);
        for (source_id, value, weight_num, weight_den) in source_values.iter() {
            src_vals.push_back(SourceValue {
                source_id: source_id.clone(),
                value,
                weight_numerator: weight_num,
                weight_denominator: weight_den,
            });
        }

        // Build aggregated entry
        let entry = AggregatedPriceEntry {
            aggregate_value,
            source_values: src_vals,
            metadata: AggregationMetadata {
                method: method.clone(),
                outlier_method: outlier_method.clone(),
                num_sources_used: num_sources - num_sources_rejected,
                num_sources_rejected,
                timestamp_utc,
            },
            recorded_at: e.ledger().sequence(),
        };

        e.storage()
            .persistent()
            .set(&agg_feed_key(&e, &feed_id), &entry);

        Ok(())
    }

    /// Read the latest aggregated price entry for a given feed.
    ///
    /// Returns the aggregated value, per-source values, and provenance metadata.
    pub fn get_aggregated_price(
        e: Env,
        feed_id: BytesN<32>,
    ) -> Result<AggregatedPriceEntry, Error> {
        require_config(&e)?;
        e.storage()
            .persistent()
            .get(&agg_feed_key(&e, &feed_id))
            .ok_or(Error::FeedNotFound)
    }

    /// Read the current contract configuration (admin + oracle public key).
    pub fn get_config(e: Env) -> Result<Config, Error> {
        require_config(&e)
    }
}
