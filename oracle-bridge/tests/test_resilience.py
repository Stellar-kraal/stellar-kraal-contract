"""
Tests for oracle_bridge.resilience — retry logic, degraded mode, DLQ.
"""

from __future__ import annotations

import json
import os
import random
import sqlite3
import tempfile
import time
from pathlib import Path
from unittest.mock import patch

import pytest

from oracle_bridge.resilience import (
    DeadLetterQueue,
    DegradedModeState,
    RetryConfig,
    compute_backoff,
    retry,
)


# ── Retry Logic ───────────────────────────────────────────────────────────────


class TestComputeBackoff:
    def test_increases_with_attempt(self) -> None:
        config = RetryConfig(base_delay_seconds=1.0, jitter_factor=0.0)
        d0 = compute_backoff(0, config)
        d1 = compute_backoff(1, config)
        d2 = compute_backoff(2, config)
        assert d0 == 1.0
        assert d1 == 2.0
        assert d2 == 4.0

    def test_capped_at_max_delay(self) -> None:
        config = RetryConfig(
            base_delay_seconds=10.0,
            max_delay_seconds=15.0,
            jitter_factor=0.0,
        )
        d = compute_backoff(10, config)
        assert d == 15.0  # capped

    def test_jitter_adds_variation(self) -> None:
        config = RetryConfig(base_delay_seconds=1.0, jitter_factor=0.5)
        delays = [compute_backoff(0, config) for _ in range(100)]
        # With jitter_factor=0.5, delays should vary between 0.5 and 1.5
        assert any(d != 1.0 for d in delays)
        assert all(0.0 <= d <= 1.5 for d in delays)

    @pytest.mark.parametrize("attempt", range(0, 21))
    def test_never_negative_across_attempts(self, attempt: int) -> None:
        """delay is never negative, even for large attempt counts where the
        exponential term would otherwise dwarf the jitter subtraction."""
        random.seed(42)
        config = RetryConfig(
            base_delay_seconds=1.0,
            max_delay_seconds=60.0,
            jitter_factor=0.5,
        )
        delay = compute_backoff(attempt, config)
        assert delay >= 0.0

    @pytest.mark.parametrize("attempt", range(0, 21))
    def test_never_exceeds_max_delay_with_jitter(self, attempt: int) -> None:
        """delay is bounded by max_delay_seconds * (1 + jitter_factor), since
        the exponential term is capped at max_delay_seconds before jitter is
        applied and jitter can add at most jitter_factor of that capped value."""
        random.seed(42)
        config = RetryConfig(
            base_delay_seconds=1.0,
            max_delay_seconds=60.0,
            jitter_factor=0.3,
        )
        delay = compute_backoff(attempt, config)
        assert delay <= config.max_delay_seconds * (1 + config.jitter_factor)

    def test_bounded_across_seeded_random_states(self) -> None:
        """Cross-check the invariants over many seeded random states and the
        full 0-20 attempt range, so the bound isn't an artifact of one seed."""
        config = RetryConfig(
            base_delay_seconds=1.0,
            max_delay_seconds=60.0,
            jitter_factor=0.4,
        )
        upper_bound = config.max_delay_seconds * (1 + config.jitter_factor)
        for seed in range(10):
            random.seed(seed)
            for attempt in range(0, 21):
                delay = compute_backoff(attempt, config)
                assert 0.0 <= delay <= upper_bound

    def test_high_attempt_counts_stay_capped(self) -> None:
        """At high attempt counts, 2**attempt vastly exceeds max_delay_seconds,
        so the pre-jitter delay must be pinned at the cap rather than
        overflowing or growing unbounded."""
        random.seed(42)
        config = RetryConfig(
            base_delay_seconds=1.0,
            max_delay_seconds=60.0,
            jitter_factor=0.1,
        )
        for attempt in (15, 18, 20):
            delay = compute_backoff(attempt, config)
            assert 0.0 <= delay <= config.max_delay_seconds * (1 + config.jitter_factor)

    def test_zero_jitter_factor_is_deterministic(self) -> None:
        """With jitter_factor=0.0 the result is exactly the capped exponential
        delay, regardless of random state."""
        config = RetryConfig(
            base_delay_seconds=2.0,
            max_delay_seconds=50.0,
            jitter_factor=0.0,
        )
        for seed in range(5):
            random.seed(seed)
            for attempt in range(0, 21):
                delay = compute_backoff(attempt, config)
                expected = min(config.base_delay_seconds * (2 ** attempt), config.max_delay_seconds)
                assert delay == expected


