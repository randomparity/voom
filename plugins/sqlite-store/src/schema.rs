use rusqlite::Connection;

/// All SQL statements to create the VOOM schema.
const SCHEMA_SQL: &str = r"
CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,
    path TEXT UNIQUE,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    expected_hash TEXT,
    status TEXT NOT NULL DEFAULT 'active',
    missing_since TEXT,
    superseded_by TEXT,
    container TEXT NOT NULL,
    duration REAL,
    bitrate INTEGER,
    crop_left INTEGER,
    crop_top INTEGER,
    crop_right INTEGER,
    crop_bottom INTEGER,
    crop_detected_at TEXT,
    crop_settings_fingerprint TEXT,
    tags TEXT,
    plugin_metadata TEXT,
    introspected_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS tracks (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    stream_index INTEGER NOT NULL,
    track_type TEXT NOT NULL,
    codec TEXT NOT NULL,
    language TEXT NOT NULL DEFAULT 'und',
    title TEXT NOT NULL DEFAULT '',
    is_default INTEGER NOT NULL DEFAULT 0,
    is_forced INTEGER NOT NULL DEFAULT 0,
    channels INTEGER,
    channel_layout TEXT,
    sample_rate INTEGER,
    bit_depth INTEGER,
    loudness_integrated_lufs REAL,
    loudness_true_peak_db REAL,
    loudness_range_lu REAL,
    loudness_measured_at TEXT,
    width INTEGER,
    height INTEGER,
    frame_rate REAL,
    is_vfr INTEGER NOT NULL DEFAULT 0,
    is_hdr INTEGER NOT NULL DEFAULT 0,
    hdr_format TEXT,
    pixel_format TEXT,
    is_animation INTEGER,
    UNIQUE(file_id, stream_index)
);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    job_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 100,
    payload TEXT,
    progress REAL DEFAULT 0.0,
    progress_message TEXT,
    output TEXT,
    error TEXT,
    worker_id TEXT,
    created_at TEXT NOT NULL,
    started_at TEXT,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS plans (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    policy_name TEXT NOT NULL,
    phase_name TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    actions TEXT NOT NULL,
    warnings TEXT,
    skip_reason TEXT,
    policy_hash TEXT,
    evaluated_at TEXT,
    created_at TEXT NOT NULL,
    executed_at TEXT,
    result TEXT,
    session_id TEXT
);

