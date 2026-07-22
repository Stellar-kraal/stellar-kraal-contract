"""
oracle_bridge.bridge
====================

High-level GEE oracle bridge:  fetches a GEE result, builds and signs an
attestation, pins a provenance record to IPFS, and submits to the
carbon_oracle Soroban contract.

Supports both single-source and multi-source aggregated submissions.

This module is intentionally thin; the heavy lifting lives in
:mod:`oracle_bridge.attestation`, :mod:`oracle_bridge.aggregation`,
:mod:`oracle_bridge.provenance`, and :mod:`oracle_bridge.ipfs`.
The Soroban submission client and IPFS client are injected as dependencies so
the module stays testable without a live network.
"""

from __future__ import annotations

import hashlib
import json
import logging
import time
from dataclasses import dataclass, asdict
from typing import Any, Protocol


from oracle_bridge.attestation import OracleSigner, SignedAttestation, sha256
logger = logging.getLogger(__name__)

from oracle_bridge.aggregation import (
    AggregationConfig,
    AggregationResult,
    PriceAggregator,
    PriceSource,
)
from oracle_bridge.provenance import (
    ProvenanceRecord,
    validate_provenance_record,
)
from oracle_bridge.ipfs import (
    SimulatedIPFSClient,
    LocalIPFSClient,
    pin_provenance_record,
    get_ipfs_client,
)


# ── Submission client protocol ────────────────────────────────────────────────


class SubmissionClient(Protocol):
    """Interface that concrete Soroban clients must satisfy."""

    def submit_price(self, attestation: SignedAttestation) -> str:
        """
        Submit a signed attestation to the on-chain oracle.

        Returns the transaction hash / ledger reference.
        """
        ...

    def submit_price_with_cid(
        self, attestation: SignedAttestation, ipfs_cid: str
    ) -> str:
        """
        Submit a signed attestation together with the IPFS CID of its
        provenance record.

        Returns the transaction hash / ledger reference.
        Falls back to ``submit_price`` if not implemented by the client.
        """
        ...

    def commit_price(self, feed_id: str | bytes, commitment_hash: bytes) -> str:
        """Submit a price commitment."""
        ...

    def reveal_price(self, attestation: SignedAttestation, salt: bytes) -> str:
        """Submit a price reveal."""
        ...


# ── GEE result model ──────────────────────────────────────────────────────────


class GEEResult:
    """
    Encapsulates the output of a single GEE script run.

    Parameters
    ----------
    script_source:
        Full source text of the GEE JavaScript.  The SHA-256 is stored in the
        attestation so the on-chain record is auditable.
    input_params:
        Dict of input parameters forwarded to the GEE script.
    output_value:
        Carbon sequestration value returned by the script (i64 micrograms CO₂-eq/m²).
    feed_id:
        Feed / asset identifier (str ≤ 32 bytes or raw bytes).
    timestamp_utc:
        Unix timestamp of the computation.  Defaults to ``time.time()`` if
        ``None``.
    script_asset_path:
        GEE asset path or identifier (e.g. 'users/org/scripts/carbon_v1').
        Defaults to a placeholder.
    script_version_tag:
        Human-readable version tag for the GEE script (e.g. 'v1.0.0').
        Defaults to 'untagged'.
    """

    def __init__(
        self,
        script_source: str,
        input_params: dict[str, Any],
        output_value: int,
        feed_id: str | bytes,
        timestamp_utc: int | None = None,
        script_asset_path: str = "users/oracle/scripts/carbon_sequestration",
        script_version_tag: str = "untagged",
    ) -> None:
        self.script_source = script_source
        self.input_params = input_params
        self.output_value = output_value
        self.feed_id = feed_id
        self.timestamp_utc: int = (
            timestamp_utc if timestamp_utc is not None else int(time.time())
        )
        self.script_asset_path = script_asset_path
        self.script_version_tag = script_version_tag

    @property
    def script_hash(self) -> bytes:
        """SHA-256 of the GEE JavaScript source (32 bytes)."""
        return sha256(self.script_source.encode("utf-8"))


