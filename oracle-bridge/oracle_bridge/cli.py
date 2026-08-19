"""
oracle_bridge.cli
=================

Command-line interface for the oracle bridge.

Provides the ``replay-dlq`` command for operators to replay failed
submissions from the dead-letter queue, and ``submit`` for running
the full GEE → attestation → IPFS → Soroban pipeline.
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
import time
from pathlib import Path
from typing import Any

from oracle_bridge.resilience import DeadLetterQueue

logger = logging.getLogger(__name__)


# ── Dry-run submission client ────────────────────────────────────────────────


class DryRunClient:
    """Drop-in :class:`SubmissionClient` that records calls without hitting the network."""

    def submit_price(self, attestation: Any) -> str:
        return "dry-run-tx"

    def submit_price_with_cid(self, attestation: Any, ipfs_cid: str) -> str:
        return "dry-run-tx"

    def commit_price(self, feed_id: Any, commitment_hash: bytes) -> str:
        return "dry-run-tx"

    def reveal_price(self, attestation: Any, salt: bytes) -> str:
        return "dry-run-tx"


def build_parser() -> argparse.ArgumentParser:
    """Build the argument parser for the CLI."""
    parser = argparse.ArgumentParser(
        prog="oracle-bridge",
        description="GEE to Soroban carbon oracle attestation pipeline",
    )
    sub = parser.add_subparsers(dest="command", help="Available commands")

    # ── submit ─────────────────────────────────────────────────────────────
    submit = sub.add_parser(
        "submit",
        help="Run the GEE → attestation → IPFS → on-chain pipeline",
    )
    submit.add_argument(
        "--feed-id",
        type=str,
        required=True,
        help="Feed / asset identifier (e.g. CATTLE-SPOT)",
    )
    submit.add_argument(
        "--results-file",
        type=str,
        required=True,
        help="Path to a JSON file containing GEE results",
    )
    submit.add_argument(
        "--dry-run",
        action="store_true",
        default=False,
        help="Run the full pipeline (attest + pin to IPFS) but skip on-chain submission; output JSON",
    )
    submit.add_argument(
        "--key-file",
        type=str,
        default=None,
        help="Path to an Ed25519 PEM key file for the oracle signer (omit to generate a throwaway key)",
    )
    submit.add_argument(
        "--outlier-method",
        type=str,
        choices=["iqr", "mad", "none"],
        default="iqr",
        help="Outlier rejection method for multi-source aggregation (default: iqr)",
    )
    submit.add_argument(
        "--iqr-multiplier",
        type=float,
        default=1.5,
        help="IQR multiplier for outlier detection (default: 1.5)",
    )
    submit.add_argument(
        "--ipfs-mode",
        type=str,
        choices=["simulated", "local"],
        default="simulated",
        help="IPFS backend: simulated (in-memory) or local (default: simulated)",
    )
    submit.add_argument(
        "--json",
        action="store_true",
        default=False,
        help="Output results as JSON (default: human-readable summary)",
    )

    # ── replay-dlq ────────────────────────────────────────────────────────
    replay = sub.add_parser(
        "replay-dlq",
        help="Replay failed submissions from the dead-letter queue",
    )
    replay.add_argument(
        "--db",
        type=str,
        default="oracle_dlq.db",
        help="Path to the DLQ SQLite database (default: oracle_dlq.db)",
    )
    replay.add_argument(
        "--id",
        type=int,
        default=None,
        help="Replay a specific entry by ID (default: all unreplayed entries)",
    )
    replay.add_argument(
        "--dry-run",
        action="store_true",
        default=False,
        help="List entries without replaying them",
    )

    # ── dlq-stats ─────────────────────────────────────────────────────────
    stats = sub.add_parser(
        "dlq-stats",
        help="Show dead-letter queue statistics",
    )
    stats.add_argument(
        "--db",
        type=str,
        default="oracle_dlq.db",
        help="Path to the DLQ SQLite database (default: oracle_dlq.db)",
    )
    stats.add_argument(
        "--json",
        action="store_true",
        default=False,
        help="Output stats as JSON",
    )

    # ── start-metrics-server ───────────────────────────────────────────────
    metrics = sub.add_parser(
        "start-metrics-server",
        help="Start the Prometheus metrics HTTP server",
    )
    metrics.add_argument(
        "--port",
        type=int,
        default=8000,
        help="HTTP server port (default: 8000)",
    )
    metrics.add_argument(
        "--addr",
        type=str,
        default="0.0.0.0",
        help="Bind address (default: 0.0.0.0)",
    )

    return parser


def cmd_submit(args: argparse.Namespace) -> int:
    """Execute the ``submit`` subcommand."""
    from oracle_bridge.attestation import OracleSigner
    from oracle_bridge.bridge import OracleBridge, GEEResult
    from oracle_bridge.aggregation import (
        AggregationConfig,
        OutlierRejectionMethod,
        PriceSource,
    )
    from oracle_bridge.ipfs import SimulatedIPFSClient, LocalIPFSClient

    # ── load GEE results from JSON file ───────────────────────────────────
    results_path = Path(args.results_file)
    if not results_path.exists():
        print(f"Results file not found: {results_path}", file=sys.stderr)
        return 1

    try:
        raw = json.loads(results_path.read_text())
    except json.JSONDecodeError as exc:
        print(f"Invalid JSON in results file: {exc}", file=sys.stderr)
        return 1

    # Parse into GEEResult objects.  The file can be either:
    #   - a dict  { source_id: { script_source, input_params, ... } }
    #   - a list  [ { script_source, input_params, ... } ]
    gee_results: dict[str, GEEResult] = {}
    if isinstance(raw, dict):
        for key, entry in raw.items():
            gee_results[key] = GEEResult(
                script_source=entry.get("script_source", "// dry-run"),
                input_params=entry.get("input_params", {}),
                output_value=int(entry["output_value"]),
                feed_id=entry.get("feed_id", args.feed_id),
                timestamp_utc=entry.get("timestamp_utc"),
            )
    elif isinstance(raw, list):
        for idx, entry in enumerate(raw):
            source_id = entry.get("source_id", f"source_{idx}")
            gee_results[source_id] = GEEResult(
                script_source=entry.get("script_source", "// dry-run"),
                input_params=entry.get("input_params", {}),
                output_value=int(entry["output_value"]),
                feed_id=entry.get("feed_id", args.feed_id),
                timestamp_utc=entry.get("timestamp_utc"),
            )
    else:
        print("Results file must be a JSON object or array", file=sys.stderr)
        return 1

    if not gee_results:
        print("No GEE results found in file", file=sys.stderr)
        return 1

    # ── signer ────────────────────────────────────────────────────────────
    if args.key_file:
        key_path = Path(args.key_file)
        if not key_path.exists():
            print(f"Key file not found: {key_path}", file=sys.stderr)
            return 1
        signer = OracleSigner.from_pem(key_path.read_text())
    else:
        signer = OracleSigner.generate()

    # ── IPFS client ───────────────────────────────────────────────────────
    if args.ipfs_mode == "local":
        from oracle_bridge.ipfs import LocalIPFSClient
        ipfs_client = LocalIPFSClient()
    else:
        ipfs_client = SimulatedIPFSClient()

    # ── submission client ─────────────────────────────────────────────────
    if args.dry_run:
        client = DryRunClient()
    else:
        # Live mode requires a Soroban RPC client (not yet implemented).
        # For now, only dry-run is supported.
        print(
            "Live on-chain submission is not yet implemented.  "
            "Use --dry-run to test the full pipeline without a network.",
            file=sys.stderr,
        )
        return 1

    # ── aggregation config ────────────────────────────────────────────────
    outlier_map = {
        "iqr": OutlierRejectionMethod.IQR,
        "mad": OutlierRejectionMethod.MAD,
        "none": OutlierRejectionMethod.NONE,
    }
    source_ids = list(gee_results.keys())
    agg_config = AggregationConfig(
        sources=source_ids,
        weights={s: 1.0 for s in source_ids},
        outlier_method=outlier_map[args.outlier_method],
        iqr_multiplier=args.iqr_multiplier,
    )

    # ── run pipeline ──────────────────────────────────────────────────────
    bridge = OracleBridge(
        signer, client,
        ipfs_client=ipfs_client,
        aggregation_config=agg_config,
    )

    try:
        result, attestation, tx_ref, provenance = bridge.aggregate_and_submit(
            gee_results
        )
    except Exception as exc:
        print(f"Pipeline failed: {exc}", file=sys.stderr)
        return 1

    # ── output ────────────────────────────────────────────────────────────
    output = {
        "feed_id": result.feed_id if isinstance(result.feed_id, str)
                   else result.feed_id.decode("utf-8").rstrip("\x00"),
        "aggregate_value": result.aggregate_value,
        "source_values": result.source_values,
        "weights_used": result.weights_used,
        "rejected_sources": result.rejected_sources,
        "outlier_method": result.outlier_method,
        "ipfs_cid": result.ipfs_cid or provenance.ipfs_cid,
        "tx_ref": tx_ref,
        "attestation_public_key": attestation.public_key.hex(),
        "attestation_signature": attestation.signature.hex(),
        "dry_run": args.dry_run,
        "timestamp_utc": result.timestamp_utc,
    }

    if args.json:
        print(json.dumps(output, indent=2))
    else:
        mode_label = "DRY-RUN" if args.dry_run else "LIVE"
        print(f"[{mode_label}] Oracle submission complete")
        print(f"  Feed:         {output['feed_id']}")
        print(f"  Aggregate:    {output['aggregate_value']}")
        print(f"  Sources:      {len(output['source_values'])}")
        if output["rejected_sources"]:
            print(f"  Rejected:     {output['rejected_sources']}")
        print(f"  IPFS CID:     {output['ipfs_cid']}")
        print(f"  TX ref:       {output['tx_ref']}")
        print(f"  Oracle key:   {output['attestation_public_key'][:16]}…")

    return 0


def cmd_replay_dlq(args: argparse.Namespace) -> int:
    """Execute the ``replay-dlq`` subcommand."""
    db_path = Path(args.db)
    if not db_path.exists():
        print(f"DLQ database not found: {db_path}", file=sys.stderr)
        return 1

    dlq = DeadLetterQueue(db_path)
    try:
        entries = dlq.list_unreplayed()

        if args.id is not None:
            entries = [e for e in entries if e.id == args.id]
            if not entries:
                print(
                    f"Unreplayed DLQ entry with id={args.id} not found",
                    file=sys.stderr,
                )
                return 1

        if not entries:
            print("No unreplayed DLQ entries found.")
            return 0

        if args.dry_run:
            print(f"Found {len(entries)} unreplayed DLQ entr{'y' if len(entries) == 1 else 'ies'}:")
            for entry in entries:
                print(
                    f"  [{entry.id}] feed={entry.feed_id} "
                    f"attempts={entry.attempt_count} "
                    f"error={entry.error_message[:80]}"
                )
            return 0

        from oracle_bridge.adapters import (
            FeedAdapterConfig,
            ToucanProtocolAdapter,
            XpansivCBLAdapter,
        )

        success_count = 0
        for entry in entries:
            print(f"Replaying DLQ entry {entry.id} (feed={entry.feed_id})...")

            config = FeedAdapterConfig(
                feed_id=entry.feed_id,
                dlq_db_path=str(db_path),
            )

            if "xpansiv" in entry.feed_id.lower() or "cbl" in entry.feed_id.lower():
                adapter = XpansivCBLAdapter(config)
            elif "toucan" in entry.feed_id.lower() or entry.feed_id in ("BCT", "NCT"):
                adapter = ToucanProtocolAdapter(config)
            else:
                print(f"  Unknown feed type for {entry.feed_id}, skipping")
                continue

            try:
                adapter.replay_dlq_entry(entry.id)
                print(f"  Entry {entry.id} replayed successfully")
                success_count += 1
            except Exception as exc:
                print(f"  Entry {entry.id} replay failed: {exc}")

        print(
            f"Replayed {success_count}/{len(entries)} DLQ entries successfully."
        )
        return 0 if success_count == len(entries) else 1

    finally:
        dlq.close()


def cmd_dlq_stats(args: argparse.Namespace) -> int:
    """Execute the ``dlq-stats`` subcommand."""
    db_path = Path(args.db)
    if not db_path.exists():
        print(f"DLQ database not found: {db_path}", file=sys.stderr)
        return 1

    dlq = DeadLetterQueue(db_path)
    try:
        total = dlq.count()
        unreplayed = dlq.count_unreplayed()
        replayed = total - unreplayed

        stats = {
            "total_entries": total,
            "replayed": replayed,
            "unreplayed": unreplayed,
            "db_path": str(db_path),
        }

        if args.json:
            print(json.dumps(stats, indent=2))
        else:
            print(f"Dead-letter queue stats for {db_path}:")
            print(f"  Total entries:  {total}")
            print(f"  Replayed:       {replayed}")
            print(f"  Unreplayed:     {unreplayed}")
        return 0
    finally:
        dlq.close()


def cmd_start_metrics_server(args: argparse.Namespace) -> int:
    """Execute the ``start-metrics-server`` subcommand."""
    from oracle_bridge.metrics import start_metrics_server

    print(
        f"Starting Prometheus metrics server on {args.addr}:{args.port} ..."
    )
    start_metrics_server(port=args.port, addr=args.addr)
    print("Metrics server running (press Ctrl+C to stop).")
    try:
        import time
        while True:
            time.sleep(3600)
    except KeyboardInterrupt:
        print("\nMetrics server stopped.")
    return 0


def main(argv: list[str] | None = None) -> int:
    """CLI entry point."""
    parser = build_parser()
    args = parser.parse_args(argv)

    if args.command == "submit":
        return cmd_submit(args)
    elif args.command == "replay-dlq":
        return cmd_replay_dlq(args)
    elif args.command == "dlq-stats":
        return cmd_dlq_stats(args)
    elif args.command == "start-metrics-server":
        return cmd_start_metrics_server(args)
    else:
        parser.print_help()
        return 0


if __name__ == "__main__":
    sys.exit(main())
