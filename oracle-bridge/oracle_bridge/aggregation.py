"""
oracle_bridge.aggregation
==========================

Multi-source price aggregation with outlier rejection for the oracle bridge.

Supports weighted median calculation with configurable outlier rejection
using Interquartile Range (IQR) or Median Absolute Deviation (MAD) methods.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any
import statistics


class OutlierRejectionMethod(Enum):
    """Outlier rejection strategy."""
    IQR = "iqr"  # Interquartile Range
    MAD = "mad"  # Median Absolute Deviation
    NONE = "none"  # No outlier rejection


@dataclass
class PriceSource:
    """Represents a single price source with optional weighting."""
    source_id: str
    value: int  # Integer representation (scaled)
    weight: float = 1.0
    metadata: dict[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if self.weight <= 0:
            raise ValueError(f"Weight must be positive, got {self.weight}")


@dataclass
class AggregationConfig:
    """Configuration for multi-source price aggregation."""
    sources: list[str]  # List of configured source IDs
    weights: dict[str, float]  # Source ID -> weight mapping
    outlier_method: OutlierRejectionMethod = OutlierRejectionMethod.IQR
    iqr_multiplier: float = 1.5  # Standard IQR multiplier
    mad_multiplier: float = 2.24  # MAD multiplier (~1.96 for normal distribution)
    min_sources_after_rejection: int = 1

    def __post_init__(self) -> None:
        # Validate all sources have weights
        for source_id in self.sources:
            if source_id not in self.weights:
                raise ValueError(f"Source {source_id} not in weights dict")
        
        # Validate weights are positive
        for source_id, weight in self.weights.items():
            if weight <= 0:
                raise ValueError(f"Weight for {source_id} must be positive, got {weight}")
        
        if self.min_sources_after_rejection < 1:
            raise ValueError("min_sources_after_rejection must be at least 1")


@dataclass
class AggregationResult:
    """Result of price aggregation."""
    aggregate_value: int  # Weighted median
    source_values: dict[str, int]  # Original per-source values
    weights_used: dict[str, float]  # Weights applied
    rejected_sources: list[str]  # Source IDs rejected as outliers
    method_used: str  # "weighted_median" or similar
    outlier_method: str  # Outlier rejection method applied


class PriceAggregator:
    """
    Multi-source price aggregator with outlier rejection.
    
    Computes weighted median from multiple price sources, optionally
    rejecting outliers using IQR or MAD methods.
    """

    def __init__(self, config: AggregationConfig) -> None:
        """Initialize with aggregation configuration."""
        self.config = config

    def aggregate(
        self,
        sources: list[PriceSource],
    ) -> AggregationResult:
        """
        Aggregate prices from multiple sources.

        Parameters
        ----------
        sources:
            List of PriceSource objects with values and weights.

        Returns
        -------
        AggregationResult with aggregate value, per-source values, and rejection info.

        Raises
        ------
        ValueError:
            If fewer than min_sources_after_rejection sources remain after rejection,
            or if not all configured sources are provided.
        """
        # Validate all configured sources are provided
        provided_ids = {s.source_id for s in sources}
        configured_ids = set(self.config.sources)
        if provided_ids != configured_ids:
            missing = configured_ids - provided_ids
            extra = provided_ids - configured_ids
            msg = ""
            if missing:
                msg += f"Missing sources: {missing}. "
            if extra:
                msg += f"Unexpected sources: {extra}."
            raise ValueError(msg.strip())

        # Sort sources by ID for deterministic processing
        sources = sorted(sources, key=lambda s: s.source_id)

        # Perform outlier rejection
        rejected_sources = []
        if self.config.outlier_method != OutlierRejectionMethod.NONE:
            sources, rejected_sources = self._reject_outliers(sources)

        # Verify minimum sources remaining
        if len(sources) < self.config.min_sources_after_rejection:
            raise ValueError(
                f"Only {len(sources)} sources remain after outlier rejection, "
                f"but minimum {self.config.min_sources_after_rejection} required. "
                f"Rejected: {rejected_sources}"
            )

        # Compute weighted median
        aggregate = self._weighted_median(sources)

        # Build result
        source_values = {s.source_id: s.value for s in sources}
        weights_used = {s.source_id: self.config.weights[s.source_id] for s in sources}

        return AggregationResult(
            aggregate_value=aggregate,
            source_values=source_values,
            weights_used=weights_used,
            rejected_sources=rejected_sources,
            method_used="weighted_median",
            outlier_method=self.config.outlier_method.value,
        )

    def _reject_outliers(
        self,
        sources: list[PriceSource],
    ) -> tuple[list[PriceSource], list[str]]:
        """
        Reject outliers using configured method (IQR or MAD).

        Returns (filtered_sources, rejected_source_ids)
        """
        if self.config.outlier_method == OutlierRejectionMethod.IQR:
            return self._reject_outliers_iqr(sources)
        elif self.config.outlier_method == OutlierRejectionMethod.MAD:
            return self._reject_outliers_mad(sources)
        else:
            return sources, []

    def _reject_outliers_iqr(
        self,
        sources: list[PriceSource],
    ) -> tuple[list[PriceSource], list[str]]:
        """Reject outliers using Interquartile Range method."""
        if len(sources) <= 2:
            return sources, []

        values = [s.value for s in sources]
        q1 = self._percentile(values, 25)
        q3 = self._percentile(values, 75)
        iqr = q3 - q1

        lower_bound = q1 - self.config.iqr_multiplier * iqr
        upper_bound = q3 + self.config.iqr_multiplier * iqr

        accepted = []
        rejected = []
        for source in sources:
            if lower_bound <= source.value <= upper_bound:
                accepted.append(source)
            else:
                rejected.append(source.source_id)

        return accepted, rejected

    def _reject_outliers_mad(
        self,
        sources: list[PriceSource],
    ) -> tuple[list[PriceSource], list[str]]:
        """Reject outliers using Median Absolute Deviation method."""
        if len(sources) <= 1:
            return sources, []

        values = [s.value for s in sources]
        median = statistics.median(values)
        abs_deviations = [abs(v - median) for v in values]
        mad = statistics.median(abs_deviations)

        if mad == 0:
            # All values equal or very close; accept all
            return sources, []

        threshold = self.config.mad_multiplier * mad
        accepted = []
        rejected = []

        for source in sources:
            if abs(source.value - median) <= threshold:
                accepted.append(source)
            else:
                rejected.append(source.source_id)

        return accepted, rejected

    @staticmethod
    def _weighted_median(sources: list[PriceSource]) -> int:
        """
        Compute weighted median from sources.
        
        Uses cumulative weight approach: sort by value, accumulate weights,
        find value where cumulative weight crosses 50%.
        """
        if not sources:
            raise ValueError("No sources provided for aggregation")

        # Sort by value
        sorted_sources = sorted(sources, key=lambda s: s.value)

        # Calculate total weight
        total_weight = sum(s.weight for s in sorted_sources)

        # Find weighted median (value where cumulative weight >= 50%)
        cumulative = 0.0
        for source in sorted_sources:
            cumulative += source.weight
            if cumulative >= total_weight / 2:
                return source.value

        # Fallback to last value (shouldn't reach here)
        return sorted_sources[-1].value

    @staticmethod
    def _percentile(values: list[int], p: float) -> float:
        """
        Calculate percentile of a list of values.
        
        Uses linear interpolation between closest ranks.
        """
        if not values:
            raise ValueError("Empty values list")
        
        sorted_vals = sorted(values)
        n = len(sorted_vals)
        rank = (p / 100) * (n + 1)
        
        if rank < 1:
            return float(sorted_vals[0])
        if rank >= n:
            return float(sorted_vals[-1])
        
        lower_idx = int(rank) - 1
        upper_idx = lower_idx + 1
        
        if upper_idx >= n:
            return float(sorted_vals[lower_idx])
        
        fraction = rank - (lower_idx + 1)
        lower_val = sorted_vals[lower_idx]
        upper_val = sorted_vals[upper_idx]
        
        return lower_val + fraction * (upper_val - lower_val)
