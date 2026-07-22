# GEE Oracle Provenance Record Schema

## Overview

Every oracle submission by the `oracle-bridge` records a **provenance record** that captures full reproducibility metadata for the GEE computation. The record is stored off-chain in IPFS (content-addressed), and the resulting CID is stored on-chain alongside the price/metric entry in `carbon_oracle`.

This enables anyone to independently re-run the GEE script at the exact same version and parameters, reproduce the result, and verify the on-chain data.

---

## Provenance Record JSON Schema

**Schema ID:** `https://stellarkraal.example.com/schemas/provenance-record/v1.json`  
**Schema Version:** `1`

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "$id": "https://stellarkraal.example.com/schemas/provenance-record/v1.json",
  "title": "GEE Oracle Provenance Record",
  "description": "Reproducibility record for a GEE oracle submission. Stored on IPFS; CID recorded on-chain.",
  "type": "object",
  "required": [
    "schema_version",
    "record_type",
    "gee_script",
    "input_params",
    "computation",
    "attestation",
    "submission"
  ],
  "additionalProperties": false,
  "properties": {
    "schema_version": {
      "type": "integer",
      "const": 1,
      "description": "Schema version; always 1 for this version."
    },
    "record_type": {
      "type": "string",
      "enum": ["single", "aggregated"],
      "description": "Whether this is a single-source or multi-source aggregated submission."
    },
    "gee_script": {
      "type": "object",
      "required": ["asset_path", "version_hash", "version_tag"],
      "additionalProperties": false,
      "properties": {
        "asset_path": {
          "type": "string",
          "description": "GEE asset path or identifier for the script (e.g. 'users/org/scripts/carbon_v1').",
          "minLength": 1
        },
        "version_hash": {
          "type": "string",
          "pattern": "^[0-9a-f]{64}$",
          "description": "SHA-256 hex digest of the exact GEE script source that was executed."
        },
        "version_tag": {
          "type": "string",
          "description": "Human-readable version tag or git ref (e.g. 'v1.2.3', 'abc1234').",
          "minLength": 1
        },
        "source_preview": {
          "type": "string",
          "description": "Optional: first 512 chars of the script source for quick inspection.",
          "maxLength": 512
        }
      }
    },
    "input_params": {
      "type": "object",
      "required": ["params", "params_hash"],
      "additionalProperties": false,
      "properties": {
        "params": {
          "type": "object",
          "description": "The exact input parameters passed to the GEE script.",
          "additionalProperties": true
        },
        "params_hash": {
          "type": "string",
          "pattern": "^[0-9a-f]{64}$",
          "description": "SHA-256 hex of the canonical (sorted-keys, compact JSON) serialisation of params."
        }
      }
    },
    "computation": {
      "type": "object",
      "required": ["output_value", "feed_id", "timestamp_utc", "timestamp_iso"],
      "additionalProperties": false,
      "properties": {
        "output_value": {
          "type": "integer",
          "description": "Carbon sequestration value in micrograms CO2-eq/m^2 (signed i64)."
        },
        "feed_id": {
          "type": "string",
          "description": "Feed / asset identifier (≤ 32 bytes UTF-8).",
          "maxLength": 32
        },
        "timestamp_utc": {
          "type": "integer",
          "description": "Unix timestamp (seconds since epoch, UTC) of the GEE computation.",
          "minimum": 0
        },
        "timestamp_iso": {
          "type": "string",
          "description": "ISO 8601 representation of timestamp_utc for human readability.",
          "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}Z$"
        }
      }
    },
    "attestation": {
      "type": "object",
      "required": ["schema_version", "public_key", "signature", "payload_hex"],
      "additionalProperties": false,
      "properties": {
        "schema_version": {
          "type": "integer",
          "const": 1,
          "description": "Attestation payload schema version (always 1)."
        },
        "public_key": {
          "type": "string",
          "pattern": "^[0-9a-f]{64}$",
          "description": "Hex-encoded 32-byte Ed25519 public key of the oracle signer."
        },
        "signature": {
          "type": "string",
          "pattern": "^[0-9a-f]{128}$",
          "description": "Hex-encoded 64-byte Ed25519 signature over the canonical 113-byte payload."
        },
        "payload_hex": {
          "type": "string",
          "pattern": "^[0-9a-f]{226}$",
          "description": "Hex-encoded 113-byte canonical attestation payload that was signed."
        }
      }
    },
    "submission": {
      "type": "object",
      "required": ["submitted_at_iso", "submitted_at_utc"],
      "additionalProperties": false,
      "properties": {
        "submitted_at_iso": {
          "type": "string",
          "description": "ISO 8601 timestamp when the record was pinned to IPFS.",
          "pattern": "^\\d{4}-\\d{2}-\\d{2}T\\d{2}:\\d{2}:\\d{2}Z$"
        },
        "submitted_at_utc": {
          "type": "integer",
          "description": "Unix timestamp of submission.",
          "minimum": 0
        },
        "tx_ref": {
          "type": "string",
          "description": "Optional: on-chain transaction reference after successful submission."
        },
        "ipfs_cid": {
          "type": "string",
          "description": "Optional: self-referential CID (set after pinning; present in index records).",
          "minLength": 10
        }
      }
    },
    "aggregation": {
      "type": "object",
      "description": "Present only when record_type is 'aggregated'.",
      "required": ["method", "outlier_method", "source_values", "weights_used", "rejected_sources"],
      "additionalProperties": false,
      "properties": {
        "method": {
          "type": "string",
          "description": "Aggregation method used (e.g. 'weighted_median').",
          "minLength": 1
        },
        "outlier_method": {
          "type": "string",
          "enum": ["iqr", "mad", "none"],
          "description": "Outlier rejection method applied."
        },
        "source_values": {
          "type": "object",
          "description": "Per-source output values after outlier filtering.",
          "additionalProperties": {
            "type": "integer"
          }
        },
        "weights_used": {
          "type": "object",
          "description": "Per-source weights applied in the aggregation.",
          "additionalProperties": {
            "type": "number",
            "exclusiveMinimum": 0
          }
        },
        "rejected_sources": {
          "type": "array",
          "description": "Source IDs rejected as outliers.",
          "items": {
            "type": "string"
          }
        }
      }
    }
  }
}
```

---

## Field Reference

| Field Path | Type | Required | Description |
|---|---|---|---|
| `schema_version` | integer (1) | ✓ | Always `1` |
| `record_type` | `"single"` \| `"aggregated"` | ✓ | Submission type |
| `gee_script.asset_path` | string | ✓ | GEE asset path for the script |
| `gee_script.version_hash` | hex[64] | ✓ | SHA-256 of exact script source |
| `gee_script.version_tag` | string | ✓ | Human-readable version tag |
| `gee_script.source_preview` | string | – | First 512 chars of script |
| `input_params.params` | object | ✓ | Raw input parameters |
| `input_params.params_hash` | hex[64] | ✓ | SHA-256 of canonical JSON params |
| `computation.output_value` | integer | ✓ | Result value (i64) |
| `computation.feed_id` | string | ✓ | Feed identifier |
| `computation.timestamp_utc` | integer | ✓ | Unix timestamp of computation |
| `computation.timestamp_iso` | ISO 8601 | ✓ | Human-readable timestamp |
| `attestation.schema_version` | integer (1) | ✓ | Payload schema version |
| `attestation.public_key` | hex[64] | ✓ | 32-byte Ed25519 public key |
| `attestation.signature` | hex[128] | ✓ | 64-byte Ed25519 signature |
| `attestation.payload_hex` | hex[226] | ✓ | 113-byte canonical payload |
| `submission.submitted_at_iso` | ISO 8601 | ✓ | IPFS pin timestamp |
| `submission.submitted_at_utc` | integer | ✓ | Unix timestamp of submission |
| `submission.tx_ref` | string | – | On-chain transaction ref |
| `submission.ipfs_cid` | string | – | Self-referential CID |
| `aggregation.*` | object | ✓ if aggregated | Aggregation metadata |

---

## Example Record (single source)

```json
{
  "schema_version": 1,
  "record_type": "single",
  "gee_script": {
    "asset_path": "users/stellarkraal/scripts/carbon_sequestration_v1",
    "version_hash": "a3f1e2d4b5c6789012345678901234567890abcdef1234567890abcdef123456",
    "version_tag": "v1.2.3",
    "source_preview": "// GEE carbon sequestration estimator v1.2.3\nvar dataset = ee.ImageCollection('MODIS/061/MOD13A3')"
  },
  "input_params": {
    "params": {
      "aoi": "POLYGON((30.1 -1.2,30.1 -1.0,30.3 -1.0,30.3 -1.2,30.1 -1.2))",
      "startDate": "2024-01-01",
      "endDate": "2024-12-31"
    },
    "params_hash": "c2b7a1d3e4f5678901234567890123456789abcdef0123456789abcdef012345"
  },
  "computation": {
    "output_value": 4815162342,
    "feed_id": "carbon/rwanda/2024",
    "timestamp_utc": 1720051200,
    "timestamp_iso": "2024-07-04T00:00:00Z"
  },
  "attestation": {
    "schema_version": 1,
    "public_key": "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
    "signature": "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    "payload_hex": "01a3f1e2d4b5c6789012...feed_id_hex"
  },
  "submission": {
    "submitted_at_iso": "2024-07-04T00:01:05Z",
    "submitted_at_utc": 1720051265,
    "tx_ref": "tx_abc123def456",
    "ipfs_cid": "bafkreihdwdcefgh..."
  }
}
```

---

## On-Chain CID Encoding

The IPFS CID is stored on-chain in the `PriceEntry` struct as a 46-byte field (`ipfs_cid: BytesN<46>`), which accommodates a CIDv1 Base32-encoded multihash.

For prototype/testnet purposes, a simulated CID is produced by hashing the provenance record JSON with SHA-256 and encoding it as `"bafkrei" + hex_digest[:39]` to match the Base32 CIDv1 structure.

---

## Verification Workflow

1. Query on-chain `PriceEntry` for a feed → obtain `ipfs_cid`
2. Fetch provenance record from IPFS: `ipfs cat <cid>`
3. Recompute `gee_script.version_hash` from script source → compare
4. Recompute `input_params.params_hash` from `params` → compare
5. Re-derive 113-byte canonical payload from fields → compare `attestation.payload_hex`
6. Verify Ed25519 signature in `attestation` → confirm authenticity

The CLI tool `oracle-bridge/tools/verify_provenance.py` automates steps 2–6.
