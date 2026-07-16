# Oracle Attestation Payload Schema

## Overview

Each GEE (Google Earth Engine) computation result submitted to the `carbon_oracle` Soroban
contract must be accompanied by a cryptographic attestation signed by the oracle operator's
Ed25519 key. The Soroban contract verifies this signature before accepting any price update,
ensuring that only values originating from an authorised GEE computation are accepted on-chain.

---

## Attestation Payload

The attestation payload is a canonical, deterministic serialisation of the GEE computation
context plus the output value. All fields are mandatory.

### Fields

| Field | Type | Description |
|---|---|---|
| `schema_version` | `u8` | Schema version for forward-compatibility. Currently `1`. |
| `script_hash` | `bytes[32]` | SHA-256 digest of the GEE JavaScript source that produced the result. |
| `input_params_hash` | `bytes[32]` | SHA-256 digest of the canonical JSON-serialised input parameters (see below). |
| `output_value` | `i64` | Carbon sequestration value in **micrograms of CO₂-equivalent per m²**, big-endian. |
| `timestamp_utc` | `i64` | Unix timestamp (seconds since epoch, UTC) of the GEE computation, big-endian. |
| `feed_id` | `bytes[32]` | Identifier of the price feed / asset this attestation relates to (zero-padded UTF-8). |

### Input Parameters Canonical JSON

Input parameters are serialised as a UTF-8 JSON object with keys sorted lexicographically, no
whitespace, and no trailing newline. Example:

```json
{"aoi":"POLYGON((30.1 -1.2,30.1 -1.0,30.3 -1.0,30.3 -1.2,30.1 -1.2))","endDate":"2024-12-31","startDate":"2024-01-01"}
```

The SHA-256 of this UTF-8 byte string is stored in `input_params_hash`.

---

## Binary Serialisation (signing payload)

The message that is signed is the concatenation of the fields in the order below, with no
length prefixes or separators. All multi-byte integers are **big-endian**.

```
[ schema_version : 1 byte  ]
[ script_hash    : 32 bytes ]
[ input_params_hash : 32 bytes ]
[ output_value   : 8 bytes  ]  ← big-endian i64
[ timestamp_utc  : 8 bytes  ]  ← big-endian i64
[ feed_id        : 32 bytes ]
```

Total message length: **113 bytes**.

### Rationale

- Fixed-length fields eliminate ambiguity without a length-prefix scheme.
- Big-endian integers match the convention used by the Soroban SDK `BytesN` helpers.
- `schema_version` as the first byte allows future parsers to switch on the version before
  reading the rest of the payload.

---

## Signature

- **Algorithm**: Ed25519 (RFC 8032)
- **Key format**: 32-byte raw public key (no PEM wrapper)
- **Signature length**: 64 bytes
- **Wire format**: the 64-byte signature is transmitted alongside the payload (not prepended to
  the message bytes that are signed).

---

## Attestation Envelope (JSON over the wire)

When the Python oracle bridge submits an attestation, it wraps the binary fields in a
JSON envelope for transport:

```jsonc
{
  "schema_version": 1,
  "script_hash":        "<hex-encoded 32 bytes>",
  "input_params_hash":  "<hex-encoded 32 bytes>",
  "output_value":       1234567,          // i64
  "timestamp_utc":      1720051200,       // Unix seconds
  "feed_id":            "<hex-encoded 32 bytes>",
  "public_key":         "<hex-encoded 32 bytes>",
  "signature":          "<hex-encoded 64 bytes>"
}
```

---

## On-Chain Verification (Soroban)

The `carbon_oracle` contract stores the authorised oracle public key in instance storage
(set during `initialize`). On each `submit_price` call it:

1. Re-constructs the 113-byte canonical payload from the supplied arguments.
2. Calls `env.crypto().ed25519_verify(&pubkey, &message, &signature)`.
3. Returns `Error::InvalidAttestation` if verification fails, or proceeds to store the
   `PriceEntry` if it succeeds.

---

## Security Considerations

- **Key rotation** is currently out of scope (tracked as a follow-up). The public key is fixed
  at contract initialisation time.
- **Replay prevention**: the combination of `(script_hash, input_params_hash, timestamp_utc)`
  should be unique for every computation. A future enhancement may add an explicit nonce.
- **GEE script auditing** is out of scope for this PR; the `script_hash` field is the hook
  point for an auditing registry.
- The private key must never appear in logs, environment dumps, or source control. Use a
  secrets manager or hardware security module in production.
