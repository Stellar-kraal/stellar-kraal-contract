"""
tests/test_integration.py
==========================

Integration test: GEE output → signed attestation → (simulated) on-chain acceptance.

This test exercises the full pipeline end-to-end:
  1. A GEE result is produced (simulated).
  2. The oracle bridge signs the result into an attestation.
  3. The submission client receives the attestation.
  4. An off-chain verifier (standing in for the Soroban contract) verifies the
     signature using the authorised public key.
  5. The test asserts acceptance for a valid attestation and rejection for a
     tampered one.

The Soroban contract itself is tested in the Rust unit tests.  This test
validates the Python-side pipeline and the cross-language byte compatibility
of the canonical serialisation.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any

import pytest

from oracle_bridge.attestation import (
    OracleSigner,
    OracleVerifier,
    SignedAttestation,
    sha256,
)
from oracle_bridge.bridge import GEEResult, OracleBridge, SubmissionClient


# ── Fake submission client ────────────────────────────────────────────────────


@dataclass
class FakeSubmissionClient:
    """
    Records every submitted attestation and simulates on-chain verification.

    Parameters
    ----------
    authorised_pubkey:
        32-byte raw public key the fake contract trusts.
    """

    authorised_pubkey: bytes
    submissions: list[SignedAttestation] = field(default_factory=list)
    rejections: list[tuple[SignedAttestation, str]] = field(default_factory=list)

    def submit_price(self, attestation: SignedAttestation) -> str:
        verifier = OracleVerifier(self.authorised_pubkey)
        if verifier.verify(attestation):
            self.submissions.append(attestation)
            # Fake TX hash derived from the signature for determinism.
            return "tx_" + attestation.signature[:8].hex()
        else:
            reason = "InvalidAttestation"
            self.rejections.append((attestation, reason))
            raise ValueError(f"On-chain rejection: {reason}")


# ── GEE script fixture ────────────────────────────────────────────────────────

GEE_SCRIPT = """\
// GEE carbon sequestration estimator v1.0
// Inputs: startDate, endDate, aoi
var dataset = ee.ImageCollection('MODIS/061/MOD13A3')
  .filterDate(params.startDate, params.endDate)
  .filterBounds(params.aoi);
