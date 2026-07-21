"""
oracle_bridge.cli
=================

Command-line interface for the oracle bridge.

Provides the ``replay-dlq`` command for operators to replay failed
submissions from the dead-letter queue.
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from pathlib import Path
from typing import Any

from oracle_bridge.resilience import DeadLetterQueue

logger = logging.getLogger(__name__)


def build_parser() -> argparse.ArgumentParser:
    """Build the argument parser for the CLI."""
    parser = argparse.ArgumentParser(
        prog="oracle-bridge",
        description="GEE to Soroban carbon oracle attestation pipeline",
    )
    sub = parser.add_subparsers(dest="command", help="Available commands")

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

    if args.command == "replay-dlq":
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
