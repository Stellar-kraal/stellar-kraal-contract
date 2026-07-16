//! Unit tests for the `carbon_oracle` Soroban contract.
//!
//! Covers:
//! - Valid Ed25519 attestation is accepted and stored.
//! - Tampered output_value is rejected with `InvalidAttestation`.
//! - Tampered timestamp is rejected.
//! - Tampered script_hash is rejected.
//! - Tampered input_params_hash is rejected.
//! - Tampered feed_id is rejected.
//! - Tampered signature (bit-flip) is rejected.
//! - Wrong public key (different signer) is rejected.
//! - Double-initialization is rejected.
//! - Uninitialized contract rejects calls.
//! - Key rotation by admin succeeds; old key no longer works.
//! - Key rotation by non-admin is rejected.
//! - `get_price` on unknown feed returns `FeedNotFound`.

#![cfg(test)]

extern crate std;

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use soroban_sdk::{testutils::Address as _, Address, BytesN, Env};

use crate::{CarbonOracle, CarbonOracleClient, CommitmentState, Error, SCHEMA_VERSION};

// ── Test helpers ──────────────────────────────────────────────────────────────

/// Generate a fresh Ed25519 signing key.
fn gen_key() -> SigningKey {
    SigningKey::generate(&mut OsRng)
}

/// Build the canonical 113-byte payload (mirrors the Rust contract logic).
fn build_payload_bytes(
    script_hash: &[u8; 32],
    input_params_hash: &[u8; 32],
    output_value: i64,
    timestamp_utc: i64,
    feed_id: &[u8; 32],
) -> [u8; 113] {
    let mut buf = [0u8; 113];
    buf[0] = SCHEMA_VERSION;
    buf[1..33].copy_from_slice(script_hash);
    buf[33..65].copy_from_slice(input_params_hash);
    buf[65..73].copy_from_slice(&output_value.to_be_bytes());
    buf[73..81].copy_from_slice(&timestamp_utc.to_be_bytes());
    buf[81..113].copy_from_slice(feed_id);
    buf
}

/// Sign the canonical payload and return the 64-byte signature.
fn sign_payload(key: &SigningKey, payload: &[u8; 113]) -> [u8; 64] {
    key.sign(payload).to_bytes()
}

fn n32(e: &Env, b: &[u8; 32]) -> BytesN<32> {
    BytesN::from_array(e, b)
}
fn n64(e: &Env, b: &[u8; 64]) -> BytesN<64> {
    BytesN::from_array(e, b)
}

// ── Fixture ───────────────────────────────────────────────────────────────────

struct Fixture {
    env: Env,
    client: CarbonOracleClient<'static>,
    admin: Address,
    oracle: Address,
    signing_key: SigningKey,
    script_hash: [u8; 32],
    input_params_hash: [u8; 32],
    feed_id: [u8; 32],
}

impl Fixture {
    fn new() -> Self {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register(CarbonOracle, ());
        let client: CarbonOracleClient<'static> =
            unsafe { std::mem::transmute(CarbonOracleClient::new(&env, &contract_id)) };

        let admin = Address::generate(&env);
        let oracle = Address::generate(&env);
        let signing_key = gen_key();
        let pubkey_bytes = signing_key.verifying_key().to_bytes();
        client.initialize(&admin, &n32(&env, &pubkey_bytes), &10); // challenge window of 10 ledgers

        let mut script_hash = [0u8; 32];
        script_hash[0] = 0xDE;
        script_hash[1] = 0xAD;

        let mut input_params_hash = [0u8; 32];
        input_params_hash[0] = 0xBE;
        input_params_hash[1] = 0xEF;

        let mut feed_id = [0u8; 32];
        feed_id[..6].copy_from_slice(b"carbon");

        Fixture {
            env,
            client,
            admin,
            oracle,
            signing_key,
            script_hash,
            input_params_hash,
            feed_id,
        }
    }

