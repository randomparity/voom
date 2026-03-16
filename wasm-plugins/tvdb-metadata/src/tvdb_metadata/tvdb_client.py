"""TVDB API v4 client using host HTTP functions.

All HTTP requests go through the WASM host's http_get/http_post functions
since Python's urllib/socket are unavailable in the WASM sandbox.
"""

import json
import time

# These will be the WIT-generated host module functions.
# In tests, they are monkeypatched. At runtime in WASM, they are provided
# by the componentize-py bindings.
_host = None

TVDB_API_BASE = "https://api4.thetvdb.com/v4"
TOKEN_TTL_SECONDS = 24 * 60 * 60  # 24 hours
CACHE_TTL_SECONDS = 60 * 60  # 1 hour


class TvdbError(Exception):
    """Error from TVDB API operations."""


class TvdbClient:
    """TVDB API v4 client that uses host functions for HTTP and storage."""

    def __init__(self, api_key: str, host_funcs):
        self.api_key = api_key
        self._host = host_funcs
        self._token: str | None = None
        self._token_expiry: float = 0

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

        Caches the token in plugin data with expiry tracking.
        """
        # Check cached token
        if self._token and time.time() < self._token_expiry:
            return self._token

        # Check plugin data for cached token
        cached = self._host.get_plugin_data("tvdb_token")
        if cached is not None:
            try:
                token_data = json.loads(bytes(cached))
                if token_data.get("expiry", 0) > time.time():
                    self._token = token_data["token"]
                    self._token_expiry = token_data["expiry"]
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
        self._token_expiry = time.time() + TOKEN_TTL_SECONDS

        # Cache token in plugin data
        token_data = json.dumps({"token": token, "expiry": self._token_expiry})
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
            self._token_expiry = 0
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
        """
        # Check cache
        cache_key = f"search:{name.lower()}"
        cached = self._host.get_plugin_data(cache_key)
        if cached is not None:
            try:
                cache_data = json.loads(bytes(cached))
                if cache_data.get("expiry", 0) > time.time():
                    return cache_data["results"]
            except (json.JSONDecodeError, KeyError, TypeError):
                pass

        # Query API
        from urllib.parse import quote
        encoded_name = quote(name)
        result = self._api_get(f"/search?query={encoded_name}&type=series")
        series_list = result.get("data", [])

        # Cache results
        cache_data = json.dumps({
            "results": series_list,
            "expiry": time.time() + CACHE_TTL_SECONDS,
        })
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

        episodes = self.get_episodes(int(series_id), season)
        ep_data = None
        for ep in episodes:
            if ep.get("number") == episode or ep.get("episodeNumber") == episode:
                ep_data = ep
                break

        if ep_data is None:
            return None

        return {
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
