"""
tests/test_provenance_e2e.py
============================

End-to-end test: GEE oracle submission → IPFS provenance pinning → on-chain
CID retrieval and verification.

This test suite covers the full version-pinning and reproducibility audit trail
described in issue #66.  It exercises:

1. Single-source oracle submission:
   - GEEResult built with script asset path + version tag.
   - OracleBridge signs, builds provenance record, validates JSON Schema,
     pins to SimulatedIPFSClient, submits attestation + CID on-chain.
   - CID retrieved from the on-chain record matches what was pinned.
   - Provenance record fetched from IPFS is self-consistent.
   - Ed25519 signature in the provenance record verifies correctly.

2. Multi-source aggregated submission:
   - Bridge aggregates three sources, builds aggregated provenance.
   - CID pinned to IPFS; record includes aggregation block.

3. CLI tool (verify_provenance):
   - verify command passes on a valid record.
   - show command prints human-readable output without error.

4. Schema validation:
   - Records with missing required fields are rejected.

The Soroban contract itself is tested in Rust (contracts/carbon_oracle/src/tests.rs).
This Python suite validates the off-chain pipeline end-to-end.
"""

from __future__ import annotations

import hashlib
import json
from dataclasses import dataclass, field
from typing import Any
from unittest.mock import patch
import io

import pytest

from oracle_bridge.attestation import (
    OracleSigner,
    OracleVerifier,
    SignedAttestation,
)
from oracle_bridge.bridge import GEEResult, OracleBridge
from oracle_bridge.aggregation import AggregationConfig, OutlierRejectionMethod
from oracle_bridge.ipfs import SimulatedIPFSClient, fetch_provenance_record
from oracle_bridge.provenance import (
    ProvenanceRecord,
    validate_provenance_record,
    ProvenanceValidationError,
)


# ── GEE script fixtures ───────────────────────────────────────────────────────

GEE_SCRIPT_V1 = """\
// GEE carbon sequestration estimator v1.2.3
// Pinned to assets/carbon_seq_v1 @ commit abc1234
var dataset = ee.ImageCollection('MODIS/061/MOD13A3')
  .filterDate(params.startDate, params.endDate)
  .filterBounds(params.aoi);
var carbonIndex = dataset.mean().select('NDVI');
return carbonIndex.reduceRegion({
  reducer: ee.Reducer.mean(),
  geometry: params.aoi,
}).get('NDVI');
"""

GEE_PARAMS = {
    "aoi": "POLYGON((30.1 -1.2,30.1 -1.0,30.3 -1.0,30.3 -1.2,30.1 -1.2))",
    "endDate": "2024-12-31",
    "startDate": "2024-01-01",
}

GEE_ASSET_PATH = "users/stellarkraal/scripts/carbon_sequestration_v1"
GEE_VERSION_TAG = "v1.2.3"


# ── Fake on-chain client ──────────────────────────────────────────────────────


@dataclass
class FakeOnChainStore:
    """
    Simulated on-chain oracle contract that records submitted price entries
    together with their IPFS CID, mimicking the carbon_oracle Soroban contract.
    """

    authorised_pubkey: bytes
    # feed_id -> (attestation, ipfs_cid)
    price_entries: dict[str, tuple[SignedAttestation, str]] = field(default_factory=dict)
    tx_counter: int = 0

    def submit_price(self, attestation: SignedAttestation) -> str:
        return self._accept(attestation, "")

    def submit_price_with_cid(
        self, attestation: SignedAttestation, ipfs_cid: str
    ) -> str:
        verifier = OracleVerifier(self.authorised_pubkey)
        if not verifier.verify(attestation):
            raise ValueError("InvalidAttestation")
        return self._accept(attestation, ipfs_cid)

    def _accept(self, attestation: SignedAttestation, cid: str) -> str:
        self.tx_counter += 1
        feed_id_hex = attestation.payload.feed_id.hex()
        self.price_entries[feed_id_hex] = (attestation, cid)
        return f"tx_{self.tx_counter:04d}"

    def get_price(self, feed_id_hex: str) -> dict[str, Any]:
        if feed_id_hex not in self.price_entries:
            raise KeyError(f"FeedNotFound: {feed_id_hex}")
        att, cid = self.price_entries[feed_id_hex]
        return {
            "output_value": att.payload.output_value,
            "timestamp_utc": att.payload.timestamp_utc,
            "script_hash": att.payload.script_hash.hex(),
            "input_params_hash": att.payload.input_params_hash.hex(),
            "ipfs_cid": cid,
        }

    def commit_price(self, feed_id: str | bytes, commitment_hash: bytes) -> str:
        self.tx_counter += 1
        return f"tx_{self.tx_counter:04d}"

    def reveal_price(self, attestation: SignedAttestation, salt: bytes) -> str:
        self.tx_counter += 1
        return f"tx_{self.tx_counter:04d}"


