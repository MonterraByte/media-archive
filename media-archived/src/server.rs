use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{routing::get, Router};
use thiserror::Error;

pub fn router() -> Router<Arc<State>> {
    Router::new().route("/", get(root))
}

#[derive(Debug)]
pub struct State {
    #[allow(dead_code)]
    db: sqlx::SqlitePool,
    #[allow(dead_code)]
    path: PathBuf,
}

impl State {
    pub async fn new(path: PathBuf) -> Result<Self, CreateStateError> {
        fs::create_dir_all(&path).map_err(CreateStateError::CreateDir)?;

        let db_path = path.join(".media-archive.sqlite");
        let db_path_str = db_path.to_str().ok_or(CreateStateError::NonUnicodePath)?;
        let db = sqlx::SqlitePool::connect(&format!("sqlite://{}?mode=rwc", db_path_str))
            .await
            .map_err(CreateStateError::DbConnection)?;
        Ok(Self { db, path })
    }
}

#[derive(Debug, Error)]
pub enum CreateStateError {
    #[error("failed to create base directory: {0}")]
    CreateDir(io::Error),
    #[error("base directory path isn't valid Unicode")]
    NonUnicodePath,
    #[error("failed to open database connection: {0}")]
    DbConnection(sqlx::Error),
}

async fn root() -> &'static str {
    "Hello World!"
}
