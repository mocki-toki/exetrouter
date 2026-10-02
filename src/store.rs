use crate::Result;
use rusqlite::{Connection, Transaction, TransactionBehavior};
use std::{io, path::PathBuf, thread, time::Duration};
use tokio::sync::{mpsc, oneshot};

const MIGRATIONS: &[&str] = &[
    include_str!("migrations/001_initial.sql"),
    include_str!("migrations/002_upstream.sql"),
    include_str!("migrations/003_quota.sql"),
    include_str!("migrations/004_context_affinity.sql"),
    include_str!("migrations/005_health.sql"),
    include_str!("migrations/006_model_metadata.sql"),
    include_str!("migrations/007_account_preferences.sql"),
];
pub const SCHEMA_VERSION: usize = MIGRATIONS.len();
const QUEUE_CAPACITY: usize = 32;

/// Version zero includes databases created by the original prototype.
pub fn init(conn: &Connection) -> Result<()> {
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL;")?;
    let tx = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)?;
    let version: usize = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if version > MIGRATIONS.len() {
        return Err("database schema is newer than this binary".into());
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(version) {
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", index + 1)?;
    }
    tx.commit()?;
    Ok(())
}

type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// One connection owned exclusively by a dedicated thread. Transactions are
/// submitted as a single job; awaiting a result never blocks a Tokio worker.
#[derive(Clone)]
pub struct Database {
    sender: mpsc::Sender<Job>,
}

impl Database {
    pub async fn open(path: PathBuf) -> Result<Self> {
        Self::start(move || Ok(Connection::open(path)?)).await
    }

    async fn start(open: impl FnOnce() -> Result<Connection> + Send + 'static) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel::<Job>(QUEUE_CAPACITY);
        let (ready, initialized) = oneshot::channel();
        thread::Builder::new()
            .name("exrd-db".into())
            .spawn(move || {
                let conn = open().and_then(|conn| {
                    init(&conn)?;
                    Ok(conn)
                });
                match conn {
                    Ok(mut conn) => {
                        if ready.send(Ok(())).is_err() {
                            return;
                        }
                        while let Some(job) = receiver.blocking_recv() {
                            job(&mut conn);
                        }
                    }
                    Err(err) => {
                        let _ = ready.send(Err(err));
                    }
                }
            })?;
        initialized
            .await
            .map_err(|_| "database worker stopped during startup")??;
        Ok(Self { sender })
    }

