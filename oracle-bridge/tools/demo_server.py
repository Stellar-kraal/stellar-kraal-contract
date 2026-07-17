#!/usr/bin/env python3
"""
oracle-bridge/tools/demo_server.py
====================================

Localhost demo server for the GEE Version Pinning & Provenance Audit Trail.

Runs a lightweight HTTP server on http://localhost:8888 that lets you:
  - Submit GEE oracle results (signs, pins to IPFS, records CID)
  - Browse all submissions and their IPFS provenance records
  - Verify any historical provenance record
  - View the JSON Schema

Usage:
    cd oracle-bridge
    python tools/demo_server.py
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
import time
from http.server import BaseHTTPRequestHandler, HTTPServer
from pathlib import Path
from urllib.parse import urlparse, parse_qs

# Allow running from oracle-bridge/ root
sys.path.insert(0, str(Path(__file__).parent.parent))

from oracle_bridge.attestation import OracleSigner
from oracle_bridge.bridge import GEEResult, OracleBridge
from oracle_bridge.ipfs import SimulatedIPFSClient, fetch_provenance_record
from oracle_bridge.provenance import (
    PROVENANCE_SCHEMA,
    validate_provenance_record,
    ProvenanceValidationError,
)

# ── Global state ──────────────────────────────────────────────────────────────

SIGNER = OracleSigner.generate()
IPFS = SimulatedIPFSClient()
BRIDGE = OracleBridge(signer=SIGNER, client=None, ipfs_client=IPFS)

# Fake on-chain store: feed_id_hex -> {output_value, timestamp, ipfs_cid, tx_ref}
ONCHAIN: dict[str, dict] = {}
# Submission log: list of {feed_id, output_value, ipfs_cid, tx_ref, submitted_at_iso}
LOG: list[dict] = []
TX_COUNTER = 0

GEE_SCRIPT = """\
// GEE carbon sequestration estimator v1.2.3
// Pinned: users/stellarkraal/scripts/carbon_seq_v1 @ v1.2.3
var dataset = ee.ImageCollection('MODIS/061/MOD13A3')
  .filterDate(params.startDate, params.endDate)
  .filterBounds(params.aoi);
