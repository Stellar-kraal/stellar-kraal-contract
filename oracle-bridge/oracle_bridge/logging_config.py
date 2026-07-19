"""
oracle_bridge.logging_config
============================

Structured logging configuration for the oracle bridge.

Provides a consistent JSON-based log format for production use and
a human-readable format for local development.
"""

from __future__ import annotations

import json
import logging
import sys
from datetime import datetime, timezone
from typing import Any


class StructuredFormatter(logging.Formatter):
    """
    JSON-structured log formatter.

    Emits log records as single-line JSON objects for consumption by
    log aggregation systems (e.g. ELK, Datadog, Grafana Loki).
    """

    def format(self, record: logging.LogRecord) -> str:
        entry: dict[str, Any] = {
            "timestamp": datetime.now(timezone.utc).isoformat(),
            "level": record.levelname,
            "logger": record.name,
            "message": record.getMessage(),
            "module": record.module,
            "function": record.funcName,
            "line": record.lineno,
        }
        if record.exc_info and record.exc_info[0]:
            entry["exception"] = self.formatException(record.exc_info)
        if hasattr(record, "feed_id"):
            entry["feed_id"] = record.feed_id
        if hasattr(record, "adapter"):
            entry["adapter"] = record.adapter
        if hasattr(record, "extra_fields"):
            entry.update(record.extra_fields)
        return json.dumps(entry, default=str)


def configure_logging(
    level: str = "INFO",
    structured: bool = False,
) -> None:
    """
    Configure the root logger.

    Parameters
    ----------
    level:
        Log level (DEBUG, INFO, WARNING, ERROR, CRITICAL).
    structured:
        If True, emit JSON-formatted logs. Otherwise, use a human-readable
        format suitable for local development.
    """
    handler = logging.StreamHandler(sys.stdout)
    if structured:
        formatter = StructuredFormatter()
    else:
        formatter = logging.Formatter(
            "%(asctime)s [%(levelname)s] %(name)s: %(message)s",
            datefmt="%Y-%m-%dT%H:%M:%S%z",
        )
    handler.setFormatter(formatter)

    root = logging.getLogger()
    root.setLevel(getattr(logging, level.upper(), logging.INFO))
    root.handlers.clear()
    root.addHandler(handler)
