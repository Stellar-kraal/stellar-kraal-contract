#!/usr/bin/env python3
"""
oracle-bridge/tools/verify_provenance.py
=========================================

CLI tool to retrieve and verify provenance records for historical GEE oracle
submissions.

USAGE
-----

Retrieve and verify a provenance record by IPFS CID:

    python tools/verify_provenance.py verify --cid bafkreiabcdef...

Verify all fields of a locally stored provenance JSON file:

    python tools/verify_provenance.py verify --file provenance.json

Display the provenance record in a human-readable table:

    python tools/verify_provenance.py show --cid bafkreiabcdef...

Run a full verification suite (hash integrity + Ed25519 signature):

    python tools/verify_provenance.py verify --cid bafkreiabcdef... --check-sig

Use a simulated IPFS backend (default; no daemon required):

    IPFS_BACKEND=simulated python tools/verify_provenance.py verify --cid ...

Use a local IPFS node (requires ``ipfs daemon``):

    IPFS_BACKEND=local python tools/verify_provenance.py verify --cid ...

EXIT CODES
----------
0 — All checks passed.
1 — One or more verification checks failed.
2 — Record not found (CID unknown or file missing).
3 — Invalid JSON or schema validation error.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import sys
import textwrap
from pathlib import Path
from typing import Any

# Allow running from the oracle-bridge/ root without installing
sys.path.insert(0, str(Path(__file__).parent.parent))

from oracle_bridge.ipfs import get_ipfs_client, fetch_provenance_record
from oracle_bridge.provenance import (
    ProvenanceRecord,
    validate_provenance_record,
    ProvenanceValidationError,
    _canonical_params_hash,
)


# ── Colours ───────────────────────────────────────────────────────────────────

_NO_COLOR = not sys.stdout.isatty() or os.environ.get("NO_COLOR")


def _green(s: str) -> str:
    return s if _NO_COLOR else f"\033[32m{s}\033[0m"


def _red(s: str) -> str:
    return s if _NO_COLOR else f"\033[31m{s}\033[0m"


def _yellow(s: str) -> str:
    return s if _NO_COLOR else f"\033[33m{s}\033[0m"


def _bold(s: str) -> str:
    return s if _NO_COLOR else f"\033[1m{s}\033[0m"


def _ok(msg: str) -> None:
    print(f"  {_green('✓')} {msg}")


def _fail(msg: str) -> None:
    print(f"  {_red('✗')} {msg}")


def _warn(msg: str) -> None:
    print(f"  {_yellow('!')} {msg}")


# ── Verification logic ────────────────────────────────────────────────────────


def _verify_script_hash(record: dict[str, Any]) -> bool:
    """
    Verify that gee_script.version_hash is the SHA-256 of the script.

    This can only be checked if source_preview is present and complete.
    Returns True if confirmed, False if mismatch, None if unverifiable.
    """
    gs = record.get("gee_script", {})
    stored_hash = gs.get("version_hash", "")
    preview = gs.get("source_preview", "")
    if not preview:
        return None  # type: ignore[return-value]
    # We can only partially verify from a truncated preview
    computed = hashlib.sha256(preview.encode("utf-8")).hexdigest()
    return computed == stored_hash


def _verify_params_hash(record: dict[str, Any]) -> bool:
    """Verify that input_params.params_hash matches the canonical hash of params."""
    ip = record.get("input_params", {})
    stored = ip.get("params_hash", "")
    params = ip.get("params", {})
    computed = _canonical_params_hash(params)
    return computed == stored


def _verify_payload_fields(record: dict[str, Any]) -> list[str]:
    """
    Verify that attestation.payload_hex encodes the correct fields.

    Returns a list of error strings; empty means all passed.
    """
    errors: list[str] = []
    att = record.get("attestation", {})
    comp = record.get("computation", {})
    payload_hex = att.get("payload_hex", "")

    if len(payload_hex) != 226:
        errors.append(f"payload_hex length {len(payload_hex)} ≠ 226")
        return errors

    payload = bytes.fromhex(payload_hex)

    # Check output_value at bytes 65-73
    stored_ov = int.from_bytes(payload[65:73], "big", signed=True)
    expected_ov = comp.get("output_value")
    if stored_ov != expected_ov:
        errors.append(f"payload output_value={stored_ov} ≠ record output_value={expected_ov}")

    # Check timestamp_utc at bytes 73-81
    stored_ts = int.from_bytes(payload[73:81], "big", signed=True)
    expected_ts = comp.get("timestamp_utc")
    if stored_ts != expected_ts:
        errors.append(f"payload timestamp={stored_ts} ≠ record timestamp={expected_ts}")

    # Check schema_version at byte 0
    if payload[0] != 1:
        errors.append(f"payload schema_version={payload[0]} ≠ 1")

    return errors


def _verify_signature(record: dict[str, Any]) -> tuple[bool, str]:
    """
    Verify the Ed25519 signature in the attestation.

    Returns (ok, message).
    """
    try:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
    except ImportError:
        return False, "cryptography library not available; skipping signature check"

    att = record.get("attestation", {})
    try:
        pubkey_bytes = bytes.fromhex(att["public_key"])
        sig_bytes = bytes.fromhex(att["signature"])
        payload_bytes = bytes.fromhex(att["payload_hex"])
    except (KeyError, ValueError) as exc:
        return False, f"Cannot decode attestation fields: {exc}"

    try:
        pubkey = Ed25519PublicKey.from_public_bytes(pubkey_bytes)
        pubkey.verify(sig_bytes, payload_bytes)
        return True, "Ed25519 signature valid"
    except Exception as exc:
        return False, f"Ed25519 signature INVALID: {exc}"


# ── Loaders ───────────────────────────────────────────────────────────────────


def _load_from_ipfs(cid: str) -> dict[str, Any]:
    """Retrieve provenance record from IPFS by CID."""
    client = get_ipfs_client()
    try:
        return fetch_provenance_record(client, cid)
    except KeyError:
        print(_red(f"Error: CID {cid!r} not found in IPFS store."))
        sys.exit(2)
    except RuntimeError as exc:
        print(_red(f"Error: {exc}"))
        sys.exit(2)


def _load_from_file(path: str) -> dict[str, Any]:
    """Load provenance record from a local JSON file."""
    p = Path(path)
    if not p.exists():
        print(_red(f"Error: File not found: {path}"))
        sys.exit(2)
    try:
        return json.loads(p.read_text())
    except json.JSONDecodeError as exc:
        print(_red(f"Error: Invalid JSON in {path}: {exc}"))
        sys.exit(3)


# ── Commands ──────────────────────────────────────────────────────────────────


def cmd_verify(args: argparse.Namespace) -> int:
    """Run the full verification suite on a provenance record."""
    # Load record
    if args.cid:
        print(_bold(f"Fetching provenance record from IPFS: {args.cid}"))
        record = _load_from_ipfs(args.cid)
    else:
        print(_bold(f"Loading provenance record from file: {args.file}"))
        record = _load_from_file(args.file)

    print()
    all_ok = True

    # 1. Schema validation
    print(_bold("1. Schema validation"))
    try:
        validate_provenance_record(record)
        _ok("Record conforms to provenance JSON Schema v1")
    except ProvenanceValidationError as exc:
        _fail(f"Schema validation failed: {exc.validation_errors}")
        all_ok = False
    print()

    # 2. Params hash integrity
    print(_bold("2. Input parameters hash"))
    if _verify_params_hash(record):
        hash_val = record["input_params"]["params_hash"][:16] + "…"
        _ok(f"params_hash matches canonical JSON: {hash_val}")
    else:
        stored = record.get("input_params", {}).get("params_hash", "N/A")
        _fail(f"params_hash mismatch (stored={stored[:16]}…)")
        all_ok = False
    print()

    # 3. Payload field consistency
    print(_bold("3. Attestation payload field consistency"))
    payload_errors = _verify_payload_fields(record)
    if not payload_errors:
        _ok("Payload bytes encode correct output_value, timestamp, schema_version")
    else:
        for err in payload_errors:
            _fail(err)
        all_ok = False
    print()

    # 4. GEE script hash (if preview available)
    print(_bold("4. GEE script version hash"))
    gs = record.get("gee_script", {})
    if gs.get("source_preview"):
        result = _verify_script_hash(record)
        if result is True:
            _ok(f"script preview SHA-256 matches version_hash")
        elif result is False:
            _warn("script_hash in record does not match preview SHA-256")
            _warn("(preview may be truncated — full source needed for definitive check)")
        # None = unverifiable
    else:
        _warn("source_preview not present; cannot verify script hash from preview")
        _warn(f"Script asset: {gs.get('asset_path', 'N/A')}  tag: {gs.get('version_tag', 'N/A')}")
    print()

    # 5. Ed25519 signature (optional unless --check-sig)
    print(_bold("5. Ed25519 signature"))
    if args.check_sig:
        ok, msg = _verify_signature(record)
        if ok:
            _ok(msg)
        else:
            _fail(msg)
            all_ok = False
    else:
        att = record.get("attestation", {})
        _warn(
            "Signature check skipped (pass --check-sig to verify).\n"
            f"     Public key: {att.get('public_key', 'N/A')[:32]}…"
        )
    print()

    # Summary
    print(_bold("Summary"))
    if all_ok:
        print(_green("  All checks passed ✓"))
    else:
        print(_red("  One or more checks FAILED ✗"))

    return 0 if all_ok else 1


def cmd_show(args: argparse.Namespace) -> int:
    """Display a provenance record in a human-readable format."""
    if args.cid:
        record = _load_from_ipfs(args.cid)
    else:
        record = _load_from_file(args.file)

    gs = record.get("gee_script", {})
    ip = record.get("input_params", {})
    comp = record.get("computation", {})
    att = record.get("attestation", {})
    sub = record.get("submission", {})
    agg = record.get("aggregation")

    width = 70
    print("─" * width)
    print(_bold(f"  GEE Oracle Provenance Record (schema v{record.get('schema_version', '?')})"))
    print("─" * width)

    def row(label: str, value: Any) -> None:
        label_str = f"  {label:<24}"
        value_str = str(value)
        if len(value_str) > 43:
            value_str = value_str[:40] + "…"
        print(f"{label_str}{value_str}")

    print(_bold("\nScript"))
    row("Asset path:", gs.get("asset_path", "N/A"))
    row("Version tag:", gs.get("version_tag", "N/A"))
    row("Version hash:", gs.get("version_hash", "N/A")[:32] + "…")

    print(_bold("\nInput Parameters"))
    row("Params hash:", ip.get("params_hash", "N/A")[:32] + "…")
    params = ip.get("params", {})
    for k, v in list(params.items())[:5]:
        row(f"  {k}:", v)
    if len(params) > 5:
        row(f"  (+ {len(params) - 5} more):", "")

    print(_bold("\nComputation"))
    row("Feed ID:", comp.get("feed_id", "N/A"))
    row("Output value:", comp.get("output_value", "N/A"))
    row("Timestamp:", comp.get("timestamp_iso", "N/A"))

    print(_bold("\nAttestation"))
    row("Schema version:", att.get("schema_version", "N/A"))
    row("Public key:", att.get("public_key", "N/A")[:32] + "…")
    row("Signature:", att.get("signature", "N/A")[:32] + "…")

    print(_bold("\nSubmission"))
    row("Submitted at:", sub.get("submitted_at_iso", "N/A"))
    row("IPFS CID:", sub.get("ipfs_cid", "(not set)"))
    if sub.get("tx_ref"):
        row("Tx reference:", sub.get("tx_ref", "N/A"))

    if agg:
        print(_bold("\nAggregation"))
        row("Method:", agg.get("method", "N/A"))
        row("Outlier method:", agg.get("outlier_method", "N/A"))
        row("Rejected sources:", ", ".join(agg.get("rejected_sources", [])) or "none")
        sv = agg.get("source_values", {})
        for src, val in list(sv.items())[:5]:
            row(f"  {src}:", val)

    print("─" * width)
    return 0


def cmd_dump(args: argparse.Namespace) -> int:
    """Dump the raw provenance record JSON."""
    if args.cid:
        record = _load_from_ipfs(args.cid)
    else:
        record = _load_from_file(args.file)
    print(json.dumps(record, indent=2))
    return 0


# ── CLI entry point ───────────────────────────────────────────────────────────


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="verify_provenance",
        description="Retrieve and verify GEE oracle provenance records.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=textwrap.dedent(
            """\
            Examples:
              python tools/verify_provenance.py verify --cid bafkreiabcdef...
              python tools/verify_provenance.py verify --file provenance.json --check-sig
              python tools/verify_provenance.py show --cid bafkreiabcdef...
              python tools/verify_provenance.py dump --file provenance.json
            """
        ),
    )

    sub = parser.add_subparsers(dest="command", required=True)

    # ── verify ────────────────────────────────────────────────────────────────
    p_verify = sub.add_parser("verify", help="Verify a provenance record")
    src_group_v = p_verify.add_mutually_exclusive_group(required=True)
    src_group_v.add_argument("--cid", metavar="CID", help="IPFS CID of the provenance record")
    src_group_v.add_argument("--file", metavar="PATH", help="Path to a local JSON provenance file")
    p_verify.add_argument(
        "--check-sig",
        action="store_true",
        default=False,
        help="Also verify the Ed25519 attestation signature",
    )

    # ── show ──────────────────────────────────────────────────────────────────
    p_show = sub.add_parser("show", help="Display provenance record in human-readable format")
    src_group_s = p_show.add_mutually_exclusive_group(required=True)
    src_group_s.add_argument("--cid", metavar="CID", help="IPFS CID of the provenance record")
    src_group_s.add_argument("--file", metavar="PATH", help="Path to a local JSON provenance file")

    # ── dump ──────────────────────────────────────────────────────────────────
    p_dump = sub.add_parser("dump", help="Dump raw provenance record JSON")
    src_group_d = p_dump.add_mutually_exclusive_group(required=True)
    src_group_d.add_argument("--cid", metavar="CID", help="IPFS CID of the provenance record")
    src_group_d.add_argument("--file", metavar="PATH", help="Path to a local JSON provenance file")

    return parser


def main() -> None:
    parser = build_parser()
    args = parser.parse_args()

    if args.command == "verify":
        sys.exit(cmd_verify(args))
    elif args.command == "show":
        sys.exit(cmd_show(args))
    elif args.command == "dump":
        sys.exit(cmd_dump(args))
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
