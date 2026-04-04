use rusqlite::Connection;

/// All SQL statements to create the VOOM schema.
const SCHEMA_SQL: &str = r#"
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
    width INTEGER,
    height INTEGER,
    frame_rate REAL,
    is_vfr INTEGER NOT NULL DEFAULT 0,
    is_hdr INTEGER NOT NULL DEFAULT 0,
    hdr_format TEXT,
    pixel_format TEXT,
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
    result TEXT
);

CREATE TABLE IF NOT EXISTS file_transitions (
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
CREATE INDEX IF NOT EXISTS idx_transitions_source ON file_transitions(source);

CREATE TABLE IF NOT EXISTS processing_stats (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    policy_name TEXT NOT NULL,
    phase_name TEXT NOT NULL,
    outcome TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    actions_taken INTEGER NOT NULL,
    tracks_modified INTEGER NOT NULL,
    file_size_before INTEGER,
    file_size_after INTEGER,
    created_at TEXT NOT NULL
);

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
CREATE INDEX IF NOT EXISTS idx_tracks_file ON tracks(file_id);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status, priority);
CREATE INDEX IF NOT EXISTS idx_plans_file ON plans(file_id);
CREATE INDEX IF NOT EXISTS idx_stats_file ON processing_stats(file_id);
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
"#;

/// Initialize the database schema.
pub fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    migrate(conn)?;
    Ok(())
}

/// Run migrations for existing databases that may lack newer columns/tables.
pub(crate) fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    // Check plans table for new columns
    const KNOWN_TABLES: &[&str] = &[
        "files",
        "tracks",
        "jobs",
        "plans",
        "processing_stats",
        "file_transitions",
        "plugin_data",
        "bad_files",
        "discovered_files",
        "health_checks",
        "event_log",
        "subtitles",
        "library_snapshots",
        "pending_operations",
    ];
    let has_column = |table: &str, column: &str| -> rusqlite::Result<bool> {
        assert!(KNOWN_TABLES.contains(&table), "unknown table: {table}");
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(columns.iter().any(|c| c == column))
    };

    migrate_missing_tables(conn)?;
    migrate_transitions_table(conn)?;
    migrate_plans_columns(conn, &has_column)?;
    migrate_files_columns(conn, &has_column)?;
    migrate_indexes_and_constraints(conn)?;

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
        conn.execute_batch(
            "ALTER TABLE files ADD COLUMN superseded_by TEXT;\
             CREATE INDEX IF NOT EXISTS idx_files_superseded_by ON files(superseded_by);",
        )?;
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

    let tracks_exists: bool = conn.query_row(
        "SELECT COUNT(*) > 0 FROM sqlite_master WHERE type='table' AND name='tracks'",
        [],
        |row| row.get(0),
    )?;
    if tracks_exists && !has_index("idx_tracks_type")? {
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_tracks_type ON tracks(track_type);")?;
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
        assert!(tables.contains(&"processing_stats".to_string()));
        assert!(tables.contains(&"plugin_data".to_string()));
        assert!(tables.contains(&"file_transitions".to_string()));
        assert!(tables.contains(&"bad_files".to_string()));
        assert!(tables.contains(&"discovered_files".to_string()));
        assert!(tables.contains(&"health_checks".to_string()));
        assert!(tables.contains(&"event_log".to_string()));
        assert!(tables.contains(&"subtitles".to_string()));
        assert!(tables.contains(&"library_snapshots".to_string()));
        assert!(tables.contains(&"pending_operations".to_string()));
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
}