var ndvi = dataset.mean().select('NDVI');
return ndvi.reduceRegion({
  reducer: ee.Reducer.mean(),
  geometry: params.aoi,
}).get('NDVI');
"""

# ── Fake submission client ────────────────────────────────────────────────────

class FakeClient:
    def submit_price_with_cid(self, attestation, ipfs_cid):
        global TX_COUNTER
        TX_COUNTER += 1
        tx = f"tx_{TX_COUNTER:04d}"
        from oracle_bridge.attestation import pad_feed_id
        fid_hex = pad_feed_id(attestation.payload.feed_id
                              if isinstance(attestation.payload.feed_id, bytes)
                              else attestation.payload.feed_id).hex()
        ONCHAIN[fid_hex] = {
            "output_value": attestation.payload.output_value,
            "timestamp_utc": attestation.payload.timestamp_utc,
            "script_hash": attestation.payload.script_hash.hex(),
            "input_params_hash": attestation.payload.input_params_hash.hex(),
            "ipfs_cid": ipfs_cid,
            "tx_ref": tx,
        }
        return tx

    def submit_price(self, attestation):
        return self.submit_price_with_cid(attestation, "")

BRIDGE._client = FakeClient()

# ── HTML helpers ──────────────────────────────────────────────────────────────

def _page(title: str, body: str) -> str:
    return f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} — GEE Provenance Demo</title>
<style>
  body{{font-family:system-ui,sans-serif;margin:0;background:#f5f7fa;color:#1a1a2e}}
  header{{background:#16213e;color:#fff;padding:1rem 2rem;display:flex;align-items:center;gap:1rem}}
  header h1{{margin:0;font-size:1.25rem}}
  nav a{{color:#a8d8ea;text-decoration:none;margin-right:1.2rem;font-size:.9rem}}
  nav a:hover{{text-decoration:underline}}
  main{{max-width:960px;margin:2rem auto;padding:0 1rem}}
  h2{{color:#16213e;border-bottom:2px solid #a8d8ea;padding-bottom:.4rem}}
  .card{{background:#fff;border-radius:8px;box-shadow:0 2px 8px rgba(0,0,0,.08);padding:1.5rem;margin-bottom:1.5rem}}
  .badge{{display:inline-block;padding:.15rem .5rem;border-radius:4px;font-size:.75rem;font-weight:600}}
  .badge-green{{background:#d4edda;color:#155724}}
  .badge-blue{{background:#cce5ff;color:#004085}}
  .badge-yellow{{background:#fff3cd;color:#856404}}
  table{{width:100%;border-collapse:collapse;font-size:.875rem}}
  th{{background:#16213e;color:#fff;padding:.5rem .75rem;text-align:left}}
  td{{padding:.5rem .75rem;border-bottom:1px solid #e9ecef;word-break:break-all}}
  tr:hover td{{background:#f0f4ff}}
  form label{{display:block;margin:.6rem 0 .2rem;font-weight:600;font-size:.875rem}}
  form input,form textarea,form select{{width:100%;box-sizing:border-box;padding:.45rem .6rem;border:1px solid #ced4da;border-radius:4px;font-size:.875rem}}
  form textarea{{font-family:monospace;height:6rem}}
  .btn{{background:#16213e;color:#fff;border:none;padding:.55rem 1.2rem;border-radius:4px;cursor:pointer;font-size:.875rem}}
  .btn:hover{{background:#0f3460}}
  .btn-green{{background:#28a745}}.btn-green:hover{{background:#218838}}
  pre{{background:#1a1a2e;color:#a8d8ea;padding:1rem;border-radius:6px;overflow-x:auto;font-size:.8rem}}
  .cid{{font-family:monospace;font-size:.8rem;color:#0f3460;word-break:break-all}}
  .alert{{padding:.75rem 1rem;border-radius:4px;margin-bottom:1rem}}
  .alert-success{{background:#d4edda;color:#155724;border:1px solid #c3e6cb}}
  .alert-danger{{background:#f8d7da;color:#721c24;border:1px solid #f5c6cb}}
</style>
</head>
<body>
<header>
  <span style="font-size:2rem">🌿</span>
  <div>
    <h1>GEE Script Version Pinning &amp; Provenance Audit Trail</h1>
    <nav>
      <a href="/">Home</a>
      <a href="/submit">Submit</a>
      <a href="/submissions">Submissions</a>
      <a href="/verify">Verify</a>
      <a href="/schema">Schema</a>
    </nav>
  </div>
</header>
<main>
<h2>{title}</h2>
{body}
</main>
</body>
</html>"""

# ── Route handlers ────────────────────────────────────────────────────────────

def handle_home() -> str:
    pubkey = SIGNER.public_key_bytes().hex()
    total = len(LOG)
    feeds = len(ONCHAIN)
    cids = len(IPFS.store)
    body = f"""
<div class="card">
  <h3 style="margin-top:0">What this demo shows</h3>
  <p>Every GEE oracle submission now records a <strong>provenance record</strong> that pins:</p>
  <ul>
    <li>The exact GEE script source hash (SHA-256) and version tag</li>
    <li>The canonical input parameter set and its hash</li>
    <li>The signed attestation payload (113-byte Ed25519 signed message)</li>
    <li>Computation output value, feed ID, and UTC timestamp</li>
  </ul>
  <p>The record is stored off-chain in <strong>IPFS</strong> (content-addressed) and the CID is
     recorded <strong>on-chain</strong> alongside every price update in <code>carbon_oracle</code>.</p>
</div>

<div class="card">
  <table>
    <tr><th>Oracle Public Key</th><td class="cid">{pubkey}</td></tr>
    <tr><th>Total Submissions</th><td>{total}</td></tr>
    <tr><th>Active Feeds (on-chain)</th><td>{feeds}</td></tr>
    <tr><th>IPFS Records Pinned</th><td>{cids}</td></tr>
    <tr><th>IPFS Backend</th><td><span class="badge badge-yellow">SimulatedIPFSClient (in-process)</span></td></tr>
  </table>
</div>

<div class="card">
  <h3 style="margin-top:0">Quick Links</h3>
  <a href="/submit" class="btn" style="margin-right:.5rem">Submit New Oracle Result</a>
  <a href="/submissions" class="btn" style="margin-right:.5rem">Browse Submissions</a>
  <a href="/verify" class="btn" style="margin-right:.5rem">Verify Provenance</a>
  <a href="/schema" class="btn">View JSON Schema</a>
</div>
"""
    return _page("Home", body)


