use rusqlite::{Connection, Transaction};

use super::StoreError;

const SCHEMA_VERSION: u32 = 1;
const SCHEMA: &str = include_str!("../../migrations/0001_initial.sql");

pub fn apply(connection: &mut Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
             version    INTEGER PRIMARY KEY NOT NULL,
             applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
         );",
    )?;

    let transaction = connection.transaction()?;
    apply_in_transaction(&transaction)?;
    transaction.commit()?;
    Ok(())
}

fn apply_in_transaction(transaction: &Transaction<'_>) -> Result<(), StoreError> {
    let applied: Option<u32> =
        transaction.query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;

    if applied.is_some() {
        return Ok(());
    }

    if SCHEMA.trim().is_empty() {
        return Err(StoreError::InvalidMigration {
            version: SCHEMA_VERSION,
            reason: "migration is empty".to_owned(),
        });
    }

    transaction.execute_batch(SCHEMA)?;
    transaction.execute(
        "INSERT INTO schema_migrations (version) VALUES (?1)",
        [SCHEMA_VERSION],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::apply;
    use rusqlite::Connection;

    #[test]
    fn migration_is_idempotent() {
        let mut connection = Connection::open_in_memory().expect("open database");

        apply(&mut connection).expect("first migration");
        apply(&mut connection).expect("second migration");

        let version: u32 = connection
            .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
                row.get(0)
            })
            .expect("read schema version");
        assert_eq!(version, 1);
    }
}
