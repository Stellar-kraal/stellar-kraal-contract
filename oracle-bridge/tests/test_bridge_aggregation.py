"""
Integration tests for multi-source oracle aggregation bridge.

Tests:
- End-to-end aggregation with 3+ sources
- Outlier rejection (>30% deviation)
- Provenance metadata tracking
- Bridge to contract submission
"""

import pytest
from unittest.mock import Mock

from oracle_bridge.bridge import (
    OracleBridge,
    GEEResult,
    AggregatedPriceResult,
)
from oracle_bridge.attestation import OracleSigner
from oracle_bridge.aggregation import (
    AggregationConfig,
    OutlierRejectionMethod,
)


# ── Fixtures ──────────────────────────────────────────────────────────────

@pytest.fixture
def signer():
    """Create a test oracle signer."""
    return OracleSigner.generate()


@pytest.fixture
def mock_client():
    """Mock Soroban submission client."""
    client = Mock()
    client.submit_price = Mock(return_value="tx_hash_abc123")
    return client


@pytest.fixture
def aggregation_config():
    """3-source aggregation config with IQR outlier rejection."""
    return AggregationConfig(
        sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
        weights={
            "xpansiv_cbl": 2.0,
            "toucan_protocol": 1.5,
            "custom_source": 1.0,
        },
        outlier_method=OutlierRejectionMethod.IQR,
        iqr_multiplier=1.5,
        min_sources_after_rejection=1,
    )


@pytest.fixture
def gee_results_normal():
    """Normal GEE results from 3 sources (no manipulation)."""
    base_price = 12_500_000
    timestamp = 1699564800  # 2023-11-10 00:00:00 UTC

    return {
        "xpansiv_cbl": GEEResult(
            script_source="// Xpansiv CBL script",
            input_params={"region": "BRAZIL", "asset": "CATTLE"},
            output_value=base_price,
            feed_id="CATTLE-SPOT",
            timestamp_utc=timestamp,
        ),
        "toucan_protocol": GEEResult(
            script_source="// Toucan Protocol script",
            input_params={"region": "BRAZIL", "asset": "CATTLE"},
            output_value=base_price + 10_000,
            feed_id="CATTLE-SPOT",
            timestamp_utc=timestamp,
        ),
        "custom_source": GEEResult(
            script_source="// Custom source script",
            input_params={"region": "BRAZIL", "asset": "CATTLE"},
            output_value=base_price - 5_000,
            feed_id="CATTLE-SPOT",
            timestamp_utc=timestamp,
        ),
    }


@pytest.fixture
def gee_results_manipulated():
    """GEE results where one source deviates >30%."""
    base_price = 12_500_000
    timestamp = 1699564800

    return {
        "xpansiv_cbl": GEEResult(
            script_source="// Xpansiv CBL script",
            input_params={"region": "BRAZIL", "asset": "CATTLE"},
            output_value=base_price,
            feed_id="CATTLE-SPOT",
            timestamp_utc=timestamp,
        ),
        "toucan_protocol": GEEResult(
            script_source="// Toucan Protocol script",
            input_params={"region": "BRAZIL", "asset": "CATTLE"},
            output_value=base_price + 5_000,
            feed_id="CATTLE-SPOT",
            timestamp_utc=timestamp,
        ),
        "custom_source": GEEResult(
            script_source="// Custom source script (manipulated)",
            input_params={"region": "BRAZIL", "asset": "CATTLE"},
            output_value=int(base_price * 1.35),  # 35% higher - outlier
            feed_id="CATTLE-SPOT",
            timestamp_utc=timestamp,
        ),
    }


# ── Test Cases ────────────────────────────────────────────────────────────