CREATE TABLE IF NOT EXISTS file_transitions (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    path TEXT NOT NULL,
    from_path TEXT,
    from_hash TEXT,
    to_hash TEXT NOT NULL,
    from_size INTEGER,
    to_size INTEGER NOT NULL,
    source TEXT NOT NULL,
    source_detail TEXT,
    plan_id TEXT,
    duration_ms INTEGER,
    actions_taken INTEGER,
    tracks_modified INTEGER,
    outcome TEXT,
    policy_name TEXT,
    phase_name TEXT,
    metadata_snapshot TEXT,
    error_message TEXT,
    session_id TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_transitions_file ON file_transitions(file_id);
CREATE INDEX IF NOT EXISTS idx_transitions_source ON file_transitions(source);
CREATE INDEX IF NOT EXISTS idx_transitions_path ON file_transitions(path);
CREATE INDEX IF NOT EXISTS idx_transitions_from_path ON file_transitions(from_path);

CREATE TABLE IF NOT EXISTS plugin_data (
    plugin_name TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (plugin_name, key)
);

CREATE TABLE IF NOT EXISTS bad_files (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    size INTEGER NOT NULL,
    content_hash TEXT,
    error TEXT NOT NULL,
    error_source TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 1,
    first_seen_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS discovered_files (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    size INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    discovered_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_discovered_status ON discovered_files(status);

CREATE TABLE IF NOT EXISTS health_checks (
    id TEXT PRIMARY KEY,
    check_name TEXT NOT NULL,
    passed INTEGER NOT NULL,
    details TEXT,
    checked_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_health_checks_name ON health_checks(check_name);
CREATE INDEX IF NOT EXISTS idx_health_checks_time ON health_checks(checked_at);

CREATE TABLE IF NOT EXISTS event_log (
    rowid INTEGER PRIMARY KEY AUTOINCREMENT,
    id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    payload TEXT NOT NULL,
    summary TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_event_log_type ON event_log(event_type);

CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
CREATE INDEX IF NOT EXISTS idx_files_hash ON files(content_hash);
CREATE INDEX IF NOT EXISTS idx_files_superseded_by ON files(superseded_by);
CREATE UNIQUE INDEX IF NOT EXISTS idx_files_superseded_by_unique
    ON files(superseded_by) WHERE superseded_by IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_tracks_file ON tracks(file_id);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status, priority);
CREATE INDEX IF NOT EXISTS idx_plans_file ON plans(file_id);
CREATE INDEX IF NOT EXISTS idx_bad_files_path ON bad_files(path);

CREATE TABLE IF NOT EXISTS subtitles (
    id INTEGER PRIMARY KEY,
    file_path TEXT NOT NULL,
    subtitle_path TEXT NOT NULL,
    language TEXT NOT NULL,
    forced INTEGER NOT NULL DEFAULT 0,
    title TEXT,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    UNIQUE(file_path, subtitle_path)
);

CREATE INDEX IF NOT EXISTS idx_subtitles_file ON subtitles(file_path);

CREATE TABLE IF NOT EXISTS library_snapshots (
    id TEXT PRIMARY KEY,
    captured_at TEXT NOT NULL,
    trigger TEXT NOT NULL,
    total_files INTEGER NOT NULL,
    total_size_bytes INTEGER NOT NULL,
    total_duration_secs REAL NOT NULL,
    snapshot_json TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_snapshots_captured
    ON library_snapshots(captured_at);
CREATE INDEX IF NOT EXISTS idx_tracks_type ON tracks(track_type);

CREATE TABLE IF NOT EXISTS pending_operations (
    id TEXT PRIMARY KEY,
    file_path TEXT NOT NULL,
    phase_name TEXT NOT NULL,
    started_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS verifications (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    verified_at TEXT NOT NULL,
    mode TEXT NOT NULL,
    outcome TEXT NOT NULL,
    error_count INTEGER NOT NULL DEFAULT 0,
    warning_count INTEGER NOT NULL DEFAULT 0,
    content_hash TEXT,
    details TEXT
);

CREATE INDEX IF NOT EXISTS idx_verifications_file ON verifications(file_id);
CREATE INDEX IF NOT EXISTS idx_verifications_outcome ON verifications(outcome);
CREATE INDEX IF NOT EXISTS idx_verifications_time ON verifications(verified_at);
CREATE INDEX IF NOT EXISTS idx_verifications_file_verified_at ON verifications(file_id, verified_at);

CREATE TABLE IF NOT EXISTS transcode_outcomes (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id),
    target_vmaf INTEGER,
    achieved_vmaf REAL,
    crf_used INTEGER,
    bitrate_used TEXT,
    iterations INTEGER NOT NULL,
    sample_strategy TEXT NOT NULL,
    fallback_used INTEGER NOT NULL,
    completed_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_transcode_outcomes_file_completed
    ON transcode_outcomes(file_id, completed_at DESC);
";

/// Initialize the database schema.
pub fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    migrate(conn)?;
    Ok(())
}

fn table_exists(conn: &Connection, name: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
        [name],
        |row| row.get(0),
    )
}

/// Run migrations for existing databases that may lack newer columns/tables.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    // Check plans table for new columns
    const KNOWN_TABLES: &[&str] = &[
        "files",
        "tracks",
        "jobs",
        "plans",
        "file_transitions",
        "plugin_data",
        "bad_files",
        "discovered_files",
        "health_checks",
        "event_log",
        "subtitles",
        "library_snapshots",
        "pending_operations",
        "verifications",
        "transcode_outcomes",
    ];
    let has_column = |table: &str, column: &str| -> rusqlite::Result<bool> {
        assert!(KNOWN_TABLES.contains(&table), "unknown table: {table}");
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(columns.iter().any(|c| c == column))
    };

    // Table creation must precede column migrations: has_column queries
    // require the target table to exist.
    migrate_missing_tables(conn)?;
    migrate_transitions_table(conn)?;
    migrate_plans_columns(conn, &has_column)?;
    migrate_files_columns(conn, &has_column)?;
    migrate_tracks_columns(conn, &has_column)?;
    migrate_indexes_and_constraints(conn)?;
    migrate_processing_stats_into_transitions(conn, &has_column)?;
    migrate_metadata_snapshot_column(conn, &has_column)?;
    migrate_execution_capture_columns(conn, &has_column)?;
    migrate_from_path_column(conn, &has_column)?;
    migrate_cover_art_track_types(conn)?;

    Ok(())
}

fn migrate_tracks_columns(
    conn: &Connection,
    has_column: &dyn Fn(&str, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    if table_exists(conn, "tracks")? && !has_column("tracks", "is_animation")? {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN is_animation INTEGER;")?;
    }
    if table_exists(conn, "tracks")? && !has_column("tracks", "loudness_integrated_lufs")? {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN loudness_integrated_lufs REAL;")?;
    }
    if table_exists(conn, "tracks")? && !has_column("tracks", "loudness_true_peak_db")? {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN loudness_true_peak_db REAL;")?;
    }
    if table_exists(conn, "tracks")? && !has_column("tracks", "loudness_range_lu")? {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN loudness_range_lu REAL;")?;
    }
    if table_exists(conn, "tracks")? && !has_column("tracks", "loudness_measured_at")? {
        conn.execute_batch("ALTER TABLE tracks ADD COLUMN loudness_measured_at TEXT;")?;
    }
    Ok(())
}

/// Migrate plans table columns added after initial schema.
fn migrate_plans_columns(
    conn: &Connection,
    has_column: &dyn Fn(&str, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    let plans_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='plans'",
        [],
        |row| row.get(0),
    )?;
    if !plans_exists {
        return Ok(());
    }
    if !has_column("plans", "skip_reason")? {
        conn.execute_batch("ALTER TABLE plans ADD COLUMN skip_reason TEXT")?;
    }
    if !has_column("plans", "policy_hash")? {
        conn.execute_batch("ALTER TABLE plans ADD COLUMN policy_hash TEXT")?;
    }
    if !has_column("plans", "evaluated_at")? {
        conn.execute_batch("ALTER TABLE plans ADD COLUMN evaluated_at TEXT")?;
    }
    Ok(())
}

/// Migrate files table lifecycle columns added after initial schema.
fn migrate_files_columns(
    conn: &Connection,
    has_column: &dyn Fn(&str, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    if !has_column("files", "expected_hash")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN expected_hash TEXT")?;
    }
    if !has_column("files", "status")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN status TEXT NOT NULL DEFAULT 'active'")?;
    }
    if !has_column("files", "missing_since")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN missing_since TEXT")?;
    }
    if !has_column("files", "superseded_by")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN superseded_by TEXT")?;
    }
    if !has_column("files", "crop_left")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN crop_left INTEGER")?;
    }
    if !has_column("files", "crop_top")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN crop_top INTEGER")?;
    }
    if !has_column("files", "crop_right")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN crop_right INTEGER")?;
    }
    if !has_column("files", "crop_bottom")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN crop_bottom INTEGER")?;
    }
    if !has_column("files", "crop_detected_at")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN crop_detected_at TEXT")?;
    }
    if !has_column("files", "crop_settings_fingerprint")? {
        conn.execute_batch("ALTER TABLE files ADD COLUMN crop_settings_fingerprint TEXT")?;
    }
    Ok(())
}

