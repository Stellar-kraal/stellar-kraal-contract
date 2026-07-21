"""
Tests for oracle_bridge.adapters — feed adapter base and resilience integration.
"""

from __future__ import annotations

import logging
import tempfile
from pathlib import Path
from typing import Any
from unittest.mock import patch

import pytest

from oracle_bridge.adapters.base import FeedAdapter, FeedAdapterConfig, FeedResult
from oracle_bridge.resilience import DeadLetterQueue, RetryConfig


class _SimpleTestAdapter(FeedAdapter):
    """A minimal adapter for testing the base class resilience machinery."""

    def __init__(self, config: FeedAdapterConfig, fail_count: int = 0) -> None:
        super().__init__(config)
        self._fail_count = fail_count
        self._attempts = 0

    def _fetch_price(self) -> FeedResult:
        self._attempts += 1
        if self._attempts <= self._fail_count:
            raise ConnectionError(f"Simulated failure {self._attempts}")
        return FeedResult(
            price=100,
            feed_id=self.config.feed_id,
            timestamp_utc=1_700_000_000,
            metadata={"source": "test"},
        )


class TestFeedAdapterBase:
    def test_fetch_succeeds_immediately(self) -> None:
        config = FeedAdapterConfig(
            feed_id="test-feed",
            retry=RetryConfig(max_retries=3, base_delay_seconds=0.01),
        )
        adapter = _SimpleTestAdapter(config, fail_count=0)
        result = adapter.fetch()
        assert result.price == 100
        assert not result.degraded

    def test_fetch_retries_and_succeeds(self) -> None:
        config = FeedAdapterConfig(
            feed_id="test-feed",
            retry=RetryConfig(max_retries=5, base_delay_seconds=0.01, jitter_factor=0.0),
        )
        adapter = _SimpleTestAdapter(config, fail_count=3)
        result = adapter.fetch()
        assert result.price == 100
        assert adapter._attempts == 4  # initial + 3 retries

    def test_fetch_enters_degraded_mode(self) -> None:
        config = FeedAdapterConfig(
            feed_id="test-feed",
            retry=RetryConfig(max_retries=0, base_delay_seconds=0.01, jitter_factor=0.0),
            degraded_mode_threshold=1,
            staleness_window_seconds=3600,
        )
        adapter = _SimpleTestAdapter(config, fail_count=999)
        # Set a last-known-good price first
        adapter.degraded_state.record_success(500)

        # Should enter degraded and return fallback
        result = adapter.fetch()
        assert result.price == 500
        assert result.degraded
        assert adapter.degraded_state.degraded

    def test_fetch_persists_to_dlq(self, tmp_path: Path) -> None:
        db_path = tmp_path / "test_dlq.db"
        config = FeedAdapterConfig(
            feed_id="test-feed",
            retry=RetryConfig(max_retries=1, base_delay_seconds=0.01, jitter_factor=0.0),
            dlq_db_path=str(db_path),
        )
        adapter = _SimpleTestAdapter(config, fail_count=999)
        with pytest.raises(ConnectionError):
            adapter.fetch()

        dlq = DeadLetterQueue(db_path)
        try:
            assert dlq.count() == 1
            entries = dlq.list_unreplayed()
            assert entries[0].feed_id == "test-feed"
            assert "Simulated failure" in entries[0].error_message
        finally:
            dlq.close()

    def test_adapter_name(self) -> None:
        config = FeedAdapterConfig(feed_id="test")
        adapter = _SimpleTestAdapter(config)
        assert adapter.adapter_name == "_SimpleTestAdapter"
