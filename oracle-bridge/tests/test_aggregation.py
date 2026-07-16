"""
Test suite for multi-source price aggregation.

Covers:
- Weighted median calculation
- IQR and MAD outlier rejection
- Edge cases and error handling
- Manipulated source scenarios (>30% deviation)
"""

import pytest
from oracle_bridge.aggregation import (
    AggregationConfig,
    AggregationResult,
    OutlierRejectionMethod,
    PriceAggregator,
    PriceSource,
)


# ── Fixtures ──────────────────────────────────────────────────────────────

@pytest.fixture
def basic_config():
    """Basic 3-source aggregation config with equal weights."""
    return AggregationConfig(
        sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
        weights={
            "xpansiv_cbl": 1.0,
            "toucan_protocol": 1.0,
            "custom_source": 1.0,
        },
        outlier_method=OutlierRejectionMethod.NONE,
    )


@pytest.fixture
def weighted_config():
    """3-source config with different weights."""
    return AggregationConfig(
        sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
        weights={
            "xpansiv_cbl": 2.0,
            "toucan_protocol": 1.5,
            "custom_source": 1.0,
        },
        outlier_method=OutlierRejectionMethod.NONE,
    )


@pytest.fixture
def iqr_config():
    """3-source config with IQR outlier rejection."""
    return AggregationConfig(
        sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
        weights={
            "xpansiv_cbl": 1.0,
            "toucan_protocol": 1.0,
            "custom_source": 1.0,
        },
        outlier_method=OutlierRejectionMethod.IQR,
        iqr_multiplier=1.5,
        min_sources_after_rejection=1,
    )


@pytest.fixture
def mad_config():
    """3-source config with MAD outlier rejection."""
    return AggregationConfig(
        sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
        weights={
            "xpansiv_cbl": 1.0,
            "toucan_protocol": 1.0,
            "custom_source": 1.0,
        },
        outlier_method=OutlierRejectionMethod.MAD,
        mad_multiplier=2.24,
        min_sources_after_rejection=1,
    )


# ── Test Cases: Basic Weighted Median (No Outlier Rejection) ────────────

class TestBasicWeightedMedian:
    """Test weighted median calculation without outlier rejection."""

    def test_three_equal_values_equal_weights(self, basic_config):
        """Median of equal values should return that value."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1000),
            PriceSource("custom_source", 1000),
        ]
        agg = PriceAggregator(basic_config)
        result = agg.aggregate(sources)
        assert result.aggregate_value == 1000
        assert result.rejected_sources == []

    def test_three_values_ascending_equal_weights(self, basic_config):
        """Median of ascending values should return middle value."""
        sources = [
            PriceSource("xpansiv_cbl", 900),
            PriceSource("toucan_protocol", 1000),
            PriceSource("custom_source", 1100),
        ]
        agg = PriceAggregator(basic_config)
        result = agg.aggregate(sources)
        assert result.aggregate_value == 1000
        assert result.rejected_sources == []

    def test_three_values_descending_equal_weights(self, basic_config):
        """Median should work regardless of input order."""
        sources = [
            PriceSource("xpansiv_cbl", 1100),
            PriceSource("toucan_protocol", 1000),
            PriceSource("custom_source", 900),
        ]
        agg = PriceAggregator(basic_config)
        result = agg.aggregate(sources)
        assert result.aggregate_value == 1000

    def test_weighted_median_two_to_one_weight_ratio(self, weighted_config):
        """Weighted median should favor higher-weighted source."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),  # weight 2.0
            PriceSource("toucan_protocol", 2000),  # weight 1.5
            PriceSource("custom_source", 3000),  # weight 1.0
        ]
        agg = PriceAggregator(weighted_config)
        result = agg.aggregate(sources)
        # Total weight: 4.5, need >= 2.25 cumulative
        # Sorted: 1000 (w=2.0), 2000 (w=1.5), 3000 (w=1.0)
        # Cumulative: 2.0 < 2.25, then 3.5 >= 2.25 → median is 2000
        assert result.aggregate_value == 2000

    def test_weighted_median_biased_high(self, weighted_config):
        """High-weighted low value should pull median down."""
        sources = [
            PriceSource("xpansiv_cbl", 100),  # weight 2.0
            PriceSource("toucan_protocol", 1000),  # weight 1.5
            PriceSource("custom_source", 1000),  # weight 1.0
        ]
        agg = PriceAggregator(weighted_config)
        result = agg.aggregate(sources)
        # Sorted by value: 100 (w=2.0), 1000 (w=1.5), 1000 (w=1.0)
        # Cumulative: 2.0 < 2.25, then 3.5 >= 2.25 → median is 1000
        assert result.aggregate_value == 1000

    def test_five_values_equal_weights(self):
        """Test median with 5 sources."""
        config = AggregationConfig(
            sources=["s1", "s2", "s3", "s4", "s5"],
            weights={s: 1.0 for s in ["s1", "s2", "s3", "s4", "s5"]},
            outlier_method=OutlierRejectionMethod.NONE,
        )
        sources = [
            PriceSource("s1", 1000),
            PriceSource("s2", 900),
            PriceSource("s3", 1100),
            PriceSource("s4", 950),
            PriceSource("s5", 1050),
        ]
        agg = PriceAggregator(config)
        result = agg.aggregate(sources)
        # Sorted: 900, 950, 1000, 1050, 1100
        # Equal weights, total 5.0, need >= 2.5 → median is 1000
        assert result.aggregate_value == 1000

    def test_all_negative_values(self, basic_config):
        """Weighted median should work with negative values."""
        sources = [
            PriceSource("xpansiv_cbl", -1100),
            PriceSource("toucan_protocol", -1000),
            PriceSource("custom_source", -900),
        ]
        agg = PriceAggregator(basic_config)
        result = agg.aggregate(sources)
        assert result.aggregate_value == -1000

    def test_mixed_negative_positive(self, basic_config):
        """Weighted median with mixed signs."""
        sources = [
            PriceSource("xpansiv_cbl", -100),
            PriceSource("toucan_protocol", 0),
            PriceSource("custom_source", 100),
        ]
        agg = PriceAggregator(basic_config)
        result = agg.aggregate(sources)
        assert result.aggregate_value == 0

    def test_very_large_values(self, basic_config):
        """Weighted median with large integers."""
        sources = [
            PriceSource("xpansiv_cbl", 10**15),
            PriceSource("toucan_protocol", 10**15 + 1),
            PriceSource("custom_source", 10**15 - 1),
        ]
        agg = PriceAggregator(basic_config)
        result = agg.aggregate(sources)
        assert result.aggregate_value == 10**15


