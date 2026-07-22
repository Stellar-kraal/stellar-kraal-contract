"""
oracle_bridge.provenance
========================

Provenance record schema definition and JSON Schema validation for GEE oracle
submissions.

Each oracle submission produces a :class:`ProvenanceRecord` that captures:
- The GEE script asset path and exact version hash (SHA-256 of source).
- The input parameter set and its canonical hash.
- The computation output, feed id, and timestamp.
- The signed attestation payload.
- IPFS submission metadata (CID recorded after pinning).

The record is validated against the JSON Schema defined here before pinning
to IPFS, ensuring that all required reproducibility fields are present.

See ``docs/oracle/provenance-schema.md`` for the full specification.
"""

from __future__ import annotations

import hashlib
import json
import struct
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from typing import Any

# ── JSON Schema ───────────────────────────────────────────────────────────────

PROVENANCE_SCHEMA: dict[str, Any] = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "$id": "https://stellarkraal.example.com/schemas/provenance-record/v1.json",
    "title": "GEE Oracle Provenance Record",
    "description": (
        "Reproducibility record for a GEE oracle submission. "
        "Stored on IPFS; CID recorded on-chain."
    ),
    "type": "object",
    "required": [
        "schema_version",
        "record_type",
        "gee_script",
        "input_params",
        "computation",
        "attestation",
        "submission",
    ],
    "additionalProperties": False,
    "properties": {
        "schema_version": {"type": "integer", "const": 1},
        "record_type": {"type": "string", "enum": ["single", "aggregated"]},
        "gee_script": {
            "type": "object",
            "required": ["asset_path", "version_hash", "version_tag"],
            "additionalProperties": False,
            "properties": {
                "asset_path": {"type": "string", "minLength": 1},
                "version_hash": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "version_tag": {"type": "string", "minLength": 1},
                "source_preview": {"type": "string", "maxLength": 512},
            },
        },
        "input_params": {
            "type": "object",
            "required": ["params", "params_hash"],
            "additionalProperties": False,
            "properties": {
                "params": {"type": "object", "additionalProperties": True},
                "params_hash": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
            },
        },
        "computation": {
            "type": "object",
            "required": ["output_value", "feed_id", "timestamp_utc", "timestamp_iso"],
            "additionalProperties": False,
            "properties": {
                "output_value": {"type": "integer"},
                "feed_id": {"type": "string", "maxLength": 32},
                "timestamp_utc": {"type": "integer", "minimum": 0},
                "timestamp_iso": {
                    "type": "string",
                    "pattern": r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$",
                },
            },
        },
        "attestation": {
            "type": "object",
            "required": ["schema_version", "public_key", "signature", "payload_hex"],
            "additionalProperties": False,
            "properties": {
                "schema_version": {"type": "integer", "const": 1},
                "public_key": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                "signature": {"type": "string", "pattern": "^[0-9a-f]{128}$"},
                "payload_hex": {"type": "string", "pattern": "^[0-9a-f]{226}$"},
            },
        },
        "submission": {
            "type": "object",
            "required": ["submitted_at_iso", "submitted_at_utc"],
            "additionalProperties": False,
            "properties": {
                "submitted_at_iso": {
                    "type": "string",
                    "pattern": r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}Z$",
                },
                "submitted_at_utc": {"type": "integer", "minimum": 0},
                "tx_ref": {"type": "string"},
                "ipfs_cid": {"type": "string", "minLength": 10},
            },
        },
        "aggregation": {
            "type": "object",
            "required": [
                "method",
                "outlier_method",
                "source_values",
                "weights_used",
                "rejected_sources",
            ],
            "additionalProperties": False,
            "properties": {
                "method": {"type": "string", "minLength": 1},
                "outlier_method": {"type": "string", "enum": ["iqr", "mad", "none"]},
                "source_values": {
                    "type": "object",
                    "additionalProperties": {"type": "integer"},
                },
                "weights_used": {
                    "type": "object",
                    "additionalProperties": {"type": "number", "exclusiveMinimum": 0},
                },
                "rejected_sources": {
                    "type": "array",
                    "items": {"type": "string"},
                },
            },
        },
    },
}


# ── Helpers ───────────────────────────────────────────────────────────────────


