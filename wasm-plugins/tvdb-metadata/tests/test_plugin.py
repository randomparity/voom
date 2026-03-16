"""Tests for the plugin entry points (get_info, handles, on_event)."""

import json
import sys
from unittest.mock import MagicMock

import pytest

from tvdb_metadata.filename_parser import parse_filename
from tvdb_metadata.msgpack_helpers import pack_event, unpack_event
from tvdb_metadata import plugin as plugin_module
from tvdb_metadata.tvdb_client import TvdbClient

from helpers import MockHost, MockHttpResponse


class TestGetInfo:
    """Plugin identity."""

    def test_returns_plugin_info(self):
        info = plugin_module.get_info()
        assert info["name"] == "tvdb-metadata"
        assert info["version"] == "0.1.0"
        assert "enrich_metadata:tvdb" in info["capabilities"]


class TestHandles:
    """Event type filtering."""

    def test_handles_file_introspected(self):
        assert plugin_module.handles("file.introspected") is True

    def test_does_not_handle_other_events(self):
        assert plugin_module.handles("file.discovered") is False
        assert plugin_module.handles("metadata.enriched") is False
        assert plugin_module.handles("plan.created") is False
        assert plugin_module.handles("") is False


class TestOnEvent:
    """End-to-end event processing."""

    def _make_event(self, file_path="/media/tv/Breaking Bad/Breaking.Bad.S01E01.720p.mkv"):
        """Build a mock event-data dict with MessagePack payload."""
        payload = pack_event("FileIntrospected", {
            "file": {
                "path": file_path,
                "size": 1_500_000_000,
                "content_hash": "abc123",
                "container": "mkv",
                "duration": 3600.0,
                "bitrate": None,
                "tracks": [],
                "tags": [],
                "plugin_metadata": [],
            },
        })
        return {"event_type": "file.introspected", "payload": payload}

    def test_processes_tv_file(self, mock_host_with_tvdb, monkeypatch):
        # Wire up host bridge to use mock
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        event = self._make_event()
        result = plugin_module.on_event(event)

        assert result is not None
        assert result["plugin_name"] == "tvdb-metadata"
        assert len(result["produced_events"]) == 1

        produced = result["produced_events"][0]
        assert produced["event_type"] == "metadata.enriched"

        # Decode the produced payload
        variant, data = unpack_event(bytes(produced["payload"]))
        assert variant == "MetadataEnriched"
        assert data["source"] == "tvdb"
        assert data["metadata"]["series_name"] == "Breaking Bad"
        assert data["metadata"]["episode_name"] == "Pilot"
        assert data["metadata"]["season_number"] == 1
        assert data["metadata"]["episode_number"] == 1

    def test_ignores_non_tv_file(self, mock_host_with_tvdb, monkeypatch):
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        event = self._make_event(file_path="/media/movies/The.Matrix.1999.mkv")
        result = plugin_module.on_event(event)
        assert result is None

    def test_ignores_wrong_event_type(self, mock_host_with_tvdb, monkeypatch):
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        event = {"event_type": "file.discovered", "payload": b""}
        result = plugin_module.on_event(event)
        assert result is None

    def test_handles_no_config(self, mock_host, monkeypatch):
        # No TVDB config set
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host)

        event = self._make_event()
        result = plugin_module.on_event(event)
        assert result is None

    def test_handles_invalid_payload(self, mock_host_with_tvdb, monkeypatch):
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        event = {"event_type": "file.introspected", "payload": b"\x00\x01\x02"}
        result = plugin_module.on_event(event)
        assert result is None

    def test_handles_no_host(self, monkeypatch):
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: None)

        event = self._make_event()
        result = plugin_module.on_event(event)
        assert result is None

    def test_episode_not_found_on_tvdb(self, mock_host_with_tvdb, monkeypatch):
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        # Use episode number that won't match
        event = self._make_event(
            file_path="/media/tv/Breaking Bad/Breaking.Bad.S01E99.mkv"
        )
        result = plugin_module.on_event(event)
        assert result is None


class TestMsgpackRoundTrip:
    """Verify MessagePack format matches rmp_serde conventions."""

    def test_event_roundtrip(self):
        original = {
            "file": {
                "path": "/test/file.mkv",
                "size": 1024,
                "content_hash": "hash",
                "container": "mkv",
                "duration": 60.0,
                "bitrate": None,
                "tracks": [],
                "tags": [],
                "plugin_metadata": [],
            },
        }
        packed = pack_event("FileIntrospected", original)
        variant, data = unpack_event(packed)
        assert variant == "FileIntrospected"
        assert data["file"]["path"] == "/test/file.mkv"
        assert data["file"]["size"] == 1024
        assert data["file"]["bitrate"] is None

    def test_metadata_enriched_format(self):
        payload = pack_event("MetadataEnriched", {
            "path": "/test/show.mkv",
            "source": "tvdb",
            "metadata": {
                "series_name": "Test",
                "season_number": 1,
                "episode_number": 1,
            },
        })
        variant, data = unpack_event(payload)
        assert variant == "MetadataEnriched"
        assert data["path"] == "/test/show.mkv"
        assert data["source"] == "tvdb"
        assert data["metadata"]["series_name"] == "Test"
