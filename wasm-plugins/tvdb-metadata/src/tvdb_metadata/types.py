"""Typed payload contracts for TVDB plugin boundaries."""

from typing import Any, NotRequired, TypeAlias, TypedDict


JsonObject: TypeAlias = dict[str, Any]


class TvdbAuthData(TypedDict, total=False):
    token: str


class TvdbAuthResponse(TypedDict, total=False):
    data: TvdbAuthData


class TvdbSearchResult(TypedDict, total=False):
    id: int | str
    tvdb_id: int | str
    name: str
    year: str | int
    originalLanguage: str


class TvdbEpisodeRecord(TypedDict, total=False):
    id: int | str
    name: str
    overview: str
    aired: str
    number: int
    episodeNumber: int


class TvdbMetadataResult(TypedDict):
    source: str
    series_id: int | str
    series_name: str
    season_number: int
    episode_number: int
    episode_name: str
    overview: str
    air_date: str
    tvdb_episode_id: int | str | None
    original_language: NotRequired[str]


class FileRecordPayload(TypedDict, total=False):
    path: str


class FileIntrospectedPayload(TypedDict, total=False):
    file: FileRecordPayload


class EventDataDict(TypedDict):
    event_type: str
    payload: list[int]


class EventResultDict(TypedDict):
    plugin_name: str
    produced_events: list[EventDataDict]
    data: JsonObject | None