# ── Fixtures ──────────────────────────────────────────────────────────────────


@pytest.fixture()
def signer() -> OracleSigner:
    return OracleSigner.generate()


@pytest.fixture()
def ipfs_client() -> SimulatedIPFSClient:
    return SimulatedIPFSClient()


@pytest.fixture()
def on_chain(signer) -> FakeOnChainStore:
    return FakeOnChainStore(authorised_pubkey=signer.public_key_bytes())


@pytest.fixture()
def bridge(signer, ipfs_client, on_chain) -> OracleBridge:
    return OracleBridge(signer=signer, client=on_chain, ipfs_client=ipfs_client)


@pytest.fixture()
def gee_result() -> GEEResult:
    return GEEResult(
        script_source=GEE_SCRIPT_V1,
        input_params=GEE_PARAMS,
        output_value=4_815_162_342,
        feed_id="carbon/rwanda/2024",
        timestamp_utc=1_720_051_200,
        script_asset_path=GEE_ASSET_PATH,
        script_version_tag=GEE_VERSION_TAG,
    )


# ── End-to-end: single-source submission ─────────────────────────────────────


class TestSingleSourceE2E:
    """Full pipeline: GEE result → provenance → IPFS → on-chain CID."""

    def test_submission_produces_tx_ref(self, bridge, gee_result):
        _att, tx_ref, prov = bridge.process(gee_result)
        assert tx_ref.startswith("tx_"), f"Expected tx ref, got {tx_ref!r}"

    def test_provenance_record_has_cid(self, bridge, gee_result):
        _att, _tx_ref, prov = bridge.process(gee_result)
        assert prov.ipfs_cid is not None, "ipfs_cid should be set after pinning"
        assert len(prov.ipfs_cid) > 0

    def test_cid_stored_on_chain(self, bridge, gee_result, on_chain):
        """CID retrieved from on-chain entry matches what was pinned to IPFS."""
        _att, _tx_ref, prov = bridge.process(gee_result)

        # Derive feed_id key as the bridge does
        from oracle_bridge.attestation import pad_feed_id
        feed_id_bytes = pad_feed_id(gee_result.feed_id)
        on_chain_entry = on_chain.get_price(feed_id_bytes.hex())

        assert on_chain_entry["ipfs_cid"] == prov.ipfs_cid, (
            f"On-chain CID {on_chain_entry['ipfs_cid']!r} ≠ "
            f"provenance CID {prov.ipfs_cid!r}"
        )

    def test_provenance_record_retrievable_from_ipfs(
        self, bridge, gee_result, ipfs_client
    ):
        """Provenance record can be fetched from IPFS by the on-chain CID."""
        _att, _tx_ref, prov = bridge.process(gee_result)

        # Retrieve from simulated IPFS
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)

        # Basic content checks
        assert retrieved["computation"]["output_value"] == gee_result.output_value
        assert retrieved["computation"]["feed_id"] == "carbon/rwanda/2024"
        assert retrieved["computation"]["timestamp_utc"] == gee_result.timestamp_utc
        assert retrieved["gee_script"]["asset_path"] == GEE_ASSET_PATH
        assert retrieved["gee_script"]["version_tag"] == GEE_VERSION_TAG

    def test_provenance_schema_valid(self, bridge, gee_result, ipfs_client):
        """The pinned record must pass JSON Schema validation."""
        _att, _tx_ref, prov = bridge.process(gee_result)
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)

        # Should not raise
        validate_provenance_record(retrieved)

    def test_script_version_hash_pinned(self, bridge, gee_result, ipfs_client):
        """The provenance record stores the SHA-256 of the exact GEE script."""
        _att, _tx_ref, prov = bridge.process(gee_result)
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)

        expected_hash = hashlib.sha256(
            GEE_SCRIPT_V1.encode("utf-8")
        ).hexdigest()
        assert retrieved["gee_script"]["version_hash"] == expected_hash

    def test_input_params_hash_pinned(self, bridge, gee_result, ipfs_client):
        """The provenance record stores the canonical SHA-256 of the input params."""
        _att, _tx_ref, prov = bridge.process(gee_result)
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)

        canonical = json.dumps(GEE_PARAMS, sort_keys=True, separators=(",", ":"))
        expected_hash = hashlib.sha256(canonical.encode("utf-8")).hexdigest()
        assert retrieved["input_params"]["params_hash"] == expected_hash

    def test_attestation_signature_verifiable(self, signer, bridge, gee_result, ipfs_client):
        """The Ed25519 signature in the provenance record is valid."""
        _att, _tx_ref, prov = bridge.process(gee_result)
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)

        att_data = retrieved["attestation"]
        pubkey_bytes = bytes.fromhex(att_data["public_key"])
        sig_bytes = bytes.fromhex(att_data["signature"])
        payload_bytes = bytes.fromhex(att_data["payload_hex"])

        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
        pubkey = Ed25519PublicKey.from_public_bytes(pubkey_bytes)
        # Should not raise
        pubkey.verify(sig_bytes, payload_bytes)

    def test_output_value_in_payload(self, bridge, gee_result, ipfs_client):
        """The 113-byte payload_hex encodes the correct output_value."""
        _att, _tx_ref, prov = bridge.process(gee_result)
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)

        payload = bytes.fromhex(retrieved["attestation"]["payload_hex"])
        assert len(payload) == 113
        stored_ov = int.from_bytes(payload[65:73], "big", signed=True)
        assert stored_ov == gee_result.output_value

    def test_same_script_same_cid(self, bridge, ipfs_client):
        """Two GEE runs with identical params but different values produce distinct CIDs."""
        r1 = GEEResult(
            script_source=GEE_SCRIPT_V1,
            input_params=GEE_PARAMS,
            output_value=1_000_000,
            feed_id="f",
            timestamp_utc=1_720_000_000,
            script_asset_path=GEE_ASSET_PATH,
            script_version_tag=GEE_VERSION_TAG,
        )
        r2 = GEEResult(
            script_source=GEE_SCRIPT_V1,
            input_params=GEE_PARAMS,
            output_value=2_000_000,  # different value → different attestation → different CID
            feed_id="f",
            timestamp_utc=1_720_000_000,
            script_asset_path=GEE_ASSET_PATH,
            script_version_tag=GEE_VERSION_TAG,
        )
        _, _, prov1 = bridge.process(r1)
        _, _, prov2 = bridge.process(r2)
        # Different runs → different CIDs (because content differs)
        assert prov1.ipfs_cid != prov2.ipfs_cid

    def test_tx_ref_stored_in_provenance(self, bridge, gee_result):
        """The transaction reference is stored in the provenance record."""
        _att, tx_ref, prov = bridge.process(gee_result)
        assert prov.tx_ref == tx_ref

    def test_multiple_feeds_tracked_independently(self, bridge, ipfs_client):
        """Each feed gets its own provenance record and CID."""
        feeds = ["carbon/rwanda/2024", "carbon/kenya/2024", "carbon/uganda/2024"]
        cids = {}
        for feed in feeds:
            result = GEEResult(
                script_source=GEE_SCRIPT_V1,
                input_params=GEE_PARAMS,
                output_value=1_000_000 + hash(feed) % 100_000,
                feed_id=feed,
                timestamp_utc=1_720_000_000,
                script_asset_path=GEE_ASSET_PATH,
                script_version_tag=GEE_VERSION_TAG,
            )
            _, _, prov = bridge.process(result)
            cids[feed] = prov.ipfs_cid

        # All CIDs should be distinct
        assert len(set(cids.values())) == len(feeds), "Each feed should have a unique CID"