def _utc_iso(ts: int) -> str:
    """Convert a Unix timestamp to an ISO 8601 UTC string (e.g. '2024-07-04T00:00:00Z')."""
    return datetime.fromtimestamp(ts, tz=timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _canonical_params_hash(params: dict[str, Any]) -> str:
    """Return the hex SHA-256 of the canonical JSON representation of params."""
    canonical = json.dumps(params, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(canonical.encode("utf-8")).hexdigest()


def _build_canonical_payload(
    schema_version: int,
    script_hash_bytes: bytes,
    input_params_hash_bytes: bytes,
    output_value: int,
    timestamp_utc: int,
    feed_id_bytes: bytes,
) -> bytes:
    """Build the 113-byte canonical attestation payload (matches Rust contract)."""
    buf = struct.pack(
        ">B32s32sqq32s",
        schema_version,
        script_hash_bytes,
        input_params_hash_bytes,
        output_value,
        timestamp_utc,
        feed_id_bytes,
    )
    assert len(buf) == 113
    return buf


# ── Provenance Record ─────────────────────────────────────────────────────────


@dataclass
class ProvenanceRecord:
    """
    Full provenance record for a GEE oracle submission.

    Parameters
    ----------
    script_asset_path:
        GEE asset path or identifier (e.g. 'users/org/scripts/carbon_v1').
    script_version_hash:
        Hex SHA-256 of the exact GEE script source.
    script_version_tag:
        Human-readable version tag (e.g. 'v1.2.3').
    input_params:
        Dict of input parameters passed to the GEE script.
    output_value:
        Carbon sequestration result (i64).
    feed_id:
        Feed identifier string (≤ 32 bytes).
    timestamp_utc:
        Unix timestamp of the GEE computation.
    attestation_public_key:
        Hex-encoded 32-byte Ed25519 public key.
    attestation_signature:
        Hex-encoded 64-byte Ed25519 signature.
    attestation_payload_bytes:
        The exact 113-byte payload that was signed.
    record_type:
        'single' or 'aggregated'.
    script_source_preview:
        Optional first 512 chars of script source.
    aggregation_metadata:
        Optional aggregation details (required when record_type=='aggregated').
    submitted_at_utc:
        Unix timestamp when the record was submitted. Defaults to now.
    tx_ref:
        Optional on-chain transaction reference.
    ipfs_cid:
        IPFS CID (set after pinning).
    """

    script_asset_path: str
    script_version_hash: str  # hex SHA-256
    script_version_tag: str
    input_params: dict[str, Any]
    output_value: int
    feed_id: str
    timestamp_utc: int
    attestation_public_key: str  # hex
    attestation_signature: str  # hex
    attestation_payload_bytes: bytes
    record_type: str = "single"
    script_source_preview: str | None = None
    aggregation_metadata: dict[str, Any] | None = None
    submitted_at_utc: int = field(default_factory=lambda: int(__import__("time").time()))
    tx_ref: str | None = None
    ipfs_cid: str | None = None

    def to_dict(self) -> dict[str, Any]:
        """Serialise to a JSON-compatible dict matching the provenance schema."""
        params_hash = _canonical_params_hash(self.input_params)

        gee_script: dict[str, Any] = {
            "asset_path": self.script_asset_path,
            "version_hash": self.script_version_hash,
            "version_tag": self.script_version_tag,
        }
        if self.script_source_preview:
            gee_script["source_preview"] = self.script_source_preview[:512]

        submission: dict[str, Any] = {
            "submitted_at_iso": _utc_iso(self.submitted_at_utc),
            "submitted_at_utc": self.submitted_at_utc,
        }
        if self.tx_ref:
            submission["tx_ref"] = self.tx_ref
        if self.ipfs_cid:
            submission["ipfs_cid"] = self.ipfs_cid

        record: dict[str, Any] = {
            "schema_version": 1,
            "record_type": self.record_type,
            "gee_script": gee_script,
            "input_params": {
                "params": self.input_params,
                "params_hash": params_hash,
            },
            "computation": {
                "output_value": self.output_value,
                "feed_id": self.feed_id,
                "timestamp_utc": self.timestamp_utc,
                "timestamp_iso": _utc_iso(self.timestamp_utc),
            },
            "attestation": {
                "schema_version": 1,
                "public_key": self.attestation_public_key,
                "signature": self.attestation_signature,
                "payload_hex": self.attestation_payload_bytes.hex(),
            },
            "submission": submission,
        }

        if self.record_type == "aggregated" and self.aggregation_metadata:
            record["aggregation"] = self.aggregation_metadata

        return record

    def to_json(self, indent: int = 2) -> str:
        """Return the provenance record as a formatted JSON string."""
        return json.dumps(self.to_dict(), indent=indent, sort_keys=False)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ProvenanceRecord":
        """Reconstruct a ProvenanceRecord from a JSON-compatible dict."""
        sub = data["submission"]
        att = data["attestation"]
        comp = data["computation"]
        gs = data["gee_script"]
        ip = data["input_params"]

        return cls(
            script_asset_path=gs["asset_path"],
            script_version_hash=gs["version_hash"],
            script_version_tag=gs["version_tag"],
            script_source_preview=gs.get("source_preview"),
            input_params=ip["params"],
            output_value=comp["output_value"],
            feed_id=comp["feed_id"],
            timestamp_utc=comp["timestamp_utc"],
            attestation_public_key=att["public_key"],
            attestation_signature=att["signature"],
            attestation_payload_bytes=bytes.fromhex(att["payload_hex"]),
            record_type=data["record_type"],
            aggregation_metadata=data.get("aggregation"),
            submitted_at_utc=sub["submitted_at_utc"],
            tx_ref=sub.get("tx_ref"),
            ipfs_cid=sub.get("ipfs_cid"),
        )

    def verify_hashes(self) -> list[str]:
        """
        Verify internal hash consistency.

        Returns a list of error strings; empty list means all checks passed.
        """
        errors: list[str] = []

        # Check params_hash matches params
        expected_hash = _canonical_params_hash(self.input_params)
        record_dict = self.to_dict()
        stored_hash = record_dict["input_params"]["params_hash"]
        if stored_hash != expected_hash:
            errors.append(
                f"params_hash mismatch: stored={stored_hash}, computed={expected_hash}"
            )

        # Check payload_hex length
        if len(self.attestation_payload_bytes) != 113:
            errors.append(
                f"payload length is {len(self.attestation_payload_bytes)}, expected 113"
            )

        # Check payload contains correct output_value and timestamp
        if len(self.attestation_payload_bytes) == 113:
            payload = self.attestation_payload_bytes
            stored_ov = int.from_bytes(payload[65:73], "big", signed=True)
            stored_ts = int.from_bytes(payload[73:81], "big", signed=True)
            if stored_ov != self.output_value:
                errors.append(
                    f"payload output_value mismatch: payload={stored_ov}, record={self.output_value}"
                )
            if stored_ts != self.timestamp_utc:
                errors.append(
                    f"payload timestamp mismatch: payload={stored_ts}, record={self.timestamp_utc}"
                )

        return errors


# ── Schema validation ─────────────────────────────────────────────────────────


class ProvenanceValidationError(ValueError):
    """Raised when a provenance record fails JSON Schema validation."""

    def __init__(self, errors: list[str]) -> None:
        self.validation_errors = errors
        super().__init__(f"Provenance record validation failed: {errors}")


def validate_provenance_record(record: dict[str, Any]) -> None:
    """
    Validate a provenance record dict against the JSON Schema.

    Always runs the lightweight structural validator (which handles conditional
    checks like aggregation block requirement).  Additionally runs jsonschema
    if the library is available for stricter structural validation.

    Raises
    ------
    ProvenanceValidationError
        If the record does not conform to the schema.
    """
    # Lightweight validation always runs first — it handles conditional
    # requirements (e.g. aggregation block required when record_type=aggregated)
    # that JSON Schema draft-2020-12 if/then would otherwise need.
    errors: list[str] = _lightweight_validate(record)
    if errors:
        raise ProvenanceValidationError(errors)

    # Additional structural validation with jsonschema if available.
    try:
        import jsonschema  # type: ignore[import]

        validator = jsonschema.Draft202012Validator(PROVENANCE_SCHEMA)
        schema_errors = [str(e.message) for e in validator.iter_errors(record)]
        if schema_errors:
            raise ProvenanceValidationError(schema_errors)
    except ImportError:
        pass  # Already validated with lightweight above


def _lightweight_validate(record: dict[str, Any]) -> list[str]:
    """Minimal validation without jsonschema library."""
    errors: list[str] = []

    required_top = ["schema_version", "record_type", "gee_script", "input_params",
                    "computation", "attestation", "submission"]
    for key in required_top:
        if key not in record:
            errors.append(f"Missing required field: {key}")

    if record.get("schema_version") != 1:
        errors.append("schema_version must be 1")

    if record.get("record_type") not in ("single", "aggregated"):
        errors.append("record_type must be 'single' or 'aggregated'")

    gs = record.get("gee_script", {})
    for k in ["asset_path", "version_hash", "version_tag"]:
        if k not in gs:
            errors.append(f"Missing gee_script.{k}")

    ip = record.get("input_params", {})
    for k in ["params", "params_hash"]:
        if k not in ip:
            errors.append(f"Missing input_params.{k}")

    comp = record.get("computation", {})
    for k in ["output_value", "feed_id", "timestamp_utc", "timestamp_iso"]:
        if k not in comp:
            errors.append(f"Missing computation.{k}")

    att = record.get("attestation", {})
    for k in ["schema_version", "public_key", "signature", "payload_hex"]:
        if k not in att:
            errors.append(f"Missing attestation.{k}")
    if "public_key" in att and len(att["public_key"]) != 64:
        errors.append("attestation.public_key must be 64 hex chars")
    if "signature" in att and len(att["signature"]) != 128:
        errors.append("attestation.signature must be 128 hex chars")
    if "payload_hex" in att and len(att["payload_hex"]) != 226:
        errors.append("attestation.payload_hex must be 226 hex chars")

    sub = record.get("submission", {})
    for k in ["submitted_at_iso", "submitted_at_utc"]:
        if k not in sub:
            errors.append(f"Missing submission.{k}")

    record_type = record.get("record_type")
    if record_type == "aggregated":
        agg = record.get("aggregation")
        if agg is None:
            errors.append("aggregation block required for aggregated records")
        else:
            for k in ["method", "outlier_method", "source_values", "weights_used", "rejected_sources"]:
                if k not in agg:
                    errors.append(f"Missing aggregation.{k}")
    elif record_type == "single":
        # aggregation block is not expected for single records
        pass

    return errors
