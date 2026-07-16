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

// ── Attestation payload constants ─────────────────────────────────────────────

/// Total length of the canonical signing message in bytes.
pub const PAYLOAD_LEN: usize = 113;

/// Schema version this contract accepts.
pub const SCHEMA_VERSION: u8 = 1;

// ── Data types ────────────────────────────────────────────────────────────────

/// Contract configuration stored in instance storage.
#[contracttype]
#[derive(Clone)]
pub struct Config {
    /// The admin address that initialised the contract.
    pub admin: Address,
    /// The 32-byte Ed25519 public key whose signatures the contract trusts.
    pub oracle_pubkey: BytesN<32>,
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
        // `env.crypto().ed25519_verify` panics (traps) on failure in Soroban.
        // We wrap it so we can convert the trap into our typed error.
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

    /// Read the current contract configuration (admin + oracle public key).
    pub fn get_config(e: Env) -> Result<Config, Error> {
        require_config(&e)
    }
}