# ── End-to-end: aggregated submission ────────────────────────────────────────


class TestAggregatedE2E:
    """Aggregated multi-source submission with provenance tracking."""

    @pytest.fixture()
    def agg_config(self) -> AggregationConfig:
        return AggregationConfig(
            sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
            weights={
                "xpansiv_cbl": 2.0,
                "toucan_protocol": 1.5,
                "custom_source": 1.0,
            },
            outlier_method=OutlierRejectionMethod.IQR,
        )

    @pytest.fixture()
    def agg_bridge(self, signer, ipfs_client, on_chain, agg_config) -> OracleBridge:
        return OracleBridge(
            signer=signer,
            client=on_chain,
            ipfs_client=ipfs_client,
            aggregation_config=agg_config,
        )

    @pytest.fixture()
    def per_source_results(self) -> dict[str, GEEResult]:
        return {
            "xpansiv_cbl": GEEResult(
                script_source=GEE_SCRIPT_V1,
                input_params=GEE_PARAMS,
                output_value=1_000_000,
                feed_id="carbon/aggregate/2024",
                timestamp_utc=1_720_051_200,
                script_asset_path=GEE_ASSET_PATH,
                script_version_tag=GEE_VERSION_TAG,
            ),
            "toucan_protocol": GEEResult(
                script_source=GEE_SCRIPT_V1,
                input_params=GEE_PARAMS,
                output_value=1_050_000,
                feed_id="carbon/aggregate/2024",
                timestamp_utc=1_720_051_200,
                script_asset_path=GEE_ASSET_PATH,
                script_version_tag=GEE_VERSION_TAG,
            ),
            "custom_source": GEEResult(
                script_source=GEE_SCRIPT_V1,
                input_params=GEE_PARAMS,
                output_value=980_000,
                feed_id="carbon/aggregate/2024",
                timestamp_utc=1_720_051_200,
                script_asset_path=GEE_ASSET_PATH,
                script_version_tag=GEE_VERSION_TAG,
            ),
        }

    def test_aggregated_submission_produces_cid(
        self, agg_bridge, per_source_results
    ):
        agg_result, _att, tx_ref, prov = agg_bridge.aggregate_and_submit(
            per_source_results
        )
        assert agg_result.ipfs_cid is not None
        assert prov.ipfs_cid == agg_result.ipfs_cid

    def test_aggregated_provenance_schema_valid(
        self, agg_bridge, per_source_results, ipfs_client
    ):
        _agg_result, _att, _tx_ref, prov = agg_bridge.aggregate_and_submit(
            per_source_results
        )
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)
        # Should not raise
        validate_provenance_record(retrieved)

    def test_aggregated_record_has_aggregation_block(
        self, agg_bridge, per_source_results, ipfs_client
    ):
        _agg_result, _att, _tx_ref, prov = agg_bridge.aggregate_and_submit(
            per_source_results
        )
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)
        assert "aggregation" in retrieved
        agg_block = retrieved["aggregation"]
        assert agg_block["method"] == "weighted_median"
        assert "source_values" in agg_block
        assert "weights_used" in agg_block

    def test_aggregated_record_type_is_aggregated(
        self, agg_bridge, per_source_results, ipfs_client
    ):
        _agg_result, _att, _tx_ref, prov = agg_bridge.aggregate_and_submit(
            per_source_results
        )
        retrieved = fetch_provenance_record(ipfs_client, prov.ipfs_cid)
        assert retrieved["record_type"] == "aggregated"


