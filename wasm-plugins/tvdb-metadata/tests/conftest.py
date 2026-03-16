"""Test fixtures for tvdb-metadata tests."""

import json
import sys
from pathlib import Path

import pytest

# Add src to path so we can import the plugin modules
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))
# Add tests to path so helpers can be imported
sys.path.insert(0, str(Path(__file__).parent))

from helpers import MockHost, MockHttpResponse


@pytest.fixture
def mock_host():
    """Provide a fresh MockHost instance."""
    return MockHost()


@pytest.fixture
def mock_host_with_tvdb(mock_host):
    """MockHost pre-configured with TVDB API responses."""
    mock_host.set_config({"api_key": "test-api-key-123"})

    # Auth response
    mock_host.set_http_response("/v4/login", MockHttpResponse(
        status=200,
        body=json.dumps({
            "status": "success",
            "data": {"token": "mock-bearer-token-abc"},
        }).encode("utf-8"),
    ))

    # Search response
    mock_host.set_http_response("/v4/search", MockHttpResponse(
        status=200,
        body=json.dumps({
            "status": "success",
            "data": [
                {
                    "id": "73255",
                    "tvdb_id": 73255,
                    "name": "Breaking Bad",
                    "year": "2008",
                    "slug": "breaking-bad",
                },
            ],
        }).encode("utf-8"),
    ))

    # Episodes response
    mock_host.set_http_response("/series/73255/episodes", MockHttpResponse(
        status=200,
        body=json.dumps({
            "status": "success",
            "data": {
                "episodes": [
                    {
                        "id": 349232,
                        "name": "Pilot",
                        "number": 1,
                        "seasonNumber": 1,
                        "aired": "2008-01-20",
                        "overview": "Walter White, a struggling chemistry teacher...",
                    },
                    {
                        "id": 349233,
                        "name": "Cat's in the Bag...",
                        "number": 2,
                        "seasonNumber": 1,
                        "aired": "2008-01-27",
                        "overview": "Walt and Jesse attempt to tie up loose ends.",
                    },
                ],
            },
        }).encode("utf-8"),
    ))

    return mock_host
