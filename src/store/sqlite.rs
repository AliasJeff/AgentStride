use std::path::Path;

use rusqlite::{Connection, params};

use crate::domain::generate_digest;

use super::{StoreError, migrations};

pub struct Store {
    pub(crate) connection: Connection,
    workspace_seed: String,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let connection = Connection::open(path)?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(Self {
            connection,
            workspace_seed: path.to_string_lossy().into_owned(),
        })
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        let connection = Connection::open_in_memory()?;
        connection.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA busy_timeout = 5000;",
        )?;
        Ok(Self {
            connection,
            workspace_seed: ":memory:".to_owned(),
        })
    }

    pub fn migrate(&mut self) -> Result<(), StoreError> {
        migrations::apply(&mut self.connection)
    }

    pub fn ensure_workspace_id(&self) -> Result<String, StoreError> {
        let existing: Option<String> = self
            .connection
            .query_row(
                "SELECT value FROM workspace_meta WHERE key = 'workspace_id'",
                [],
                |row| row.get(0),
            )
            .optional()?;

        if let Some(value) = existing {
            return Ok(value);
        }

        let workspace_id =
            generate_digest("workspace", &self.workspace_seed, "agent-stride", "", 0);
        self.connection.execute(
            "INSERT INTO workspace_meta (key, value) VALUES ('workspace_id', ?1)",
            params![workspace_id],
        )?;
        Ok(workspace_id)
    }
}

trait OptionalRow {
    fn optional(self) -> Result<Option<String>, rusqlite::Error>;
}

impl OptionalRow for Result<String, rusqlite::Error> {
    fn optional(self) -> Result<Option<String>, rusqlite::Error> {
        match self {
            Ok(value) => Ok(Some(value)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Store;

    #[test]
    fn workspace_id_is_created_once() {
        let mut store = Store::open_in_memory().expect("open database");
        store.migrate().expect("migrate database");

        let first = store.ensure_workspace_id().expect("create workspace id");
        let second = store.ensure_workspace_id().expect("read workspace id");

        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn different_database_paths_get_different_workspace_ids() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let first_path = directory.path().join("first.db");
        let second_path = directory.path().join("second.db");

        let mut first = Store::open(&first_path).expect("open first database");
        first.migrate().expect("migrate first database");
        let first_id = first.ensure_workspace_id().expect("first workspace id");

        let mut second = Store::open(&second_path).expect("open second database");
        second.migrate().expect("migrate second database");
        let second_id = second.ensure_workspace_id().expect("second workspace id");

        assert_ne!(first_id, second_id);
    }
}
