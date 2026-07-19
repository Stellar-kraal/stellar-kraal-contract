"""Feed adapters for market data sources."""

from oracle_bridge.adapters.base import (
    FeedAdapter,
    FeedAdapterConfig,
    FeedResult,
)
from oracle_bridge.adapters.xpansiv import XpansivCBLAdapter
from oracle_bridge.adapters.toucan import ToucanProtocolAdapter

__all__ = [
    "FeedAdapter",
    "FeedAdapterConfig",
    "FeedResult",
    "XpansivCBLAdapter",
    "ToucanProtocolAdapter",
]