var carbonIndex = dataset.mean().select('NDVI');
return carbonIndex.reduceRegion({reducer: ee.Reducer.mean(), geometry: params.aoi}).get('NDVI');
"""

GEE_PARAMS = {
    "aoi": "POLYGON((30.1 -1.2,30.1 -1.0,30.3 -1.0,30.3 -1.2,30.1 -1.2))",
    "endDate": "2024-12-31",
    "startDate": "2024-01-01",
}


# ── Tests ─────────────────────────────────────────────────────────────────────


class TestEndToEndPipeline:
    """Full GEE → sign → submit pipeline."""

    @pytest.fixture()
    def signer(self) -> OracleSigner:
        return OracleSigner.generate()

    @pytest.fixture()
    def client(self, signer) -> FakeSubmissionClient:
        return FakeSubmissionClient(authorised_pubkey=signer.public_key_bytes())

    @pytest.fixture()
    def bridge(self, signer, client) -> OracleBridge:
        return OracleBridge(signer=signer, client=client)

    @pytest.fixture()
    def gee_result(self) -> GEEResult:
        return GEEResult(
            script_source=GEE_SCRIPT,
            input_params=GEE_PARAMS,
            output_value=4_815_162_342,
            feed_id="carbon/rwanda/2024",
            timestamp_utc=1_720_051_200,
        )

    # ── Happy path ────────────────────────────────────────────────────────────

    def test_valid_submission_accepted(self, bridge, client, gee_result):
        attestation, tx_ref = bridge.process(gee_result)

        assert len(client.submissions) == 1
        assert len(client.rejections) == 0
        assert tx_ref.startswith("tx_")

    def test_attestation_payload_matches_gee_result(self, bridge, gee_result):
        attestation, _ = bridge.process(gee_result)

        assert attestation.payload.output_value == gee_result.output_value
        assert attestation.payload.timestamp_utc == gee_result.timestamp_utc
        assert attestation.payload.script_hash == gee_result.script_hash

    def test_script_hash_is_sha256_of_source(self, bridge, gee_result):
        attestation, _ = bridge.process(gee_result)
        expected = sha256(gee_result.script_source.encode("utf-8"))
        assert attestation.payload.script_hash == expected

    def test_public_key_matches_signer(self, signer, bridge, gee_result):
        attestation, _ = bridge.process(gee_result)
        assert attestation.public_key == signer.public_key_bytes()

    def test_multiple_submissions_all_accepted(self, bridge, client):
        for i in range(5):
            result = GEEResult(
                script_source=GEE_SCRIPT,
                input_params={**GEE_PARAMS, "run": i},
                output_value=1_000_000 + i,
                feed_id="carbon/rwanda/2024",
                timestamp_utc=1_720_051_200 + i * 3600,
            )
            bridge.process(result)

        assert len(client.submissions) == 5
        assert len(client.rejections) == 0

    # ── Rejection path ────────────────────────────────────────────────────────

    def test_tampered_output_value_rejected_on_chain(self, signer, client, gee_result):
        """Mutate the output value after signing — the contract must reject it."""
        bridge = OracleBridge(signer=signer, client=client)
        attestation, _ = bridge.process(gee_result)

        # Construct a tampered attestation using the original signature but
        # a different output_value (simulates a man-in-the-middle attack).
        from oracle_bridge.attestation import AttestationPayload

        tampered_payload = AttestationPayload(
            script_hash=attestation.payload.script_hash,
            input_params_hash=attestation.payload.input_params_hash,
            output_value=attestation.payload.output_value + 1,  # ← tampered
            timestamp_utc=attestation.payload.timestamp_utc,
            feed_id=attestation.payload.feed_id,
        )
        tampered = SignedAttestation(
            payload=tampered_payload,
            public_key=attestation.public_key,
            signature=attestation.signature,  # original signature (now invalid)
        )

        with pytest.raises(ValueError, match="InvalidAttestation"):
            client.submit_price(tampered)

        assert len(client.rejections) == 1

    def test_wrong_key_rejected_on_chain(self, bridge, client, gee_result):
        """Attestation signed by a different key must be rejected."""
        other_signer = OracleSigner.generate()
        other_bridge = OracleBridge(signer=other_signer, client=client)

        with pytest.raises(ValueError, match="InvalidAttestation"):
            other_bridge.process(gee_result)

        assert len(client.rejections) == 1

    def test_tampered_signature_rejected(self, signer, client, gee_result):
        """A bit-flipped signature must be rejected."""
        bridge = OracleBridge(signer=signer, client=client)
        attestation, _ = bridge.process(gee_result)

        # Clear the accepted submission so the next call starts fresh.
        client.submissions.clear()

        bad_sig = bytearray(attestation.signature)
        bad_sig[0] ^= 0xFF
        tampered = SignedAttestation(
            payload=attestation.payload,
            public_key=attestation.public_key,
            signature=bytes(bad_sig),
        )

        with pytest.raises(ValueError, match="InvalidAttestation"):
            client.submit_price(tampered)

        assert len(client.rejections) == 1

    # ── Serialisation round-trip across the wire ──────────────────────────────

    def test_dict_serialisation_roundtrip(self, bridge, gee_result):
        attestation, _ = bridge.process(gee_result)
        wire = attestation.to_dict()
        recovered = SignedAttestation.from_dict(wire)

        verifier = OracleVerifier(recovered.public_key)
        assert verifier.verify(recovered) is True

    def test_canonical_params_order_invariant(self, signer, client):
        """
        Params submitted in different key orders must hash to the same value
        and therefore produce identical, accepted attestations.
        """
        params_a = {"endDate": "2024-12-31", "startDate": "2024-01-01", "aoi": "POLY"}
        params_b = {"aoi": "POLY", "startDate": "2024-01-01", "endDate": "2024-12-31"}

        result_a = GEEResult(
            script_source=GEE_SCRIPT,
            input_params=params_a,
            output_value=100,
            feed_id="f",
            timestamp_utc=1_720_000_000,
        )
        result_b = GEEResult(
            script_source=GEE_SCRIPT,
            input_params=params_b,
            output_value=100,
            feed_id="f",
            timestamp_utc=1_720_000_000,
        )

        bridge = OracleBridge(signer=signer, client=client)
        att_a, _ = bridge.process(result_a)

        client.submissions.clear()  # reset

        att_b, _ = bridge.process(result_b)

        assert att_a.payload.input_params_hash == att_b.payload.input_params_hash