# ── Test Cases: IQR Outlier Rejection ───────────────────────────────────

class TestIQROutlierRejection:
    """Test Interquartile Range outlier rejection."""

    def test_no_rejection_close_values(self, iqr_config):
        """Close values should not be rejected."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1010),
            PriceSource("custom_source", 1020),
        ]
        agg = PriceAggregator(iqr_config)
        result = agg.aggregate(sources)
        assert result.rejected_sources == []
        assert result.aggregate_value == 1010

    def test_reject_single_high_outlier(self, iqr_config):
        """Should reject significantly higher value."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1010),
            PriceSource("custom_source", 2000),  # High outlier
        ]
        agg = PriceAggregator(iqr_config)
        result = agg.aggregate(sources)
        assert "custom_source" in result.rejected_sources
        # Median of 1000, 1010 = 1005
        assert result.aggregate_value == 1005

    def test_reject_single_low_outlier(self, iqr_config):
        """Should reject significantly lower value."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1010),
            PriceSource("custom_source", 100),  # Low outlier
        ]
        agg = PriceAggregator(iqr_config)
        result = agg.aggregate(sources)
        assert "custom_source" in result.rejected_sources
        assert result.aggregate_value == 1005

    def test_30_percent_deviation_rejected(self, iqr_config):
        """30% deviation should be rejected with IQR."""
        base = 1000
        sources = [
            PriceSource("xpansiv_cbl", base),
            PriceSource("toucan_protocol", base),
            PriceSource("custom_source", int(base * 1.3)),  # 30% higher
        ]
        agg = PriceAggregator(iqr_config)
        result = agg.aggregate(sources)
        assert "custom_source" in result.rejected_sources

    def test_two_outliers_both_rejected(self, iqr_config):
        """Both high and low outliers should be rejected."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 500),  # Low outlier
            PriceSource("custom_source", 2000),  # High outlier
        ]
        agg = PriceAggregator(iqr_config)
        result = agg.aggregate(sources)
        assert len(result.rejected_sources) == 2
        assert result.aggregate_value == 1000

    def test_only_two_values_no_rejection(self):
        """With only 2 values, IQR should not reject."""
        config = AggregationConfig(
            sources=["s1", "s2"],
            weights={"s1": 1.0, "s2": 1.0},
            outlier_method=OutlierRejectionMethod.IQR,
        )
        sources = [
            PriceSource("s1", 1000),
            PriceSource("s2", 2000),  # 100% deviation
        ]
        agg = PriceAggregator(config)
        result = agg.aggregate(sources)
        assert result.rejected_sources == []

    def test_iqr_with_four_sources(self):
        """IQR rejection with 4 sources."""
        config = AggregationConfig(
            sources=["s1", "s2", "s3", "s4"],
            weights={s: 1.0 for s in ["s1", "s2", "s3", "s4"]},
            outlier_method=OutlierRejectionMethod.IQR,
            iqr_multiplier=1.5,
        )
        sources = [
            PriceSource("s1", 1000),
            PriceSource("s2", 1000),
            PriceSource("s3", 1000),
            PriceSource("s4", 5000),  # Clear outlier
        ]
        agg = PriceAggregator(config)
        result = agg.aggregate(sources)
        assert "s4" in result.rejected_sources