/// Drop legacy `file_history` table and create `file_transitions` if needed.
fn migrate_transitions_table(conn: &Connection) -> rusqlite::Result<()> {
    let has_file_history: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='file_history'",
        [],
        |row| row.get(0),
    )?;
    if has_file_history {
        conn.execute_batch("DROP TABLE IF EXISTS file_history")?;
    }

    let has_file_transitions: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='file_transitions'",
        [],
        |row| row.get(0),
    )?;
    if !has_file_transitions {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS file_transitions (
                id TEXT PRIMARY KEY,
                file_id TEXT NOT NULL,
                path TEXT NOT NULL,
                from_hash TEXT,
                to_hash TEXT NOT NULL,
                from_size INTEGER,
                to_size INTEGER NOT NULL,
                source TEXT NOT NULL,
                source_detail TEXT,
                plan_id TEXT,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_transitions_file ON file_transitions(file_id);
            CREATE INDEX IF NOT EXISTS idx_transitions_source ON file_transitions(source);",
        )?;
    }
    Ok(())
}

/// Create tables that may be missing from older databases.
fn migrate_missing_tables(conn: &Connection) -> rusqlite::Result<()> {
    let table_missing = |name: &str| -> rusqlite::Result<bool> {
        let exists: bool = conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |row| row.get(0),
        )?;
        Ok(!exists)
    };

    if table_missing("discovered_files")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS discovered_files (
                id TEXT PRIMARY KEY,
                path TEXT NOT NULL UNIQUE,
                size INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                status TEXT NOT NULL DEFAULT 'pending',
                discovered_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_discovered_status ON discovered_files(status);",
        )?;
    }

    if table_missing("health_checks")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS health_checks (
                id TEXT PRIMARY KEY,
                check_name TEXT NOT NULL,
                passed INTEGER NOT NULL,
                details TEXT,
                checked_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_health_checks_name ON health_checks(check_name);
            CREATE INDEX IF NOT EXISTS idx_health_checks_time ON health_checks(checked_at);",
        )?;
    }

    if table_missing("event_log")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS event_log (
                rowid INTEGER PRIMARY KEY AUTOINCREMENT,
                id TEXT NOT NULL UNIQUE,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                summary TEXT NOT NULL,
                created_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_event_log_type ON event_log(event_type);",
        )?;
    }

    if table_missing("subtitles")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS subtitles (
                id INTEGER PRIMARY KEY,
                file_path TEXT NOT NULL,
                subtitle_path TEXT NOT NULL,
                language TEXT NOT NULL,
                forced INTEGER NOT NULL DEFAULT 0,
                title TEXT,
                created_at TEXT NOT NULL DEFAULT (datetime('now')),
                UNIQUE(file_path, subtitle_path)
            );
            CREATE INDEX IF NOT EXISTS idx_subtitles_file ON subtitles(file_path);",
        )?;
    }

    if table_missing("library_snapshots")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS library_snapshots (
                id TEXT PRIMARY KEY,
                captured_at TEXT NOT NULL,
                trigger TEXT NOT NULL,
                total_files INTEGER NOT NULL,
                total_size_bytes INTEGER NOT NULL,
                total_duration_secs REAL NOT NULL,
                snapshot_json TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_snapshots_captured
                ON library_snapshots(captured_at);",
        )?;
    }

    if table_missing("pending_operations")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS pending_operations (
                id TEXT PRIMARY KEY,
                file_path TEXT NOT NULL,
                phase_name TEXT NOT NULL,
                started_at TEXT NOT NULL
            );",
        )?;
    }

    if table_missing("verifications")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS verifications (
                id TEXT PRIMARY KEY,
                file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                verified_at TEXT NOT NULL,
                mode TEXT NOT NULL,
                outcome TEXT NOT NULL,
                error_count INTEGER NOT NULL DEFAULT 0,
                warning_count INTEGER NOT NULL DEFAULT 0,
                content_hash TEXT,
                details TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_verifications_file ON verifications(file_id);
            CREATE INDEX IF NOT EXISTS idx_verifications_outcome ON verifications(outcome);
            CREATE INDEX IF NOT EXISTS idx_verifications_time ON verifications(verified_at);
            CREATE INDEX IF NOT EXISTS idx_verifications_file_verified_at \
                ON verifications(file_id, verified_at);",
        )?;
    }

    if table_missing("transcode_outcomes")? {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS transcode_outcomes (
                id TEXT PRIMARY KEY,
                file_id TEXT NOT NULL REFERENCES files(id),
                target_vmaf INTEGER,
                achieved_vmaf REAL,
                crf_used INTEGER,
                bitrate_used TEXT,
                iterations INTEGER NOT NULL,
                sample_strategy TEXT NOT NULL,
                fallback_used INTEGER NOT NULL,
                completed_at TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_transcode_outcomes_file_completed
                ON transcode_outcomes(file_id, completed_at DESC);",
        )?;
    }

    Ok(())
}

