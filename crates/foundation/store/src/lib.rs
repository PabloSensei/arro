//! Application state in SQLite: a small key-value table behind numbered migrations.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension, params};

pub use rusqlite::Error;
pub use trust::TrustList;

mod trust;

/// Applied in order, `PRAGMA user_version` remembers how many.
const MIGRATIONS: &[&str] = &["CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);"];

/// Where the state of `app` lives: `%LOCALAPPDATA%` on Windows, `$XDG_DATA_HOME` or
/// `~/.local/share` on Unix, `~/Library/Application Support` on macOS, the temporary
/// folder as a fallback.
pub fn state_dir(app: &str) -> PathBuf {
    #[cfg(windows)]
    let base = std::env::var_os("LOCALAPPDATA").map_or_else(std::env::temp_dir, PathBuf::from);
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
        .unwrap_or_else(std::env::temp_dir);
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
        .unwrap_or_else(std::env::temp_dir);
    base.join(app)
}

pub struct Store {
    connection: Mutex<Connection>,
}

impl Store {
    /// Opens the database file, creating the folder and the schema when needed.
    pub fn open(path: &Path) -> Result<Self, Error> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        Self::prepare(Connection::open(path)?)
    }

    pub fn in_memory() -> Result<Self, Error> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(mut connection: Connection) -> Result<Self, Error> {
        let applied: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        for (version, migration) in (1..).zip(MIGRATIONS).skip(applied as usize) {
            let transaction = connection.transaction()?;
            transaction.execute_batch(migration)?;
            transaction.pragma_update(None, "user_version", version as i64)?;
            transaction.commit()?;
        }
        Ok(Self {
            connection: Mutex::new(connection),
        })
    }

    pub fn get(&self, key: &str) -> Result<Option<String>, Error> {
        self.connection()
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
    }

    pub fn put(&self, key: &str, value: &str) -> Result<(), Error> {
        self.connection().execute(
            "INSERT INTO kv (key, value) VALUES (?1, ?2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    fn connection(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_and_replaces_values() {
        let store = Store::in_memory().unwrap();
        assert_eq!(store.get("layout").unwrap(), None);
        store.put("layout", "one").unwrap();
        store.put("layout", "two").unwrap();
        assert_eq!(store.get("layout").unwrap().as_deref(), Some("two"));
    }

    #[test]
    fn values_survive_reopening_and_migrations_run_once() {
        let folder = test_support::scratch("store");
        let path = folder.join("nested").join("state.sqlite");
        Store::open(&path).unwrap().put("key", "value").unwrap();
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.get("key").unwrap().as_deref(), Some("value"));
        drop(reopened);
    }
}