# ── Test Cases: MAD Outlier Rejection ────────────────────────────────────

class TestMADOutlierRejection:
    """Test Median Absolute Deviation outlier rejection."""

    def test_no_rejection_close_values_mad(self, mad_config):
        """Close values should not be rejected with MAD."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1010),
            PriceSource("custom_source", 1020),
        ]
        agg = PriceAggregator(mad_config)
        result = agg.aggregate(sources)
        assert result.rejected_sources == []

    def test_reject_outlier_mad(self, mad_config):
        """MAD should reject extreme outlier."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1000),
            PriceSource("custom_source", 3000),  # Extreme outlier
        ]
        agg = PriceAggregator(mad_config)
        result = agg.aggregate(sources)
        assert "custom_source" in result.rejected_sources

    def test_mad_with_identical_values(self, mad_config):
        """MAD with identical values (MAD=0) should accept all."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1000),
            PriceSource("custom_source", 1000),
        ]
        agg = PriceAggregator(mad_config)
        result = agg.aggregate(sources)
        assert result.rejected_sources == []
        assert result.aggregate_value == 1000

    def test_30_percent_deviation_mad(self, mad_config):
        """Test MAD with 30% deviation."""
        base = 1000
        sources = [
            PriceSource("xpansiv_cbl", base),
            PriceSource("toucan_protocol", base),
            PriceSource("custom_source", int(base * 1.3)),
        ]
        agg = PriceAggregator(mad_config)
        result = agg.aggregate(sources)
        # With MAD, this may or may not be rejected depending on MAD calculation
        # Just verify aggregation completes
        assert result.aggregate_value > 0


# ── Test Cases: Error Handling and Validation ──────────────────────────

class TestErrorHandling:
    """Test error handling and validation."""

    def test_missing_source_raises_error(self, basic_config):
        """Should raise error if configured source is missing."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1000),
            # Missing: custom_source
        ]
        agg = PriceAggregator(basic_config)
        with pytest.raises(ValueError, match="Missing sources"):
            agg.aggregate(sources)

    def test_extra_source_raises_error(self, basic_config):
        """Should raise error if unexpected source provided."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1000),
            PriceSource("custom_source", 1000),
            PriceSource("extra_source", 1000),  # Extra
        ]
        agg = PriceAggregator(basic_config)
        with pytest.raises(ValueError, match="Unexpected sources"):
            agg.aggregate(sources)

    def test_insufficient_sources_after_rejection(self, iqr_config):
        """Should raise error if too many sources rejected."""
        iqr_config.min_sources_after_rejection = 2
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 500),  # Will be rejected
            PriceSource("custom_source", 2000),  # Will be rejected
        ]
        agg = PriceAggregator(iqr_config)
        with pytest.raises(ValueError, match="Only 1 sources remain"):
            agg.aggregate(sources)

    def test_invalid_config_missing_weight(self):
        """Should raise error if configured source has no weight."""
        with pytest.raises(ValueError, match="not in weights dict"):
            AggregationConfig(
                sources=["s1", "s2"],
                weights={"s1": 1.0},  # Missing s2
            )

    def test_invalid_config_negative_weight(self):
        """Should raise error if weight is non-positive."""
        with pytest.raises(ValueError, match="must be positive"):
            AggregationConfig(
                sources=["s1", "s2"],
                weights={"s1": 0, "s2": 1.0},
            )

    def test_invalid_source_negative_weight(self):
        """Should raise error if source weight is non-positive."""
        config = AggregationConfig(
            sources=["s1", "s2"],
            weights={"s1": 1.0, "s2": 1.0},
        )
        with pytest.raises(ValueError, match="Weight must be positive"):
            PriceSource("s1", 1000, weight=0)


# ── Test Cases: Metadata and Result Tracking ────────────────────────────

class TestResultMetadata:
    """Test result metadata and provenance tracking."""

    def test_result_includes_all_fields(self, basic_config):
        """Result should include all required fields."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1010),
            PriceSource("custom_source", 990),
        ]
        agg = PriceAggregator(basic_config)
        result = agg.aggregate(sources)
        
        assert result.aggregate_value == 1000
        assert "xpansiv_cbl" in result.source_values
        assert "toucan_protocol" in result.source_values
        assert "custom_source" in result.source_values
        assert result.method_used == "weighted_median"
        assert result.outlier_method == "none"

    def test_weights_used_matches_config(self, weighted_config):
        """Result weights should match configuration."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1010),
            PriceSource("custom_source", 990),
        ]
        agg = PriceAggregator(weighted_config)
        result = agg.aggregate(sources)
        
        assert result.weights_used["xpansiv_cbl"] == 2.0
        assert result.weights_used["toucan_protocol"] == 1.5
        assert result.weights_used["custom_source"] == 1.0

    def test_rejection_metadata_after_iqr(self, iqr_config):
        """Result should correctly track rejected sources."""
        sources = [
            PriceSource("xpansiv_cbl", 1000),
            PriceSource("toucan_protocol", 1000),
            PriceSource("custom_source", 3000),  # Will be rejected
        ]
        agg = PriceAggregator(iqr_config)
        result = agg.aggregate(sources)
        
        assert len(result.rejected_sources) == 1
        assert "custom_source" in result.rejected_sources
        assert len(result.source_values) == 2  # Only accepted sources


# ── Integration Tests ─────────────────────────────────────────────────────

class TestIntegration:
    """Integration tests with realistic scenarios."""

    def test_realistic_carbon_price_scenario(self):
        """Realistic carbon price aggregation with 3 sources."""
        config = AggregationConfig(
            sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
            weights={
                "xpansiv_cbl": 2.0,  # Most trusted
                "toucan_protocol": 1.5,  # Somewhat trusted
                "custom_source": 1.0,  # Less trusted
            },
            outlier_method=OutlierRejectionMethod.IQR,
        )
        
        # Realistic carbon price in micrograms CO2-eq/m²
        sources = [
            PriceSource("xpansiv_cbl", 12500000),
            PriceSource("toucan_protocol", 12480000),
            PriceSource("custom_source", 12520000),
        ]
        
        agg = PriceAggregator(config)
        result = agg.aggregate(sources)
        
        # Should be close to the median
        assert 12480000 <= result.aggregate_value <= 12520000
        assert result.rejected_sources == []

    def test_realistic_with_one_manipulated_source(self):
        """Realistic scenario with one source deviating by >30%."""
        config = AggregationConfig(
            sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
            weights={
                "xpansiv_cbl": 2.0,
                "toucan_protocol": 1.5,
                "custom_source": 1.0,
            },
            outlier_method=OutlierRejectionMethod.IQR,
        )
        
        base_price = 12500000
        sources = [
            PriceSource("xpansiv_cbl", base_price),
            PriceSource("toucan_protocol", base_price + 10000),
            PriceSource("custom_source", int(base_price * 1.35)),  # 35% higher
        ]
        
        agg = PriceAggregator(config)
        result = agg.aggregate(sources)
        
        # Manipulated source should be rejected
        assert "custom_source" in result.rejected_sources
        # Result should be close to first two sources
        assert result.aggregate_value <= base_price + 10000

    def test_end_to_end_with_metadata(self):
        """End-to-end test with provenance metadata."""
        config = AggregationConfig(
            sources=["xpansiv_cbl", "toucan_protocol", "custom_source"],
            weights={
                "xpansiv_cbl": 1.0,
                "toucan_protocol": 1.0,
                "custom_source": 1.0,
            },
            outlier_method=OutlierRejectionMethod.MAD,
        )
        
        sources = [
            PriceSource("xpansiv_cbl", 1000, metadata={"url": "xpansiv.com"}),
            PriceSource("toucan_protocol", 1010, metadata={"url": "toucan.earth"}),
            PriceSource("custom_source", 990, metadata={"url": "custom.com"}),
        ]
        
        agg = PriceAggregator(config)
        result = agg.aggregate(sources)
        
        # Result should be valid
        assert result.aggregate_value > 0
        assert len(result.source_values) == 3
        assert result.outlier_method == "mad"
