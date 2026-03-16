"""Tests for TV filename parsing."""

from tvdb_metadata.filename_parser import EpisodeInfo, parse_filename


class TestStandardPatterns:
    """SxxExx format — the most common TV naming convention."""

    def test_standard_sxxexx(self):
        info = parse_filename("Breaking.Bad.S01E02.720p.BluRay.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"
        assert info.season_number == 1
        assert info.episode_number == 2

    def test_lowercase_sxxexx(self):
        info = parse_filename("breaking.bad.s01e02.mkv")
        assert info is not None
        assert info.season_number == 1
        assert info.episode_number == 2

    def test_dash_separated(self):
        info = parse_filename("Breaking Bad - S01E02 - Seven Thirty-Seven.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"
        assert info.season_number == 1
        assert info.episode_number == 2

    def test_underscore_separated(self):
        info = parse_filename("Breaking_Bad_S01E02_720p.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"

    def test_space_separated(self):
        info = parse_filename("Breaking Bad S01E02 720p.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"
        assert info.season_number == 1
        assert info.episode_number == 2

    def test_three_digit_episode(self):
        info = parse_filename("Naruto.S01E100.mkv")
        assert info is not None
        assert info.episode_number == 100


class TestMultiEpisode:
    """Multi-episode patterns: S01E02E03, S01E02-E03."""

    def test_consecutive_episodes(self):
        info = parse_filename("Show.S01E02E03.mkv")
        assert info is not None
        assert info.episode_numbers == [2, 3]
        assert info.is_multi_episode

    def test_dash_episodes(self):
        info = parse_filename("Show.S01E02-E03.mkv")
        assert info is not None
        assert info.episode_numbers == [2, 3]

    def test_single_episode_not_multi(self):
        info = parse_filename("Show.S01E02.mkv")
        assert info is not None
        assert not info.is_multi_episode
        assert info.episode_numbers == [2]


class TestAlternateFormat:
    """NxNN format — less common but still used."""

    def test_nxn_format(self):
        info = parse_filename("Breaking Bad 1x02 720p.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"
        assert info.season_number == 1
        assert info.episode_number == 2

    def test_nxn_dot_separated(self):
        info = parse_filename("Breaking.Bad.1x02.mkv")
        assert info is not None
        assert info.season_number == 1
        assert info.episode_number == 2


class TestYearExtraction:
    """Year in parentheses for disambiguation."""

    def test_year_in_parens(self):
        info = parse_filename("Doctor Who (2005) S01E01.mkv")
        assert info is not None
        assert info.series_name == "Doctor Who (2005)"
        assert info.year == 2005

    def test_no_year(self):
        info = parse_filename("Breaking.Bad.S01E01.mkv")
        assert info is not None
        assert info.year is None

    def test_year_in_path(self):
        info = parse_filename("/tv/Doctor Who (2005)/Doctor.Who.S01E01.mkv")
        assert info is not None
        assert info.year == 2005


class TestDirectoryBased:
    """Season N/Episode N directory patterns."""

    def test_season_dir_episode_file(self):
        info = parse_filename("/tv/Breaking Bad/Season 1/Episode 2.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"
        assert info.season_number == 1
        assert info.episode_number == 2

    def test_season_dir_ep_abbrev(self):
        info = parse_filename("/tv/Some Show/Season 3/Ep. 5 - Title.mkv")
        assert info is not None
        assert info.season_number == 3
        assert info.episode_number == 5


class TestFullPaths:
    """Full filesystem paths with directories."""

    def test_full_unix_path(self):
        info = parse_filename("/media/tv/Breaking Bad/Breaking.Bad.S01E02.720p.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"
        assert info.season_number == 1
        assert info.episode_number == 2

    def test_windows_path(self):
        info = parse_filename("D:\\TV\\Breaking Bad\\Breaking.Bad.S01E02.mkv")
        assert info is not None
        assert info.series_name == "Breaking Bad"


class TestNoMatch:
    """Cases where no episode pattern should be found."""

    def test_movie_file(self):
        assert parse_filename("The.Matrix.1999.1080p.BluRay.mkv") is None

    def test_random_file(self):
        assert parse_filename("vacation_photos.mp4") is None

    def test_empty_string(self):
        assert parse_filename("") is None

    def test_no_extension(self):
        # Should still parse if pattern is there
        info = parse_filename("Show.S01E01")
        assert info is not None


class TestEpisodeInfoProperties:
    """EpisodeInfo dataclass properties."""

    def test_episode_number_single(self):
        info = EpisodeInfo(series_name="Test", season_number=1, episode_numbers=[5])
        assert info.episode_number == 5

    def test_episode_number_multi(self):
        info = EpisodeInfo(series_name="Test", season_number=1, episode_numbers=[5, 6])
        assert info.episode_number == 5

    def test_episode_number_empty(self):
        info = EpisodeInfo(series_name="Test", season_number=1, episode_numbers=[])
        assert info.episode_number == 0
