"""Shared test helpers: MockHost and MockHttpResponse."""

import json
from dataclasses import dataclass, field


@dataclass
class MockHttpResponse:
    """Mimics the WIT HttpResponse record."""

    status: int
    headers: list = field(default_factory=list)
    body: bytes = b""


class MockHost:
    """Mock host functions for testing without WASM runtime.

    Simulates the WIT host interface: http_get, http_post,
    get_plugin_data, set_plugin_data, log.
    """

    def __init__(self):
        self._data: dict[str, bytes] = {}
        self._http_responses: dict[str, MockHttpResponse] = {}
        self._log_messages: list[tuple[str, str]] = []

    def set_config(self, config: dict):
        """Store plugin config as JSON bytes."""
        self._data["config"] = json.dumps(config).encode("utf-8")

    def set_http_response(self, url_pattern: str, response: MockHttpResponse):
        """Register a mock HTTP response for URLs containing the pattern."""
        self._http_responses[url_pattern] = response

    def http_get(self, url, headers):
        for pattern, resp in self._http_responses.items():
            if pattern in url:
                return resp
        return MockHttpResponse(status=404, body=b'{"status":"not found"}')

    def http_post(self, url, headers, body):
        for pattern, resp in self._http_responses.items():
            if pattern in url:
                return resp
        return MockHttpResponse(status=404, body=b'{"status":"not found"}')

    def get_plugin_data(self, key):
        data = self._data.get(key)
        if data is not None:
            return list(data)
        return None

    def set_plugin_data(self, key, value):
        self._data[key] = bytes(value)

    def log(self, level, message):
        self._log_messages.append((level, message))
