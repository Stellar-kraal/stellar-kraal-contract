"""
oracle_bridge.bridge
====================

High-level GEE oracle bridge:  fetches a GEE result, builds and signs an
attestation, and submits it to the carbon_oracle Soroban contract.

This module is intentionally thin; the heavy lifting lives in
:mod:`oracle_bridge.attestation`.  The Soroban submission client is injected
as a dependency so the module stays testable without a live network.
"""

from __future__ import annotations

import hashlib
import json
import time
from typing import Any, Protocol


from oracle_bridge.attestation import OracleSigner, SignedAttestation, sha256


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


# ── Bridge ────────────────────────────────────────────────────────────────────


class OracleBridge:
    """
    Orchestrates the full GEE → signed attestation → on-chain submission flow.

    Parameters
    ----------
    signer:
        An :class:`~oracle_bridge.attestation.OracleSigner` holding the
        oracle operator's Ed25519 private key.
    client:
        A :class:`SubmissionClient` implementation (e.g. a Soroban RPC wrapper).
    """

    def __init__(self, signer: OracleSigner, client: SubmissionClient) -> None:
        self._signer = signer
        self._client = client

    def process(self, result: GEEResult) -> tuple[SignedAttestation, str]:
        """
        Sign *result* and submit the attestation.

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