# ── Multi-source aggregated result model ──────────────────────────────────────


@dataclass
class AggregatedPriceResult:
    """
    Encapsulates the output of multi-source price aggregation.

    Parameters
    ----------
    aggregate_value:
        The computed aggregate price (weighted median).
    source_values:
        Dict mapping source_id -> individual price value.
    weights_used:
        Dict mapping source_id -> weight applied.
    rejected_sources:
        List of source IDs rejected as outliers.
    feed_id:
        Feed / asset identifier.
    timestamp_utc:
        Unix timestamp of aggregation (defaults to now).
    outlier_method:
        Name of outlier rejection method used (e.g., "iqr", "mad", "none").
    ipfs_cid:
        IPFS CID of the provenance record (set after pinning).
    """
    aggregate_value: int
    source_values: dict[str, int]
    weights_used: dict[str, float]
    rejected_sources: list[str]
    feed_id: str | bytes
    timestamp_utc: int | None = None
    outlier_method: str = "none"
    ipfs_cid: str | None = None

    def __post_init__(self) -> None:
        if self.timestamp_utc is None:
            self.timestamp_utc = int(time.time())


# ── Bridge ────────────────────────────────────────────────────────────────────


class OracleBridge:
    """
    Orchestrates the full GEE → signed attestation → IPFS provenance → on-chain
    submission flow.

    Supports both single-source and multi-source aggregated submissions.

    On every submission the bridge:
    1. Signs the GEE result into a :class:`~oracle_bridge.attestation.SignedAttestation`.
    2. Builds a :class:`~oracle_bridge.provenance.ProvenanceRecord` capturing
       the GEE script version hash, input parameters, output, and attestation.
    3. Validates the record against the JSON Schema.
    4. Pins the record to IPFS via the injected :class:`~oracle_bridge.ipfs.IPFSClient`.
    5. Submits the attestation (plus CID) to the on-chain oracle via the
       injected :class:`SubmissionClient`.

    Parameters
    ----------
    signer:
        An :class:`~oracle_bridge.attestation.OracleSigner` holding the
        oracle operator's Ed25519 private key.
    client:
        A :class:`SubmissionClient` implementation (e.g. a Soroban RPC wrapper).
    ipfs_client:
        An IPFS client instance.  Defaults to :class:`SimulatedIPFSClient`.
    aggregation_config:
        Optional :class:`~oracle_bridge.aggregation.AggregationConfig` for
        multi-source aggregation. If provided, enables aggregate() method.
    """

    def __init__(
        self,
        signer: OracleSigner,
        client: SubmissionClient,
        ipfs_client: SimulatedIPFSClient | LocalIPFSClient | None = None,
        aggregation_config: AggregationConfig | None = None,
    ) -> None:
        self._signer = signer
        self._client = client
        self._ipfs = ipfs_client if ipfs_client is not None else get_ipfs_client()
        self._aggregation_config = aggregation_config

    def _submit_with_circuit_breaker_alert(
        self, attestation: SignedAttestation
    ) -> str:
        """Submit a price, logging an error if the circuit breaker trips."""
        try:
            return self._client.submit_price(attestation)
        except Exception as exc:
            message = str(exc)
            if "CircuitBreaker" in message or "#13" in message or "#14" in message or "#15" in message:
                logger.error(
                    "oracle_circuit_breaker_tripped",
                    extra={
                        "event": "oracle_circuit_breaker_tripped",
                        "feed_id": attestation.payload.feed_id.hex(),
                        "timestamp_utc": attestation.payload.timestamp_utc,
                        "output_value": attestation.payload.output_value,
                        "reason": message,
                    },
                )
            raise

    def process(
        self, result: GEEResult
    ) -> tuple[SignedAttestation, str, ProvenanceRecord]:
        """
        Sign *result*, pin a provenance record to IPFS, and submit on-chain.

        Returns
        -------
        (attestation, tx_ref, provenance_record)
            The signed attestation, the on-chain transaction reference, and
            the provenance record (with ``ipfs_cid`` set after pinning).
        """
        # 1. Build attestation
        attestation = self._signer.attest(
            script_hash=result.script_hash,
            input_params=result.input_params,
            output_value=result.output_value,
            timestamp_utc=result.timestamp_utc,
            feed_id=result.feed_id,
        )

        # 2. Build provenance record
        submitted_at = int(time.time())
        provenance = ProvenanceRecord(
            script_asset_path=result.script_asset_path,
            script_version_hash=result.script_hash.hex(),
            script_version_tag=result.script_version_tag,
            input_params=result.input_params,
            output_value=result.output_value,
            feed_id=result.feed_id if isinstance(result.feed_id, str)
                    else result.feed_id.decode("utf-8").rstrip("\x00"),
            timestamp_utc=result.timestamp_utc,
            attestation_public_key=attestation.public_key.hex(),
            attestation_signature=attestation.signature.hex(),
            attestation_payload_bytes=attestation.payload.to_bytes(),
            record_type="single",
            script_source_preview=result.script_source[:512],
            submitted_at_utc=submitted_at,
        )

        # 3. Validate against schema
        record_dict = provenance.to_dict()
        validate_provenance_record(record_dict)

        # 4. Pin to IPFS
        ipfs_cid = pin_provenance_record(self._ipfs, record_dict)
        provenance.ipfs_cid = ipfs_cid

        # 5. Submit on-chain (with CID if client supports it)
        tx_ref = self._submit_with_cid(attestation, ipfs_cid)
        provenance.tx_ref = tx_ref

        return attestation, tx_ref, provenance

    def _submit_with_cid(
        self, attestation: SignedAttestation, ipfs_cid: str
    ) -> str:
        """Try submit_price_with_cid first; fall back to submit_price."""
        try:
            return self._client.submit_price_with_cid(attestation, ipfs_cid)
        except AttributeError:
            return self._submit_with_circuit_breaker_alert(attestation)

    def commit(self, result: GEEResult) -> tuple[bytes, str]:
        """
        Commit to a result by computing its commitment hash and submitting it.
        Generates a random 32-byte salt.

        Returns
        -------
        (salt, tx_ref)
            The generated salt and the transaction reference.
        """
        import os
        from oracle_bridge.attestation import canonical_params_hash, pad_feed_id

        salt = os.urandom(32)
        feed_id = pad_feed_id(result.feed_id)
        input_params_hash = canonical_params_hash(result.input_params)

        payload = b''
        payload += result.script_hash
        payload += input_params_hash
        payload += result.output_value.to_bytes(8, byteorder='big', signed=True)
        payload += result.timestamp_utc.to_bytes(8, byteorder='big', signed=True)
        payload += feed_id
        payload += salt

        commitment_hash = sha256(payload)
        tx_ref = self._client.commit_price(result.feed_id, commitment_hash)

        return salt, tx_ref

    def reveal(self, result: GEEResult, salt: bytes) -> tuple[SignedAttestation, str]:
        """
        Reveal a previously committed result.

        Returns
        -------
        (attestation, tx_ref)
            The signed attestation that was produced and the transaction reference.
        """
        attestation = self._signer.attest(
            script_hash=result.script_hash,
            input_params=result.input_params,
            output_value=result.output_value,
            timestamp_utc=result.timestamp_utc,
            feed_id=result.feed_id,
        )
        tx_ref = self._client.reveal_price(attestation, salt)
        return attestation, tx_ref

    def aggregate_and_submit(
        self,
        per_source_results: dict[str, GEEResult],
    ) -> tuple[AggregatedPriceResult, SignedAttestation, str, ProvenanceRecord]:
        """
        Aggregate prices from multiple sources, pin provenance to IPFS, and submit.

        Parameters
        ----------
        per_source_results:
            Dict mapping source_id -> GEEResult from that source.

        Returns
        -------
        (aggregation_result, attestation, tx_ref, provenance_record)
            The aggregation result with per-source values, the signed attestation,
            the submission tx_ref, and the provenance record with IPFS CID.

        Raises
        ------
        ValueError:
            If aggregation_config is not configured.
        """
        if not self._aggregation_config:
            raise ValueError(
                "aggregation_config not configured; cannot aggregate"
            )

        # Extract price sources from per-source results
        sources = [
            PriceSource(
                source_id=source_id,
                value=result.output_value,
                weight=self._aggregation_config.weights[source_id],
                metadata={"feed_id": result.feed_id, "script_hash": result.script_hash.hex()},
            )
            for source_id, result in per_source_results.items()
        ]

        # Perform aggregation
        aggregator = PriceAggregator(self._aggregation_config)
        agg_result = aggregator.aggregate(sources)

        # Determine feed_id and timestamp from sources
        first_result = next(iter(per_source_results.values()))
        feed_id = first_result.feed_id

        # Use most recent timestamp
        timestamp_utc = max(
            result.timestamp_utc for result in per_source_results.values()
        )

        # Build provenance metadata
        provenance_agg_meta: dict[str, Any] = {
            "method": agg_result.method_used,
            "outlier_method": agg_result.outlier_method,
            "sources": sorted(agg_result.source_values.keys()),
            "weights": agg_result.weights_used,
            "rejected_sources": agg_result.rejected_sources,
            "num_sources_accepted": len(agg_result.source_values),
            "num_sources_rejected": len(agg_result.rejected_sources),
        }

        # Create a synthetic GEE result representing the aggregate
        synthetic_input_params = {
            **provenance_agg_meta,
            "per_source_values": agg_result.source_values,
        }

        # Sign the aggregate with provenance metadata
        attestation = self._signer.attest(
            script_hash=sha256(b"aggregated"),  # Marker for aggregated submissions
            input_params=synthetic_input_params,
            output_value=agg_result.aggregate_value,
            timestamp_utc=timestamp_utc,
            feed_id=feed_id,
        )

        # Build aggregation provenance block for schema compliance
        aggregation_block = {
            "method": agg_result.method_used,
            "outlier_method": agg_result.outlier_method,
            "source_values": agg_result.source_values,
            "weights_used": agg_result.weights_used,
            "rejected_sources": agg_result.rejected_sources,
        }

        # Build provenance record
        submitted_at = int(time.time())
        feed_id_str = feed_id if isinstance(feed_id, str) else feed_id.decode("utf-8").rstrip("\x00")
        provenance = ProvenanceRecord(
            script_asset_path=first_result.script_asset_path,
            script_version_hash=sha256(b"aggregated").hex(),
            script_version_tag="aggregated",
            input_params=synthetic_input_params,
            output_value=agg_result.aggregate_value,
            feed_id=feed_id_str,
            timestamp_utc=timestamp_utc,
            attestation_public_key=attestation.public_key.hex(),
            attestation_signature=attestation.signature.hex(),
            attestation_payload_bytes=attestation.payload.to_bytes(),
            record_type="aggregated",
            submitted_at_utc=submitted_at,
            aggregation_metadata=aggregation_block,
        )

        # Validate and pin
        record_dict = provenance.to_dict()
        validate_provenance_record(record_dict)
        ipfs_cid = pin_provenance_record(self._ipfs, record_dict)
        provenance.ipfs_cid = ipfs_cid

        # Submit the aggregate attestation with CID
        tx_ref = self._submit_with_cid(attestation, ipfs_cid)
        provenance.tx_ref = tx_ref

        # Build result with provenance
        result = AggregatedPriceResult(
            aggregate_value=agg_result.aggregate_value,
            source_values=agg_result.source_values,
            weights_used=agg_result.weights_used,
            rejected_sources=agg_result.rejected_sources,
            feed_id=feed_id,
            timestamp_utc=timestamp_utc,
            outlier_method=agg_result.outlier_method,
            ipfs_cid=ipfs_cid,
        )

        return result, attestation, tx_ref, provenance
