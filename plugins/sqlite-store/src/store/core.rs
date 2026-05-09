use std::path::Path;

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

use voom_domain::errors::Result;

use crate::schema;

use super::sql::{other_storage_err, storage_err};

/// Configuration for the `SQLite` store.
pub struct SqliteStoreConfig {
    /// Maximum number of connections in the pool. Default: 8.
    pub pool_size: u32,
}

impl Default for SqliteStoreConfig {
    fn default() -> Self {
        Self { pool_size: 8 }
    }
}

/// SQLite-backed storage implementation using r2d2 connection pooling.
pub struct SqliteStore {
    pub(crate) pool: Pool<SqliteConnectionManager>,
}

impl SqliteStore {
    /// Open (or create) a `SQLite` database at the given path.
    pub fn open(db_path: &Path) -> Result<Self> {
        Self::open_with_config(db_path, SqliteStoreConfig::default())
    }

    /// Open with custom configuration.
    pub fn open_with_config(db_path: &Path, config: SqliteStoreConfig) -> Result<Self> {
        let manager = SqliteConnectionManager::file(db_path);
        Self::from_manager(manager, config.pool_size)
    }

    /// Create an in-memory `SQLite` store (useful for testing).
    pub fn in_memory() -> Result<Self> {
        let manager = SqliteConnectionManager::memory();
        Self::from_manager(manager, SqliteStoreConfig::default().pool_size)
    }

    fn from_manager(manager: SqliteConnectionManager, pool_size: u32) -> Result<Self> {
        let manager = manager.with_init(|conn| schema::configure_connection(conn));

        let pool = Pool::builder()
            .max_size(pool_size)
            .min_idle(Some(0))
            .build(manager)
            .map_err(other_storage_err("failed to create connection pool"))?;

        let conn = pool
            .get()
            .map_err(other_storage_err("failed to get connection"))?;
        schema::create_schema(&conn).map_err(storage_err("failed to create schema"))?;

        Ok(Self { pool })
    }

    pub(crate) fn conn(&self) -> Result<r2d2::PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(other_storage_err("failed to get connection"))
    }
}
