"""
oracle_bridge.adapters.base
===========================

Abstract base class for market data feed adapters.

All adapters (Xpansiv CBL, Toucan Protocol, etc.) inherit from
:class:`FeedAdapter` and gain retry logic, degraded-mode fallback,
dead-letter queue integration, and structured observability.
"""

from __future__ import annotations

import logging
import time
from abc import ABC, abstractmethod
from dataclasses import dataclass, field
from typing import Any

from oracle_bridge.resilience import (
    DeadLetterQueue,
    DegradedModeState,
    RetryConfig,
    retry,
)

logger = logging.getLogger(__name__)


@dataclass
class FeedResult:
    """
    Result of a single feed adapter fetch.

    Parameters
    ----------
    price:
        The fetched price as an integer (scaled to i64 micro-units).
    feed_id:
        Feed identifier string.
    timestamp_utc:
        Unix timestamp of the fetch.
    metadata:
        Arbitrary metadata from the adapter (e.g. source, confidence).
    degraded:
        Whether the result was served from degraded-mode fallback.
    """

    price: int
    feed_id: str
    timestamp_utc: int
    metadata: dict[str, Any] = field(default_factory=dict)
    degraded: bool = False


@dataclass
class FeedAdapterConfig:
    """
    Configuration for a feed adapter.

    Parameters
    ----------
    feed_id:
        Unique identifier for this feed.
    retry:
        Retry configuration. If None, defaults are used.
    degraded_mode_threshold:
        Consecutive failures before entering degraded mode.
    staleness_window_seconds:
        Max age of last-known-good price for degraded-mode fallback.
    dlq_db_path:
        Path to the SQLite database for the dead-letter queue.
        If None, DLQ persistence is disabled.
    """

    feed_id: str = ""
    retry: RetryConfig | None = None
    degraded_mode_threshold: int = 3
    staleness_window_seconds: float = 300.0
    dlq_db_path: str | None = None


class FeedAdapter(ABC):
    """
    Abstract base for market data feed adapters.

    Subclasses must implement :meth:`_fetch_price` which performs the
    actual API call.  The public :meth:`fetch` method adds:
        - Retry with exponential backoff and jitter
        - Degraded-mode fallback to last-known-good price
        - Dead-letter queue persistence on terminal failure
        - Structured observability (logging, metrics)
    """

    def __init__(self, config: FeedAdapterConfig) -> None:
        self.config = config
        self.retry_config = config.retry or RetryConfig()
        self.degraded_state = DegradedModeState(
            consecutive_failures_threshold=config.degraded_mode_threshold,
            staleness_window_seconds=config.staleness_window_seconds,
            feed_id=config.feed_id,
        )
        self.dlq: DeadLetterQueue | None = None
        if config.dlq_db_path:
            self.dlq = DeadLetterQueue(config.dlq_db_path)

    @property
    def adapter_name(self) -> str:
        """Human-readable adapter name (derived from class name)."""
        return type(self).__name__

    @abstractmethod
    def _fetch_price(self) -> FeedResult:
        """
        Perform the actual market data API call.

        Must be implemented by subclasses.
        """

    def fetch(self) -> FeedResult:
        """
        Fetch the current price with full resilience machinery.

        Returns
        -------
        A :class:`FeedResult` with the price (potentially from degraded
        mode fallback) and metadata.

        Raises
        ------
        Exception:
            If all retries are exhausted and no degraded-mode fallback
            is available.
        """
        from oracle_bridge import metrics

        feed_id = self.config.feed_id
        start = time.time()

        try:
            result = retry(
                self._fetch_price,
                config=self.retry_config,
                context=f"{self.adapter_name}/{feed_id}",
            )
            self.degraded_state.record_success(result.price)
            metrics.record_success(feed_id, self.adapter_name)
            metrics.set_degraded(feed_id, self.adapter_name, False)
            duration = time.time() - start
            metrics.record_fetch_duration(feed_id, self.adapter_name, duration)
            logger.info(
                "Feed %s/%s fetched price=%d (took %.2fs)",
                self.adapter_name,
                feed_id,
                result.price,
                duration,
            )
            return result

        except Exception as exc:
            metrics.record_failure(
                feed_id, self.adapter_name, reason=type(exc).__name__
            )
            self.degraded_state.record_failure()
            degraded = self.degraded_state.degraded
            metrics.set_degraded(feed_id, self.adapter_name, degraded)

            duration = time.time() - start
            metrics.record_fetch_duration(feed_id, self.adapter_name, duration)

            if degraded:
                fallback_price = self.degraded_state.last_known_good_price
                if fallback_price is not None:
                    logger.warning(
                        "Feed %s/%s using degraded-mode fallback price=%d",
                        self.adapter_name,
                        feed_id,
                        fallback_price,
                    )
                    return FeedResult(
                        price=fallback_price,
                        feed_id=feed_id,
                        timestamp_utc=int(time.time()),
                        metadata={
                            "degraded_fallback": True,
                            "original_error": str(exc),
                        },
                        degraded=True,
                    )
                else:
                    logger.error(
                        "Feed %s/%s degraded but no last-known-good price available",
                        self.adapter_name,
                        feed_id,
                    )

            # Persist to dead-letter queue
            if self.dlq:
                payload_json = (
                    f'{{"feed_id": "{feed_id}", '
                    f'"error": "{exc}"}}'
                )
                self.dlq.enqueue(
                    feed_id=feed_id,
                    payload_json=payload_json,
                    error_message=str(exc),
                    attempt_count=self.retry_config.max_retries + 1,
                )
                metrics.record_dlq_entry(feed_id, self.adapter_name)

            raise

    def replay_dlq_entry(self, entry_id: int) -> bool:
        """
        Replay a single dead-letter queue entry.

        Returns True if the replay succeeded, False otherwise.
        """
        if not self.dlq:
            logger.warning("DLQ not configured; cannot replay entry %d", entry_id)
            return False

        entries = self.dlq.list_unreplayed()
        target = [e for e in entries if e.id == entry_id]
        if not target:
            logger.warning("DLQ entry %d not found or already replayed", entry_id)
            return False

        entry = target[0]
        try:
            result = self.fetch()
            if result:
                self.dlq.mark_replayed(entry_id)
                logger.info("DLQ entry %d replayed successfully", entry_id)
                return True
        except Exception as exc:
            logger.error("DLQ replay of entry %d failed: %s", entry_id, exc)
        return False
