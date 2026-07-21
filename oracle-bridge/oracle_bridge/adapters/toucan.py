"""
oracle_bridge.adapters.toucan
==============================

Feed adapter for the Toucan Protocol carbon credit pricing API.

Fetches the latest Toucan BCT (Base Carbon Tonne) or NCT (Nature
Carbon Tonne) price from the Toucan Protocol data endpoint.
"""

from __future__ import annotations

import json
import logging
import time
import urllib.request
import urllib.error
from typing import Any

from oracle_bridge.adapters.base import FeedAdapter, FeedAdapterConfig, FeedResult

logger = logging.getLogger(__name__)

ToucanPool = str

BCT: ToucanPool = "BCT"
NCT: ToucanPool = "NCT"

_TOUCAN_POOL_MAP: dict[str, str] = {
    BCT: "bct",
    NCT: "nct",
}

_TOUCAN_DEFAULT_BASE_URL = "https://api.toucan.earth/carbon/v1"


class ToucanProtocolAdapter(FeedAdapter):
    """
    Feed adapter for the Toucan Protocol carbon pool pricing API.

    Parameters
    ----------
    config:
        Base adapter config.  The *feed_id* should be ``"BCT"`` or
        ``"NCT"``.
    api_key:
        Toucan API key (optional, depends on endpoint).
    base_url:
        Override the default API base URL.
    pool:
        The Toucan pool to query.  Defaults to ``BCT``.
    """

    def __init__(
        self,
        config: FeedAdapterConfig,
        api_key: str = "",
        base_url: str = _TOUCAN_DEFAULT_BASE_URL,
        pool: ToucanPool = BCT,
    ) -> None:
        super().__init__(config)
        self._api_key = api_key
        self._base_url = base_url.rstrip("/")
        self._pool = pool
        self._pool_code = _TOUCAN_POOL_MAP.get(pool, "bct")

    def _fetch_price(self) -> FeedResult:
        """
        Call the Toucan Protocol pricing API and return the current price.

        The price is returned as an integer (scaled to micrograms
        CO₂-eq/m² * 1000 to preserve decimal precision).
        """
        url = f"{self._base_url}/pools/{self._pool_code}/price"

        headers: dict[str, str] = {
            "Accept": "application/json",
            "User-Agent": "stellar-kraal-oracle-bridge/0.1.0",
        }
        if self._api_key:
            headers["Authorization"] = f"Bearer {self._api_key}"

        req = urllib.request.Request(url, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                body = json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            raise RuntimeError(
                f"Toucan API HTTP {e.code} for {url}: {e.reason}"
            ) from e
        except urllib.error.URLError as e:
            raise RuntimeError(
                f"Toucan API connection failed for {url}: {e.reason}"
            ) from e
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"Toucan API returned invalid JSON: {e}"
            ) from e

        price_raw = body.get("price", body.get("lastPrice", body.get("usdPrice")))
        if price_raw is None:
            raise RuntimeError(
                f"Toucan API response missing 'price' field: {body}"
            )

        if isinstance(price_raw, (int, float)):
            price = int(round(price_raw * 1_000_000))
        else:
            raise RuntimeError(
                f"Unexpected price type from Toucan API: {type(price_raw).__name__}"
            )

        timestamp_raw = body.get("timestamp", body.get("updatedAt"))
        if timestamp_raw:
            try:
                timestamp_utc = int(
                    time.mktime(
                        time.strptime(timestamp_raw[:19], "%Y-%m-%dT%H:%M:%S")
                    )
                )
            except (ValueError, TypeError):
                timestamp_utc = int(time.time())
        else:
            timestamp_utc = int(time.time())

        return FeedResult(
            price=price,
            feed_id=self.config.feed_id,
            timestamp_utc=timestamp_utc,
            metadata={
                "source": "toucan",
                "pool": self._pool,
                "raw_price": price_raw,
                "url": url,
            },
        )