class TestRetry:
    def test_succeeds_on_first_try(self) -> None:
        config = RetryConfig(max_retries=3)
        result = retry(lambda: 42, config)
        assert result == 42

    def test_retries_and_succeeds(self) -> None:
        call_count = [0]

        def flaky() -> int:
            call_count[0] += 1
            if call_count[0] < 3:
                raise ValueError("not ready yet")
            return 99

        config = RetryConfig(max_retries=5, base_delay_seconds=0.01, jitter_factor=0.0)
        result = retry(flaky, config)
        assert result == 99
        assert call_count[0] == 3

    def test_exhausts_retries(self) -> None:
        call_count = [0]

        def always_fails() -> int:
            call_count[0] += 1
            raise ValueError("boom")

        config = RetryConfig(max_retries=2, base_delay_seconds=0.01, jitter_factor=0.0)
        with pytest.raises(ValueError, match="boom"):
            retry(always_fails, config)
        assert call_count[0] == 3  # initial + 2 retries

    def test_custom_exception_types(self) -> None:
        def raises_type_error() -> int:
            raise TypeError("type mismatch")

        config = RetryConfig(max_retries=1, base_delay_seconds=0.01)
        # ValueError is not caught, so it should propagate immediately
        with pytest.raises(TypeError, match="type mismatch"):
            retry(raises_type_error, config, exc_types=(ValueError,))


# ── Degraded Mode ──────────────────────────────────────────────────────────────


class TestDegradedModeState:
    def test_initial_state(self) -> None:
        state = DegradedModeState(feed_id="test-feed")
        assert not state.degraded
        assert state.last_known_good_price is None
        assert state._consecutive_failures == 0

    def test_enters_degraded_after_threshold(self) -> None:
        state = DegradedModeState(
            consecutive_failures_threshold=3,
            feed_id="test-feed",
        )
        assert not state.degraded
        state.record_failure()
        assert not state.degraded
        state.record_failure()
        assert not state.degraded
        state.record_failure()
        assert state.degraded
        assert state._consecutive_failures == 3

    def test_success_resets_and_stores_price(self) -> None:
        state = DegradedModeState(feed_id="test-feed")
        state.record_failure()
        state.record_failure()
        state.record_success(100)
        assert not state.degraded
        assert state._consecutive_failures == 0
        assert state.last_known_good_price == 100

    def test_degraded_fallback_returns_stale_price(self) -> None:
        state = DegradedModeState(
            consecutive_failures_threshold=1,
            staleness_window_seconds=3600,
            feed_id="test-feed",
        )
        state.record_success(200)
        state.record_failure()
        assert state.degraded
        assert state.last_known_good_price == 200

    def test_stale_price_expires(self) -> None:
        state = DegradedModeState(
            consecutive_failures_threshold=1,
            staleness_window_seconds=0.1,
            feed_id="test-feed",
        )
        state.record_success(300)
        state.record_failure()
        assert state.degraded
        assert state.last_known_good_price == 300
        time.sleep(0.15)
        assert state.last_known_good_price is None

    def test_exit_degraded(self) -> None:
        state = DegradedModeState(
            consecutive_failures_threshold=1,
            feed_id="test-feed",
        )
        state.record_failure()
        assert state.degraded
        state.exit_degraded()
        assert not state.degraded
        assert state._consecutive_failures == 0

    def test_state_info(self) -> None:
        state = DegradedModeState(feed_id="test-feed", staleness_window_seconds=300)
        state.record_success(150)
        info = state.state_info()
        assert info["feed_id"] == "test-feed"
        assert info["degraded"] is False
        assert info["last_known_good_price"] == 150
        assert info["staleness_window_seconds"] == 300