def handle_submit_form(msg: str = "", err: str = "") -> str:
    alert = ""
    if msg:
        alert = f'<div class="alert alert-success">{msg}</div>'
    if err:
        alert = f'<div class="alert alert-danger">{err}</div>'
    body = f"""
{alert}
<div class="card">
<form method="POST" action="/submit">
  <label>Feed ID <small>(≤32 chars)</small></label>
  <input name="feed_id" value="carbon/rwanda/2024" required maxlength="32">

  <label>Output Value <small>(integer, carbon sequestration µg CO₂-eq/m²)</small></label>
  <input name="output_value" value="4815162342" type="number" required>

  <label>Timestamp UTC <small>(Unix seconds; leave 0 for now)</small></label>
  <input name="timestamp_utc" value="0" type="number">

  <label>GEE Script Asset Path</label>
  <input name="asset_path" value="users/stellarkraal/scripts/carbon_seq_v1">

  <label>Version Tag</label>
  <input name="version_tag" value="v1.2.3">

  <label>Input Parameters <small>(JSON object)</small></label>
  <textarea name="input_params">{{"aoi":"POLYGON((30.1 -1.2,30.1 -1.0,30.3 -1.0,30.3 -1.2,30.1 -1.2))","startDate":"2024-01-01","endDate":"2024-12-31"}}</textarea>

  <br><br>
  <button type="submit" class="btn btn-green">Sign, Pin to IPFS &amp; Submit On-Chain</button>
</form>
</div>
"""
    return _page("Submit Oracle Result", body)


def handle_submit_post(body_bytes: bytes) -> str:
    from urllib.parse import parse_qs, unquote_plus
    raw = body_bytes.decode("utf-8")
    fields: dict[str, str] = {}
    for part in raw.split("&"):
        if "=" in part:
            k, v = part.split("=", 1)
            fields[unquote_plus(k)] = unquote_plus(v)

    feed_id = fields.get("feed_id", "").strip()
    try:
        output_value = int(fields.get("output_value", "0"))
    except ValueError:
        return handle_submit_form(err="output_value must be an integer")

    ts = int(fields.get("timestamp_utc", "0") or "0")
    if ts == 0:
        ts = int(time.time())

    asset_path = fields.get("asset_path", "users/oracle/scripts/carbon_seq").strip()
    version_tag = fields.get("version_tag", "untagged").strip()
    params_raw = fields.get("input_params", "{}").strip()
    try:
        input_params = json.loads(params_raw)
    except json.JSONDecodeError as e:
        return handle_submit_form(err=f"Invalid JSON in input_params: {e}")

    result = GEEResult(
        script_source=GEE_SCRIPT,
        input_params=input_params,
        output_value=output_value,
        feed_id=feed_id,
        timestamp_utc=ts,
        script_asset_path=asset_path,
        script_version_tag=version_tag,
    )

    try:
        attestation, tx_ref, prov = BRIDGE.process(result)
    except Exception as e:
        return handle_submit_form(err=f"Submission failed: {e}")

    LOG.append({
        "feed_id": feed_id,
        "output_value": output_value,
        "ipfs_cid": prov.ipfs_cid,
        "tx_ref": tx_ref,
        "submitted_at_iso": prov.to_dict()["submission"]["submitted_at_iso"],
    })

    msg = (f'Submitted! Tx: <code>{tx_ref}</code> &nbsp;|&nbsp; '
           f'IPFS CID: <span class="cid">{prov.ipfs_cid}</span>'
           f' &nbsp;<a href="/verify?cid={prov.ipfs_cid}">Verify →</a>')
    return handle_submit_form(msg=msg)


def handle_submissions() -> str:
    if not LOG:
        body = '<div class="card"><p>No submissions yet. <a href="/submit">Submit one →</a></p></div>'
        return _page("All Submissions", body)

    rows = ""
    for entry in reversed(LOG):
        cid = entry["ipfs_cid"]
        rows += f"""<tr>
<td>{entry['submitted_at_iso']}</td>
<td><code>{entry['feed_id']}</code></td>
<td>{entry['output_value']:,}</td>
<td><code>{entry['tx_ref']}</code></td>
<td class="cid"><a href="/verify?cid={cid}">{cid[:30]}…</a></td>
</tr>"""

    body = f"""
<div class="card">
<table>
<thead><tr>
  <th>Submitted At</th><th>Feed ID</th><th>Output Value</th><th>Tx Ref</th><th>IPFS CID</th>
</tr></thead>
<tbody>{rows}</tbody>
</table>
</div>
"""
    return _page("All Submissions", body)


