"""
oracle_bridge.attestation
=========================

Canonical serialisation and Ed25519 signing / verification of GEE oracle
attestation payloads.

Payload layout (113 bytes, all multi-byte integers big-endian):
  [1]  schema_version   : u8
  [32] script_hash      : bytes
  [32] input_params_hash: bytes
  [8]  output_value     : i64
  [8]  timestamp_utc    : i64
  [32] feed_id          : bytes

See docs/oracle/attestation-schema.md for the full specification.
"""

from __future__ import annotations

import hashlib
import json
import struct
from dataclasses import dataclass, field
from typing import Any

from cryptography.hazmat.primitives.asymmetric.ed25519 import (
    Ed25519PrivateKey,
    Ed25519PublicKey,
)
from cryptography.hazmat.primitives.serialization import (
    Encoding,
    NoEncryption,
    PrivateFormat,
    PublicFormat,
)

# ── Constants ─────────────────────────────────────────────────────────────────

SCHEMA_VERSION: int = 1
PAYLOAD_LENGTH: int = 113  # 1 + 32 + 32 + 8 + 8 + 32

# Struct format: 1-byte version, 32-byte script_hash, 32-byte input_params_hash,
# 8-byte signed big-endian int, 8-byte signed big-endian int, 32-byte feed_id.
_PACK_FMT: str = ">B32s32sqq32s"


# ── Helpers ───────────────────────────────────────────────────────────────────

def sha256(data: bytes) -> bytes:
    """Return the SHA-256 digest of *data*."""
    return hashlib.sha256(data).digest()


def canonical_params_hash(params: dict[str, Any]) -> bytes:
    """
    Produce the input_params_hash from an arbitrary dict.

    Keys are sorted lexicographically; the result is a compact UTF-8 JSON
    string with no whitespace whose SHA-256 is returned.
    """
    canonical = json.dumps(params, sort_keys=True, separators=(",", ":"))
    return sha256(canonical.encode("utf-8"))


def pad_feed_id(feed_id: str | bytes) -> bytes:
    """Zero-pad or truncate a feed identifier to exactly 32 bytes."""
    raw = feed_id.encode("utf-8") if isinstance(feed_id, str) else feed_id
    if len(raw) > 32:
        raise ValueError(f"feed_id exceeds 32 bytes: {len(raw)} bytes given")
    return raw.ljust(32, b"\x00")


# ── Payload ───────────────────────────────────────────────────────────────────

@dataclass
class AttestationPayload:
    """
    Represents the 113-byte signing payload for one GEE oracle submission.

    Attributes
    ----------
    script_hash:
        SHA-256 of the GEE JavaScript source (32 bytes).
    input_params_hash:
        SHA-256 of the canonical input-parameters JSON (32 bytes).
    output_value:
        Carbon sequestration value in micrograms CO₂-eq/m² (signed i64).
    timestamp_utc:
        Unix timestamp (seconds since epoch, UTC) of the computation (signed i64).
    feed_id:
        Zero-padded 32-byte feed identifier bytes.
    schema_version:
        Schema version byte; currently always ``1``.
    """

    script_hash: bytes
    input_params_hash: bytes
    output_value: int
    timestamp_utc: int
    feed_id: bytes
    schema_version: int = field(default=SCHEMA_VERSION)

    # ── Validation ────────────────────────────────────────────────────────────

    def __post_init__(self) -> None:
        if len(self.script_hash) != 32:
            raise ValueError("script_hash must be exactly 32 bytes")
        if len(self.input_params_hash) != 32:
            raise ValueError("input_params_hash must be exactly 32 bytes")
        if len(self.feed_id) != 32:
            raise ValueError("feed_id must be exactly 32 bytes")
        if not (-(2**63) <= self.output_value < 2**63):
            raise ValueError("output_value must fit in an i64")
        if not (-(2**63) <= self.timestamp_utc < 2**63):
            raise ValueError("timestamp_utc must fit in an i64")

    # ── Serialisation ─────────────────────────────────────────────────────────

    def to_bytes(self) -> bytes:
        """
        Return the 113-byte canonical binary representation.

        This is the exact byte string that is signed / verified.
        """
        packed = struct.pack(
            _PACK_FMT,
            self.schema_version,
            self.script_hash,
            self.input_params_hash,
            self.output_value,
            self.timestamp_utc,
            self.feed_id,
        )
        assert len(packed) == PAYLOAD_LENGTH, "BUG: unexpected payload length"
        return packed

    @classmethod
    def from_bytes(cls, data: bytes) -> "AttestationPayload":
        """Deserialise a 113-byte canonical payload back into an object."""
        if len(data) != PAYLOAD_LENGTH:
            raise ValueError(
                f"Expected {PAYLOAD_LENGTH} bytes, got {len(data)}"
            )
        (
            version,
            script_hash,
            input_params_hash,
            output_value,
            timestamp_utc,
            feed_id,
        ) = struct.unpack(_PACK_FMT, data)
        return cls(
            schema_version=version,
            script_hash=bytes(script_hash),
            input_params_hash=bytes(input_params_hash),
            output_value=output_value,
            timestamp_utc=timestamp_utc,
            feed_id=bytes(feed_id),
        )

    def to_dict(self) -> dict[str, Any]:
        """Return a JSON-friendly dict (hex-encoded byte fields)."""
        return {
            "schema_version": self.schema_version,
            "script_hash": self.script_hash.hex(),
            "input_params_hash": self.input_params_hash.hex(),
            "output_value": self.output_value,
            "timestamp_utc": self.timestamp_utc,
            "feed_id": self.feed_id.hex(),
        }


