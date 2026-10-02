//! Local snapshots of state and its matching keys. Never sends credentials.
use crate::{oauth, store::SCHEMA_VERSION, Result};
use rusqlite::{
    backup::{Backup, StepResult},
    Connection, OpenFlags,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const FILES: [&str; 3] = [
    "exetrouter.sqlite",
    "exetrouter.key",
    "exetrouter.oauth.key",
];
const MANIFEST: &str = "backup.json";
const FORMAT: u32 = 1;

pub struct Paths {
    pub db: PathBuf,
    pub key: PathBuf,
    pub oauth_key: PathBuf,
}
impl Paths {
    pub fn in_directory(directory: &Path) -> Self {
        Self {
            db: directory.join(FILES[0]),
            key: directory.join(FILES[1]),
            oauth_key: directory.join(FILES[2]),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileRecord {
    bytes: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format_version: u32,
    schema_version: usize,
    created_at_utc: i64,
    files: BTreeMap<String, FileRecord>,
}
#[derive(Serialize)]
pub struct Report {
    pub format_version: u32,
    pub schema_version: usize,
    pub created_at_utc: i64,
    pub users: i64,
    pub oauth_accounts: i64,
    pub usage_events: i64,
    pub pending_requests: i64,
}

fn private_file(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("state files must be regular, private and owned by the current user".into());
    }
    Ok(file)
}
fn private_directory(path: &Path) -> Result<File> {
    let dir = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)?;
    let meta = dir.metadata()?;
    if meta.mode() & 0o077 != 0 || meta.uid() != unsafe { libc::geteuid() } {
        return Err("backup directory must be private and owned by the current user".into());
    }
    Ok(dir)
}
fn read_key(path: &Path) -> Result<[u8; 32]> {
    let mut bytes = Vec::new();
    private_file(path)?.take(33).read_to_end(&mut bytes)?;
    bytes
        .try_into()
        .map_err(|_| "state key must contain exactly 32 bytes".into())
}
fn keys(paths: &Paths) -> Result<([u8; 32], [u8; 32])> {
    let key = read_key(&paths.key)?;
    let oauth = read_key(&paths.oauth_key)?;
    if key == oauth {
        return Err("state keys must differ".into());
    }
    Ok((key, oauth))
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

struct NewDirectory {
    path: PathBuf,
    handle: File,
    committed: bool,
}
impl NewDirectory {
    fn create(path: &Path) -> Result<Self> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent = fs::canonicalize(parent)?;
        let meta = parent.metadata()?;
        if meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o022 != 0 {
            return Err(
                "destination parent must be owned by the current user and not writable by others"
                    .into(),
            );
        }
        let name = path
            .file_name()
            .ok_or("destination must name a new directory")?;
        let path = parent.join(name);
        fs::DirBuilder::new().mode(0o700).create(&path)?;
        let handle = private_directory(&path)?;
        Ok(Self {
            path,
            handle,
            committed: false,
        })
    }
    fn commit(mut self) -> Result<()> {
        self.handle.sync_all()?;
        File::open(self.path.parent().ok_or("destination parent missing")?)?.sync_all()?;
        self.committed = true;
        Ok(())
    }
}
impl Drop for NewDirectory {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        if let (Ok(owned), Ok(current)) = (self.handle.metadata(), fs::symlink_metadata(&self.path))
        {
            if owned.dev() == current.dev()
                && owned.ino() == current.ino()
                && current.is_dir()
                && fs::remove_dir_all(&self.path).is_err()
            {
                tracing::error!(event = "state_snapshot_cleanup_failed");
            }
        }
    }
}

fn record(path: &Path) -> Result<FileRecord> {
    let mut file = private_file(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    let mut bytes = 0;
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        hash.update(&buffer[..n]);
    }
    Ok(FileRecord {
        bytes,
        sha256: hex::encode(hash.finalize()),
    })
}
fn readonly(path: &Path) -> Result<Connection> {
    let _ = private_file(path)?;
    // SQLite NOFOLLOW checks the full path. Resolve parent aliases such as
    // macOS /var -> /private/var after rejecting a symlink at the file itself.
    let canonical = fs::canonicalize(path)?;
    let conn = Connection::open_with_flags(
        canonical,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.busy_timeout(Duration::from_secs(1))?;
    Ok(conn)
}
fn inspect(conn: &Connection, oauth_key: [u8; 32], created_at: i64) -> Result<Report> {
    let schema: usize = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if schema != SCHEMA_VERSION {
        return Err("snapshot requires the current database schema".into());
    }
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" || conn.prepare("PRAGMA foreign_key_check")?.exists([])? {
        return Err("snapshot database integrity check failed".into());
    }
    let vault = oauth::Vault::new(oauth_key);
    for account in oauth::list(conn)? {
        oauth::load(conn, &vault, account.id)
            .map_err(|_| "snapshot OAuth credentials do not match the encryption key")?;
    }
    let (users,accounts,usage,pending) = conn.query_row("SELECT (SELECT COUNT(*) FROM users),(SELECT COUNT(*) FROM oauth_accounts),(SELECT COUNT(*) FROM usage_events),(SELECT COUNT(*) FROM usage_events WHERE status IN ('accepted','sent'))", [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
    Ok(Report {
        format_version: FORMAT,
        schema_version: schema,
        created_at_utc: created_at,
        users,
        oauth_accounts: accounts,
        usage_events: usage,
        pending_requests: pending,
    })
}

/// SQLite's backup API includes committed WAL content without copying sidecars.
/// A completed manifest is published only after data, keys and checks pass.
pub fn create(source: &Paths, destination: &Path) -> Result<Report> {
    let (key, oauth_key) = keys(source)?;
    let source_conn = readonly(&source.db)?;
    let directory = NewDirectory::create(destination)?;
    let paths = Paths::in_directory(&directory.path);
    write_private(&paths.db, &[])?;
    let mut conn = Connection::open(&paths.db)?;
    {
        let backup = Backup::new(&source_conn, &mut conn)?;
        let started = Instant::now();
        loop {
            if started.elapsed() > Duration::from_secs(120) {
                return Err("SQLite snapshot timed out; incomplete destination removed".into());
            }
            match backup.step(256)? {
                StepResult::Done => break,
                StepResult::More | StepResult::Busy | StepResult::Locked => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                _ => return Err("unexpected SQLite backup state".into()),
            }
        }
    }
    // A standalone snapshot must not depend on an external WAL file.
    conn.pragma_update(None, "journal_mode", "DELETE")?;
    let created_at = chrono::Utc::now().timestamp();
    let report = inspect(&conn, oauth_key, created_at)?;
    conn.close().map_err(|(_, error)| error)?;
    private_file(&paths.db)?.sync_all()?;
    if keys(source)? != (key, oauth_key) {
        return Err("state keys changed while creating the snapshot".into());
    }
    write_private(&paths.key, &key)?;
    write_private(&paths.oauth_key, &oauth_key)?;
    let files = FILES
        .into_iter()
        .map(|name| Ok((name.into(), record(&directory.path.join(name))?)))
        .collect::<Result<BTreeMap<_, _>>>()?;
    directory.handle.sync_all()?;
    let manifest = Manifest {
        format_version: FORMAT,
        schema_version: SCHEMA_VERSION,
        created_at_utc: created_at,
        files,
    };
    write_private(
        &directory.path.join(MANIFEST),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    directory.commit()?;
    Ok(report)
}

fn manifest(directory: &Path) -> Result<Manifest> {
    let _ = private_directory(directory)?;
    let mut bytes = Vec::new();
    private_file(&directory.join(MANIFEST))?
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err("snapshot manifest is too large".into());
    }
    let manifest: Manifest =
        serde_json::from_slice(&bytes).map_err(|_| "invalid snapshot manifest")?;
    if manifest.format_version != FORMAT
        || manifest.schema_version != SCHEMA_VERSION
        || manifest.files.len() != FILES.len()
        || FILES.iter().any(|name| !manifest.files.contains_key(*name))
    {
        return Err("unsupported or incomplete snapshot manifest".into());
    }
    Ok(manifest)
}
fn verify_files(directory: &Path, manifest: &Manifest) -> Result<()> {
    for name in FILES {
        let actual = record(&directory.join(name))?;
        let expected = &manifest.files[name];
        if actual.bytes != expected.bytes || actual.sha256 != expected.sha256 {
            return Err("snapshot file checksum mismatch".into());
        }
    }
    // Sidecars would make the hashed database differ from SQLite's read view.
    for suffix in ["-wal", "-shm", "-journal"] {
        if fs::symlink_metadata(directory.join(format!("{}{suffix}", FILES[0]))).is_ok() {
            return Err("snapshot must be a standalone database without sidecars".into());
        }
    }
    Ok(())
}
pub fn verify(directory: &Path) -> Result<Report> {
    let manifest = manifest(directory)?;
    verify_files(directory, &manifest)?;
    let paths = Paths::in_directory(directory);
    let (_, oauth_key) = keys(&paths)?;
    inspect(&readonly(&paths.db)?, oauth_key, manifest.created_at_utc)
}
/// Restores into a newly-created directory; never overwrites a live state.
pub fn restore(source: &Path, destination: &Path) -> Result<Report> {
    let manifest = manifest(source)?;
    let _ = verify(source)?;
    let directory = NewDirectory::create(destination)?;
    for name in FILES {
        let mut input = private_file(&source.join(name))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.path.join(name))?;
        std::io::copy(&mut input, &mut output)?;
        output.sync_all()?;
    }
    verify_files(&directory.path, &manifest)?;
    let paths = Paths::in_directory(&directory.path);
    let (_, oauth_key) = keys(&paths)?;
    let report = inspect(&readonly(&paths.db)?, oauth_key, manifest.created_at_utc)?;
    directory.handle.sync_all()?;
    write_private(
        &directory.path.join("restore.json"),
        &serde_json::to_vec_pretty(&report)?,
    )?;
    directory.commit()?;
    Ok(report)
}