def handle_verify_form(cid: str = "", result_html: str = "") -> str:
    body = f"""
<div class="card">
<form method="GET" action="/verify">
  <label>IPFS CID</label>
  <input name="cid" value="{cid}" placeholder="bafkrei..." required>
  <br><br>
  <button type="submit" class="btn">Verify Provenance Record</button>
</form>
</div>
{result_html}
"""
    return _page("Verify Provenance", body)


def handle_verify(cid: str) -> str:
    if not cid:
        return handle_verify_form()

    try:
        record = fetch_provenance_record(IPFS, cid)
    except KeyError:
        return handle_verify_form(cid=cid, result_html='<div class="alert alert-danger">CID not found in IPFS store.</div>')

    # Run validation
    errors: list[str] = []
    try:
        validate_provenance_record(record)
    except ProvenanceValidationError as e:
        errors = e.validation_errors

    # Hash checks
    gs = record["gee_script"]
    ip = record["input_params"]
    comp = record["computation"]
    att = record["attestation"]
    sub = record["submission"]

    canonical = json.dumps(ip["params"], sort_keys=True, separators=(",", ":"))
    computed_params_hash = hashlib.sha256(canonical.encode()).hexdigest()
    params_ok = computed_params_hash == ip["params_hash"]

    payload = bytes.fromhex(att["payload_hex"])
    payload_ov = int.from_bytes(payload[65:73], "big", signed=True) if len(payload) == 113 else None
    payload_ts = int.from_bytes(payload[73:81], "big", signed=True) if len(payload) == 113 else None
    payload_ok = (payload_ov == comp["output_value"] and payload_ts == comp["timestamp_utc"])

    # Ed25519 signature check
    sig_ok = False
    sig_msg = ""
    try:
        from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PublicKey
        pk = Ed25519PublicKey.from_public_bytes(bytes.fromhex(att["public_key"]))
        pk.verify(bytes.fromhex(att["signature"]), payload)
        sig_ok = True
        sig_msg = "Valid ✓"
    except Exception as exc:
        sig_msg = f"Invalid ✗ ({exc})"

    def chk(ok: bool) -> str:
        return '<span class="badge badge-green">PASS ✓</span>' if ok else '<span class="badge" style="background:#f8d7da;color:#721c24">FAIL ✗</span>'

    schema_ok = not errors
    checks_html = f"""
<table>
<thead><tr><th>Check</th><th>Result</th><th>Detail</th></tr></thead>
<tbody>
<tr><td>JSON Schema v1</td><td>{chk(schema_ok)}</td><td>{'OK' if schema_ok else '; '.join(errors)}</td></tr>
<tr><td>Params hash integrity</td><td>{chk(params_ok)}</td><td>SHA-256(canonical params) matches stored hash</td></tr>
<tr><td>Payload field consistency</td><td>{chk(payload_ok)}</td><td>output_value={payload_ov}, timestamp={payload_ts}</td></tr>
<tr><td>Ed25519 signature</td><td>{chk(sig_ok)}</td><td>{sig_msg}</td></tr>
</tbody>
</table>
"""
    agg_section = ""
    if record.get("aggregation"):
        agg = record["aggregation"]
        agg_section = f"""<h3>Aggregation</h3>
<table>
<tr><th>Method</th><td>{agg.get('method')}</td></tr>
<tr><th>Outlier Method</th><td>{agg.get('outlier_method')}</td></tr>
<tr><th>Rejected Sources</th><td>{', '.join(agg.get('rejected_sources', [])) or 'none'}</td></tr>
</table>"""

    result_html = f"""
<div class="card">
  <h3 style="margin-top:0">Verification Results</h3>
  {checks_html}
</div>
<div class="card">
  <h3 style="margin-top:0">Record Details</h3>
  <table>
    <tr><th>Schema Version</th><td>{record.get('schema_version')}</td></tr>
    <tr><th>Record Type</th><td><span class="badge badge-blue">{record.get('record_type')}</span></td></tr>
    <tr><th>GEE Asset Path</th><td><code>{gs.get('asset_path')}</code></td></tr>
    <tr><th>Version Tag</th><td><code>{gs.get('version_tag')}</code></td></tr>
    <tr><th>Version Hash (SHA-256)</th><td class="cid">{gs.get('version_hash', '')}</td></tr>
    <tr><th>Feed ID</th><td><code>{comp.get('feed_id')}</code></td></tr>
    <tr><th>Output Value</th><td>{comp.get('output_value'):,}</td></tr>
    <tr><th>Timestamp</th><td>{comp.get('timestamp_iso')}</td></tr>
    <tr><th>Oracle Public Key</th><td class="cid">{att.get('public_key', '')}</td></tr>
    <tr><th>Submitted At</th><td>{sub.get('submitted_at_iso')}</td></tr>
    <tr><th>Tx Ref</th><td><code>{sub.get('tx_ref', 'N/A')}</code></td></tr>
    <tr><th>Self-Referential CID</th><td class="cid">{sub.get('ipfs_cid', 'N/A')}</td></tr>
  </table>
  {agg_section}
</div>
<div class="card">
  <h3 style="margin-top:0">Raw JSON</h3>
  <pre>{json.dumps(record, indent=2)}</pre>
</div>
"""
    return handle_verify_form(cid=cid, result_html=result_html)


