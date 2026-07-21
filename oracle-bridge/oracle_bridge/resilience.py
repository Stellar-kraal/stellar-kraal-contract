"""
oracle_bridge.resilience
=======================

Retry logic with exponential backoff and jitter, degraded-mode state machine,
and dead-letter queue persistence for the GEE oracle bridge feed adapters.
"""

from __future__ import annotations

import json
import logging
import math
import os
import random
import sqlite3
import time
from dataclasses import dataclass, field, asdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, TypeVar

T = TypeVar("T")

logger = logging.getLogger(__name__)


# ── Retry Logic ───────────────────────────────────────────────────────────────


@dataclass
class RetryConfig:
    """
    Configuration for retry behaviour.

    Parameters
    ----------
    max_retries:
        Maximum number of retry attempts before giving up.
    base_delay_seconds:
        Base delay in seconds (doubles each retry).
    max_delay_seconds:
        Maximum delay cap in seconds.
    jitter_factor:
        Random jitter fraction (0.0 = no jitter, 1.0 = up to 100%).
    """

    max_retries: int = 3
    base_delay_seconds: float = 1.0
    max_delay_seconds: float = 60.0
    jitter_factor: float = 0.1


def compute_backoff(
    attempt: int,
    config: RetryConfig,
) -> float:
    """
    Compute the sleep delay for a given retry *attempt* (0-indexed).

    Applies exponential backoff with configurable jitter:

        delay = min(base * 2^attempt, max_delay)
        delay += random.uniform(-jitter, jitter) * delay
    """
    delay = config.base_delay_seconds * (2 ** attempt)
    delay = min(delay, config.max_delay_seconds)
    jitter = delay * config.jitter_factor
    delay += random.uniform(-jitter, jitter)
    return max(0.0, delay)


def retry(
    fn: Callable[..., T],
    config: RetryConfig,
    context: str = "",
    exc_types: tuple[type[Exception], ...] = (Exception,),
) -> T:
    """
    Execute *fn* with retry logic described by *config*.

    Parameters
    ----------
    fn:
        Callable to execute.
    config:
        Retry configuration.
    context:
        Optional human-readable context string for logging.
    exc_types:
        Tuple of exception types that trigger a retry.

    Returns
    -------
    The return value of *fn* on success.

    Raises
    ------
    The last exception caught if all retries are exhausted.
    """
    last_exc: Exception | None = None
    for attempt in range(config.max_retries + 1):
        try:
            return fn()
        except exc_types as e:
            last_exc = e
            if attempt < config.max_retries:
                delay = compute_backoff(attempt, config)
                ctx = f" ({context})" if context else ""
                logger.warning(
                    "Retry attempt %d/%d failed%s: %s. "
                    "Retrying in %.2fs...",
                    attempt + 1, config.max_retries, ctx, e, delay,
                )
                time.sleep(delay)
            else:
                ctx = f" ({context})" if context else ""
                logger.error(
                    "All %d retries exhausted%s: %s",
                    config.max_retries, ctx, e,
                )
    raise last_exc  # type: ignore[misc]


# ── Degraded Mode State Machine ──────────────────────────────────────────────


@dataclass
class DegradedModeState:
    """
    Tracks whether a feed adapter is operating in degraded mode.

    Degraded mode activates after *consecutive_failures_threshold*
    consecutive failures. While degraded, the adapter returns the
    last-known-good price as long as it is within *staleness_window_seconds*.

    Parameters
    ----------
    consecutive_failures_threshold:
        Number of consecutive failures before entering degraded mode.
    staleness_window_seconds:
        Maximum age (in seconds) for a last-known-good price to be usable.
    feed_id:
        Identifier for the feed this state belongs to.
    """

    consecutive_failures_threshold: int = 3
    staleness_window_seconds: float = 300.0
    feed_id: str = ""

    _consecutive_failures: int = 0
    _degraded: bool = False
    _last_known_good_price: int | None = None
    _last_good_timestamp: float = 0.0

    @property
    def degraded(self) -> bool:
        """Whether the adapter is currently in degraded mode."""
        return self._degraded

    @property
    def last_known_good_price(self) -> int | None:
        """Return the last-known-good price if still within the staleness window."""
        if self._last_known_good_price is None:
            return None
        age = time.time() - self._last_good_timestamp
        if age > self.staleness_window_seconds:
            return None
        return self._last_known_good_price

    def record_success(self, price: int) -> None:
        """Record a successful fetch and reset failure count."""
        self._consecutive_failures = 0
        self._degraded = False
        self._last_known_good_price = price
        self._last_good_timestamp = time.time()

    def record_failure(self) -> None:
        """Record a failure and potentially enter degraded mode."""
        self._consecutive_failures += 1
        if self._consecutive_failures >= self.consecutive_failures_threshold:
            if not self._degraded:
                logger.warning(
                    "Feed %s entering degraded mode after %d consecutive failures",
                    self.feed_id,
                    self._consecutive_failures,
                )
            self._degraded = True

    def exit_degraded(self) -> None:
        """Manually exit degraded mode (e.g. after operator intervention)."""
        self._degraded = False
        self._consecutive_failures = 0

    def state_info(self) -> dict[str, Any]:
        """Return a snapshot of the current state for observability."""
        return {
            "feed_id": self.feed_id,
            "degraded": self._degraded,
            "consecutive_failures": self._consecutive_failures,
            "last_known_good_price": self._last_known_good_price,
            "last_good_timestamp": (
                datetime.fromtimestamp(
                    self._last_good_timestamp, tz=timezone.utc
                ).isoformat()
                if self._last_good_timestamp > 0
                else None
            ),
            "staleness_window_seconds": self.staleness_window_seconds,
        }