# ── Dead-Letter Queue ─────────────────────────────────────────────────────────


class TestDeadLetterQueue:
    @pytest.fixture
    def db_path(self) -> Path:
        with tempfile.NamedTemporaryFile(suffix=".db", delete=False) as f:
            path = Path(f.name)
        yield path
        os.unlink(path)

    def test_enqueue_and_count(self, db_path: Path) -> None:
        dlq = DeadLetterQueue(db_path)
        try:
            assert dlq.count() == 0
            dlq.enqueue("test-feed", '{"price": 100}', "API error", 3)
            assert dlq.count() == 1
            dlq.enqueue("other-feed", '{"price": 200}', "timeout", 2)
            assert dlq.count() == 2
        finally:
            dlq.close()

    def test_list_unreplayed(self, db_path: Path) -> None:
        dlq = DeadLetterQueue(db_path)
        try:
            dlq.enqueue("feed-a", "{}", "error 1", 1)
            dlq.enqueue("feed-b", "{}", "error 2", 2)
            entries = dlq.list_unreplayed()
            assert len(entries) == 2
            assert entries[0].feed_id == "feed-a"
            assert entries[1].feed_id == "feed-b"
            assert not entries[0].replayed
        finally:
            dlq.close()

    def test_mark_replayed(self, db_path: Path) -> None:
        dlq = DeadLetterQueue(db_path)
        try:
            entry_id = dlq.enqueue("feed-a", "{}", "error", 1)
            dlq.mark_replayed(entry_id)
            unreplayed = dlq.list_unreplayed()
            assert len(unreplayed) == 0
            assert dlq.count() == 1
            assert dlq.count_unreplayed() == 0
        finally:
            dlq.close()

    def test_count_unreplayed(self, db_path: Path) -> None:
        dlq = DeadLetterQueue(db_path)
        try:
            dlq.enqueue("feed-a", "{}", "e1", 1)
            id2 = dlq.enqueue("feed-b", "{}", "e2", 1)
            dlq.mark_replayed(id2)
            assert dlq.count_unreplayed() == 1
        finally:
            dlq.close()

    def test_full_metadata_in_entry(self, db_path: Path) -> None:
        dlq = DeadLetterQueue(db_path)
        try:
            payload = json.dumps({"price": 5000, "feed_id": "CBL-EUA"})
            dlq.enqueue("CBL-EUA", payload, "HTTP 503", 3)
            entries = dlq.list_unreplayed()
            assert len(entries) == 1
            entry = entries[0]
            assert entry.feed_id == "CBL-EUA"
            assert entry.payload_json == payload
            assert entry.error_message == "HTTP 503"
            assert entry.attempt_count == 3
            assert entry.id is not None
            assert entry.created_at != ""
            assert not entry.replayed
            assert entry.replayed_at is None
        finally:
            dlq.close()

    def test_context_manager(self, db_path: Path) -> None:
        with DeadLetterQueue(db_path) as dlq:
            dlq.enqueue("test", "{}", "err", 1)
            assert dlq.count() == 1


# ── Integration: Degraded Mode Feed Adapter ───────────────────────────────────


class TestIntegrationDegradedMode:
    def test_fails_then_uses_fallback(self) -> None:
        from oracle_bridge.adapters.base import FeedAdapter, FeedAdapterConfig, FeedResult

        class _FailingAdapter(FeedAdapter):
            def __init__(self, config: FeedAdapterConfig) -> None:
                super().__init__(config)

            def _fetch_price(self) -> FeedResult:
                raise ConnectionError("API unavailable")

        config = FeedAdapterConfig(
            feed_id="test-feed",
            retry=RetryConfig(max_retries=0, base_delay_seconds=0.01, jitter_factor=0.0),
            degraded_mode_threshold=1,
            staleness_window_seconds=3600,
        )
        adapter = _FailingAdapter(config)

        # Record a "good" price manually as last-known-good
        adapter.degraded_state.record_success(9999)

        # Now the adapter should fail, enter degraded, and return fallback
        result = adapter.fetch()
        assert result.price == 9999
        assert result.degraded
        assert adapter.degraded_state.degraded
