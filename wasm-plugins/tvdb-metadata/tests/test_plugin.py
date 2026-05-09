"""Tests for the plugin entry points (get_info, handles, on_event)."""

from unittest.mock import patch

import pytest

from tvdb_metadata.msgpack_helpers import pack_event, unpack_event
from tvdb_metadata import plugin as plugin_module
from tvdb_metadata.tvdb_client import TvdbClient


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
                "id": "00000000-0000-0000-0000-000000000000",
                "path": file_path,
                "size": 1_500_000_000,
                "content_hash": "abc123",
                "container": "Mkv",
                "duration": 3600.0,
                "bitrate": None,
                "tracks": [],
                "tags": {},
                "plugin_metadata": {},
                "introspected_at": "2024-01-01T00:00:00Z",
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
                "id": "00000000-0000-0000-0000-000000000000",
                "path": "/test/file.mkv",
                "size": 1024,
                "content_hash": "hash",
                "container": "Mkv",
                "duration": 60.0,
                "bitrate": None,
                "tracks": [],
                "tags": {},
                "plugin_metadata": {},
                "introspected_at": "2024-01-01T00:00:00Z",
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


class TestExceptionHandling:
    """Verify exceptions in lookup path don't propagate as WASM traps."""

    def _make_event(self, file_path="/media/tv/Breaking Bad/Breaking.Bad.S01E01.720p.mkv"):
        payload = pack_event("FileIntrospected", {
            "file": {
                "id": "00000000-0000-0000-0000-000000000000",
                "path": file_path,
                "size": 1_500_000_000,
                "content_hash": "abc123",
                "container": "Mkv",
                "duration": 3600.0,
                "bitrate": None,
                "tracks": [],
                "tags": {},
                "plugin_metadata": {},
                "introspected_at": "2024-01-01T00:00:00Z",
            },
        })
        return {"event_type": "file.introspected", "payload": payload}

    def test_uncaught_exception_in_lookup(self, mock_host_with_tvdb, monkeypatch):
        """ValueError in TvdbClient.lookup should be caught by on_event."""
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        with patch.object(TvdbClient, "lookup", side_effect=ValueError("unexpected")):
            result = plugin_module.on_event(self._make_event())
        assert result is None

    def test_missing_file_key_in_payload(self, mock_host_with_tvdb, monkeypatch):
        """Payload without 'file' key returns None."""
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        payload = pack_event("FileIntrospected", {"not_file": "data"})
        event = {"event_type": "file.introspected", "payload": payload}
        result = plugin_module.on_event(event)
        assert result is None

    def test_deeply_nested_payload(self, mock_host_with_tvdb, monkeypatch):
        """Deeply nested msgpack should not crash — returns None."""
        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: mock_host_with_tvdb)

        # Build a payload that will fail to unpack as a valid event
        import umsgpack
        nested = "leaf"
        for _ in range(500):
            nested = {"a": nested}
        try:
            bad_payload = umsgpack.packb(nested)
        except RecursionError:
            pytest.skip("Cannot build deeply nested payload")

        event = {"event_type": "file.introspected", "payload": bad_payload}
        result = plugin_module.on_event(event)
        # Either returns None (not a valid event) or handles gracefully
        assert result is None


class TestLogging:
    """Logging failures should not affect plugin execution."""

    def test_log_bridge_failure_is_non_fatal(self, monkeypatch):
        class BrokenBridge:
            def log(self, level, message):
                raise RuntimeError("boom")

        monkeypatch.setattr(plugin_module, "_get_host_bridge", lambda: BrokenBridge())
        plugin_module._log("warn", "test message")