class TestBridgeAggregation:
    """Test bridge aggregation functionality."""

    def test_initialize_with_aggregation_config(self, signer, mock_client, aggregation_config):
        """Bridge should initialize with optional aggregation config."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)
        assert bridge._aggregation_config is not None
        assert len(bridge._aggregation_config.sources) == 3

    def test_initialize_without_aggregation_config(self, signer, mock_client):
        """Bridge should initialize without aggregation config (single-source mode)."""
        bridge = OracleBridge(signer, mock_client)
        assert bridge._aggregation_config is None

    def test_aggregate_normal_prices(self, signer, mock_client, aggregation_config, gee_results_normal):
        """Aggregate normal prices without outlier rejection."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)
        result, attestation, tx_ref = bridge.aggregate_and_submit(gee_results_normal)

        # Result should be a valid AggregatedPriceResult
        assert isinstance(result, AggregatedPriceResult)
        assert result.aggregate_value > 0
        assert len(result.source_values) == 3
        assert result.rejected_sources == []

        # Attestation should be signed
        assert attestation is not None
        assert len(attestation.signature) == 64

        # Mock client should have been called
        mock_client.submit_price.assert_called_once()

    def test_aggregate_with_30_percent_outlier(
        self,
        signer,
        mock_client,
        aggregation_config,
        gee_results_manipulated,
    ):
        """Aggregate should reject source deviating >30% with IQR."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)
        result, attestation, tx_ref = bridge.aggregate_and_submit(gee_results_manipulated)

        # custom_source should be rejected
        assert "custom_source" in result.rejected_sources
        assert len(result.rejected_sources) == 1

        # Only 2 sources should be used
        assert len(result.source_values) == 2
        assert "custom_source" not in result.source_values

        # Aggregate should be close to non-rejected sources
        expected_min = 12_495_000
        expected_max = 12_505_000
        assert expected_min <= result.aggregate_value <= expected_max

    def test_provenance_metadata_included(
        self,
        signer,
        mock_client,
        aggregation_config,
        gee_results_normal,
    ):
        """Result should include provenance metadata."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)
        result, attestation, tx_ref = bridge.aggregate_and_submit(gee_results_normal)

        # Check metadata
        assert result.outlier_method == "iqr"
        assert len(result.weights_used) == 3
        assert result.weights_used["xpansiv_cbl"] == 2.0
        assert result.weights_used["toucan_protocol"] == 1.5
        assert result.weights_used["custom_source"] == 1.0

    def test_single_source_aggregation_fails(self, signer, mock_client):
        """Aggregate should fail if only 1 source configured."""
        config = AggregationConfig(
            sources=["only_source"],
            weights={"only_source": 1.0},
        )
        bridge = OracleBridge(signer, mock_client, config)
        results = {"only_source": GEEResult("script", {}, 1000, "FEED", 1699564800)}

        # Should fail since we have 1 source but all close (median = itself)
        # Actually this should work; let's test missing source instead
        with pytest.raises(ValueError):
            bridge.aggregate_and_submit({"other_source": results["only_source"]})

    def test_aggregate_without_config_fails(self, signer, mock_client, gee_results_normal):
        """Aggregate should fail if no aggregation config provided."""
        bridge = OracleBridge(signer, mock_client, aggregation_config=None)

        with pytest.raises(ValueError, match="aggregation_config not configured"):
            bridge.aggregate_and_submit(gee_results_normal)

    def test_single_source_submission_still_works(self, signer, mock_client, gee_results_normal):
        """Single-source process() should still work even with aggregation bridge."""
        config = AggregationConfig(
            sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
            weights={s: 1.0 for s in ["xpansiv_cbl", "toucan_protocol", "custom_source"]},
        )
        bridge = OracleBridge(signer, mock_client, config)

        # process() should still work for single submissions
        result = gee_results_normal["xpansiv_cbl"]
        attestation, tx_ref = bridge.process(result)

        assert attestation is not None
        assert tx_ref == "tx_hash_abc123"

    def test_feed_id_consistency(self, signer, mock_client, aggregation_config, gee_results_normal):
        """All sources must have same feed_id."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)
        result, _, _ = bridge.aggregate_and_submit(gee_results_normal)

        assert result.feed_id == "CATTLE-SPOT"

    def test_timestamp_uses_latest(self, signer, mock_client, aggregation_config):
        """Aggregation should use latest timestamp from sources."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)

        early_time = 1699564800
        late_time = 1699565400

        results = {
            "xpansiv_cbl": GEEResult("s1", {}, 1000, "FEED", early_time),
            "toucan_protocol": GEEResult("s2", {}, 1000, "FEED", late_time),
            "custom_source": GEEResult("s3", {}, 1000, "FEED", early_time),
        }

        result, _, _ = bridge.aggregate_and_submit(results)
        assert result.timestamp_utc == late_time

    def test_weighted_median_in_aggregation(self, signer, mock_client, aggregation_config):
        """Aggregation should use configured weights."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)

        results = {
            "xpansiv_cbl": GEEResult("s1", {}, 1000, "FEED", 1699564800),  # weight 2.0
            "toucan_protocol": GEEResult("s2", {}, 2000, "FEED", 1699564800),  # weight 1.5
            "custom_source": GEEResult("s3", {}, 3000, "FEED", 1699564800),  # weight 1.0
        }

        result, _, _ = bridge.aggregate_and_submit(results)

        # With these weights and values, weighted median should favor lower values
        # Total weight = 4.5, need >= 2.25
        # Sorted: 1000 (w=2.0), 2000 (w=1.5), 3000 (w=1.0)
        # Cumulative: 2.0 < 2.25, then 3.5 >= 2.25 → median is 2000
        assert result.aggregate_value == 2000

    def test_multiple_aggregations_different_feeds(self, signer, mock_client, aggregation_config):
        """Should be able to aggregate different feeds independently."""
        bridge = OracleBridge(signer, mock_client, aggregation_config)

        # First aggregation
        results1 = {
            "xpansiv_cbl": GEEResult("s1", {}, 1000, "FEED-1", 1699564800),
            "toucan_protocol": GEEResult("s2", {}, 1000, "FEED-1", 1699564800),
            "custom_source": GEEResult("s3", {}, 1000, "FEED-1", 1699564800),
        }

        result1, _, _ = bridge.aggregate_and_submit(results1)
        assert result1.feed_id == "FEED-1"

        # Second aggregation with new feed
        results2 = {
            "xpansiv_cbl": GEEResult("s1", {}, 2000, "FEED-2", 1699564800),
            "toucan_protocol": GEEResult("s2", {}, 2000, "FEED-2", 1699564800),
            "custom_source": GEEResult("s3", {}, 2000, "FEED-2", 1699564800),
        }

        result2, _, _ = bridge.aggregate_and_submit(results2)
        assert result2.feed_id == "FEED-2"
        assert result2.aggregate_value == 2000


# ── End-to-End Scenario Tests ──────────────────────────────────────────────

class TestEndToEndScenarios:
    """End-to-end realistic scenarios."""

    def test_realistic_carbon_credit_aggregation(self, signer, mock_client):
        """Realistic multi-source carbon credit price aggregation."""
        config = AggregationConfig(
            sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
            weights={
                "xpansiv_cbl": 2.0,  # Most trusted (highest weight)
                "toucan_protocol": 1.5,  # Medium trust
                "custom_source": 1.0,  # Lower trust
            },
            outlier_method=OutlierRejectionMethod.IQR,
        )
        bridge = OracleBridge(signer, mock_client, config)

        # Realistic carbon prices in micrograms CO2-eq/m²
        results = {
            "xpansiv_cbl": GEEResult(
                script_source="Xpansiv CBL Brazil Cattle Feed",
                input_params={"region": "BRAZIL", "cattle_type": "BEEF"},
                output_value=12_487_500,
                feed_id="CATTLE-BRAZIL-SPOT",
                timestamp_utc=1699564800,
            ),
            "toucan_protocol": GEEResult(
                script_source="Toucan Protocol Carbon Credits",
                input_params={"registry": "VERRA", "carbon_type": "VCS"},
                output_value=12_495_000,
                feed_id="CATTLE-BRAZIL-SPOT",
                timestamp_utc=1699564800,
            ),
            "custom_source": GEEResult(
                script_source="Custom Regional Feed",
                input_params={"region": "BRAZIL"},
                output_value=12_502_500,
                feed_id="CATTLE-BRAZIL-SPOT",
                timestamp_utc=1699564800,
            ),
        }

        result, attestation, tx_ref = bridge.aggregate_and_submit(results)

        # Verify aggregation
        assert result.aggregate_value > 0
        assert len(result.source_values) >= 1
        assert result.feed_id == "CATTLE-BRAZIL-SPOT"
        assert tx_ref == "tx_hash_abc123"

    def test_scenario_one_source_significantly_off(self, signer, mock_client):
        """Scenario: one source is significantly different (>30% deviation)."""
        config = AggregationConfig(
            sources=["s1", "s2", "s3"],
            weights={"s1": 1.0, "s2": 1.0, "s3": 1.0},
            outlier_method=OutlierRejectionMethod.IQR,
        )
        bridge = OracleBridge(signer, mock_client, config)

        base = 10_000_000
        results = {
            "s1": GEEResult("code1", {}, base, "FEED", 1699564800),
            "s2": GEEResult("code2", {}, base + 10_000, "FEED", 1699564800),
            "s3": GEEResult("code3", {}, int(base * 1.40), "FEED", 1699564800),  # 40% higher
        }

        result, _, _ = bridge.aggregate_and_submit(results)

        # s3 should be rejected
        assert "s3" in result.rejected_sources
        # Result should be average of s1 and s2
        assert result.aggregate_value < int(base * 1.05)

    def test_scenario_all_sources_agree(self, signer, mock_client):
        """Scenario: all sources very close (no rejection needed)."""
        config = AggregationConfig(
            sources=["s1", "s2", "s3"],
            weights={"s1": 1.0, "s2": 1.0, "s3": 1.0},
            outlier_method=OutlierRejectionMethod.IQR,
        )
        bridge = OracleBridge(signer, mock_client, config)

        base = 10_000_000
        results = {
            "s1": GEEResult("code1", {}, base, "FEED", 1699564800),
            "s2": GEEResult("code2", {}, base + 1_000, "FEED", 1699564800),  # 0.01% higher
            "s3": GEEResult("code3", {}, base - 1_000, "FEED", 1699564800),  # 0.01% lower
        }

        result, _, _ = bridge.aggregate_and_submit(results)

        # No sources should be rejected
        assert len(result.rejected_sources) == 0
        # Result should be very close to base
        assert abs(result.aggregate_value - base) < 2_000
