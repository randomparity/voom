"""TVDB API v4 client using host HTTP functions.

All HTTP requests go through the WASM host's http_get/http_post functions
since Python's urllib/socket are unavailable in the WASM sandbox.
"""

import json

try:
    from urllib.parse import quote as _url_quote
except ImportError:
    def _url_quote(s, safe=""):
        """Minimal percent-encoding fallback for WASM environments."""
        import string
        _safe = set(string.ascii_letters + string.digits + "-._~" + safe)
        return "".join(c if c in _safe else f"%{ord(c):02X}" for c in s)

# These will be the WIT-generated host module functions.
# In tests, they are monkeypatched. At runtime in WASM, they are provided
# by the componentize-py bindings.
_host = None

TVDB_API_BASE = "https://api4.thetvdb.com/v4"


class TvdbError(Exception):
    """Error from TVDB API operations."""


class TvdbClient:
    """TVDB API v4 client that uses host functions for HTTP and storage."""

    def __init__(self, api_key: str, host_funcs):
        self.api_key = api_key
        self._host = host_funcs
        self._token: str | None = None

    @classmethod
    def from_config(cls, host_funcs) -> "TvdbClient | None":
        """Create a client from stored plugin config.

        Config is stored as JSON bytes via get_plugin_data("config"):
        {"api_key": "...", "pin": "..."}
        """
        data = host_funcs.get_plugin_data("config")
        if data is None:
            return None
        try:
            config = json.loads(bytes(data))
        except (json.JSONDecodeError, TypeError):
            return None
        api_key = config.get("api_key")
        if not api_key:
            return None
        return cls(api_key=api_key, host_funcs=host_funcs)

    def authenticate(self) -> str:
        """Authenticate with TVDB API and return bearer token.

        Caches the token in-memory and in plugin data. The 401 retry
        logic in _api_get() handles token expiration by re-authenticating,
        so no TTL tracking is needed (avoids time.time() WASI issues).
        """
        # Check in-memory cache
        if self._token:
            return self._token

        # Check plugin data for cached token
        cached = self._host.get_plugin_data("tvdb_token")
        if cached is not None:
            try:
                token_data = json.loads(bytes(cached))
                token = token_data.get("token")
                if token:
                    self._token = token
                    return self._token
            except (json.JSONDecodeError, KeyError, TypeError):
                pass

        # Request new token
        url = f"{TVDB_API_BASE}/login"
        body = json.dumps({"apikey": self.api_key}).encode("utf-8")
        headers = [("Content-Type", "application/json")]

        resp = self._host.http_post(url, headers, body)
        if resp.status != 200:
            raise TvdbError(f"TVDB auth failed with status {resp.status}")

        result = json.loads(bytes(resp.body))
        token = result.get("data", {}).get("token")
        if not token:
            raise TvdbError("No token in TVDB auth response")

        self._token = token

        # Cache token in plugin data
        token_data = json.dumps({"token": token})
        self._host.set_plugin_data("tvdb_token", token_data.encode("utf-8"))

        return token

    def _api_get(self, path: str) -> dict:
        """Make an authenticated GET request to the TVDB API."""
        token = self.authenticate()
        url = f"{TVDB_API_BASE}{path}"
        headers = [
            ("Authorization", f"Bearer {token}"),
            ("Accept", "application/json"),
        ]

        resp = self._host.http_get(url, headers)
        if resp.status == 401:
            # Token expired, clear cache and retry once
            self._token = None
            self._host.set_plugin_data("tvdb_token", b"")
            token = self.authenticate()
            headers = [
                ("Authorization", f"Bearer {token}"),
                ("Accept", "application/json"),
            ]
            resp = self._host.http_get(url, headers)

        if resp.status != 200:
            raise TvdbError(f"TVDB API returned status {resp.status} for {path}")

        return json.loads(bytes(resp.body))

    def search_series(self, name: str) -> list[dict]:
        """Search for TV series by name.

        Returns list of series results with id, name, year, etc.
        Results are cached in plugin data for the plugin's lifetime.
        """
        # Check cache (no TTL — plugin data is per-lifetime only)
        cache_key = f"search:{name[:128].lower()}"
        cached = self._host.get_plugin_data(cache_key)
        if cached is not None:
            try:
                cache_data = json.loads(bytes(cached))
                return cache_data["results"]
            except (json.JSONDecodeError, KeyError, TypeError):
                pass

        # Query API
        encoded_name = _url_quote(name, safe="")
        result = self._api_get(f"/search?query={encoded_name}&type=series")
        series_list = result.get("data", [])

        # Cache results
        cache_data = json.dumps({"results": series_list})
        self._host.set_plugin_data(cache_key, cache_data.encode("utf-8"))

        return series_list

    def get_episodes(self, series_id: int, season: int) -> list[dict]:
        """Get episodes for a series/season.

        Returns list of episode records.
        """
        result = self._api_get(
            f"/series/{series_id}/episodes/default?season={season}"
        )
        return result.get("data", {}).get("episodes", [])

    def lookup(self, series_name: str, season: int, episode: int,
               year: int | None = None) -> dict | None:
        """Full lookup: search series → find episode → return metadata.

        Returns a metadata dict or None if not found.
        """
        series_results = self.search_series(series_name)
        if not series_results:
            return None

        # Pick best match — prefer exact name match, then year match
        series = self._best_match(series_results, series_name, year)
        if series is None:
            return None

        series_id = series.get("tvdb_id") or series.get("id")
        if series_id is None:
            return None

        try:
            series_id_int = int(series_id)
        except (ValueError, TypeError):
            return None
        episodes = self.get_episodes(series_id_int, season)
        ep_data = None
        for ep in episodes:
            if ep.get("number") == episode or ep.get("episodeNumber") == episode:
                ep_data = ep
                break

        if ep_data is None:
            return None

        result = {
            "source": "tvdb",
            "series_id": series_id,
            "series_name": series.get("name", series_name),
            "season_number": season,
            "episode_number": episode,
            "episode_name": ep_data.get("name", ""),
            "overview": ep_data.get("overview", ""),
            "air_date": ep_data.get("aired", ""),
            "tvdb_episode_id": ep_data.get("id"),
        }
        original_lang = series.get("originalLanguage")
        if original_lang:
            result["original_language"] = original_lang
        return result

    @staticmethod
    def _best_match(results: list[dict], name: str,
                    year: int | None) -> dict | None:
        """Pick the best series match from search results."""
        if not results:
            return None

        name_lower = name.lower()

        # Exact name match
        for r in results:
            r_name = (r.get("name") or "").lower()
            if r_name == name_lower:
                if year is None or str(year) in str(r.get("year", "")):
                    return r

        # Year match if provided
        if year is not None:
            for r in results:
                if str(year) in str(r.get("year", "")):
                    return r

        # Fall back to first result
        return results[0]
