"""
tests/test_cli_submit.py
=========================

Tests for the ``submit`` subcommand of the oracle-bridge CLI,
focusing on the ``--dry-run`` flag.
"""

from __future__ import annotations

import json
import textwrap
from pathlib import Path

import pytest

from oracle_bridge.cli import main


@pytest.fixture
def gee_results_file(tmp_path: Path) -> Path:
    """Write a multi-source GEE results JSON file and return its path."""
    results = {
        "xpansiv_cbl": {
            "script_source": "// Xpansiv CBL script",
            "input_params": {"region": "BRAZIL", "asset": "CATTLE"},
            "output_value": 12_500_000,
            "feed_id": "CATTLE-SPOT",
            "timestamp_utc": 1699564800,
        },
        "toucan_protocol": {
            "script_source": "// Toucan Protocol script",
            "input_params": {"region": "BRAZIL", "asset": "CATTLE"},
            "output_value": 12_510_000,
            "feed_id": "CATTLE-SPOT",
            "timestamp_utc": 1699564800,
        },
        "custom_source": {
            "script_source": "// Custom source script",
            "input_params": {"region": "BRAZIL", "asset": "CATTLE"},
            "output_value": 12_495_000,
            "feed_id": "CATTLE-SPOT",
            "timestamp_utc": 1699564800,
        },
    }
    path = tmp_path / "results.json"
    path.write_text(json.dumps(results))
    return path


@pytest.fixture
def single_source_file(tmp_path: Path) -> Path:
    """Write a single-source GEE results JSON file (as a list)."""
    results = [
        {
            "source_id": "only_source",
            "script_source": "// Single source",
            "input_params": {"region": "RWANDA"},
            "output_value": 4_815_162_342,
            "feed_id": "carbon/rwanda/2024",
            "timestamp_utc": 1720051200,
        }
    ]
    path = tmp_path / "single.json"
    path.write_text(json.dumps(results))
    return path


class TestSubmitDryRun:
    """Tests for ``oracle-bridge submit --dry-run``."""

    def test_dry_run_json_output(
        self, gee_results_file: Path, capsys: pytest.CaptureFixture[str]
    ):
        rc = main([
            "submit",
            "--feed-id", "CATTLE-SPOT",
            "--results-file", str(gee_results_file),
            "--dry-run",
            "--json",
        ])
        assert rc == 0

        output = json.loads(capsys.readouterr().out)
        assert output["dry_run"] is True
        assert output["tx_ref"] == "dry-run-tx"
        assert output["feed_id"] == "CATTLE-SPOT"
        assert output["ipfs_cid"] is not None
        assert output["aggregate_value"] > 0
        assert len(output["source_values"]) == 3
        assert "attestation_public_key" in output
        assert "attestation_signature" in output

    def test_dry_run_human_output(
        self, gee_results_file: Path, capsys: pytest.CaptureFixture[str]
    ):
        rc = main([
            "submit",
            "--feed-id", "CATTLE-SPOT",
            "--results-file", str(gee_results_file),
            "--dry-run",
        ])
        assert rc == 0
        out = capsys.readouterr().out
        assert "DRY-RUN" in out
        assert "CATTLE-SPOT" in out
        assert "dry-run-tx" in out

    def test_dry_run_single_source(
        self, single_source_file: Path, capsys: pytest.CaptureFixture[str]
    ):
        rc = main([
            "submit",
            "--feed-id", "carbon/rwanda/2024",
            "--results-file", str(single_source_file),
            "--dry-run",
            "--json",
        ])
        assert rc == 0
        output = json.loads(capsys.readouterr().out)
        assert output["aggregate_value"] == 4_815_162_342
        assert len(output["source_values"]) == 1
        assert output["rejected_sources"] == []

    def test_dry_run_outlier_rejection(self, tmp_path: Path, capsys):
        """A source deviating far from the cluster should be rejected via IQR."""
        base = 10_000_000
        results = {}
        # 6 sources tightly clustered around 10M
        for i, offset in enumerate([0, 50_000, 100_000, 150_000, 200_000, 250_000]):
            results[f"s{i}"] = {
                "script_source": f"// s{i}",
                "input_params": {},
                "output_value": base + offset,
                "feed_id": "F",
                "timestamp_utc": 1699564800,
            }
        # 1 extreme outlier at 100M (10x the cluster)
        results["outlier"] = {
            "script_source": "// outlier",
            "input_params": {},
            "output_value": 100_000_000,
            "feed_id": "F",
            "timestamp_utc": 1699564800,
        }
        path = tmp_path / "outlier.json"
        path.write_text(json.dumps(results))

        rc = main([
            "submit", "--feed-id", "F",
            "--results-file", str(path),
            "--dry-run", "--json",
        ])
        assert rc == 0
        output = json.loads(capsys.readouterr().out)
        assert "outlier" in output["rejected_sources"]

    def test_live_mode_not_implemented(
        self, gee_results_file: Path, capsys: pytest.CaptureFixture[str]
    ):
        rc = main([
            "submit",
            "--feed-id", "CATTLE-SPOT",
            "--results-file", str(gee_results_file),
        ])
        assert rc == 1
        err = capsys.readouterr().err
        assert "not yet implemented" in err

    def test_missing_results_file(self, capsys):
        rc = main([
            "submit",
            "--feed-id", "F",
            "--results-file", "/nonexistent/results.json",
            "--dry-run",
        ])
        assert rc == 1
        assert "not found" in capsys.readouterr().err

    def test_invalid_json(self, tmp_path: Path, capsys):
        path = tmp_path / "bad.json"
        path.write_text("{not valid json")
        rc = main([
            "submit", "--feed-id", "F",
            "--results-file", str(path),
            "--dry-run",
        ])
        assert rc == 1
        assert "Invalid JSON" in capsys.readouterr().err

    def test_empty_results_file(self, tmp_path: Path, capsys):
        path = tmp_path / "empty.json"
        path.write_text("{}")
        rc = main([
            "submit", "--feed-id", "F",
            "--results-file", str(path),
            "--dry-run",
        ])
        assert rc == 1
        assert "No GEE results" in capsys.readouterr().err
