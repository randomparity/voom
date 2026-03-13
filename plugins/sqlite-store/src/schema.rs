use rusqlite::Connection;

/// All SQL statements to create the VOOM schema.
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS files (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    filename TEXT NOT NULL,
    size INTEGER NOT NULL,
    content_hash TEXT NOT NULL,
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
    file_id TEXT NOT NULL REFERENCES files(id),
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

CREATE TABLE IF NOT EXISTS file_history (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL,
    path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    container TEXT NOT NULL,
    track_count INTEGER NOT NULL,
    introspected_at TEXT NOT NULL,
    archived_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_file_history_file ON file_history(file_id);

CREATE TABLE IF NOT EXISTS processing_stats (
    id TEXT PRIMARY KEY,
    file_id TEXT NOT NULL REFERENCES files(id),
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

CREATE INDEX IF NOT EXISTS idx_files_path ON files(path);
CREATE INDEX IF NOT EXISTS idx_files_hash ON files(content_hash);
CREATE INDEX IF NOT EXISTS idx_tracks_file ON tracks(file_id);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status, priority);
CREATE INDEX IF NOT EXISTS idx_plans_file ON plans(file_id);
CREATE INDEX IF NOT EXISTS idx_stats_file ON processing_stats(file_id);
"#;

/// Initialize the database schema.
pub fn create_schema(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA_SQL)?;
    migrate(conn)?;
    Ok(())
}

/// Run migrations for existing databases that may lack newer columns/tables.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    // Check plans table for new columns
    let has_column = |table: &str, column: &str| -> rusqlite::Result<bool> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns: Vec<String> = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .filter_map(|r| r.ok())
            .collect();
        Ok(columns.iter().any(|c| c == column))
    };

    if !has_column("plans", "skip_reason")? {
        conn.execute_batch("ALTER TABLE plans ADD COLUMN skip_reason TEXT")?;
    }
    if !has_column("plans", "policy_hash")? {
        conn.execute_batch("ALTER TABLE plans ADD COLUMN policy_hash TEXT")?;
    }
    if !has_column("plans", "evaluated_at")? {
        conn.execute_batch("ALTER TABLE plans ADD COLUMN evaluated_at TEXT")?;
    }

    // Create file_history table if it doesn't exist
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS file_history (
            id TEXT PRIMARY KEY,
            file_id TEXT NOT NULL,
            path TEXT NOT NULL,
            content_hash TEXT NOT NULL,
            container TEXT NOT NULL,
            track_count INTEGER NOT NULL,
            introspected_at TEXT NOT NULL,
            archived_at TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_file_history_file ON file_history(file_id);",
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
        assert!(tables.contains(&"processing_stats".to_string()));
        assert!(tables.contains(&"plugin_data".to_string()));
        assert!(tables.contains(&"file_history".to_string()));
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
}