    /// Happy-path: sign and submit a price update (panics on contract error).
    fn submit_ok(
        &self,
        output_value: i64,
        timestamp_utc: i64,
        script_hash: Option<[u8; 32]>,
        input_params_hash: Option<[u8; 32]>,
        feed_id: Option<[u8; 32]>,
        key_override: Option<&SigningKey>,
    ) {
        let sh = script_hash.unwrap_or(self.script_hash);
        let iph = input_params_hash.unwrap_or(self.input_params_hash);
        let fid = feed_id.unwrap_or(self.feed_id);
        let payload = build_payload_bytes(&sh, &iph, output_value, timestamp_utc, &fid);
        let signer = key_override.unwrap_or(&self.signing_key);
        let raw_sig = sign_payload(signer, &payload);
        self.client.submit_price(
            &self.oracle,
            &n32(&self.env, &sh),
            &n32(&self.env, &iph),
            &output_value,
            &timestamp_utc,
            &n32(&self.env, &fid),
            &n64(&self.env, &raw_sig),
        );
    }

    /// Supply an explicit (possibly tampered) signature and expect rejection.
    /// Returns `true` if the contract rejected the call, `false` if it accepted.
    fn try_submit_with_sig(
        &self,
        output_value: i64,
        timestamp_utc: i64,
        script_hash: [u8; 32],
        input_params_hash: [u8; 32],
        feed_id: [u8; 32],
        sig: [u8; 64],
    ) -> bool {
        self.client
            .try_submit_price(
                &self.oracle,
                &n32(&self.env, &script_hash),
                &n32(&self.env, &input_params_hash),
                &output_value,
                &timestamp_utc,
                &n32(&self.env, &feed_id),
                &n64(&self.env, &sig),
            )
            .is_err()
    }
}

// ── Happy-path tests ──────────────────────────────────────────────────────────

#[test]
fn valid_attestation_is_accepted() {
    let f = Fixture::new();
    f.submit_ok(1_234_567, 1_720_051_200, None, None, None, None);
}

#[test]
fn price_entry_stored_correctly() {
    let f = Fixture::new();
    f.submit_ok(9_999_999, 1_720_000_000, None, None, None, None);

    let entry = f.client.get_price(&n32(&f.env, &f.feed_id));
    assert_eq!(entry.output_value, 9_999_999);
    assert_eq!(entry.timestamp_utc, 1_720_000_000);
    assert_eq!(entry.script_hash, n32(&f.env, &f.script_hash));
    assert_eq!(entry.input_params_hash, n32(&f.env, &f.input_params_hash));
}

#[test]
fn subsequent_updates_overwrite_previous() {
    let f = Fixture::new();
    f.submit_ok(100, 1_720_000_000, None, None, None, None);
    f.submit_ok(200, 1_720_003_600, None, None, None, None);
    let entry = f.client.get_price(&n32(&f.env, &f.feed_id));
    assert_eq!(entry.output_value, 200);
}

#[test]
fn negative_output_value_accepted() {
    let f = Fixture::new();
    f.submit_ok(-42, 1_720_000_000, None, None, None, None);
    let entry = f.client.get_price(&n32(&f.env, &f.feed_id));
    assert_eq!(entry.output_value, -42);
}

// ── Tamper-detection helpers ──────────────────────────────────────────────────

fn assert_tampered_payload_rejected(idx: usize) {
    let f = Fixture::new();
    let output_value: i64 = 1_234_567;
    let timestamp_utc: i64 = 1_720_051_200;

    let mut payload =
        build_payload_bytes(&f.script_hash, &f.input_params_hash, output_value, timestamp_utc, &f.feed_id);

    let sig = sign_payload(&f.signing_key, &payload); // sign BEFORE tamper

    payload[idx] ^= 0xFF; // tamper AFTER signing

    let sh: [u8; 32] = payload[1..33].try_into().unwrap();
    let iph: [u8; 32] = payload[33..65].try_into().unwrap();
    let ov = i64::from_be_bytes(payload[65..73].try_into().unwrap());
    let ts = i64::from_be_bytes(payload[73..81].try_into().unwrap());
    let fid: [u8; 32] = payload[81..113].try_into().unwrap();

    let rejected = f.try_submit_with_sig(ov, ts, sh, iph, fid, sig);
    assert!(rejected, "tampered payload at byte {idx} was incorrectly accepted");
}

