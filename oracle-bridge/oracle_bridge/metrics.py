"""
oracle_bridge.metrics
=====================

Prometheus metrics for the oracle bridge feed adapters.

Exposes counters for successful/failed submissions and a gauge for
degraded mode status per feed.
"""

from __future__ import annotations

import logging
from typing import Any

logger = logging.getLogger(__name__)

try:
    from prometheus_client import Counter, Gauge, start_http_server

    _PROMETHEUS_AVAILABLE = True
except ImportError:

    class _FakeMetric:
        def inc(self, amount: float = 1) -> None:
            pass

        def set(self, value: float) -> None:
            pass

        def labels(self, **label_values: Any) -> "_FakeMetric":
            return self

    def start_http_server(port: int, addr: str = "") -> None:
        logger.info("Prometheus client not installed; metrics HTTP server disabled")

    _PROMETHEUS_AVAILABLE = False

    # Stub Counter
    class _FakeCounter:  # type: ignore[no-redef]
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            pass

        def inc(self, amount: float = 1) -> None:
            pass

        def labels(self, **label_values: Any) -> "_FakeMetric":
            return _FakeMetric()

    # Stub Gauge
    class _FakeGauge:  # type: ignore[no-redef]
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            pass

        def set(self, value: float) -> None:
            pass

        def labels(self, **label_values: Any) -> "_FakeMetric":
            return _FakeMetric()

    Counter = _FakeCounter  # type: ignore[assignment]
    Gauge = _FakeGauge  # type: ignore[assignment]


# ── Metric Definitions ────────────────────────────────────────────────────────

oracle_feed_success_total = Counter(
    "oracle_feed_success_total",
    "Total number of successful oracle feed submissions",
    ["feed_id", "adapter"],
)

oracle_feed_failure_total = Counter(
    "oracle_feed_failure_total",
    "Total number of failed oracle feed submissions",
    ["feed_id", "adapter", "failure_reason"],
)

oracle_degraded_mode = Gauge(
    "oracle_degraded_mode",
    "Whether a feed adapter is in degraded mode (1 = degraded, 0 = normal)",
    ["feed_id", "adapter"],
)

oracle_retry_attempts_total = Counter(
    "oracle_retry_attempts_total",
    "Total number of retry attempts across all feeds",
    ["feed_id", "adapter"],
)

oracle_dlq_entries_total = Counter(
    "oracle_dlq_entries_total",
    "Total number of dead-letter queue entries created",
    ["feed_id", "adapter"],
)

oracle_fetch_duration_seconds = Gauge(
    "oracle_fetch_duration_seconds",
    "Duration of the most recent feed fetch in seconds",
    ["feed_id", "adapter"],
)


def record_success(feed_id: str, adapter: str) -> None:
    """Increment the success counter for a feed."""
    oracle_feed_success_total.labels(feed_id=feed_id, adapter=adapter).inc()


def record_failure(feed_id: str, adapter: str, reason: str) -> None:
    """Increment the failure counter for a feed."""
    oracle_feed_failure_total.labels(
        feed_id=feed_id, adapter=adapter, failure_reason=reason
    ).inc()


def set_degraded(feed_id: str, adapter: str, degraded: bool) -> None:
    """Set the degraded mode gauge (1 = degraded, 0 = normal)."""
    oracle_degraded_mode.labels(feed_id=feed_id, adapter=adapter).set(
        1.0 if degraded else 0.0
    )


def record_retry(feed_id: str, adapter: str) -> None:
    """Increment the retry counter for a feed."""
    oracle_retry_attempts_total.labels(feed_id=feed_id, adapter=adapter).inc()


def record_dlq_entry(feed_id: str, adapter: str) -> None:
    """Increment the DLQ entry counter for a feed."""
    oracle_dlq_entries_total.labels(feed_id=feed_id, adapter=adapter).inc()


def record_fetch_duration(feed_id: str, adapter: str, duration: float) -> None:
    """Record the fetch duration in seconds."""
    oracle_fetch_duration_seconds.labels(feed_id=feed_id, adapter=adapter).set(
        duration
    )


def start_metrics_server(port: int = 8000, addr: str = "0.0.0.0") -> None:
    """Start the Prometheus metrics HTTP server."""
    if _PROMETHEUS_AVAILABLE:
        start_http_server(port, addr)
        logger.info("Prometheus metrics server started on %s:%d", addr, port)
    else:
        logger.warning(
            "prometheus_client not installed; install with: "
            "pip install prometheus-client"
        )
