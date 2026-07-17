"""
oracle_bridge.ipfs
==================

IPFS storage integration for oracle provenance records.

This module provides an :class:`IPFSClient` that pins provenance records to
IPFS and returns their content-addressed CID.  For the prototype the client
supports two backends:

1. **LocalIPFSClient** – calls the Kubo HTTP API (``http://localhost:5001``).
   Works when a local IPFS node is running (``ipfs daemon``).

2. **SimulatedIPFSClient** – an in-process fake that stores records in a dict
   keyed by a deterministic CID derived from SHA-256.  Use this for tests or
   when no IPFS node is available.

The active backend is selected by environment variable ``IPFS_BACKEND``
(``"local"`` or ``"simulated"``; default ``"simulated"``).

CID format
----------
A real CIDv1 multihash is 59 bytes when encoded as Base32 (``bafy...``).  For
the simulated backend we produce a deterministic CID of the form
``bafkrei<sha256_hex[:39]>`` which has the same length and prefix and is
distinguishable from real CIDs.

See ``docs/oracle/provenance-schema.md`` for context.
"""

from __future__ import annotations

import hashlib
import json
import os
import urllib.error
import urllib.parse
import urllib.request
from typing import Any, Protocol


# ── CID helpers ───────────────────────────────────────────────────────────────


def _simulated_cid(content: bytes) -> str:
    """
    Produce a deterministic fake CIDv1-like string from ``content``.

    Format: ``bafkrei`` + first 39 hex chars of SHA-256.
    This gives a 46-character string that looks like a CIDv1.
    """
    digest = hashlib.sha256(content).hexdigest()
    return "bafkrei" + digest[:39]


# ── Protocol ──────────────────────────────────────────────────────────────────


class IPFSClient(Protocol):
    """Interface that all IPFS backends must satisfy."""

    def pin(self, content: bytes) -> str:
        """
        Add ``content`` to IPFS and return the CID string.

        The content is stored content-addressed so the same bytes always
        produce the same CID.
        """
        ...

    def get(self, cid: str) -> bytes:
        """
        Retrieve content by CID.

        Raises
        ------
        KeyError
            If the CID is not found (simulated backend).
        urllib.error.URLError
            If the IPFS gateway is unreachable (local backend).
        """
        ...


# ── Simulated (in-process) backend ────────────────────────────────────────────


class SimulatedIPFSClient:
    """
    In-process IPFS backend for tests and local development.

    Stores records in a Python dict keyed by their deterministic CID.
    No network calls; fully reproducible.
    """

    def __init__(self) -> None:
        self._store: dict[str, bytes] = {}

    def pin(self, content: bytes) -> str:
        cid = _simulated_cid(content)
        self._store[cid] = content
        return cid

    def get(self, cid: str) -> bytes:
        if cid not in self._store:
            raise KeyError(f"CID not found in simulated store: {cid!r}")
        return self._store[cid]

    @property
    def store(self) -> dict[str, bytes]:
        """Direct access to the backing store (useful for assertions in tests)."""
        return dict(self._store)


# ── Local Kubo HTTP API backend ───────────────────────────────────────────────


class LocalIPFSClient:
    """
    IPFS backend that calls the Kubo RPC HTTP API.

    Parameters
    ----------
    api_url:
        Kubo API base URL.  Defaults to ``http://localhost:5001``.
    gateway_url:
        IPFS gateway URL for retrieval.  Defaults to ``http://localhost:8080``.
    timeout:
        Request timeout in seconds.
    """

    def __init__(
        self,
        api_url: str = "http://localhost:5001",
        gateway_url: str = "http://localhost:8080",
        timeout: float = 10.0,
    ) -> None:
        self._api_url = api_url.rstrip("/")
        self._gateway_url = gateway_url.rstrip("/")
        self._timeout = timeout

    def pin(self, content: bytes) -> str:
        """
        Add ``content`` to the local IPFS node via ``/api/v0/add``.

        Returns the CIDv0/CIDv1 string.
        """
        boundary = "----KuboFormBoundary"
        body = (
            f"--{boundary}\r\n"
            f'Content-Disposition: form-data; name="file"; filename="provenance.json"\r\n'
            f"Content-Type: application/json\r\n\r\n"
        ).encode() + content + f"\r\n--{boundary}--\r\n".encode()

        headers = {
            "Content-Type": f"multipart/form-data; boundary={boundary}",
        }
        url = f"{self._api_url}/api/v0/add?cid-version=1&hash=sha2-256"
        req = urllib.request.Request(url, data=body, headers=headers, method="POST")
        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                result = json.loads(resp.read().decode())
                return result["Hash"]
        except urllib.error.URLError as exc:
            raise RuntimeError(
                f"Cannot reach IPFS API at {self._api_url}: {exc}"
            ) from exc

    def get(self, cid: str) -> bytes:
        """Retrieve content via the local IPFS gateway."""
        url = f"{self._gateway_url}/ipfs/{cid}"
        req = urllib.request.Request(url, method="GET")
        try:
            with urllib.request.urlopen(req, timeout=self._timeout) as resp:
                return resp.read()
        except urllib.error.URLError as exc:
            raise RuntimeError(
                f"Cannot retrieve CID {cid!r} from gateway {self._gateway_url}: {exc}"
            ) from exc


# ── Public factory ────────────────────────────────────────────────────────────


def get_ipfs_client(backend: str | None = None) -> SimulatedIPFSClient | LocalIPFSClient:
    """
    Return an IPFS client based on the ``IPFS_BACKEND`` env var or ``backend`` arg.

    Parameters
    ----------
    backend:
        ``"local"`` → :class:`LocalIPFSClient` (requires ``ipfs daemon``).
        ``"simulated"`` → :class:`SimulatedIPFSClient` (default).

    Returns
    -------
    An object satisfying the :class:`IPFSClient` protocol.
    """
    chosen = backend or os.environ.get("IPFS_BACKEND", "simulated")
    if chosen == "local":
        api_url = os.environ.get("IPFS_API_URL", "http://localhost:5001")
        gateway_url = os.environ.get("IPFS_GATEWAY_URL", "http://localhost:8080")
        return LocalIPFSClient(api_url=api_url, gateway_url=gateway_url)
    return SimulatedIPFSClient()


# ── Provenance pinning helpers ────────────────────────────────────────────────


def pin_provenance_record(
    client: SimulatedIPFSClient | LocalIPFSClient,
    record_dict: dict[str, Any],
) -> str:
    """
    Serialise ``record_dict`` to compact JSON and pin it to IPFS.

    Returns the CID string.
    """
    content = json.dumps(record_dict, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return client.pin(content)


def fetch_provenance_record(
    client: SimulatedIPFSClient | LocalIPFSClient,
    cid: str,
) -> dict[str, Any]:
    """
    Retrieve and deserialise a provenance record from IPFS by CID.

    Returns the record as a dict.
    """
    content = client.get(cid)
    return json.loads(content.decode("utf-8"))