// ── Tamper tests ──────────────────────────────────────────────────────────────

#[test]
fn tampered_schema_version_rejected() {
    // The schema_version byte is embedded by the CONTRACT at position 0 of the
    // canonical payload (always SCHEMA_VERSION = 1).  An attacker cannot
    // override it through the function arguments — it is a compile-time
    // constant in the contract.
    //
    // This test verifies the corollary: a signature produced with a DIFFERENT
    // schema_version byte (simulating a future version or an attacker trying
    // to reuse a v2 signature) is rejected, because the contract always
    // rebuilds the message with SCHEMA_VERSION.
    let f = Fixture::new();
    let ov: i64 = 1_234_567;
    let ts: i64 = 1_720_051_200;

    // Build payload with a WRONG schema_version byte (e.g., 0xFF).
    let mut payload =
        build_payload_bytes(&f.script_hash, &f.input_params_hash, ov, ts, &f.feed_id);
    payload[0] = 0xFF; // wrong schema version

    // Sign the tampered (wrong-version) payload.
    let sig = sign_payload(&f.signing_key, &payload);

    // The contract will rebuild the message with SCHEMA_VERSION=1 and verify
    // against that — so the signature over the 0xFF version must be rejected.
    let rejected = f.try_submit_with_sig(ov, ts, f.script_hash, f.input_params_hash, f.feed_id, sig);
    assert!(
        rejected,
        "signature over wrong schema_version should be rejected"
    );
}

#[test]
fn tampered_script_hash_rejected() {
    assert_tampered_payload_rejected(1);
}

#[test]
fn tampered_input_params_hash_rejected() {
    assert_tampered_payload_rejected(33);
}

#[test]
fn tampered_output_value_rejected() {
    assert_tampered_payload_rejected(65);
}

#[test]
fn tampered_timestamp_rejected() {
    assert_tampered_payload_rejected(73);
}

#[test]
fn tampered_feed_id_rejected() {
    assert_tampered_payload_rejected(81);
}

#[test]
fn tampered_signature_rejected() {
    let f = Fixture::new();
    let ov: i64 = 1_234_567;
    let ts: i64 = 1_720_051_200;
    let payload = build_payload_bytes(&f.script_hash, &f.input_params_hash, ov, ts, &f.feed_id);
    let mut sig = sign_payload(&f.signing_key, &payload);
    sig[0] ^= 0xFF;

    let rejected = f.try_submit_with_sig(ov, ts, f.script_hash, f.input_params_hash, f.feed_id, sig);
    assert!(rejected, "tampered signature was incorrectly accepted");
}

#[test]
fn wrong_key_rejected() {
    let f = Fixture::new();
    let wrong_key = gen_key();
    let ov: i64 = 1_000;
    let ts: i64 = 1_720_000_000;
    let payload = build_payload_bytes(&f.script_hash, &f.input_params_hash, ov, ts, &f.feed_id);
    let sig = sign_payload(&wrong_key, &payload);

    let rejected = f.try_submit_with_sig(ov, ts, f.script_hash, f.input_params_hash, f.feed_id, sig);
    assert!(rejected, "wrong-key attestation was incorrectly accepted");
}

// ── Initialization tests ──────────────────────────────────────────────────────

#[test]
fn double_initialization_rejected() {
    let f = Fixture::new();
    let pubkey_bytes = f.signing_key.verifying_key().to_bytes();
    let result = f
        .client
        .try_initialize(&f.admin, &n32(&f.env, &pubkey_bytes), &10);
    assert_eq!(result, Err(Ok(Error::AlreadyInitialized)));
}