/// Create missing indexes and add constraints to existing tables.
fn migrate_indexes_and_constraints(conn: &Connection) -> rusqlite::Result<()> {
    let has_index = |name: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name=?1",
            [name],
            |row| row.get(0),
        )
    };

    if table_exists(conn, "tracks")? && !has_index("idx_tracks_type")? {
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_tracks_type ON tracks(track_type);")?;
    }

    if table_exists(conn, "transcode_outcomes")?
        && !has_index("idx_transcode_outcomes_file_completed")?
    {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_transcode_outcomes_file_completed
                ON transcode_outcomes(file_id, completed_at DESC);",
        )?;
    }

    if !has_index("idx_files_superseded_by")? {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_files_superseded_by \
             ON files(superseded_by);",
        )?;
    }

    if !has_index("idx_files_superseded_by_unique")? {
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_files_superseded_by_unique \
             ON files(superseded_by) WHERE superseded_by IS NOT NULL;",
        )?;
    }

    // Add UNIQUE constraint on subtitles(file_path, subtitle_path) for existing
    // databases that created the table before the constraint was added.
    let has_subtitles: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='subtitles'",
        [],
        |row| row.get(0),
    )?;
    if has_subtitles && !has_index("idx_subtitles_unique")? {
        conn.execute_batch(
            "DELETE FROM subtitles WHERE id NOT IN (
                SELECT MIN(id) FROM subtitles GROUP BY file_path, subtitle_path
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_subtitles_unique \
                ON subtitles(file_path, subtitle_path);",
        )?;
    }

    let jobs_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='jobs'",
        [],
        |row| row.get(0),
    )?;
    if jobs_exists && !has_index("idx_jobs_completed_at")? {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_jobs_completed_at ON jobs(completed_at);",
        )?;
    }

    let event_log_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='event_log'",
        [],
        |row| row.get(0),
    )?;
    if event_log_exists && !has_index("idx_event_log_created_at")? {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_event_log_created_at ON event_log(created_at);",
        )?;
    }

    let transitions_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='file_transitions'",
        [],
        |row| row.get(0),
    )?;
    if transitions_exists && !has_index("idx_transitions_created_at")? {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_transitions_created_at \
             ON file_transitions(created_at);",
        )?;
    }

    let verifications_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='verifications'",
        [],
        |row| row.get(0),
    )?;
    if verifications_exists && !has_index("idx_verifications_file_verified_at")? {
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_verifications_file_verified_at \
             ON verifications(file_id, verified_at);",
        )?;
    }

    Ok(())
}

