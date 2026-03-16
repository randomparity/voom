"""TV filename parser — extracts series/season/episode info from filenames."""

import re
from dataclasses import dataclass, field


@dataclass
class EpisodeInfo:
    """Parsed TV episode information from a filename."""

    series_name: str
    season_number: int
    episode_numbers: list[int] = field(default_factory=list)
    year: int | None = None

    @property
    def episode_number(self) -> int:
        """First (or only) episode number."""
        return self.episode_numbers[0] if self.episode_numbers else 0

    @property
    def is_multi_episode(self) -> bool:
        return len(self.episode_numbers) > 1


# --- Regex patterns ---

# S01E02, S01E02E03, S01E02-E03
_SXXEXX = re.compile(
    r"^(?P<series>.+?)"
    r"[.\s_-]+"
    r"[Ss](?P<season>\d{1,2})"
    r"[Ee](?P<ep>\d{1,3})"
    r"(?:[-Ee]+(?P<ep2>\d{1,3}))*",
)

# 1x02
_NXN = re.compile(
    r"^(?P<series>.+?)"
    r"[.\s_-]+"
    r"(?P<season>\d{1,2})"
    r"x(?P<ep>\d{1,3})",
)

# Season 1/Episode 2 (directory-based)
_DIR_SEASON = re.compile(
    r"[Ss]eason\s*(?P<season>\d{1,2})",
)
_DIR_EPISODE = re.compile(
    r"(?:[Ee]pisode|[Ee]p\.?)\s*(?P<ep>\d{1,3})",
)

# Year in parentheses for disambiguation: "Show (2005)"
_YEAR = re.compile(r"\((\d{4})\)")


def _clean_series_name(raw: str) -> str:
    """Clean up a raw series name extracted from a filename."""
    # Replace dots, underscores with spaces
    name = re.sub(r"[._]", " ", raw)
    # Collapse multiple spaces
    name = re.sub(r"\s+", " ", name).strip()
    # Remove trailing dash/whitespace
    name = name.rstrip("- ")
    return name


def _extract_year(text: str) -> int | None:
    """Extract a year in parentheses from text."""
    m = _YEAR.search(text)
    if m:
        year = int(m.group(1))
        if 1900 <= year <= 2099:
            return year
    return None


def _collect_episodes(match: re.Match) -> list[int]:
    """Collect episode numbers from a regex match, including multi-episode."""
    eps = [int(match.group("ep"))]
    ep2 = match.group("ep2") if "ep2" in match.groupdict() else None
    if ep2 is not None:
        ep2_int = int(ep2)
        if ep2_int == eps[0] + 1:
            # Adjacent episodes: S01E02E03 or S01E02-E03
            eps.append(ep2_int)
        elif ep2_int > eps[0]:
            # Range: S01E02-E05
            eps = list(range(eps[0], ep2_int + 1))
    return eps


def parse_filename(filename: str) -> EpisodeInfo | None:
    """Parse a TV filename/path and extract episode information.

    Supports patterns:
      - S01E02 / s01e02 (standard)
      - S01E02E03, S01E02-E03 (multi-episode)
      - 1x02 (alternate)
      - Dot.Separated.S01E02.720p...
      - Dash - Separated - S01E02 - Title
      - Directory-based: Season 1/Episode 2

    Returns None if no episode pattern is found.
    """
    # Extract just the path components we care about
    # Try the filename first, then include parent directory
    parts = filename.replace("\\", "/").split("/")
    basename = parts[-1] if parts else filename

    # Remove file extension
    name_no_ext = re.sub(r"\.\w{2,4}$", "", basename)

    # Try SxxExx pattern
    m = _SXXEXX.match(name_no_ext)
    if m:
        return EpisodeInfo(
            series_name=_clean_series_name(m.group("series")),
            season_number=int(m.group("season")),
            episode_numbers=_collect_episodes(m),
            year=_extract_year(filename),
        )

    # Try NxN pattern
    m = _NXN.match(name_no_ext)
    if m:
        return EpisodeInfo(
            series_name=_clean_series_name(m.group("series")),
            season_number=int(m.group("season")),
            episode_numbers=[int(m.group("ep"))],
            year=_extract_year(filename),
        )

    # Try directory-based: look at parent path for "Season X"
    full_path = "/".join(parts)
    season_match = _DIR_SEASON.search(full_path)
    ep_match = _DIR_EPISODE.search(name_no_ext)
    if season_match and ep_match:
        # Try to get series name from grandparent directory
        series_name = ""
        if len(parts) >= 3:
            series_name = parts[-3]
        elif len(parts) >= 2:
            series_name = parts[-2]
        return EpisodeInfo(
            series_name=_clean_series_name(series_name),
            season_number=int(season_match.group("season")),
            episode_numbers=[int(ep_match.group("ep"))],
            year=_extract_year(filename),
        )

    return None