    pub async fn call<T: Send + 'static>(
        &self,
        operation: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (reply, result) = oneshot::channel();
        self.sender
            .try_send(Box::new(move |conn| {
                // Accepted mutations finish even if the caller disconnects.
                let _ = reply.send(operation(conn));
            }))
            .map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => {
                    io::Error::new(io::ErrorKind::WouldBlock, "database queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    io::Error::new(io::ErrorKind::BrokenPipe, "database worker is unavailable")
                }
            })?;
        result.await.map_err(|_| "database worker stopped")?
    }

    /// Wait for all previously accepted operations before stopping the service.
    pub async fn drain(&self) -> Result<()> {
        let (reply, result) = oneshot::channel();
        self.sender
            .send(Box::new(move |_| {
                let _ = reply.send(());
            }))
            .await
            .map_err(|_| "database worker unavailable")?;
        result.await.map_err(|_| "database worker stopped")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{authenticate, create_token, create_user};

    #[test]
    fn migration_adopts_legacy_database_without_losing_tokens() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        let user = create_user(&conn, "alice").unwrap();
        let token = create_token(&conn, &[1; 32], user, "laptop", 90).unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap();
        assert_eq!(
            authenticate(&conn, &[1; 32], &token.secret)
                .unwrap()
                .unwrap()
                .0,
            user
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            MIGRATIONS.len() as i64
        );
        assert_eq!(
            conn.query_row("PRAGMA foreign_keys", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }

    #[test]
    fn quota_migration_preserves_existing_oauth_catalog_and_tokens() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.execute_batch(MIGRATIONS[1]).unwrap();
        conn.pragma_update(None, "user_version", 2).unwrap();
        conn.execute("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at) VALUES('fixture','active',X'1234',9999,1000)",[]).unwrap();
        conn.execute(
            "INSERT INTO oauth_models(account_id,model,display_name) VALUES(1,'gpt-test','Test')",
            [],
        )
        .unwrap();
        let user = create_user(&conn, "alice").unwrap();
        let token = create_token(&conn, &[1; 32], user, "test", 1).unwrap();
        init(&conn).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT hex(encrypted_credentials) FROM oauth_accounts",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "1234"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM oauth_models", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert!(authenticate(&conn, &[1; 32], &token.secret)
            .unwrap()
            .is_some());
        assert_eq!(
            crate::quota::summary(&conn, 1, 1000).unwrap().status,
            "unknown"
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            MIGRATIONS.len() as i64
        );
    }

    #[test]
    fn context_migration_preserves_quota_and_usage_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..3] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 3).unwrap();
        conn.execute_batch("INSERT INTO users VALUES(1,'alice',0); INSERT INTO access_tokens(id,user_id,name,digest,created_at,expires_at) VALUES('fixture',1,'fixture',X'1234',0,99999); INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at,cooldown_until) VALUES('fixture','active',X'5678',9999,0,1234); INSERT INTO oauth_quota_windows VALUES(1,'primary',75,300,2000,1000,1); INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status,input_tokens) VALUES(1000,1,'fixture','responses','gpt-test','completed',10);").unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            MIGRATIONS.len() as i64
        );
        assert_eq!(
            conn.query_row("SELECT cooldown_until FROM oauth_accounts", [], |row| row
                .get::<_, i64>(
                0
            ))
            .unwrap(),
            1234
        );
        assert_eq!(
            conn.query_row("SELECT used_percent FROM oauth_quota_windows", [], |row| {
                row.get::<_, f64>(0)
            })
            .unwrap(),
            75.0
        );
        assert_eq!(
            conn.query_row("SELECT input_tokens FROM usage_events", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            10
        );
        assert_eq!(
            conn.query_row(
                "SELECT hex(encrypted_credentials) FROM oauth_accounts",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "5678"
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM context_bindings", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }

    #[test]
    fn migration_rolls_back_and_rejects_future_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE VIEW access_tokens AS SELECT 1;")
            .unwrap();
        assert!(init(&conn).is_err());
        let tables: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(tables, 0);
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        conn.pragma_update(None, "user_version", 99).unwrap();
        assert!(init(&conn).unwrap_err().to_string().contains("newer"));
    }

    #[test]
    fn health_migration_preserves_context_quota_credentials_and_usage() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..4] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 4).unwrap();
        conn.execute_batch("INSERT INTO users VALUES(1,'alice',0); INSERT INTO access_tokens(id,user_id,name,digest,created_at,expires_at) VALUES('fixture',1,'fixture',X'1234',0,99999); INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at,cooldown_until) VALUES('fixture','active',X'5678',9999,0,1234); INSERT INTO oauth_quota_windows VALUES(1,'primary',75,300,2000,1000,1); INSERT INTO usage_events(at_utc,user_id,token_id,api_surface,model,status,input_tokens) VALUES(1000,1,'fixture','responses','gpt-test','completed',10); INSERT INTO context_bindings VALUES(1,zeroblob(32),1,90000);").unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap();
        let preserved=conn.query_row("SELECT (SELECT hex(encrypted_credentials) FROM oauth_accounts),(SELECT cooldown_until FROM oauth_accounts),(SELECT used_percent FROM oauth_quota_windows),(SELECT input_tokens FROM usage_events),(SELECT expires_at FROM context_bindings)",[],|row|Ok((row.get::<_,String>(0)?,row.get::<_,i64>(1)?,row.get::<_,f64>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?))).unwrap();
        assert_eq!(preserved, ("5678".into(), 1234, 75.0, 10, 90000));
        assert_eq!(crate::health::next(&conn).unwrap(), 1);
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, usize>(0))
                .unwrap(),
            MIGRATIONS.len()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn slow_database_job_does_not_block_runtime() {
        let db = Database::start(|| Ok(Connection::open_in_memory()?))
            .await
            .unwrap();
        let (started, wait_started) = oneshot::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let job = tokio::spawn(async move {
            db.call(move |_| {
                started.send(()).unwrap();
                wait_release.recv_timeout(Duration::from_secs(2))?;
                Ok(thread::current().name().unwrap().to_owned())
            })
            .await
            .unwrap()
        });
        wait_started.await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        release.send(()).unwrap();
        assert_eq!(job.await.unwrap(), "exrd-db");
    }

    #[test]
    fn metadata_migration_invalidates_id_only_catalog_without_losing_account_state() {
        let conn = Connection::open_in_memory().unwrap();
        for sql in &MIGRATIONS[..5] {
            conn.execute_batch(sql).unwrap();
        }
        conn.pragma_update(None, "user_version", 5).unwrap();
        conn.execute_batch("INSERT INTO oauth_accounts(account_id,state,encrypted_credentials,expires_at,created_at,catalog_updated_at,generation,cooldown_until) VALUES('fixture','active',X'1234',9999,1000,1000,3,7000); INSERT INTO oauth_models VALUES(1,'gpt-test','Test'); INSERT INTO oauth_health VALUES(1,'responses',3,1,9000,'transport',1);").unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap();
        let saved=conn.query_row("SELECT encrypted_credentials,generation,cooldown_until,catalog_updated_at FROM oauth_accounts",[],|row|Ok((row.get::<_,Vec<u8>>(0)?,row.get::<_,i64>(1)?,row.get::<_,i64>(2)?,row.get::<_,Option<i64>>(3)?))).unwrap();
        assert_eq!(saved, (vec![0x12, 0x34], 3, 7000, None));
        assert_eq!(
            conn.query_row("SELECT model,metadata FROM oauth_models", [], |row| Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?
            )))
            .unwrap(),
            ("gpt-test".into(), None)
        );
        assert_eq!(
            conn.query_row("SELECT retry_at FROM oauth_health", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            9000
        );
    }

    #[tokio::test]
    async fn saturated_queue_rejects_jobs_without_executing_them() {
        let db = Database::start(|| Ok(Connection::open_in_memory()?))
            .await
            .unwrap();
        let (started, wait_started) = oneshot::channel();
        let (release, wait_release) = std::sync::mpsc::channel();
        let busy = db.clone();
        let job = tokio::spawn(async move {
            busy.call(move |_| {
                started.send(()).unwrap();
                wait_release.recv_timeout(Duration::from_secs(2))?;
                Ok(())
            })
            .await
            .unwrap();
        });
        wait_started.await.unwrap();
        for _ in 0..QUEUE_CAPACITY {
            db.sender.try_send(Box::new(|_| {})).unwrap();
        }
        let result: Result<()> = db.call(|_| panic!("rejected job executed")).await;
        assert_eq!(
            result
                .unwrap_err()
                .downcast_ref::<io::Error>()
                .unwrap()
                .kind(),
            io::ErrorKind::WouldBlock
        );
        release.send(()).unwrap();
        job.await.unwrap();
        db.drain().await.unwrap();
        db.call(|_| Ok(())).await.unwrap();
    }
}
