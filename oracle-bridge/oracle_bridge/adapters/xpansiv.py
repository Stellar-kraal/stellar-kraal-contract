"""
oracle_bridge.adapters.xpansiv
==============================

Feed adapter for the Xpansiv CBL (Carbon Border Levy) spot market API.

Fetches the latest CBL-EUA (European Union Allowance) or CBL-CFD
(Carbon Forward) price from the Xpansiv market data endpoint.
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

XpansivProduct = str

CBL_EUA: XpansivProduct = "CBL-EUA"
CBL_CFD: XpansivProduct = "CBL-CFD"

_XPANSIV_PRODUCT_MAP: dict[str, str] = {
    CBL_EUA: "EUA",
    CBL_CFD: "CFD",
}

_XPANSIV_DEFAULT_BASE_URL = "https://api.xpansiv.com/market-data/v1"


class XpansivCBLAdapter(FeedAdapter):
    """
    Feed adapter for the Xpansiv CBL market.

    Parameters
    ----------
    config:
        Base adapter config.  The *feed_id* should be one of
        :const:`CBL_EUA` or :const:`CBL_CFD`.
    api_key:
        Xpansiv API key for authenticated access.
    base_url:
        Override the default API base URL.
    product:
        The Xpansiv product to query.  Defaults to ``CBL_EUA``.
    """

    def __init__(
        self,
        config: FeedAdapterConfig,
        api_key: str = "",
        base_url: str = _XPANSIV_DEFAULT_BASE_URL,
        product: XpansivProduct = CBL_EUA,
    ) -> None:
        super().__init__(config)
        self._api_key = api_key
        self._base_url = base_url.rstrip("/")
        self._product = product
        self._product_code = _XPANSIV_PRODUCT_MAP.get(product, "EUA")

    def _fetch_price(self) -> FeedResult:
        """
        Call the Xpansiv CBL market data API and return the current price.

        The price is returned as an integer (scaled to micrograms
        CO₂-eq/m² * 1000 to preserve decimal precision).
        """
        url = f"{self._base_url}/prices/{self._product_code}/latest"

        headers: dict[str, str] = {
            "Accept": "application/json",
            "User-Agent": "stellar-kraal-oracle-bridge/0.1.0",
        }
        if self._api_key:
            headers["X-API-Key"] = self._api_key

        req = urllib.request.Request(url, headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=30) as resp:
                body = json.loads(resp.read().decode("utf-8"))
        except urllib.error.HTTPError as e:
            raise RuntimeError(
                f"Xpansiv API HTTP {e.code} for {url}: {e.reason}"
            ) from e
        except urllib.error.URLError as e:
            raise RuntimeError(
                f"Xpansiv API connection failed for {url}: {e.reason}"
            ) from e
        except json.JSONDecodeError as e:
            raise RuntimeError(
                f"Xpansiv API returned invalid JSON: {e}"
            ) from e

        price_raw = body.get("price", body.get("lastPrice"))
        if price_raw is None:
            raise RuntimeError(
                f"Xpansiv API response missing 'price' field: {body}"
            )

        if isinstance(price_raw, (int, float)):
            price = int(round(price_raw * 1_000_000))
        else:
            raise RuntimeError(
                f"Unexpected price type from Xpansiv API: {type(price_raw).__name__}"
            )

        timestamp_raw = body.get("timestamp", body.get("asOf"))
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
                "source": "xpansiv",
                "product": self._product,
                "raw_price": price_raw,
                "url": url,
            },
        )
