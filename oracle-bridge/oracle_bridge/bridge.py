"""
oracle_bridge.bridge
====================

High-level GEE oracle bridge:  fetches a GEE result, builds and signs an
attestation, and submits it to the carbon_oracle Soroban contract.

Supports both single-source and multi-source aggregated submissions.

This module is intentionally thin; the heavy lifting lives in
:mod:`oracle_bridge.attestation` and :mod:`oracle_bridge.aggregation`.
The Soroban submission client is injected as a dependency so the module
stays testable without a live network.
"""

from __future__ import annotations

import hashlib
import json
import time
from dataclasses import dataclass, asdict
from typing import Any, Protocol


from oracle_bridge.attestation import OracleSigner, SignedAttestation, sha256
from oracle_bridge.aggregation import (
    AggregationConfig,
    AggregationResult,
    PriceAggregator,
    PriceSource,
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
    """

    def __init__(
        self,
        script_source: str,
        input_params: dict[str, Any],
        output_value: int,
        feed_id: str | bytes,
        timestamp_utc: int | None = None,
    ) -> None:
        self.script_source = script_source
        self.input_params = input_params
        self.output_value = output_value
        self.feed_id = feed_id
        self.timestamp_utc: int = (
            timestamp_utc if timestamp_utc is not None else int(time.time())
        )

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
    """
    aggregate_value: int
    source_values: dict[str, int]
    weights_used: dict[str, float]
    rejected_sources: list[str]
    feed_id: str | bytes
    timestamp_utc: int | None = None
    outlier_method: str = "none"

    def __post_init__(self) -> None:
        if self.timestamp_utc is None:
            self.timestamp_utc = int(time.time())


# ── Bridge ────────────────────────────────────────────────────────────────────


class OracleBridge:
    """
    Orchestrates the full GEE → signed attestation → on-chain submission flow.

    Supports both single-source and multi-source aggregated submissions.

    Parameters
    ----------
    signer:
        An :class:`~oracle_bridge.attestation.OracleSigner` holding the
        oracle operator's Ed25519 private key.
    client:
        A :class:`SubmissionClient` implementation (e.g. a Soroban RPC wrapper).
    aggregation_config:
        Optional :class:`~oracle_bridge.aggregation.AggregationConfig` for
        multi-source aggregation. If provided, enables aggregate() method.
    """

    def __init__(
        self,
        signer: OracleSigner,
        client: SubmissionClient,
        aggregation_config: AggregationConfig | None = None,
    ) -> None:
        self._signer = signer
        self._client = client
        self._aggregation_config = aggregation_config

    def process(self, result: GEEResult) -> tuple[SignedAttestation, str]:
        """
        Sign *result* and submit the attestation (single-source).

        Returns
        -------
        (attestation, tx_ref)
            The :class:`~oracle_bridge.attestation.SignedAttestation` that was
            produced and the transaction reference returned by the client.
        """
        attestation = self._signer.attest(
            script_hash=result.script_hash,
            input_params=result.input_params,
            output_value=result.output_value,
            timestamp_utc=result.timestamp_utc,
            feed_id=result.feed_id,
        )
        tx_ref = self._client.submit_price(attestation)
        return attestation, tx_ref

    def aggregate_and_submit(
        self,
        per_source_results: dict[str, GEEResult],
    ) -> tuple[AggregatedPriceResult, SignedAttestation, str]:
        """
        Aggregate prices from multiple sources and submit.

        Parameters
        ----------
        per_source_results:
            Dict mapping source_id -> GEEResult from that source.

        Returns
        -------
        (aggregation_result, attestation, tx_ref)
            The aggregation result with per-source values, provenance metadata,
            the signed attestation of the aggregate, and the submission tx_ref.

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
        # Use first source's feed_id (all should be same for given aggregation)
        first_result = next(iter(per_source_results.values()))
        feed_id = first_result.feed_id

        # Use most recent timestamp
        timestamp_utc = max(
            result.timestamp_utc for result in per_source_results.values()
        )

        # Build provenance metadata (included in attestation hashing)
        provenance = {
            "method": agg_result.method_used,
            "outlier_method": agg_result.outlier_method,
            "sources": sorted(agg_result.source_values.keys()),
            "weights": agg_result.weights_used,
            "rejected_sources": agg_result.rejected_sources,
            "num_sources_accepted": len(agg_result.source_values),
            "num_sources_rejected": len(agg_result.rejected_sources),
        }

        # Create a synthetic GEE result representing the aggregate
        # The provenance is embedded in input_params for deterministic hashing
        synthetic_input_params = {
            **provenance,
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

        # Submit the aggregate attestation
        tx_ref = self._client.submit_price(attestation)

        # Build result with provenance
        result = AggregatedPriceResult(
            aggregate_value=agg_result.aggregate_value,
            source_values=agg_result.source_values,
            weights_used=agg_result.weights_used,
            rejected_sources=agg_result.rejected_sources,
            feed_id=feed_id,
            timestamp_utc=timestamp_utc,
            outlier_method=agg_result.outlier_method,
        )

        return result, attestation, tx_ref