# ── Schema validation ─────────────────────────────────────────────────────────


class TestSchemaValidation:
    """Provenance schema validation edge cases."""

    @pytest.fixture()
    def valid_record(self, bridge, gee_result, ipfs_client) -> dict[str, Any]:
        _att, _tx_ref, prov = bridge.process(gee_result)
        return fetch_provenance_record(ipfs_client, prov.ipfs_cid)

    def test_valid_record_passes(self, valid_record):
        validate_provenance_record(valid_record)

    def test_missing_schema_version_fails(self, valid_record):
        record = dict(valid_record)
        del record["schema_version"]
        with pytest.raises(ProvenanceValidationError):
            validate_provenance_record(record)

    def test_missing_gee_script_fails(self, valid_record):
        record = dict(valid_record)
        del record["gee_script"]
        with pytest.raises(ProvenanceValidationError):
            validate_provenance_record(record)

    def test_missing_attestation_fails(self, valid_record):
        record = dict(valid_record)
        del record["attestation"]
        with pytest.raises(ProvenanceValidationError):
            validate_provenance_record(record)

    def test_wrong_schema_version_fails(self, valid_record):
        record = {**valid_record, "schema_version": 99}
        with pytest.raises(ProvenanceValidationError):
            validate_provenance_record(record)

    def test_invalid_record_type_fails(self, valid_record):
        record = {**valid_record, "record_type": "unknown_type"}
        with pytest.raises(ProvenanceValidationError):
            validate_provenance_record(record)

    def test_aggregated_without_block_fails(self, valid_record):
        record = {**valid_record, "record_type": "aggregated"}
        # No aggregation block — should fail
        with pytest.raises(ProvenanceValidationError):
            validate_provenance_record(record)