/// Add processing-stats columns to `file_transitions` and drop legacy
/// `processing_stats` table.
fn migrate_processing_stats_into_transitions(
    conn: &Connection,
    has_column: &dyn Fn(&str, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    let columns = [
        "duration_ms INTEGER",
        "actions_taken INTEGER",
        "tracks_modified INTEGER",
        "outcome TEXT",
        "policy_name TEXT",
        "phase_name TEXT",
    ];
    for col_def in &columns {
        let col_name = col_def.split_whitespace().next().unwrap_or(col_def);
        if !has_column("file_transitions", col_name)? {
            conn.execute_batch(&format!(
                "ALTER TABLE file_transitions ADD COLUMN {col_def};"
            ))?;
        }
    }
    // Drop the legacy table if it exists.
    conn.execute_batch("DROP TABLE IF EXISTS processing_stats")?;
    Ok(())
}

/// Add `metadata_snapshot` column to `file_transitions`.
fn migrate_metadata_snapshot_column(
    conn: &Connection,
    has_column: &dyn Fn(&str, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    if !has_column("file_transitions", "metadata_snapshot")? {
        conn.execute_batch("ALTER TABLE file_transitions ADD COLUMN metadata_snapshot TEXT;")?;
    }
    Ok(())
}

/// Add `error_message` and `session_id` columns to `file_transitions`, and
/// `session_id` to plans, for execution output capture.
fn migrate_execution_capture_columns(
    conn: &Connection,
    has_column: &dyn Fn(&str, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    let table_exists = |name: &str| -> rusqlite::Result<bool> {
        conn.query_row(
            "SELECT COUNT(*) > 0 FROM sqlite_master \
             WHERE type='table' AND name=?1",
            [name],
            |row| row.get(0),
        )
    };

    if table_exists("file_transitions")? {
        if !has_column("file_transitions", "error_message")? {
            conn.execute_batch("ALTER TABLE file_transitions ADD COLUMN error_message TEXT;")?;
        }
        if !has_column("file_transitions", "session_id")? {
            conn.execute_batch("ALTER TABLE file_transitions ADD COLUMN session_id TEXT;")?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_transitions_session \
             ON file_transitions(session_id);",
        )?;
    }

    if table_exists("plans")? {
        if !has_column("plans", "session_id")? {
            conn.execute_batch("ALTER TABLE plans ADD COLUMN session_id TEXT;")?;
        }
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_plans_session \
             ON plans(session_id);",
        )?;
    }

    Ok(())
}

/// Reclassify cover-art / thumbnail tracks misclassified as primary video by
/// older introspector versions. See issue #156.
///
/// - `png`/`bmp`/`gif`/`webp` rows are always image-only in a film library.
/// - `mjpeg` rows are reclassified only when a sibling video track exists for
///   the same file (protects rare genuine motion-mjpeg encodes).
///
/// Idempotent: a clean database has no `track_type='video'` rows with image
/// codecs, so the UPDATE statements affect zero rows.
fn migrate_cover_art_track_types(conn: &Connection) -> rusqlite::Result<()> {
    if !table_exists(conn, "tracks")? {
        return Ok(());
    }

    // Cheap pre-check using idx_tracks_type: skip the UPDATE work entirely once
    // a database has been migrated. Avoids re-evaluating the correlated mjpeg
    // subquery on every startup.
    let has_candidates: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tracks \
         WHERE track_type = 'video' \
         AND codec IN ('png', 'bmp', 'gif', 'webp', 'mjpeg'))",
        [],
        |row| row.get(0),
    )?;
    if !has_candidates {
        return Ok(());
    }

    conn.execute_batch(
        "UPDATE tracks SET track_type = 'attachment' \
         WHERE track_type = 'video' \
         AND codec IN ('png', 'bmp', 'gif', 'webp');",
    )?;

    conn.execute_batch(
        "UPDATE tracks SET track_type = 'attachment' \
         WHERE track_type = 'video' \
         AND codec = 'mjpeg' \
         AND file_id IN ( \
             SELECT file_id FROM tracks \
             WHERE track_type = 'video' \
             GROUP BY file_id \
             HAVING COUNT(*) > 1 \
         );",
    )?;

    Ok(())
}

