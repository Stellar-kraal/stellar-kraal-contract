"""
tests/test_attestation.py
=========================

Unit tests for :mod:`oracle_bridge.attestation`.

Covers:
- Canonical payload serialisation round-trips.
- Valid attestation signatures are accepted.
- Tampered payloads (field-level mutations) are rejected.
- Tampered signatures are rejected.
- Wrong public key is rejected.
- Helper utilities (canonical_params_hash, pad_feed_id).
"""

from __future__ import annotations

import hashlib
import struct
import time

import pytest

from oracle_bridge.attestation import (
    PAYLOAD_LENGTH,
    AttestationPayload,
    OracleSigner,
    OracleVerifier,
    SignedAttestation,
    canonical_params_hash,
    pad_feed_id,
    sha256,
)


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture()
def signer() -> OracleSigner:
    """A fresh ephemeral signer for each test."""
    return OracleSigner.generate()


@pytest.fixture()
def sample_script_hash() -> bytes:
    return sha256(b"function computeCarbon(params){return 42;}")


@pytest.fixture()
def sample_params() -> dict:
    return {
        "startDate": "2024-01-01",
        "endDate": "2024-12-31",
        "aoi": "POLYGON((30.1 -1.2,30.1 -1.0,30.3 -1.0,30.3 -1.2,30.1 -1.2))",
    }


@pytest.fixture()
def sample_payload(sample_script_hash, sample_params) -> AttestationPayload:
    return AttestationPayload(
        script_hash=sample_script_hash,
        input_params_hash=canonical_params_hash(sample_params),
        output_value=1_234_567,
        timestamp_utc=1_720_051_200,
        feed_id=pad_feed_id("carbon/rwanda/2024"),
    )


@pytest.fixture()
def signed(signer, sample_payload) -> SignedAttestation:
    sig = signer.sign(sample_payload)
    return SignedAttestation(
        payload=sample_payload,
        public_key=signer.public_key_bytes(),
        signature=sig,
    )


# ── Payload serialisation ─────────────────────────────────────────────────────


class TestPayloadSerialisation:
    def test_payload_is_113_bytes(self, sample_payload):
        assert len(sample_payload.to_bytes()) == PAYLOAD_LENGTH

    def test_round_trip(self, sample_payload):
        raw = sample_payload.to_bytes()
        recovered = AttestationPayload.from_bytes(raw)
        assert recovered.schema_version == sample_payload.schema_version
        assert recovered.script_hash == sample_payload.script_hash
        assert recovered.input_params_hash == sample_payload.input_params_hash
        assert recovered.output_value == sample_payload.output_value
        assert recovered.timestamp_utc == sample_payload.timestamp_utc
        assert recovered.feed_id == sample_payload.feed_id

    def test_schema_version_is_first_byte(self, sample_payload):
        raw = sample_payload.to_bytes()
        assert raw[0] == 1

    def test_output_value_big_endian(self, sample_payload):
        raw = sample_payload.to_bytes()
        # output_value starts at byte 65 (1 + 32 + 32)
        packed_val = struct.unpack(">q", raw[65:73])[0]
        assert packed_val == sample_payload.output_value

    def test_timestamp_utc_big_endian(self, sample_payload):
        raw = sample_payload.to_bytes()
        # timestamp_utc starts at byte 73 (1 + 32 + 32 + 8)
        packed_ts = struct.unpack(">q", raw[73:81])[0]
        assert packed_ts == sample_payload.timestamp_utc

    def test_from_bytes_wrong_length_raises(self):
        with pytest.raises(ValueError, match="Expected 113 bytes"):
            AttestationPayload.from_bytes(b"\x00" * 112)

    def test_invalid_script_hash_length(self, sample_payload):
        with pytest.raises(ValueError, match="script_hash"):
            AttestationPayload(
                script_hash=b"\x00" * 31,  # too short
                input_params_hash=sample_payload.input_params_hash,
                output_value=sample_payload.output_value,
                timestamp_utc=sample_payload.timestamp_utc,
                feed_id=sample_payload.feed_id,
            )

    def test_to_dict_hex_fields(self, sample_payload):
        d = sample_payload.to_dict()
        assert d["script_hash"] == sample_payload.script_hash.hex()
        assert len(d["script_hash"]) == 64  # 32 bytes → 64 hex chars

    def test_negative_output_value_allowed(self):
        """i64 must accept negative values."""
        p = AttestationPayload(
            script_hash=b"\xab" * 32,
            input_params_hash=b"\xcd" * 32,
            output_value=-999,
            timestamp_utc=1_700_000_000,
            feed_id=b"\x00" * 32,
        )
        assert AttestationPayload.from_bytes(p.to_bytes()).output_value == -999