# ── Dead-Letter Queue ────────────────────────────────────────────────────────


@dataclass
class DLQEntry:
    """A single entry in the dead-letter queue."""

    id: int | None = None
    feed_id: str = ""
    payload_json: str = ""
    error_message: str = ""
    attempt_count: int = 0
    created_at: str = ""
    replayed: bool = False
    replayed_at: str | None = None


class DeadLetterQueue:
    """
    SQLite-backed dead-letter queue for failed oracle submissions.

    Entries are persisted with full metadata and can be replayed by
    operators via the ``replay-dlq`` CLI command.
    """

    def __init__(self, db_path: str | Path) -> None:
        self._db_path = Path(db_path)
        self._conn: sqlite3.Connection | None = None
        self._init_db()

    def _init_db(self) -> None:
        """Create the DLQ table if it does not exist."""
        self._conn = sqlite3.connect(str(self._db_path))
        self._conn.execute(
            """
            CREATE TABLE IF NOT EXISTS dead_letter_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                feed_id TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                error_message TEXT NOT NULL,
                attempt_count INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                replayed INTEGER NOT NULL DEFAULT 0,
                replayed_at TEXT
            )
            """
        )
        self._conn.commit()

    def enqueue(
        self,
        feed_id: str,
        payload_json: str,
        error_message: str,
        attempt_count: int = 0,
    ) -> int:
        """
        Insert a failed submission into the dead-letter queue.

        Returns the row id of the new entry.
        """
        now = datetime.now(timezone.utc).isoformat()
        cursor = self._conn.execute(
            """
            INSERT INTO dead_letter_queue
                (feed_id, payload_json, error_message, attempt_count, created_at)
            VALUES (?, ?, ?, ?, ?)
            """,
            (feed_id, payload_json, error_message, attempt_count, now),
        )
        self._conn.commit()
        entry_id = cursor.lastrowid
        logger.info(
            "DLQ entry %d created for feed %s (attempts=%d)",
            entry_id,
            feed_id,
            attempt_count,
        )
        return entry_id  # type: ignore[return-value]

    def list_unreplayed(self) -> list[DLQEntry]:
        """Return all entries that have not been replayed."""
        rows = self._conn.execute(
            """
            SELECT id, feed_id, payload_json, error_message, attempt_count,
                   created_at, replayed, replayed_at
            FROM dead_letter_queue
            WHERE replayed = 0
            ORDER BY created_at ASC
            """
        ).fetchall()
        return [
            DLQEntry(
                id=row[0],
                feed_id=row[1],
                payload_json=row[2],
                error_message=row[3],
                attempt_count=row[4],
                created_at=row[5],
                replayed=bool(row[6]),
                replayed_at=row[7],
            )
            for row in rows
        ]

    def mark_replayed(self, entry_id: int) -> None:
        """Mark a DLQ entry as replayed."""
        now = datetime.now(timezone.utc).isoformat()
        self._conn.execute(
            """
            UPDATE dead_letter_queue
            SET replayed = 1, replayed_at = ?
            WHERE id = ?
            """,
            (now, entry_id),
        )
        self._conn.commit()
        logger.info("DLQ entry %d marked as replayed", entry_id)

    def count(self) -> int:
        """Return the total number of entries in the DLQ."""
        row = self._conn.execute(
            "SELECT COUNT(*) FROM dead_letter_queue"
        ).fetchone()
        return row[0] if row else 0

    def count_unreplayed(self) -> int:
        """Return the number of unreplayed entries."""
        row = self._conn.execute(
            "SELECT COUNT(*) FROM dead_letter_queue WHERE replayed = 0"
        ).fetchone()
        return row[0] if row else 0

    def close(self) -> None:
        """Close the database connection."""
        if self._conn:
            self._conn.close()

    def __enter__(self) -> "DeadLetterQueue":
        return self

    def __exit__(self, *args: Any) -> None:
        self.close()