#[test]
fn uninitialized_contract_rejects_submit() {
    let env = Env::default();
    env.mock_all_auths();
    let contract_id = env.register(CarbonOracle, ());
    let client = CarbonOracleClient::new(&env, &contract_id);

    let key = gen_key();
    let sh = [0u8; 32];
    let iph = [0u8; 32];
    let fid = [0u8; 32];
    let payload = build_payload_bytes(&sh, &iph, 0, 0, &fid);
    let sig = sign_payload(&key, &payload);
    let oracle = Address::generate(&env);

    let result = client.try_submit_price(
        &oracle,
        &BytesN::from_array(&env, &sh),
        &BytesN::from_array(&env, &iph),
        &0i64,
        &0i64,
        &BytesN::from_array(&env, &fid),
        &BytesN::from_array(&env, &sig),
    );
    assert_eq!(result, Err(Ok(Error::NotInitialized)));
}

// ── Key rotation tests ────────────────────────────────────────────────────────

#[test]
fn key_rotation_by_admin_succeeds() {
    let f = Fixture::new();
    let new_key = gen_key();
    let new_pubkey_bytes = new_key.verifying_key().to_bytes();
    f.client.rotate_key(&f.admin, &n32(&f.env, &new_pubkey_bytes));

    // New key must now be accepted.
    f.submit_ok(42, 1_720_000_000, None, None, None, Some(&new_key));
}

#[test]
fn old_key_rejected_after_rotation() {
    let f = Fixture::new();
    let new_key = gen_key();
    let new_pubkey_bytes = new_key.verifying_key().to_bytes();
    f.client.rotate_key(&f.admin, &n32(&f.env, &new_pubkey_bytes));

    // Original (old) key must now fail.
    let ov: i64 = 42;
    let ts: i64 = 1_720_000_000;
    let payload = build_payload_bytes(&f.script_hash, &f.input_params_hash, ov, ts, &f.feed_id);
    let sig = sign_payload(&f.signing_key, &payload); // old key

    let rejected = f.try_submit_with_sig(ov, ts, f.script_hash, f.input_params_hash, f.feed_id, sig);
    assert!(rejected, "old key should be rejected after rotation");
}

#[test]
fn key_rotation_by_non_admin_rejected() {
    let f = Fixture::new();
    let non_admin = Address::generate(&f.env);
    let new_key = gen_key();
    let new_pubkey_bytes = new_key.verifying_key().to_bytes();

    let result = f
        .client
        .try_rotate_key(&non_admin, &n32(&f.env, &new_pubkey_bytes));
    assert_eq!(result, Err(Ok(Error::Unauthorized)));
}

// ── Feed queries ──────────────────────────────────────────────────────────────

#[test]
fn get_price_unknown_feed_returns_not_found() {
    let f = Fixture::new();
    let unknown = [0xFFu8; 32];
    let result = f.client.try_get_price(&n32(&f.env, &unknown));
    assert_eq!(result, Err(Ok(Error::FeedNotFound)));
}

#[test]
fn multiple_feeds_stored_independently() {
    let f = Fixture::new();
    let mut feed_a = [0u8; 32];
    feed_a[0] = 0xAA;
    let mut feed_b = [0u8; 32];
    feed_b[0] = 0xBB;

    f.submit_ok(111, 1_720_000_000, None, None, Some(feed_a), None);
    f.submit_ok(222, 1_720_003_600, None, None, Some(feed_b), None);

    let entry_a = f.client.get_price(&n32(&f.env, &feed_a));
    let entry_b = f.client.get_price(&n32(&f.env, &feed_b));
    assert_eq!(entry_a.output_value, 111);
    assert_eq!(entry_b.output_value, 222);
}

// ── Commit-Reveal tests ───────────────────────────────────────────────────────