# ── CLI tool ──────────────────────────────────────────────────────────────────


class TestCLITool:
    """Smoke tests for the verify_provenance CLI tool."""

    @pytest.fixture()
    def pinned_record(self, bridge, gee_result, ipfs_client):
        _att, _tx_ref, prov = bridge.process(gee_result)
        return prov.ipfs_cid, ipfs_client

    def _load_cli_module(self):
        """Load the verify_provenance CLI module."""
        import importlib.util
        tool_path = (
            __import__("pathlib").Path(__file__).parent.parent / "tools" / "verify_provenance.py"
        )
        spec = importlib.util.spec_from_file_location("verify_provenance", tool_path)
        vp = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(vp)
        return vp

    def test_verify_command_passes(self, pinned_record, capsys):
        """verify command returns 0 for a valid record."""
        cid, ipfs_client = pinned_record
        vp = self._load_cli_module()

        # Patch get_ipfs_client on the loaded module directly
        vp.get_ipfs_client = lambda *a, **kw: ipfs_client

        import argparse
        args = argparse.Namespace(
            command="verify",
            cid=cid,
            file=None,
            check_sig=True,
        )
        exit_code = vp.cmd_verify(args)
        assert exit_code == 0

    def test_show_command_runs(self, pinned_record, capsys):
        """show command runs without error."""
        cid, ipfs_client = pinned_record
        vp = self._load_cli_module()

        vp.get_ipfs_client = lambda *a, **kw: ipfs_client

        import argparse
        args = argparse.Namespace(command="show", cid=cid, file=None)
        exit_code = vp.cmd_show(args)
        assert exit_code == 0

    def test_verify_from_file(self, tmp_path, bridge, gee_result, ipfs_client):
        """verify command works with a local JSON file."""
        _att, _tx_ref, prov = bridge.process(gee_result)
        record = fetch_provenance_record(ipfs_client, prov.ipfs_cid)

        prov_file = tmp_path / "provenance.json"
        prov_file.write_text(json.dumps(record, indent=2))

        vp = self._load_cli_module()

        import argparse
        args = argparse.Namespace(
            command="verify",
            cid=None,
            file=str(prov_file),
            check_sig=True,
        )
        exit_code = vp.cmd_verify(args)
        assert exit_code == 0


