"""oracle_bridge — GEE to Soroban carbon oracle attestation pipeline."""

from oracle_bridge.attestation import (
    AttestationPayload,
    OracleSigner,
    OracleVerifier,
    SignedAttestation,
    canonical_params_hash,
    pad_feed_id,
    sha256,
)

from oracle_bridge.resilience import (
    DeadLetterQueue,
    DegradedModeState,
    RetryConfig,
    compute_backoff,
    retry,
)

from oracle_bridge.adapters import (
    FeedAdapter,
    FeedAdapterConfig,
    FeedResult,
    ToucanProtocolAdapter,
    XpansivCBLAdapter,
)

__all__ = [
    "AttestationPayload",
    "OracleSigner",
    "OracleVerifier",
    "SignedAttestation",
    "canonical_params_hash",
    "pad_feed_id",
    "sha256",
    "DeadLetterQueue",
    "DegradedModeState",
    "RetryConfig",
    "compute_backoff",
    "retry",
    "FeedAdapter",
    "FeedAdapterConfig",
    "FeedResult",
    "ToucanProtocolAdapter",
    "XpansivCBLAdapter",
]