# ── Signing ───────────────────────────────────────────────────────────────────


class TestSigning:
    def test_valid_signature_verifies(self, signer, sample_payload):
        sig = signer.sign(sample_payload)
        verifier = OracleVerifier(signer.public_key_bytes())
        attestation = SignedAttestation(
            payload=sample_payload,
            public_key=signer.public_key_bytes(),
            signature=sig,
        )
        assert verifier.verify(attestation) is True

    def test_attest_helper_produces_valid_signature(self, signer, sample_params):
        script_src = b"function computeCarbon(params){return 42;}"
        attestation = signer.attest(
            script_hash=sha256(script_src),
            input_params=sample_params,
            output_value=9_999,
            timestamp_utc=1_720_051_200,
            feed_id="carbon/test",
        )
        verifier = OracleVerifier(signer.public_key_bytes())
        assert verifier.verify(attestation) is True

    def test_attest_accepts_precomputed_params_hash(self, signer):
        params_hash = sha256(b"precomputed")
        attestation = signer.attest(
            script_hash=b"\x11" * 32,
            input_params=params_hash,  # bytes, not dict
            output_value=1,
            timestamp_utc=1_720_000_000,
            feed_id="feed/x",
        )
        verifier = OracleVerifier(signer.public_key_bytes())
        assert verifier.verify(attestation) is True

    def test_seed_roundtrip(self):
        signer = OracleSigner.generate()
        seed = signer.private_key_seed()
        signer2 = OracleSigner.from_seed(seed)
        assert signer.public_key_bytes() == signer2.public_key_bytes()

    def test_from_seed_wrong_length_raises(self):
        with pytest.raises(ValueError, match="32 bytes"):
            OracleSigner.from_seed(b"\x00" * 31)


# ── Tamper detection ──────────────────────────────────────────────────────────