/// Add `from_path` column to `file_transitions` and create the path-lookup
/// indexes that the OR-match in `transitions_for_path` relies on.
fn migrate_from_path_column(
    conn: &Connection,
    has_column: &dyn Fn(&str, &str) -> rusqlite::Result<bool>,
) -> rusqlite::Result<()> {
    if !has_column("file_transitions", "from_path")? {
        conn.execute_batch("ALTER TABLE file_transitions ADD COLUMN from_path TEXT;")?;
    }
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_transitions_path \
         ON file_transitions(path); \
         CREATE INDEX IF NOT EXISTS idx_transitions_from_path \
         ON file_transitions(from_path);",
    )?;
    Ok(())
}

/// Configure `SQLite` connection for optimal performance.
pub fn configure_connection(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA synchronous = NORMAL;
         PRAGMA cache_size = -8000;",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_schema_creation() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        create_schema(&conn).unwrap();

        // Verify tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();

        assert!(tables.contains(&"files".to_string()));
        assert!(tables.contains(&"tracks".to_string()));
        assert!(tables.contains(&"jobs".to_string()));
        assert!(tables.contains(&"plans".to_string()));
        assert!(!tables.contains(&"processing_stats".to_string()));
        assert!(tables.contains(&"plugin_data".to_string()));
        assert!(tables.contains(&"file_transitions".to_string()));
        assert!(tables.contains(&"bad_files".to_string()));
        assert!(tables.contains(&"discovered_files".to_string()));
        assert!(tables.contains(&"health_checks".to_string()));
        assert!(tables.contains(&"event_log".to_string()));
        assert!(tables.contains(&"subtitles".to_string()));
        assert!(tables.contains(&"library_snapshots".to_string()));
        assert!(tables.contains(&"pending_operations".to_string()));
        assert!(tables.contains(&"transcode_outcomes".to_string()));
    }

    #[test]
    fn test_schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        create_schema(&conn).unwrap();
        // Should not error on second call
        create_schema(&conn).unwrap();
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        let fk: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn test_wal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.db");
        let conn = Connection::open(&path).unwrap();
        configure_connection(&conn).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }

    #[test]
    fn test_fresh_schema_has_superseded_by_index() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        create_schema(&conn).unwrap();

        let has_index: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' \
                 AND name='idx_files_superseded_by'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            has_index,
            "fresh database should have idx_files_superseded_by"
        );
    }

    #[test]
    fn test_superseded_by_unique_constraint() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        create_schema(&conn).unwrap();

        // Insert two files
        conn.execute_batch(
            "INSERT INTO files (id, path, filename, size, content_hash, \
             container, introspected_at, created_at, updated_at) \
             VALUES ('aaa', '/a.mkv', 'a.mkv', 100, 'h1', 'mkv', \
                     '2026-01-01', '2026-01-01', '2026-01-01');
             INSERT INTO files (id, path, filename, size, content_hash, \
             container, introspected_at, created_at, updated_at) \
             VALUES ('bbb', '/b.mkv', 'b.mkv', 100, 'h2', 'mkv', \
                     '2026-01-01', '2026-01-01', '2026-01-01');",
        )
        .unwrap();

        // First file superseded by 'ccc' — should succeed
        conn.execute(
            "UPDATE files SET superseded_by = 'ccc' WHERE id = 'aaa'",
            [],
        )
        .unwrap();

        // Second file also superseded by 'ccc' — should fail (UNIQUE violation)
        let result = conn.execute(
            "UPDATE files SET superseded_by = 'ccc' WHERE id = 'bbb'",
            [],
        );
        assert!(
            result.is_err(),
            "duplicate superseded_by should violate UNIQUE constraint"
        );

        // Multiple NULLs are allowed (only non-NULL values are unique)
        conn.execute("UPDATE files SET superseded_by = NULL WHERE id = 'aaa'", [])
            .unwrap();
    }

    #[test]
    fn test_migrate_adds_superseded_by_column() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        // Create schema WITHOUT superseded_by to simulate old DB
        conn.execute_batch(
            "CREATE TABLE files (
                id TEXT PRIMARY KEY,
                path TEXT UNIQUE,
                filename TEXT NOT NULL,
                size INTEGER NOT NULL,
                content_hash TEXT NOT NULL,
                expected_hash TEXT,
                status TEXT NOT NULL DEFAULT 'active',
                missing_since TEXT,
                container TEXT NOT NULL,
                duration REAL,
                bitrate INTEGER,
                tags TEXT,
                plugin_metadata TEXT,
                introspected_at TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            )",
        )
        .unwrap();

        // Run migration
        migrate(&conn).unwrap();

        // Verify superseded_by column exists and is nullable
        conn.execute(
            "UPDATE files SET superseded_by = 'test-uuid' WHERE id = 'nonexistent'",
            [],
        )
        .unwrap();

        // Verify index exists
        let has_index: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='index' AND name='idx_files_superseded_by'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(has_index, "idx_files_superseded_by index should exist");
    }

    #[test]
    fn migration_creates_retention_indexes() {
        let conn = Connection::open_in_memory().unwrap();
        configure_connection(&conn).unwrap();
        create_schema(&conn).unwrap();

        let names: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name LIKE 'idx_%'")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();

        for required in &[
            "idx_jobs_completed_at",
            "idx_event_log_created_at",
            "idx_transitions_created_at",
        ] {
            assert!(
                names.iter().any(|n| n == required),
                "missing index {required}; got {names:?}"
            );
        }
    }

    #[test]
    fn migration_reclassifies_cover_art_tracks() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();

        // file_a: real h264 + mjpeg cover (mjpeg should be reclassified)
        conn.execute_batch(
            "INSERT INTO files (id, path, filename, size, content_hash, container, introspected_at, created_at, updated_at) \
             VALUES ('file_a', '/a.mkv', 'a.mkv', 0, 'h', 'mkv', '2026-01-01', '2026-01-01', '2026-01-01'); \
             INSERT INTO tracks (id, file_id, stream_index, track_type, codec) \
             VALUES ('t_a0', 'file_a', 0, 'video', 'h264'), \
                    ('t_a1', 'file_a', 1, 'video', 'mjpeg');",
        ).unwrap();

        // file_b: lone mjpeg track (real motion encode — must NOT be reclassified)
        conn.execute_batch(
            "INSERT INTO files (id, path, filename, size, content_hash, container, introspected_at, created_at, updated_at) \
             VALUES ('file_b', '/b.mkv', 'b.mkv', 0, 'h', 'mkv', '2026-01-01', '2026-01-01', '2026-01-01'); \
             INSERT INTO tracks (id, file_id, stream_index, track_type, codec) \
             VALUES ('t_b0', 'file_b', 0, 'video', 'mjpeg');",
        ).unwrap();

        // file_c: hevc + png cover (png ALWAYS reclassified)
        conn.execute_batch(
            "INSERT INTO files (id, path, filename, size, content_hash, container, introspected_at, created_at, updated_at) \
             VALUES ('file_c', '/c.mkv', 'c.mkv', 0, 'h', 'mkv', '2026-01-01', '2026-01-01', '2026-01-01'); \
             INSERT INTO tracks (id, file_id, stream_index, track_type, codec) \
             VALUES ('t_c0', 'file_c', 0, 'video', 'hevc'), \
                    ('t_c1', 'file_c', 1, 'video', 'png');",
        ).unwrap();

        migrate_cover_art_track_types(&conn).unwrap();

        let track_type = |id: &str| -> String {
            conn.query_row("SELECT track_type FROM tracks WHERE id = ?", [id], |row| {
                row.get(0)
            })
            .unwrap()
        };
        assert_eq!(track_type("t_a0"), "video");
        assert_eq!(
            track_type("t_a1"),
            "attachment",
            "mjpeg paired with real video reclassified"
        );
        assert_eq!(track_type("t_b0"), "video", "lone mjpeg preserved");
        assert_eq!(track_type("t_c0"), "video");
        assert_eq!(track_type("t_c1"), "attachment", "png always reclassified");

        // Re-running is a no-op.
        migrate_cover_art_track_types(&conn).unwrap();
        assert_eq!(track_type("t_a1"), "attachment");
    }

    #[test]
    fn verifications_table_is_created() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='verifications'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn verifications_indexes_are_created() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND tbl_name='verifications'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(
            count >= 3,
            "expected at least 3 indexes on verifications, got {count}"
        );
    }

    #[test]
    fn verification_latest_lookup_index_is_created() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type='index' AND name='idx_verifications_file_verified_at'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(exists);
    }

    #[test]
    fn transcode_outcomes_table_and_index_are_created() {
        let conn = Connection::open_in_memory().unwrap();
        create_schema(&conn).unwrap();
        let table_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type='table' AND name='transcode_outcomes'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let index_exists: bool = conn
            .query_row(
                "SELECT COUNT(*) > 0 FROM sqlite_master \
                 WHERE type='index' AND name='idx_transcode_outcomes_file_completed'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(table_exists);
        assert!(index_exists);
    }
}