#[test]
fn commit_and_reveal_success() {
    let f = Fixture::new();
    let output_value: i64 = 555_555;
    let timestamp_utc: i64 = 1_800_000_000;
    let salt = [0x77u8; 32];

    // Compute commitment hash
    let mut payload = soroban_sdk::Bytes::new(&f.env);
    payload.append(&n32(&f.env, &f.script_hash).into());
    payload.append(&n32(&f.env, &f.input_params_hash).into());
    for b in output_value.to_be_bytes() { payload.push_back(b); }
    for b in timestamp_utc.to_be_bytes() { payload.push_back(b); }
    payload.append(&n32(&f.env, &f.feed_id).into());
    payload.append(&n32(&f.env, &salt).into());
    let commitment_hash = f.env.crypto().sha256(&payload);

    f.client.commit_price(&f.oracle, &n32(&f.env, &f.feed_id), &commitment_hash);

    let commitment = f.client.get_commitment(&n32(&f.env, &f.feed_id));
    assert_eq!(commitment.state, CommitmentState::ChallengeWindow);

    // Fast forward ledger sequence to bypass challenge window (duration is 10)
    let current_seq = f.env.ledger().sequence();
    
    // Attempt reveal before window expires (should fail)
    let attestation_payload = build_payload_bytes(&f.script_hash, &f.input_params_hash, output_value, timestamp_utc, &f.feed_id);
    let sig = sign_payload(&f.signing_key, &attestation_payload);

    let early_reveal = f.client.try_reveal_price(
        &f.oracle,
        &n32(&f.env, &f.feed_id),
        &n32(&f.env, &f.script_hash),
        &n32(&f.env, &f.input_params_hash),
        &output_value,
        &timestamp_utc,
        &n32(&f.env, &salt),
        &n64(&f.env, &sig)
    );
    assert_eq!(early_reveal, Err(Ok(Error::ChallengeWindowNotExpired)));

    f.env.ledger().set_sequence_number(current_seq + 11);

    f.client.reveal_price(
        &f.oracle,
        &n32(&f.env, &f.feed_id),
        &n32(&f.env, &f.script_hash),
        &n32(&f.env, &f.input_params_hash),
        &output_value,
        &timestamp_utc,
        &n32(&f.env, &salt),
        &n64(&f.env, &sig)
    );

    let revealed_commitment = f.client.get_commitment(&n32(&f.env, &f.feed_id));
    assert_eq!(revealed_commitment.state, CommitmentState::Revealed);

    let price = f.client.get_price(&n32(&f.env, &f.feed_id));
    assert_eq!(price.output_value, output_value);
}

#[test]
fn challenge_during_window_succeeds() {
    let f = Fixture::new();
    let output_value: i64 = 555_555;
    let timestamp_utc: i64 = 1_800_000_000;
    let salt = [0x77u8; 32];

    let mut payload = soroban_sdk::Bytes::new(&f.env);
    payload.append(&n32(&f.env, &f.script_hash).into());
    payload.append(&n32(&f.env, &f.input_params_hash).into());
    for b in output_value.to_be_bytes() { payload.push_back(b); }
    for b in timestamp_utc.to_be_bytes() { payload.push_back(b); }
    payload.append(&n32(&f.env, &f.feed_id).into());
    payload.append(&n32(&f.env, &salt).into());
    let commitment_hash = f.env.crypto().sha256(&payload);

    f.client.commit_price(&f.oracle, &n32(&f.env, &f.feed_id), &commitment_hash);

    let challenger = Address::generate(&f.env);
    f.client.challenge_price(&challenger, &n32(&f.env, &f.feed_id), &999_999);

    let commitment = f.client.get_commitment(&n32(&f.env, &f.feed_id));
    assert_eq!(commitment.state, CommitmentState::Disputed);

    // Now reveal should fail because state is not ChallengeWindow
    let attestation_payload = build_payload_bytes(&f.script_hash, &f.input_params_hash, output_value, timestamp_utc, &f.feed_id);
    let sig = sign_payload(&f.signing_key, &attestation_payload);

    let current_seq = f.env.ledger().sequence();
    f.env.ledger().set_sequence_number(current_seq + 11);

    let reveal_res = f.client.try_reveal_price(
        &f.oracle,
        &n32(&f.env, &f.feed_id),
        &n32(&f.env, &f.script_hash),
        &n32(&f.env, &f.input_params_hash),
        &output_value,
        &timestamp_utc,
        &n32(&f.env, &salt),
        &n64(&f.env, &sig)
    );
    assert_eq!(reveal_res, Err(Ok(Error::InvalidCommitmentState)));
}