# ── Reproducibility ───────────────────────────────────────────────────────────


class TestReproducibility:
    """Verify that provenance records enable independent reproducibility checks."""

    def test_provenance_hash_integrity(self, bridge, gee_result, ipfs_client):
        """ProvenanceRecord.verify_hashes() returns no errors for a valid record."""
        _att, _tx_ref, prov = bridge.process(gee_result)
        errors = prov.verify_hashes()
        assert errors == [], f"Hash integrity errors: {errors}"

    def test_deterministic_cid_for_identical_content(self, bridge, ipfs_client):
        """
        Two identical GEE submissions (same script, params, value, timestamp)
        produce identical provenance record content and therefore the same CID.
        """
        fixed_ts = 1_720_000_000
        r1 = GEEResult(
            script_source=GEE_SCRIPT_V1,
            input_params=GEE_PARAMS,
            output_value=1_000_000,
            feed_id="repro/test",
            timestamp_utc=fixed_ts,
            script_asset_path=GEE_ASSET_PATH,
            script_version_tag=GEE_VERSION_TAG,
        )
        r2 = GEEResult(
            script_source=GEE_SCRIPT_V1,
            input_params=GEE_PARAMS,
            output_value=1_000_000,
            feed_id="repro/test",
            timestamp_utc=fixed_ts,
            script_asset_path=GEE_ASSET_PATH,
            script_version_tag=GEE_VERSION_TAG,
        )

        # Use a fixed submission time so records are identical
        import time
        with patch("oracle_bridge.bridge.time") as mock_time:
            mock_time.time.return_value = 1_720_001_000
            _, _, prov1 = bridge.process(r1)
            _, _, prov2 = bridge.process(r2)

        # Provenance records with identical content → same CID
        assert prov1.ipfs_cid == prov2.ipfs_cid, (
            "Identical submissions should produce the same provenance CID"
        )

    def test_different_script_version_different_cid(self, bridge, ipfs_client):
        """Changing the GEE script source changes the provenance CID."""
        script_v2 = GEE_SCRIPT_V1 + "\n// version bump"
        fixed_ts = 1_720_000_000

        r1 = GEEResult(
            script_source=GEE_SCRIPT_V1,
            input_params=GEE_PARAMS,
            output_value=1_000_000,
            feed_id="version/test",
            timestamp_utc=fixed_ts,
            script_asset_path=GEE_ASSET_PATH,
            script_version_tag="v1.0.0",
        )
        r2 = GEEResult(
            script_source=script_v2,
            input_params=GEE_PARAMS,
            output_value=1_000_000,
            feed_id="version/test",
            timestamp_utc=fixed_ts,
            script_asset_path=GEE_ASSET_PATH,
            script_version_tag="v1.0.1",
        )

        import time
        with patch("oracle_bridge.bridge.time") as mock_time:
            mock_time.time.return_value = 1_720_001_000
            _, _, prov1 = bridge.process(r1)
            _, _, prov2 = bridge.process(r2)

        assert prov1.ipfs_cid != prov2.ipfs_cid, (
            "Different script versions should produce different CIDs"
        )

    def test_from_dict_roundtrip(self, bridge, gee_result, ipfs_client):
        """ProvenanceRecord can be serialised and deserialised without loss."""
        _att, _tx_ref, prov = bridge.process(gee_result)

        # Serialise
        d = prov.to_dict()

        # Deserialise
        prov2 = ProvenanceRecord.from_dict(d)

        assert prov2.output_value == prov.output_value
        assert prov2.feed_id == prov.feed_id
        assert prov2.script_version_hash == prov.script_version_hash
        assert prov2.attestation_signature == prov.attestation_signature
        assert prov2.ipfs_cid == prov.ipfs_cid
