"""WIT Guest implementation for the tvdb-metadata WASM plugin.

Implements the three exported functions from voom:plugin@0.2.0/plugin:
  - get_info() -> PluginInfo
  - handles(event_type: str) -> bool
  - on_event(event: EventData) -> Option<EventResult>

When compiled with componentize-py, this module's top-level functions are
bound to the WIT exports. Host imports (http_get, http_post, get_plugin_data,
set_plugin_data, log) are available through the generated bindings.
"""

from tvdb_metadata.filename_parser import parse_filename
from tvdb_metadata.msgpack_helpers import pack_event, unpack_event
from tvdb_metadata.tvdb_client import TvdbClient, TvdbError

# The host module is injected by componentize-py at build time.
# For native testing, tests monkeypatch this.
try:
    from voom.plugin import host as _host_module  # type: ignore[import]
except ImportError:
    _host_module = None  # type: ignore[assignment]


class _HostBridge:
    """Adapter between WIT-generated host bindings and our client API.

    WIT host functions use typed records (Header, HttpResponse);
    our TvdbClient expects simpler Python types. This bridge translates.
    """

    def __init__(self, host_mod):
        self._host = host_mod

    def http_get(self, url, headers):
        wit_headers = [self._host.Header(name=n, value=v) for n, v in headers]
        result = self._host.http_get(url, wit_headers)
        if isinstance(result, Exception) or (hasattr(result, "is_err") and result.is_err()):
            raise TvdbError(f"HTTP GET failed: {result}")
        resp = result if not hasattr(result, "value") else result.value
        return resp

    def http_post(self, url, headers, body):
        wit_headers = [self._host.Header(name=n, value=v) for n, v in headers]
        result = self._host.http_post(url, wit_headers, list(body))
        if isinstance(result, Exception) or (hasattr(result, "is_err") and result.is_err()):
            raise TvdbError(f"HTTP POST failed: {result}")
        resp = result if not hasattr(result, "value") else result.value
        return resp

    def get_plugin_data(self, key):
        return self._host.get_plugin_data(key)

    def set_plugin_data(self, key, value):
        self._host.set_plugin_data(key, list(value))

    def log(self, level, message):
        level_map = {
            "trace": self._host.LogLevel.TRACE if hasattr(self._host, "LogLevel") else 0,
            "debug": self._host.LogLevel.DEBUG if hasattr(self._host, "LogLevel") else 1,
            "info": self._host.LogLevel.INFO if hasattr(self._host, "LogLevel") else 2,
            "warn": self._host.LogLevel.WARN if hasattr(self._host, "LogLevel") else 3,
            "error": self._host.LogLevel.ERROR if hasattr(self._host, "LogLevel") else 4,
        }
        self._host.log(level_map.get(level, 2), message)


# --- WIT exports ---


def get_info():
    """Return plugin identity and capabilities.

    WIT signature: get-info() -> plugin-info
    """
    # When running under componentize-py, this returns a WIT PluginInfo record.
    # For native testing, we return a plain dict.
    if _host_module is not None and hasattr(_host_module, "__package__"):
        # Under componentize-py: import the generated types
        try:
            from voom.plugin.plugin import PluginInfo, Capability, EnrichCap  # type: ignore[import]
            return PluginInfo(
                name="tvdb-metadata",
                version="0.1.0",
                description="TV metadata enrichment via TVDB API v4",
                author="David Christensen",
                license="MIT",
                homepage="https://github.com/randomparity/voom",
                capabilities=[Capability.enrich_metadata(EnrichCap(source="tvdb"))],
            )
        except ImportError:
            pass

    # Fallback for testing: return a simple object
    return {
        "name": "tvdb-metadata",
        "version": "0.1.0",
        "description": "TV metadata enrichment via TVDB API v4",
        "author": "David Christensen",
        "license": "MIT",
        "homepage": "https://github.com/randomparity/voom",
        "capabilities": ["enrich_metadata:tvdb"],
    }


def handles(event_type: str) -> bool:
    """Check if this plugin handles the given event type.

    WIT signature: handles(event-type: string) -> bool
    """
    return event_type == "file.introspected"


def on_event(event) -> "object | None":
    """Process an event and optionally return a result.

    WIT signature: on-event(event: event-data) -> option<event-result>

    The event payload is MessagePack bytes matching rmp_serde's format:
      {"FileIntrospected": {"file": {...}}}
    """
    event_type = event.event_type if hasattr(event, "event_type") else event.get("event_type", "")
    payload = event.payload if hasattr(event, "payload") else event.get("payload", b"")

    if event_type != "file.introspected":
        return None

    # Deserialize the MessagePack event payload
    try:
        variant, data = unpack_event(bytes(payload))
    except Exception as e:
        _log("warn", f"Failed to deserialize event: {e}")
        return None

    if variant != "FileIntrospected":
        return None

    file_data = data.get("file")
    if not file_data or not isinstance(file_data, dict):
        _log("warn", f"FileIntrospected payload missing 'file' key: {list(data.keys())}")
        return None
    file_path = file_data.get("path", "")
    if not file_path:
        _log("warn", "FileIntrospected payload has empty path")
        return None

    _log("info", f"Processing file for TVDB lookup: {file_path}")

    # Parse filename for episode info
    info = parse_filename(file_path)
    if info is None:
        _log("debug", f"No episode pattern found in: {file_path}")
        return None

    _log("info", f"Parsed: {info.series_name} S{info.season_number:02d}E{info.episode_number:02d}")

    # Create TVDB client from config
    host_funcs = _get_host_bridge()
    if host_funcs is None:
        _log("warn", "No host functions available")
        return None

    client = TvdbClient.from_config(host_funcs)
    if client is None:
        _log("warn", "No TVDB config found (set api_key via plugin data)")
        return None

    # Look up episode metadata
    try:
        metadata = client.lookup(
            series_name=info.series_name,
            season=info.season_number,
            episode=info.episode_number,
            year=info.year,
        )
    except Exception as e:
        _log("error", f"TVDB lookup failed: {e}")
        return None

    if metadata is None:
        _log("info", f"No TVDB match for: {info.series_name}")
        return None

    # Build MetadataEnriched event
    enriched_payload = pack_event("MetadataEnriched", {
        "path": file_path,
        "source": "tvdb",
        "metadata": metadata,
    })

    # Return event result
    return _make_event_result(
        plugin_name="tvdb-metadata",
        produced_events=[{
            "event_type": "metadata.enriched",
            "payload": list(enriched_payload),
        }],
    )


# --- Internal helpers ---


def _get_host_bridge():
    """Get the host function bridge, or None if unavailable."""
    if _host_module is not None:
        return _HostBridge(_host_module)
    return None


_log_sink = None  # Set by tests to capture log output


def _log(level: str, message: str):
    """Log via host if available, otherwise no-op."""
    if _log_sink is not None:
        _log_sink(level, message)
        return
    bridge = _get_host_bridge()
    if bridge is not None:
        try:
            bridge.log(level, message)
        except Exception:
            pass


def _make_event_result(plugin_name: str, produced_events: list, data=None):
    """Build an EventResult, using WIT types if available."""
    try:
        from voom.plugin.plugin import EventResult  # type: ignore[import]
        from voom.plugin.types import EventData  # type: ignore[import]

        wit_events = [
            EventData(event_type=e["event_type"], payload=e["payload"])
            for e in produced_events
        ]
        return EventResult(
            plugin_name=plugin_name,
            produced_events=wit_events,
            data=data,
        )
    except ImportError:
        # Testing fallback
        return {
            "plugin_name": plugin_name,
            "produced_events": produced_events,
            "data": data,
        }
