"""
tests/test_bridge_cid_validation.py
====================================

Covers issue #131: the bridge must reject an oversized/malformed IPFS CID
*before* calling ``submit_price_with_cid``, rather than letting the Soroban
contract fail the transaction with ``CidTooLong`` after a fee has already
been spent.
"""

from __future__ import annotations

import pytest

from oracle_bridge.attestation import OracleSigner
from oracle_bridge.bridge import GEEResult, OracleBridge
from oracle_bridge.config import MAX_CID_LEN
from oracle_bridge.ipfs import SimulatedIPFSClient


GEE_SCRIPT = "var x = ee.Image('x'); return x;"
GEE_PARAMS = {"aoi": "POLYGON((0 0,0 1,1 1,1 0,0 0))"}


class RecordingClient:
    """Minimal SubmissionClient that records whether submission was attempted."""

    def __init__(self) -> None:
        self.submitted = False

    def submit_price_with_cid(self, attestation, ipfs_cid: str) -> str:
        self.submitted = True
        return "tx_0001"

    def submit_price(self, attestation) -> str:
        self.submitted = True
        return "tx_0001"


class FixedCIDIPFSClient:
    """Fake IPFS client that always returns a pre-set CID, valid or not."""

    def __init__(self, cid: str) -> None:
        self._cid = cid

    def pin(self, content: bytes) -> str:
        return self._cid

    def get(self, cid: str) -> bytes:
        raise KeyError(cid)


@pytest.fixture()
def signer() -> OracleSigner:
    return OracleSigner.generate()


@pytest.fixture()
def gee_result() -> GEEResult:
    return GEEResult(
        script_source=GEE_SCRIPT,
        input_params=GEE_PARAMS,
        output_value=1_000_000,
        feed_id="carbon/cid-validation/test",
        timestamp_utc=1_720_000_000,
    )


# ── Unit tests: OracleBridge._validate_cid ────────────────────────────────────


class TestValidateCidUnit:
    def test_valid_46_char_cid_passes(self, signer):
        client = RecordingClient()
        bridge = OracleBridge(signer=signer, client=client, ipfs_client=SimulatedIPFSClient())

        valid_cid = "bafkrei" + "a" * 39  # matches SimulatedIPFSClient's 46-char format
        assert len(valid_cid) == 46

        # Should not raise.
        bridge._validate_cid(valid_cid)

    def test_cid_exactly_at_max_len_passes(self, signer):
        client = RecordingClient()
        bridge = OracleBridge(signer=signer, client=client, ipfs_client=SimulatedIPFSClient())

        cid_at_limit = "Q" * MAX_CID_LEN
        assert len(cid_at_limit) == MAX_CID_LEN

        bridge._validate_cid(cid_at_limit)  # should not raise

    def test_cid_over_max_len_raises_value_error_referencing_max_cid_len(self, signer):
        client = RecordingClient()
        bridge = OracleBridge(signer=signer, client=client, ipfs_client=SimulatedIPFSClient())

        oversized_cid = "Q" * (MAX_CID_LEN + 1)
        assert len(oversized_cid) == 65

        with pytest.raises(ValueError, match="MAX_CID_LEN"):
            bridge._validate_cid(oversized_cid)

    def test_empty_cid_raises_value_error(self, signer):
        client = RecordingClient()
        bridge = OracleBridge(signer=signer, client=client, ipfs_client=SimulatedIPFSClient())

        with pytest.raises(ValueError):
            bridge._validate_cid("")


# ── Integration tests: through OracleBridge.process() ─────────────────────────


class TestProcessRejectsOversizedCid:
    def test_process_raises_before_submission_when_cid_too_long(self, signer, gee_result):
        """
        An oversized CID (as would come back from a misbehaving/corrupted IPFS
        pin) must be rejected *before* submit_price_with_cid is called, so no
        transaction fee is wasted.
        """
        oversized_cid = "Q" * (MAX_CID_LEN + 1)
        client = RecordingClient()
        bridge = OracleBridge(
            signer=signer,
            client=client,
            ipfs_client=FixedCIDIPFSClient(oversized_cid),
        )

        with pytest.raises(ValueError, match="MAX_CID_LEN"):
            bridge.process(gee_result)

        assert client.submitted is False, (
            "submit_price_with_cid must not be called for an oversized CID"
        )

    def test_process_succeeds_with_valid_length_cid(self, signer, gee_result):
        valid_cid = "bafkrei" + "b" * 39
        assert len(valid_cid) == 46

        client = RecordingClient()
        bridge = OracleBridge(
            signer=signer,
            client=client,
            ipfs_client=FixedCIDIPFSClient(valid_cid),
        )

        _attestation, tx_ref, provenance = bridge.process(gee_result)

        assert tx_ref == "tx_0001"
        assert provenance.ipfs_cid == valid_cid
        assert client.submitted is True