def handle_schema() -> str:
    schema_json = json.dumps(PROVENANCE_SCHEMA, indent=2)
    body = f"""
<div class="card">
  <p>JSON Schema for the GEE Oracle Provenance Record (v1). 
     See <a href="/docs/provenance-schema">provenance-schema.md</a> for full documentation.</p>
  <pre>{schema_json}</pre>
</div>
"""
    return _page("Provenance Record JSON Schema", body)


# ── HTTP handler ──────────────────────────────────────────────────────────────

class Handler(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        print(f"  {self.address_string()} {fmt % args}")

    def send_html(self, html: str, status: int = 200):
        enc = html.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(enc)))
        self.end_headers()
        self.wfile.write(enc)

    def send_redirect(self, location: str):
        self.send_response(302)
        self.send_header("Location", location)
        self.end_headers()

    def do_GET(self):
        parsed = urlparse(self.path)
        path = parsed.path
        qs = parse_qs(parsed.query)

        if path == "/":
            self.send_html(handle_home())
        elif path == "/submit":
            self.send_html(handle_submit_form())
        elif path == "/submissions":
            self.send_html(handle_submissions())
        elif path == "/verify":
            cid = qs.get("cid", [""])[0]
            self.send_html(handle_verify(cid))
        elif path == "/schema":
            self.send_html(handle_schema())
        else:
            self.send_html(_page("404", "<p>Page not found.</p>"), 404)

    def do_POST(self):
        parsed = urlparse(self.path)
        path = parsed.path
        length = int(self.headers.get("Content-Length", 0))
        body = self.rfile.read(length)

        if path == "/submit":
            self.send_html(handle_submit_post(body))
        else:
            self.send_html(_page("404", "<p>Not found.</p>"), 404)


# ── Entry point ───────────────────────────────────────────────────────────────

PORT = int(os.environ.get("DEMO_PORT", "8888"))

if __name__ == "__main__":
    # Pre-load a couple of example submissions so the UI isn't empty
    def _seed():
        examples = [
            ("carbon/rwanda/2024", 4_815_162_342, {"aoi": "POLYGON((30.1 -1.2,30.3 -1.0))", "startDate": "2024-01-01", "endDate": "2024-06-30"}, 1_720_051_200),
            ("carbon/kenya/2024",  3_221_456_789, {"aoi": "POLYGON((36.8 -1.3,37.0 -1.1))", "startDate": "2024-01-01", "endDate": "2024-06-30"}, 1_720_051_200),
        ]
        for feed_id, ov, params, ts in examples:
            r = GEEResult(
                script_source=GEE_SCRIPT,
                input_params=params,
                output_value=ov,
                feed_id=feed_id,
                timestamp_utc=ts,
                script_asset_path="users/stellarkraal/scripts/carbon_seq_v1",
                script_version_tag="v1.2.3",
            )
            _, tx_ref, prov = BRIDGE.process(r)
            LOG.append({
                "feed_id": feed_id,
                "output_value": ov,
                "ipfs_cid": prov.ipfs_cid,
                "tx_ref": tx_ref,
                "submitted_at_iso": prov.to_dict()["submission"]["submitted_at_iso"],
            })
    _seed()

    server = HTTPServer(("0.0.0.0", PORT), Handler)
    print(f"\n🌿 GEE Provenance Demo running at http://localhost:{PORT}/\n")
    print(f"   Oracle pubkey : {SIGNER.public_key_bytes().hex()[:32]}...")
    print(f"   Pre-seeded    : {len(LOG)} example submissions")
    print("   Press Ctrl+C to stop\n")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nStopped.")
