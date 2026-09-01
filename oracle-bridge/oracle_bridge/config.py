"""
oracle_bridge.config
=====================

Shared configuration constants for the oracle-bridge package.

These mirror on-chain limits so the bridge can fail fast, before spending a
transaction fee, rather than relying on the Soroban contract to reject an
invalid submission.
"""

from __future__ import annotations

# Maximum length of an IPFS CID string in bytes, mirroring
# ``MAX_CID_LEN`` in ``contracts/carbon_oracle/src/lib.rs``. CIDv1 Base32 is
# typically ≤59 bytes, so 64 leaves headroom while still catching malformed
# or corrupted CIDs before they reach the chain.
MAX_CID_LEN = 64
