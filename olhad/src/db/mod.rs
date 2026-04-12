pub mod schema;
pub mod queries;

use rusqlite::Connection;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DbError {
    #[error("Database error: {0}")]
    Rusqlite(#[from] rusqlite::Error),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type DbResult<T> = Result<T, DbError>;

/// Initialize the database, creating tables if they don't exist
pub fn init(db_path: &Path) -> DbResult<Connection> {
    // Create parent directories if they don't exist
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(db_path)?;
    schema::init_schema(&conn)?;
    Ok(conn)
}