# ── Signer ────────────────────────────────────────────────────────────────────

class OracleSigner:
    """
    Signs attestation payloads using an Ed25519 private key.

    Parameters
    ----------
    private_key:
        An :class:`Ed25519PrivateKey` instance.  Generate with
        ``Ed25519PrivateKey.generate()`` or load from a seed.
    """

    def __init__(self, private_key: Ed25519PrivateKey) -> None:
        self._private_key = private_key
        self._public_key: Ed25519PublicKey = private_key.public_key()

    # ── Class-method constructors ─────────────────────────────────────────────

    @classmethod
    def generate(cls) -> "OracleSigner":
        """Generate a new ephemeral Ed25519 keypair (useful for testing)."""
        return cls(Ed25519PrivateKey.generate())

    @classmethod
    def from_seed(cls, seed: bytes) -> "OracleSigner":
        """
        Load from a 32-byte raw seed (the private key scalar).

        Use this when the key is stored in a secrets manager.
        """
        if len(seed) != 32:
            raise ValueError("Ed25519 seed must be exactly 32 bytes")
        return cls(Ed25519PrivateKey.from_private_bytes(seed))

    # ── Key export ────────────────────────────────────────────────────────────

    def public_key_bytes(self) -> bytes:
        """Return the 32-byte raw public key."""
        return self._public_key.public_bytes(Encoding.Raw, PublicFormat.Raw)

    def private_key_seed(self) -> bytes:
        """
        Return the 32-byte raw private key seed.

        Handle with care — never log or store in plaintext.
        """
        return self._private_key.private_bytes(
            Encoding.Raw, PrivateFormat.Raw, NoEncryption()
        )

    # ── Sign / attest ─────────────────────────────────────────────────────────

    def sign(self, payload: AttestationPayload) -> bytes:
        """
        Sign the canonical 113-byte payload and return the 64-byte signature.
        """
        message = payload.to_bytes()
        return self._private_key.sign(message)

    def attest(
        self,
        script_hash: bytes,
        input_params: dict[str, Any] | bytes,
        output_value: int,
        timestamp_utc: int,
        feed_id: str | bytes,
    ) -> "SignedAttestation":
        """
        Build, sign, and return a :class:`SignedAttestation`.

        Parameters
        ----------
        script_hash:
            SHA-256 of the GEE script source (32 bytes).
        input_params:
            Either a dict (will be canonicalised and hashed automatically) or
            the pre-computed 32-byte SHA-256 hash.
        output_value:
            Carbon sequestration result (i64).
        timestamp_utc:
            Unix timestamp of the GEE computation.
        feed_id:
            Feed identifier string or raw bytes (≤ 32 bytes).
        """
        if isinstance(input_params, dict):
            params_hash = canonical_params_hash(input_params)
        else:
            params_hash = input_params

        payload = AttestationPayload(
            script_hash=script_hash,
            input_params_hash=params_hash,
            output_value=output_value,
            timestamp_utc=timestamp_utc,
            feed_id=pad_feed_id(feed_id),
        )
        signature = self.sign(payload)
        return SignedAttestation(
            payload=payload,
            public_key=self.public_key_bytes(),
            signature=signature,
        )


# ── Verifier ──────────────────────────────────────────────────────────────────

class OracleVerifier:
    """
    Verifies Ed25519 attestation signatures (Python-side helper for tests and
    off-chain validation before submitting on-chain).

    Parameters
    ----------
    public_key_bytes:
        32-byte raw Ed25519 public key.
    """

    def __init__(self, public_key_bytes: bytes) -> None:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import (
            Ed25519PublicKey,
        )
        self._public_key = Ed25519PublicKey.from_public_bytes(public_key_bytes)

    def verify(self, attestation: "SignedAttestation") -> bool:
        """
        Return ``True`` if the signature is valid, ``False`` otherwise.

        Does **not** raise on invalid signatures — callers can decide whether
        to raise or handle silently.
        """
        try:
            self._public_key.verify(
                attestation.signature,
                attestation.payload.to_bytes(),
            )
            return True
        except Exception:
            return False


# ── SignedAttestation ─────────────────────────────────────────────────────────

@dataclass
class SignedAttestation:
    """
    A fully-signed attestation ready for on-chain submission.

    Attributes
    ----------
    payload:    The 113-byte attestation payload.
    public_key: The 32-byte raw Ed25519 public key used to sign.
    signature:  The 64-byte Ed25519 signature over ``payload.to_bytes()``.
    """

    payload: AttestationPayload
    public_key: bytes
    signature: bytes

    def __post_init__(self) -> None:
        if len(self.public_key) != 32:
            raise ValueError("public_key must be 32 bytes")
        if len(self.signature) != 64:
            raise ValueError("signature must be 64 bytes")

    def to_dict(self) -> dict[str, Any]:
        """JSON-friendly envelope (hex-encoded binary fields)."""
        d = self.payload.to_dict()
        d["public_key"] = self.public_key.hex()
        d["signature"] = self.signature.hex()
        return d

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SignedAttestation":
        """Reconstruct a :class:`SignedAttestation` from a hex-encoded dict."""
        payload = AttestationPayload(
            schema_version=data["schema_version"],
            script_hash=bytes.fromhex(data["script_hash"]),
            input_params_hash=bytes.fromhex(data["input_params_hash"]),
            output_value=data["output_value"],
            timestamp_utc=data["timestamp_utc"],
            feed_id=bytes.fromhex(data["feed_id"]),
        )
        return cls(
            payload=payload,
            public_key=bytes.fromhex(data["public_key"]),
            signature=bytes.fromhex(data["signature"]),
        )