class TestTamperDetection:
    """Any mutation to the signed message must cause verification failure."""

    def _flip_byte(self, data: bytes, index: int) -> bytes:
        lst = bytearray(data)
        lst[index] ^= 0xFF
        return bytes(lst)

    def test_tampered_output_value_rejected(self, signed):
        verifier = OracleVerifier(signed.public_key)
        # Flip a bit in output_value (bytes 65-72)
        raw = signed.payload.to_bytes()
        tampered_raw = self._flip_byte(raw, 66)
        tampered_payload = AttestationPayload.from_bytes(tampered_raw)
        bad = SignedAttestation(
            payload=tampered_payload,
            public_key=signed.public_key,
            signature=signed.signature,
        )
        assert verifier.verify(bad) is False

    def test_tampered_timestamp_rejected(self, signed):
        verifier = OracleVerifier(signed.public_key)
        raw = signed.payload.to_bytes()
        tampered_raw = self._flip_byte(raw, 74)
        tampered_payload = AttestationPayload.from_bytes(tampered_raw)
        bad = SignedAttestation(
            payload=tampered_payload,
            public_key=signed.public_key,
            signature=signed.signature,
        )
        assert verifier.verify(bad) is False

    def test_tampered_script_hash_rejected(self, signed):
        verifier = OracleVerifier(signed.public_key)
        raw = signed.payload.to_bytes()
        tampered_raw = self._flip_byte(raw, 1)  # first byte of script_hash
        tampered_payload = AttestationPayload.from_bytes(tampered_raw)
        bad = SignedAttestation(
            payload=tampered_payload,
            public_key=signed.public_key,
            signature=signed.signature,
        )
        assert verifier.verify(bad) is False

    def test_tampered_input_params_hash_rejected(self, signed):
        verifier = OracleVerifier(signed.public_key)
        raw = signed.payload.to_bytes()
        tampered_raw = self._flip_byte(raw, 33)  # first byte of input_params_hash
        tampered_payload = AttestationPayload.from_bytes(tampered_raw)
        bad = SignedAttestation(
            payload=tampered_payload,
            public_key=signed.public_key,
            signature=signed.signature,
        )
        assert verifier.verify(bad) is False

    def test_tampered_feed_id_rejected(self, signed):
        verifier = OracleVerifier(signed.public_key)
        raw = signed.payload.to_bytes()
        tampered_raw = self._flip_byte(raw, 81)  # first byte of feed_id
        tampered_payload = AttestationPayload.from_bytes(tampered_raw)
        bad = SignedAttestation(
            payload=tampered_payload,
            public_key=signed.public_key,
            signature=signed.signature,
        )
        assert verifier.verify(bad) is False

    def test_tampered_signature_rejected(self, signed):
        verifier = OracleVerifier(signed.public_key)
        bad_sig = self._flip_byte(signed.signature, 0)
        bad = SignedAttestation(
            payload=signed.payload,
            public_key=signed.public_key,
            signature=bad_sig,
        )
        assert verifier.verify(bad) is False

    def test_wrong_public_key_rejected(self, signed):
        other_signer = OracleSigner.generate()
        verifier = OracleVerifier(other_signer.public_key_bytes())
        assert verifier.verify(signed) is False

    def test_tampered_schema_version_rejected(self, signed):
        verifier = OracleVerifier(signed.public_key)
        raw = signed.payload.to_bytes()
        # Change schema version byte
        tampered_raw = bytes([raw[0] ^ 0x01]) + raw[1:]
        tampered_payload = AttestationPayload.from_bytes(tampered_raw)
        bad = SignedAttestation(
            payload=tampered_payload,
            public_key=signed.public_key,
            signature=signed.signature,
        )
        assert verifier.verify(bad) is False


# ── Helpers ───────────────────────────────────────────────────────────────────


class TestHelpers:
    def test_canonical_params_hash_is_deterministic(self):
        params = {"b": 2, "a": 1}
        h1 = canonical_params_hash(params)
        h2 = canonical_params_hash({"a": 1, "b": 2})
        assert h1 == h2

    def test_canonical_params_hash_different_values_differ(self):
        h1 = canonical_params_hash({"a": 1})
        h2 = canonical_params_hash({"a": 2})
        assert h1 != h2

    def test_pad_feed_id_pads_short_string(self):
        result = pad_feed_id("abc")
        assert len(result) == 32
        assert result[:3] == b"abc"
        assert result[3:] == b"\x00" * 29

    def test_pad_feed_id_exact_32_bytes(self):
        raw = b"x" * 32
        assert pad_feed_id(raw) == raw

    def test_pad_feed_id_too_long_raises(self):
        with pytest.raises(ValueError, match="exceeds 32 bytes"):
            pad_feed_id("x" * 33)


# ── SignedAttestation dict roundtrip ──────────────────────────────────────────


class TestSignedAttestationDict:
    def test_to_dict_from_dict_roundtrip(self, signed):
        d = signed.to_dict()
        recovered = SignedAttestation.from_dict(d)
        assert recovered.payload.to_bytes() == signed.payload.to_bytes()
        assert recovered.public_key == signed.public_key
        assert recovered.signature == signed.signature

    def test_to_dict_has_expected_keys(self, signed):
        d = signed.to_dict()
        expected = {
            "schema_version",
            "script_hash",
            "input_params_hash",
            "output_value",
            "timestamp_utc",
            "feed_id",
            "public_key",
            "signature",
        }
        assert set(d.keys()) == expected
