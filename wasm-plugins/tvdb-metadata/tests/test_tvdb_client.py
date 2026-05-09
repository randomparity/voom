"""Tests for the TVDB API v4 client."""

import json

import pytest

from tvdb_metadata.tvdb_client import TvdbClient, TvdbError

from helpers import MockHttpResponse


class TestFromConfig:
    """TvdbClient.from_config() config loading."""

    def test_loads_config(self, mock_host):
        mock_host.set_config({"api_key": "test-key"})
        client = TvdbClient.from_config(mock_host)
        assert client is not None
        assert client.api_key == "test-key"

    def test_no_config_returns_none(self, mock_host):
        client = TvdbClient.from_config(mock_host)
        assert client is None

    def test_invalid_json_returns_none(self, mock_host):
        mock_host._data["config"] = b"not json"
        client = TvdbClient.from_config(mock_host)
        assert client is None

    def test_missing_api_key_returns_none(self, mock_host):
        mock_host.set_config({"pin": "12345"})
        client = TvdbClient.from_config(mock_host)
        assert client is None


class TestAuthentication:
    """Token authentication flow."""

    def test_auth_success(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        token = client.authenticate()
        assert token == "mock-bearer-token-abc"

    def test_auth_caches_token(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        token1 = client.authenticate()
        token2 = client.authenticate()
        assert token1 == token2

    def test_auth_stores_in_plugin_data(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        client.authenticate()
        cached = mock_host_with_tvdb.get_plugin_data("tvdb_token")
        assert cached is not None
        data = json.loads(bytes(cached))
        assert data["token"] == "mock-bearer-token-abc"

    def test_auth_failure_raises(self, mock_host):
        mock_host.set_config({"api_key": "bad-key"})
        mock_host.set_http_response("/v4/login", MockHttpResponse(
            status=401,
            body=b'{"status":"failure"}',
        ))
        client = TvdbClient.from_config(mock_host)
        with pytest.raises(TvdbError) as excinfo:
            client.authenticate()
        assert "401" in str(excinfo.value)

    def test_invalid_cached_token_refetches(self, mock_host):
        mock_host.set_config({"api_key": "test-key"})
        mock_host._data["tvdb_token"] = b"not json"
        mock_host.set_http_response("/v4/login", MockHttpResponse(
            status=200,
            body=json.dumps({"data": {"token": "fresh-token"}}).encode(),
        ))
        client = TvdbClient.from_config(mock_host)
        token = client.authenticate()
        assert token == "fresh-token"


class TestSearchSeries:
    """Series search functionality."""

    def test_search_returns_results(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        results = client.search_series("Breaking Bad")
        assert len(results) == 1
        assert results[0]["name"] == "Breaking Bad"

    def test_search_caches_results(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        client.search_series("Breaking Bad")
        # Check cache key exists
        cached = mock_host_with_tvdb.get_plugin_data("search:breaking bad")
        assert cached is not None

    def test_search_no_results(self, mock_host_with_tvdb):
        mock_host_with_tvdb.set_http_response("/v4/search", MockHttpResponse(
            status=200,
            body=json.dumps({"status": "success", "data": []}).encode(),
        ))
        client = TvdbClient.from_config(mock_host_with_tvdb)
        results = client.search_series("Nonexistent Show")
        assert results == []


class TestGetEpisodes:
    """Episode retrieval."""

    def test_get_episodes(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        episodes = client.get_episodes(73255, 1)
        assert len(episodes) == 2
        assert episodes[0]["name"] == "Pilot"

    def test_get_episodes_api_error(self, mock_host):
        mock_host.set_config({"api_key": "key"})
        mock_host.set_http_response("/v4/login", MockHttpResponse(
            status=200,
            body=json.dumps({"data": {"token": "t"}}).encode(),
        ))
        mock_host.set_http_response("/v4/series/", MockHttpResponse(
            status=500,
            body=b"server error",
        ))
        client = TvdbClient.from_config(mock_host)
        with pytest.raises(TvdbError):
            client.get_episodes(99999, 1)


class TestLookup:
    """Full lookup: search → episodes → metadata."""

    def test_full_lookup_success(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        metadata = client.lookup("Breaking Bad", season=1, episode=1)
        assert metadata is not None
        assert metadata["source"] == "tvdb"
        assert metadata["series_id"] == 73255
        assert metadata["series_name"] == "Breaking Bad"
        assert metadata["season_number"] == 1
        assert metadata["episode_number"] == 1
        assert metadata["episode_name"] == "Pilot"
        assert metadata["air_date"] == "2008-01-20"

    def test_lookup_episode_not_found(self, mock_host_with_tvdb):
        client = TvdbClient.from_config(mock_host_with_tvdb)
        metadata = client.lookup("Breaking Bad", season=1, episode=99)
        assert metadata is None

    def test_lookup_series_not_found(self, mock_host_with_tvdb):
        mock_host_with_tvdb.set_http_response("/v4/search", MockHttpResponse(
            status=200,
            body=json.dumps({"status": "success", "data": []}).encode(),
        ))
        client = TvdbClient.from_config(mock_host_with_tvdb)
        metadata = client.lookup("Nonexistent", season=1, episode=1)
        assert metadata is None

    def test_lookup_with_year_disambiguation(self, mock_host_with_tvdb):
        mock_host_with_tvdb.set_http_response("/v4/search", MockHttpResponse(
            status=200,
            body=json.dumps({
                "data": [
                    {"id": "100", "tvdb_id": 100, "name": "Doctor Who", "year": "1963"},
                    {"id": "200", "tvdb_id": 200, "name": "Doctor Who", "year": "2005"},
                ],
            }).encode(),
        ))
        mock_host_with_tvdb.set_http_response("/v4/series/200", MockHttpResponse(
            status=200,
            body=json.dumps({
                "data": {
                    "episodes": [{"id": 1, "name": "Rose", "number": 1, "aired": "2005-03-26"}],
                },
            }).encode(),
        ))
        client = TvdbClient.from_config(mock_host_with_tvdb)
        metadata = client.lookup("Doctor Who", season=1, episode=1, year=2005)
        assert metadata is not None
        assert metadata["series_id"] == 200


class TestLookupEdgeCases:
    """Edge cases in the full lookup flow."""

    def test_lookup_non_numeric_series_id(self, mock_host_with_tvdb):
        """API returning non-numeric series id should return None."""
        mock_host_with_tvdb.set_http_response("/v4/search", MockHttpResponse(
            status=200,
            body=json.dumps({
                "data": [{"id": "not-a-number", "name": "Test Show", "year": "2020"}],
            }).encode(),
        ))
        client = TvdbClient.from_config(mock_host_with_tvdb)
        result = client.lookup("Test Show", season=1, episode=1)
        assert result is None

    def test_lookup_with_invalid_cached_search_refetches(self, mock_host_with_tvdb):
        mock_host_with_tvdb._data["search:breaking bad"] = b"not json"
        client = TvdbClient.from_config(mock_host_with_tvdb)
        metadata = client.lookup("Breaking Bad", season=1, episode=1)
        assert metadata is not None
        assert metadata["episode_name"] == "Pilot"


class TestBestMatch:
    """Series matching logic."""

    def test_exact_name_match(self):
        results = [
            {"name": "Bad", "year": "2020"},
            {"name": "Breaking Bad", "year": "2008"},
        ]
        match = TvdbClient._best_match(results, "Breaking Bad", None)
        assert match["name"] == "Breaking Bad"

    def test_year_match_when_name_differs(self):
        results = [
            {"name": "Show v1", "year": "2000"},
            {"name": "Show v2", "year": "2020"},
        ]
        match = TvdbClient._best_match(results, "Show", year=2020)
        assert match["year"] == "2020"

    def test_fallback_to_first(self):
        results = [{"name": "Something Else", "year": "2010"}]
        match = TvdbClient._best_match(results, "Nonexistent", None)
        assert match["name"] == "Something Else"

    def test_empty_results(self):
        assert TvdbClient._best_match([], "Test", None) is None
