//! Headless desktop service. Owns metadata and new stores, never source installs.
use anyhow::{Context, Result, ensure};
use playsparse_store::{PackOptions, Progress, Store, directory_allocation, directory_bytes};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub mod product;

static SEQUENCE: AtomicU64 = AtomicU64::new(0);
fn identity(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        now(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    )
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Settings {
    pub theme: String,
    pub storage_dir: PathBuf,
    pub temp_dir: PathBuf,
    pub cache_mib: u64,
    #[serde(default = "default_logs")]
    pub retain_logs: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LaunchDescriptor {
    /// Relative executable in the mounted store; no shell expansion.
    pub executable: String,
    pub args: Vec<String>,
    pub compatibility_confirmed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub state: String,
    pub mountpoint: PathBuf,
    pub overlay: PathBuf,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Game {
    pub id: String,
    pub name: String,
    pub source: PathBuf,
    pub analysis: Option<Value>,
    pub store: Option<PathBuf>,
    pub verified: bool,
    pub store_stats: Option<Value>,
    #[serde(default)]
    pub overlay_allocated_bytes: Option<u64>,
    pub launch: Option<LaunchDescriptor>,
    pub session: Option<Session>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub game_id: String,
    pub operation: Operation,
    pub state: String,
    pub stage: String,
    pub bytes: u64,
    pub files: usize,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub error: Option<String>,
    pub cancellable: bool,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Analyze,
    Optimize,
    Verify,
    Mount,
    Launch,
    Stop,
    Unmount,
    Recover,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Snapshot {
    pub version: u32,
    pub games: Vec<Game>,
    pub jobs: Vec<Job>,
    pub settings: Settings,
}
struct Inner {
    db: Snapshot,
    active: Option<(String, Arc<AtomicBool>)>,
    mounts: BTreeMap<String, Child>,
    processes: BTreeMap<String, Child>,
}
pub struct Service {
    root: PathBuf,
    engine: PathBuf,
    inner: Mutex<Inner>,
    _lock: File,
}

impl Service {
    pub fn open(root: &Path, engine: &Path) -> Result<Arc<Self>> {
        fs::create_dir_all(root)?;
        let root = root.canonicalize()?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(root.join("library.lock"))?;
        lock.try_lock()
            .context("PlaySparse is already using this library")?;
        let path = root.join("library.json");
        let mut db: Snapshot = if path.exists() {
            ensure!(
                fs::metadata(&path)?.len() <= 16 * 1024 * 1024,
                "library exceeds 16 MiB limit"
            );
            serde_json::from_slice(&fs::read(&path)?)
                .context("Library could not be read; retained for recovery")?
        } else {
            Snapshot {
                version: 1,
                games: vec![],
                jobs: vec![],
                settings: Settings {
                    theme: "system".into(),
                    storage_dir: root.join("stores"),
                    temp_dir: std::env::temp_dir(),
                    cache_mib: 256,
                    retain_logs: true,
                },
            }
        };
        ensure!(
            db.version == 1,
            "unsupported library version; retained for recovery"
        );
        for job in &mut db.jobs {
            if job.state == "running" {
                job.state = "interrupted".into();
                job.cancellable = false;
                job.finished_at = Some(now());
                job.error = Some("Application stopped before completion. Retry; temporary engine staging may remain.".into());
            }
        }
        for game in &mut db.games {
            ensure!(
                game.id.starts_with("g-")
                    && game
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-'),
                "Invalid persisted game identity; library retained"
            );
            let source = game
                .source
                .canonicalize()
                .unwrap_or_else(|_| game.source.clone());
            ensure!(
                source.is_absolute() && !overlap(&source, &root),
                "Persisted installation overlaps application data; library retained"
            );
            if !game.source.is_dir() {
                game.error = Some("Original installation is missing. Restore its folder or forget this entry; no files will be deleted.".into());
            }
            game.overlay_allocated_bytes =
                overlay_allocation(&root.join("runtimes").join(&game.id).join("overlay"));
            // Never trust a persisted process ID or pretend a session survived restart.
            if let Some(session) = &mut game.session {
                session.state = "needs_attention".into();
                session.error = Some("Previous session requires inspection. Close game processes before unmounting. No persisted PID is killed.".into());
            }
            if game
                .store
                .as_ref()
                .is_some_and(|p| !p.join("COMMITTED.json").is_file())
            {
                game.verified = false;
                game.error =
                    Some("Store is missing or incomplete. Verify or optimize again.".into());
            }
        }
        let service = Arc::new(Self {
            root,
            engine: engine.into(),
            inner: Mutex::new(Inner {
                db,
                active: None,
                mounts: BTreeMap::new(),
                processes: BTreeMap::new(),
            }),
            _lock: lock,
        });
        service.persist(&service.inner.lock().unwrap().db)?;
        Ok(service)
    }
    fn persist(&self, db: &Snapshot) -> Result<()> {
        let mut temp = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer_pretty(&mut temp, db)?;
        temp.write_all(b"\n")?;
        temp.as_file().sync_all()?;
        temp.persist(self.root.join("library.json"))?;
        #[cfg(unix)]
        File::open(&self.root)?.sync_all()?;
        Ok(())
    }
    fn change<T>(&self, f: impl FnOnce(&mut Snapshot) -> Result<T>) -> Result<T> {
        self.change_checked(false, f)
    }
    fn change_idle<T>(&self, f: impl FnOnce(&mut Snapshot) -> Result<T>) -> Result<T> {
        self.change_checked(true, f)
    }
    fn change_checked<T>(
        &self,
        require_idle: bool,
        f: impl FnOnce(&mut Snapshot) -> Result<T>,
    ) -> Result<T> {
        let mut inner = self.inner.lock().unwrap();
        if require_idle {
            ensure!(
                inner.active.is_none() && !inner.db.jobs.iter().any(|j| j.state == "running"),
                "Another operation is running"
            );
            ensure!(
                inner.db.games.iter().all(|g| g.session.is_none()),
                "Unmount runtime sessions first"
            );
        }
        let mut next = inner.db.clone();
        let result = f(&mut next)?;
        self.persist(&next)?;
        inner.db = next;
        Ok(result)
    }
    pub fn snapshot(&self) -> Snapshot {
        self.inner.lock().unwrap().db.clone()
    }
    fn idle(&self) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        ensure!(
            inner.active.is_none() && !inner.db.jobs.iter().any(|j| j.state == "running"),
            "Another storage operation is running. Wait or cancel it first."
        );
        ensure!(
            inner.db.games.iter().all(|g| g.session.is_none()),
            "Unmount all runtime sessions before changing the library or storage."
        );
        Ok(())
    }
    fn validate_installation(&self, source: &Path) -> Result<PathBuf> {
        let source = source
            .canonicalize()
            .context("Choose an existing installation folder")?;
        ensure!(source.is_dir(), "Installation must be a directory");
        ensure!(
            !overlap(&source, &self.root),
            "Installation and application data must be separate trees"
        );
        let snapshot = self.snapshot();
        let storage = playsparse_cli::workspace::projected(&snapshot.settings.storage_dir)?.0;
        ensure!(
            !overlap(&source, &storage),
            "Installation and store directory must be separate trees"
        );
        ensure!(
            !snapshot.games.iter().any(|g| overlap(&g.source, &source)),
            "This installation overlaps an existing library entry"
        );
        ensure!(
            snapshot.games.len() < 1000,
            "Library limit: 1000 installations"
        );
        ensure!(
            !source
                .ancestors()
                .any(|p| p.join("COMMITTED.json").is_file()),
            "Choose an original installation, not a PlaySparse store"
        );
        ensure!(
            !snapshot
                .games
                .iter()
                .filter_map(|g| g.store.as_ref())
                .any(|p| overlap(p, &source)),
            "Installation overlaps a registered store"
        );
        Ok(source)
    }
    pub fn inspect_installation(&self, source: &Path) -> Result<product::InstallationInspection> {
        product::inspect(&self.validate_installation(source)?)
    }
    pub fn game_locations(&self, id: &str) -> Result<Value> {
        let db = self.snapshot();
        let game = db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?;
        Ok(
            json!({"source":game.source,"store":game.store,"overlay":self.root.join("runtimes").join(id).join("overlay"),"mount":game.session.as_ref().map(|s| &s.mountpoint)}),
        )
    }
    pub fn inspect_location(&self, id: &str, kind: &str) -> Result<()> {
        ensure!(
            ["source", "store", "overlay", "mount"].contains(&kind),
            "Unknown location"
        );
        let locations = self.game_locations(id)?;
        let path = PathBuf::from(
            locations[kind]
                .as_str()
                .context("Location not created yet")?,
        );
        ensure!(path.is_dir(), "Folder is missing or unavailable");
        #[cfg(target_os = "macos")]
        let opener = "open";
        #[cfg(target_os = "windows")]
        let opener = "explorer.exe";
        #[cfg(target_os = "linux")]
        let opener = "xdg-open";
        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
        Command::new(opener).arg(path).spawn()?;
        Ok(())
    }
    pub fn discover_launch(&self, id: &str) -> Result<Vec<product::LaunchCandidate>> {
        let db = self.snapshot();
        let game = db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?;
        Ok(product::inspect(&game.source)?.candidates)
    }
    pub fn optimize_preflight(&self, id: &str) -> Result<Value> {
        let db = self.snapshot();
        let game = db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?;
        ensure!(
            game.session.is_none() && game.store.is_none(),
            "Unmount or inspect the existing store first"
        );
        ensure!(game.analysis.is_some(), "Analyze first");
        let parent = playsparse_cli::workspace::projected(&db.settings.storage_dir)?.0;
        ensure!(
            !parent.join(id).exists(),
            "Destination already exists; retained for inspection. Choose another location"
        );
        ensure!(
            !db.games.iter().any(|g| overlap(&parent, &g.source)),
            "Destination overlaps an installation"
        );
        let mut report = playsparse_cli::workspace::preflight(
            &game.source,
            &parent,
            256 * 1024,
            playsparse_core::Layout::Packs,
            1,
            "destination",
        )?;
        report["destination"] = json!(parent.join(id));
        Ok(report)
    }
    pub fn add_game(&self, source: &Path) -> Result<Game> {
        self.idle()?;
        let source = self.validate_installation(source)?;
        let game = Game {
            id: identity("g"),
            name: source
                .file_name()
                .context("Cannot register filesystem root")?
                .to_string_lossy()
                .into(),
            source,
            analysis: None,
            store: None,
            verified: false,
            store_stats: None,
            overlay_allocated_bytes: Some(0),
            launch: None,
            session: None,
            error: None,
        };
        self.change_idle(|db| {
            ensure!(
                !db.games.iter().any(|g| overlap(&g.source, &game.source)),
                "Installation already registered or overlaps another entry"
            );
            ensure!(db.games.len() < 1000, "Library limit: 1000 installations");
            let storage = playsparse_cli::workspace::projected(&db.settings.storage_dir)?.0;
            ensure!(
                !overlap(&storage, &game.source),
                "Installation overlaps current store directory"
            );
            db.games.push(game.clone());
            Ok(())
        })?;
        Ok(game)
    }
    /// Unregister only. Never recursively remove source, stores or runtime data.
    pub fn remove_game(&self, id: &str) -> Result<()> {
        self.idle()?;
        self.change_idle(|db| {
            ensure!(db.games.iter().any(|g| g.id == id), "Unknown game");
            db.games.retain(|g| g.id != id);
            Ok(())
        })
    }
    /// Forget missing store metadata only; never removes any filesystem data.
    pub fn forget_missing_store(&self, id: &str) -> Result<()> {
        self.change_idle(|db| {
            let game = game_mut(db, id)?;
            let store = game.store.as_ref().context("No store registered")?;
            ensure!(
                !store.try_exists()?,
                "Store still exists. Verify or inspect it instead of forgetting a missing store"
            );
            game.store = None;
            game.store_stats = None;
            game.verified = false;
            game.error = None;
            Ok(())
        })
    }
    pub fn update_settings(&self, settings: Settings) -> Result<()> {
        self.idle()?;
        ensure!(
            ["light", "dark", "system"].contains(&settings.theme.as_str()),
            "Invalid theme"
        );
        ensure!(
            (16..=16384).contains(&settings.cache_mib),
            "Cache must be 16..16384 MiB"
        );
        for path in [&settings.storage_dir, &settings.temp_dir] {
            let location = playsparse_cli::workspace::projected(path)?.0;
            ensure!(
                !self
                    .snapshot()
                    .games
                    .iter()
                    .any(|g| overlap(&location, &g.source)),
                "Storage and temporary directories must be outside installations"
            );
        }
        self.change_idle(|db| {
            for path in [&settings.storage_dir, &settings.temp_dir] {
                let location = playsparse_cli::workspace::projected(path)?.0;
                ensure!(
                    !db.games.iter().any(|g| overlap(&location, &g.source)),
                    "Storage or temporary directory overlaps installation"
                );
            }
            db.settings = settings;
            Ok(())
        })
    }
    pub fn configure_launch(&self, id: &str, descriptor: LaunchDescriptor) -> Result<()> {
        self.idle()?;
        ensure!(
            relative_executable(&descriptor.executable),
            "Executable must be a relative path without traversal"
        );
        ensure!(
            descriptor.args.len() <= 128
                && descriptor
                    .args
                    .iter()
                    .all(|s| s.len() <= 8192 && !s.contains('\0')),
            "Launch arguments exceed limits"
        );
        self.change_idle(|db| {
            let game = game_mut(db, id)?;
            let root = game
                .source
                .canonicalize()
                .context("Source missing; restore installation before configuring launch")?;
            let target = root
                .join(&descriptor.executable)
                .canonicalize()
                .context("Launch target does not exist in installation")?;
            ensure!(
                target.starts_with(&root),
                "Launch target escapes installation"
            );
            #[cfg(target_os = "macos")]
            let bundle = target.is_dir() && target.extension().is_some_and(|s| s == "app");
            #[cfg(not(target_os = "macos"))]
            let bundle = false;
            ensure!(
                target.is_file() || bundle,
                "Launch target must be a file or supported app bundle"
            );
            #[cfg(target_os = "macos")]
            if bundle {
                product::bundle_executable(&target)?;
            }
            game.launch = Some(descriptor);
            Ok(())
        })
    }
    pub fn start_job(self: &Arc<Self>, id: &str, operation: Operation) -> Result<Job> {
        ensure!(
            matches!(
                operation,
                Operation::Analyze | Operation::Optimize | Operation::Verify
            ),
            "Use runtime action for session operations"
        );
        let mut inner = self.inner.lock().unwrap();
        ensure!(
            inner.active.is_none() && !inner.db.jobs.iter().any(|j| j.state == "running"),
            "Another storage operation is running"
        );
        ensure!(
            inner.db.games.iter().all(|g| g.session.is_none()),
            "Unmount runtime sessions first"
        );
        let game = inner
            .db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?;
        if operation == Operation::Verify {
            ensure!(game.store.is_some(), "Optimize the installation first");
        }
        if operation == Operation::Optimize {
            ensure!(game.analysis.is_some(), "Analyze the installation first");
            ensure!(
                game.store.is_none(),
                "An existing store is registered. Remove the library entry to create a separate store."
            );
        }
        let job = Job {
            id: identity("j"),
            game_id: id.into(),
            operation,
            state: "running".into(),
            stage: "preparing".into(),
            bytes: 0,
            files: 0,
            started_at: now(),
            finished_at: None,
            error: None,
            cancellable: true,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let mut next = inner.db.clone();
        next.jobs.retain(|j| {
            j.state == "running" || j.finished_at.unwrap_or(0) > now().saturating_sub(30 * 86400000)
        });
        if next.jobs.len() >= 200 {
            next.jobs.remove(0);
        }
        next.jobs.push(job.clone());
        self.persist(&next)?;
        inner.db = next;
        inner.active = Some((job.id.clone(), cancel.clone()));
        drop(inner);
        let service = self.clone();
        let job_id = job.id.clone();
        let game_id = id.to_owned();
        thread::spawn(move || {
            let result = service.execute(&game_id, &job_id, operation, &cancel);
            let mut inner = service.inner.lock().unwrap();
            if let Some(job) = inner.db.jobs.iter_mut().find(|j| j.id == job_id) {
                job.finished_at = Some(now());
                job.cancellable = false;
                match result {
                    Ok(()) => {
                        job.state = "completed".into();
                        job.stage = "complete".into();
                    }
                    Err(error) => {
                        job.state = if cancel.load(Ordering::Relaxed) {
                            "cancelled"
                        } else {
                            "failed"
                        }
                        .into();
                        job.error = Some(format!("{error:#}"));
                    }
                }
            }
            if let Err(error) = service.persist(&inner.db)
                && let Some(job) = inner.db.jobs.iter_mut().find(|j| j.id == job_id)
            {
                job.state = "failed".into();
                job.error = Some(format!(
                    "Could not persist completion: {error:#}. Inspect store before retrying."
                ));
            }
            inner.active = None;
        });
        Ok(job)
    }
    pub fn cancel_job(&self, id: &str) -> Result<()> {
        let inner = self.inner.lock().unwrap();
        let (active, flag) = inner.active.as_ref().context("No active job")?;
        ensure!(active == id, "Job is not active");
        let job = inner
            .db
            .jobs
            .iter()
            .find(|j| j.id == id)
            .context("Unknown job")?;
        ensure!(
            job.cancellable,
            "Publication has started; wait for completion"
        );
        flag.store(true, Ordering::Relaxed);
        Ok(())
    }
    fn execute(
        &self,
        id: &str,
        job_id: &str,
        operation: Operation,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let snapshot = self.snapshot();
        let game = snapshot
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?;
        let mut last = std::time::Instant::now() - Duration::from_secs(1);
        let mut observer = |progress: Progress| -> playsparse_core::Result<()> {
            let mut inner = self.inner.lock().unwrap();
            if cancel.load(Ordering::Relaxed) {
                return Err(playsparse_core::Error::Invalid(
                    "Cancelled before publication".into(),
                ));
            }
            if let Some(job) = inner.db.jobs.iter_mut().find(|j| j.id == job_id) {
                if last.elapsed() >= Duration::from_millis(100) || job.stage != progress.stage {
                    job.stage = progress.stage.into();
                    job.bytes = progress.bytes;
                    job.files = progress.files;
                    last = std::time::Instant::now();
                }
                if progress.stage == "publishing" && operation == Operation::Optimize {
                    job.cancellable = false;
                }
            }
            Ok(())
        };
        match operation {
            Operation::Analyze => {
                let mut value = playsparse_cli::analyze_observed(
                    &game.source,
                    256 * 1024,
                    &snapshot.settings.temp_dir,
                    &mut observer,
                )?;
                value["original_allocated_bytes"] = json!(directory_allocation(&game.source)?.0);
                value["destination_required_estimated_bytes"] =
                    value["temporary_workspace"]["required_estimated_bytes"]
                        .as_u64()
                        .map(|n| json!(n / 2))
                        .unwrap_or(Value::Null);
                self.change(|db| {
                    let g = game_mut(db, id)?;
                    g.analysis = Some(value);
                    g.error = None;
                    Ok(())
                })?;
            }
            Operation::Optimize => {
                let parent =
                    playsparse_cli::workspace::projected(&snapshot.settings.storage_dir)?.0;
                for source in snapshot.games.iter().map(|g| &g.source) {
                    ensure!(
                        !overlap(&parent, source),
                        "Store location overlaps an installation"
                    );
                }
                let destination = parent.join(id);
                ensure!(
                    !destination.exists(),
                    "Destination already exists. It was retained; choose another storage directory or inspect it."
                );
                playsparse_cli::workspace::preflight(
                    &game.source,
                    &parent,
                    256 * 1024,
                    playsparse_core::Layout::Packs,
                    1,
                    "destination",
                )?;
                let stats = playsparse_store::pack_directory_observed(
                    &game.source,
                    &destination,
                    &PackOptions::default(),
                    &mut observer,
                )?;
                // Engine has verified staging and atomically published. Do not honour late cancellation.
                self.change(|db| {
                    let g = game_mut(db, id)?;
                    g.store = Some(destination);
                    g.store_stats = Some(serde_json::to_value(stats)?);
                    g.verified = true;
                    g.error = None;
                    Ok(())
                })?;
            }
            Operation::Verify => {
                self.change(|db| {
                    game_mut(db, id)?.verified = false;
                    Ok(())
                })?;
                Store::open(game.store.as_ref().context("No store")?)?
                    .verify_observed(&mut observer)?;
                self.change(|db| {
                    game_mut(db, id)?.verified = true;
                    Ok(())
                })?;
            }
            _ => anyhow::bail!("Runtime operation is not a storage job"),
        }
        Ok(())
    }
    pub fn test_readiness(&self) -> Result<Value> {
        self.idle()?;
        let output = Command::new(&self.engine)
            .args(["doctor", "--json", "--mount-test"])
            .output()?;
        let mut report: Value = serde_json::from_slice(&output.stdout)
            .context("Diagnostics could not return a structured report")?;
        report["readiness"] = readiness(&report);
        Ok(report)
    }
    pub fn system_status(&self) -> Result<Value> {
        let settings = self.snapshot().settings;
        let output = Command::new(&self.engine)
            .args(["doctor", "--json", "--temp-dir"])
            .arg(settings.temp_dir)
            .output()
            .context("Bundled PlaySparse engine could not start")?;
        let mut report: Value = serde_json::from_slice(&output.stdout)
            .context("Engine diagnostics returned an unreadable report")?;
        report["readiness"] = readiness(&report);
        Ok(report)
    }
    /// Sizes are individual measured representations. Originals remain installed;
    /// saved representation bytes do not imply free disk space was reclaimed.
    pub fn storage_statistics(&self) -> Result<Value> {
        let snapshot = self.snapshot();
        let mut rows = vec![];
        for game in snapshot.games {
            let measure = |path: &Path| -> Value {
                match directory_allocation(path) {
                    Ok((allocated, _)) => {
                        json!({"allocated_bytes": allocated, "logical_bytes": directory_bytes(path).ok()})
                    }
                    Err(e) => json!({"error":e.to_string()}),
                }
            };
            rows.push(json!({"game_id":game.id, "original":measure(&game.source), "store":game.store.as_deref().map(measure),
                "overlay":measure(&self.root.join("runtimes").join(&game.id).join("overlay")), "compatibility_shadow":null,
                "cache_disk_bytes":0, "cache_note":"Runtime cache is memory-only; no disk cache is configured"}));
        }
        Ok(
            json!({"games":rows,"shared_objects":"Stores are independent; no cross-store deduplication. Hardlinks/reflinks across installations are not deduplicated in totals.","originals_retained":true}),
        )
    }
    /// Runtime events are audited separately from byte-counted storage jobs.
    pub fn perform_runtime(&self, id: &str, action: &str, processes_closed: bool) -> Result<()> {
        let operation = match action {
            "mount" => Operation::Mount,
            "launch" => Operation::Launch,
            "stop" => Operation::Stop,
            "unmount" => Operation::Unmount,
            "recover" => Operation::Recover,
            _ => anyhow::bail!("Unknown runtime action"),
        };
        let job_id = identity("runtime");
        self.change(|db| {
            ensure!(
                !db.jobs.iter().any(|j| j.state == "running"),
                "Another operation is running"
            );
            let game = game_mut(db, id)?;
            validate_transition(game, operation)?;
            if db.jobs.len() >= 200 {
                db.jobs.remove(0);
            }
            db.jobs.push(Job {
                id: job_id.clone(),
                game_id: id.into(),
                operation,
                state: "running".into(),
                stage: action.into(),
                bytes: 0,
                files: 0,
                started_at: now(),
                finished_at: None,
                error: None,
                cancellable: false,
            });
            Ok(())
        })?;
        let result = match operation {
            Operation::Mount => self.mount_game(id),
            Operation::Launch => self.launch_game(id),
            Operation::Stop => self.stop_game(id),
            Operation::Unmount => self.unmount_game(id, processes_closed),
            Operation::Recover => self.recover_session(id),
            _ => unreachable!(),
        };
        self.change(|db| {
            let job = db
                .jobs
                .iter_mut()
                .find(|j| j.id == job_id)
                .context("Missing runtime job")?;
            job.finished_at = Some(now());
            job.state = if result.is_ok() {
                "completed"
            } else {
                "failed"
            }
            .into();
            job.error = result.as_ref().err().map(|e| format!("{e:#}"));
            Ok(())
        })?;
        result
    }
    pub fn mount_game(&self, id: &str) -> Result<()> {
        let diagnostic = self.system_status()?;
        ensure!(
            diagnostic["mount_backend"]["available"].as_bool() == Some(true),
            "Filesystem backend unavailable. Run diagnostics and install the platform driver explicitly."
        );
        let mut inner = self.inner.lock().unwrap();
        ensure!(inner.active.is_none(), "Wait for storage operation");
        let game = inner
            .db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?
            .clone();
        ensure!(game.verified, "Verify the store first");
        ensure!(
            game.session.is_none(),
            "Inspect or unmount the existing session first"
        );
        let store_path = game.store.context("No store")?;
        let store = Store::open(&store_path)?;
        store.verify()?;
        let runtime = self.root.join("runtimes").join(id);
        validate_owned_path(&runtime)?;
        validate_owned_path(&runtime.join("overlay"))?;
        #[cfg(not(windows))]
        validate_owned_path(&runtime.join("mount"))?;
        fs::create_dir_all(&runtime)?;
        let overlay = runtime.join("overlay");
        #[cfg(not(windows))]
        let mountpoint = {
            let p = runtime.join("mount");
            fs::create_dir_all(&p)?;
            ensure!(
                fs::read_dir(&p)?.next().is_none(),
                "Mountpoint is not empty; inspect stale mount"
            );
            p
        };
        #[cfg(windows)]
        let mountpoint = (b'D'..=b'Z')
            .rev()
            .map(|c| PathBuf::from(format!("{}:", c as char)))
            .find(|p| !PathBuf::from(format!("{}\\", p.display())).exists())
            .context("No unused drive letter")?;
        let session = Session {
            state: "preparing".into(),
            mountpoint: mountpoint.clone(),
            overlay: overlay.clone(),
            error: None,
        };
        game_mut(&mut inner.db, id)?.overlay_allocated_bytes = None;
        game_mut(&mut inner.db, id)?.session = Some(session);
        self.persist(&inner.db)?;
        let mut command = Command::new(&self.engine);
        command
            .arg("mount")
            .arg(&store_path)
            .arg(&mountpoint)
            .arg("--overlay")
            .arg(&overlay)
            .arg("--cache")
            .arg(format!("{}M", inner.db.settings.cache_mib));
        let child = spawn_logged(
            &mut command,
            &runtime.join("mount.log"),
            inner.db.settings.retain_logs,
        )?;
        inner.mounts.insert(id.into(), child);
        // Establish mount readiness from actual bytes, not process existence alone.
        let probe = store.manifest().files.iter().find(|f| f.size > 0);
        let expected = probe
            .map(|f| {
                let chunk = f.chunks.first().context("Missing probe chunk")?;
                Ok::<_, anyhow::Error>(
                    store.read_object(&playsparse_core::parse_hash(&chunk.hash)?)?,
                )
            })
            .transpose()?;
        let mut mounted = false;
        for _ in 0..100 {
            if inner
                .mounts
                .get_mut(id)
                .context("Missing mount child")?
                .try_wait()?
                .is_some()
            {
                break;
            }
            if let (Some(probe), Some(bytes)) = (probe, &expected) {
                use std::io::Read;
                let n = bytes.len().min(4096);
                let mut read = vec![0; n];
                if File::open(mounted_root(&mountpoint).join(&probe.path))
                    .and_then(|mut f| f.read_exact(&mut read))
                    .is_ok()
                    && read == bytes[..n]
                {
                    mounted = true;
                    break;
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        let session = game_mut(&mut inner.db, id)?.session.as_mut().unwrap();
        if mounted {
            session.state = "mounted".into();
        } else {
            session.state = "needs_attention".into();
            session.error = Some("Mount did not pass exact-byte readiness check. Inspect diagnostics and mount.log, then unmount. Empty stores cannot be probed.".into());
        }
        self.persist(&inner.db)?;
        ensure!(
            mounted,
            "Mount readiness failed; session retained for safe recovery"
        );
        Ok(())
    }
    pub fn launch_game(&self, id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let game = inner
            .db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?
            .clone();
        let session = game.session.context("Mount the verified store first")?;
        ensure!(
            session.state == "mounted",
            "Runtime must be mounted and idle"
        );
        let descriptor = game.launch.context("Configure a tested executable and literal arguments first. Engine detection is not compatibility proof.")?;
        ensure!(
            descriptor.compatibility_confirmed,
            "Confirm compatibility for this installation before launching"
        );
        ensure!(
            relative_executable(&descriptor.executable),
            "Invalid executable path"
        );
        let mount = mounted_root(&session.mountpoint).canonicalize()?;
        let executable = mount.join(&descriptor.executable).canonicalize()?;
        #[cfg(target_os = "macos")]
        let bundle = executable.is_dir() && executable.extension().is_some_and(|s| s == "app");
        #[cfg(not(target_os = "macos"))]
        let bundle = false;
        ensure!(
            executable.starts_with(&mount) && (executable.is_file() || bundle),
            "Executable escapes mount or is not a file"
        );
        #[cfg(target_os = "macos")]
        let mut command = if bundle {
            product::bundle_executable(&executable)?;
            let mut command = Command::new("/usr/bin/open");
            command
                .args(["-W", "-n", "-a"])
                .arg(&executable)
                .arg("--args");
            command
        } else {
            Command::new(&executable)
        };
        #[cfg(not(target_os = "macos"))]
        let mut command = Command::new(&executable);
        command.args(descriptor.args).current_dir(&mount);
        let child = spawn_logged(&mut command, &self.root.join("runtimes").join(id).join("launch.log"), inner.db.settings.retain_logs)
            .context("Launch failed; signed native macOS code may require an independently prepared APFS compatibility shadow")?;
        inner.processes.insert(id.into(), child);
        game_mut(&mut inner.db, id)?.session.as_mut().unwrap().state = "running".into();
        self.persist(&inner.db)?;
        Ok(())
    }
    pub fn reconcile(&self) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        let mut exited = vec![];
        for (id, child) in &mut inner.processes {
            if let Some(status) = child.try_wait()? {
                exited.push((id.clone(), status));
            }
        }
        let mut changed = !exited.is_empty();
        for (id, status) in exited {
            inner.processes.remove(&id);
            if let Some(session) = &mut game_mut(&mut inner.db, &id)?.session {
                session.state = "mounted".into();
                session.error = Some(if status.success() {
                    "Tracked process exited. Child/launcher processes are not tracked; close them before unmounting.".into()
                } else {
                    format!(
                        "Launch exited with {status}. Inspect launch.log and compatibility; close any remaining child processes before unmounting."
                    )
                });
            }
        }
        let mut failed = vec![];
        for (id, child) in &mut inner.mounts {
            if child.try_wait()?.is_some() {
                failed.push(id.clone());
            }
        }
        changed |= !failed.is_empty();
        for id in failed {
            inner.mounts.remove(&id);
            if let Some(session) = &mut game_mut(&mut inner.db, &id)?.session {
                session.state = "needs_attention".into();
                session.error =
                    Some("Mount process exited. Inspect the mount before cleanup.".into());
            }
        }
        if changed {
            self.persist(&inner.db)?;
        }
        Ok(())
    }
    pub fn stop_game(&self, id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        ensure!(
            !inner
                .db
                .games
                .iter()
                .find(|g| g.id == id)
                .and_then(|g| g.launch.as_ref())
                .is_some_and(|d| d.executable.ends_with(".app")),
            "Quit the app using its own Quit command. PlaySparse tracks Launch Services waiting, and cannot safely terminate the application by that helper PID."
        );
        let child = inner
            .processes
            .get_mut(id)
            .context("No tracked running process")?;
        child.kill()?;
        child.wait()?;
        inner.processes.remove(id);
        let session = game_mut(&mut inner.db, id)?
            .session
            .as_mut()
            .context("No session")?;
        session.state = "mounted".into();
        session.error = Some(
            "Tracked process stopped. Close any launcher/child processes before unmounting.".into(),
        );
        self.persist(&inner.db)?;
        Ok(())
    }
    pub fn unmount_game(&self, id: &str, processes_closed: bool) -> Result<()> {
        ensure!(
            processes_closed,
            "Confirm all game and launcher processes are closed"
        );
        let mut inner = self.inner.lock().unwrap();
        ensure!(
            !inner.processes.contains_key(id),
            "Stop the tracked game process first"
        );
        let game = inner
            .db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?;
        let session = game.session.as_ref().context("No runtime session")?;
        self.validate_mount_location(id, &session.mountpoint)?;
        let output = Command::new(&self.engine)
            .arg("unmount")
            .arg(&session.mountpoint)
            .output()?;
        ensure!(
            output.status.success(),
            "Ordinary unmount failed; retained session: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if let Some(child) = inner.mounts.get_mut(id) {
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while child.try_wait()?.is_none() && std::time::Instant::now() < deadline {
                thread::sleep(Duration::from_millis(50));
            }
            ensure!(
                child.try_wait()?.is_some(),
                "Unmount returned, but backend is still shutting down. Session retained; reconcile when it exits."
            );
        }
        inner.mounts.remove(id);
        game_mut(&mut inner.db, id)?.session = None;
        game_mut(&mut inner.db, id)?.overlay_allocated_bytes =
            overlay_allocation(&self.root.join("runtimes").join(id).join("overlay"));
        self.persist(&inner.db)?;
        Ok(())
    }
    fn validate_mount_location(&self, id: &str, mountpoint: &Path) -> Result<()> {
        #[cfg(not(windows))]
        ensure!(
            mountpoint == self.root.join("runtimes").join(id).join("mount"),
            "Session mountpoint is outside the owned runtime location"
        );
        #[cfg(windows)]
        {
            let drive = mountpoint.to_string_lossy();
            ensure!(
                drive.len() == 2
                    && (b'D'..=b'Z').contains(&drive.as_bytes()[0])
                    && drive.ends_with(':'),
                "Invalid persisted runtime drive"
            );
            let _ = id;
        }
        Ok(())
    }
    /// Clear stale metadata only after proving the ordinary mount is absent.
    /// This does not kill a process, detach a mount or delete runtime files.
    pub fn recover_session(&self, id: &str) -> Result<()> {
        let mut inner = self.inner.lock().unwrap();
        ensure!(inner.active.is_none(), "Wait for storage operation");
        ensure!(
            !inner.processes.contains_key(id),
            "Stop the tracked game process first"
        );
        if let Some(child) = inner.mounts.get_mut(id) {
            ensure!(
                child.try_wait()?.is_some(),
                "Mount process is still alive; use ordinary unmount"
            );
        }
        let game = inner
            .db
            .games
            .iter()
            .find(|g| g.id == id)
            .context("Unknown game")?;
        let session = game.session.as_ref().context("No stale runtime session")?;
        self.validate_mount_location(id, &session.mountpoint)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            match fs::metadata(&session.mountpoint) {
                Ok(metadata) => {
                    let parent = session
                        .mountpoint
                        .parent()
                        .context("No mountpoint parent")?;
                    ensure!(
                        metadata.dev() == fs::metadata(parent)?.dev(),
                        "A separate filesystem is still mounted. Close all processes and use ordinary unmount."
                    );
                    validate_owned_path(&session.mountpoint)?;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        #[cfg(windows)]
        ensure!(
            !PathBuf::from(format!("{}\\", session.mountpoint.display())).try_exists()?,
            "Runtime drive exists; inspect it and use ordinary unmount. No external drive is detached automatically."
        );
        inner.mounts.remove(id);
        let mut next = inner.db.clone();
        game_mut(&mut next, id)?.session = None;
        game_mut(&mut next, id)?.overlay_allocated_bytes =
            overlay_allocation(&self.root.join("runtimes").join(id).join("overlay"));
        self.persist(&next)?;
        inner.db = next;
        Ok(())
    }
    pub fn can_close(&self) -> bool {
        let inner = self.inner.lock().unwrap();
        inner.active.is_none()
            && !inner.db.jobs.iter().any(|j| j.state == "running")
            && inner.db.games.iter().all(|g| g.session.is_none())
    }
}
fn game_mut<'a>(db: &'a mut Snapshot, id: &str) -> Result<&'a mut Game> {
    db.games
        .iter_mut()
        .find(|g| g.id == id)
        .context("Unknown game")
}
fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn relative_executable(value: &str) -> bool {
    !value.is_empty()
        && !value.contains('\\')
        && !value.contains(':')
        && Path::new(value)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn default_logs() -> bool {
    true
}

fn validate_owned_path(path: &Path) -> Result<()> {
    ensure!(
        playsparse_cli::workspace::projected(path)?.0 == path,
        "Owned runtime path contains a symlink; inspect it before proceeding"
    );
    Ok(())
}

#[cfg(all(test, unix))]
mod owned_path_tests {
    use super::*;
    #[test]
    fn runtime_symlink_is_rejected_before_creating_source_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let source = root.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("original"), b"unchanged").unwrap();
        let owned = root.join("runtime");
        std::os::unix::fs::symlink(&source, &owned).unwrap();
        assert!(validate_owned_path(&owned.join("overlay")).is_err());
        assert!(!source.join("overlay").exists());
        assert_eq!(fs::read(source.join("original")).unwrap(), b"unchanged");
        assert!(validate_owned_path(&root.join("safe/new/overlay")).is_ok());
    }
}
/// Bound combined stdout/stderr on disk while continuing to drain both pipes.
fn spawn_logged(command: &mut Command, path: &Path, retain: bool) -> Result<Child> {
    if !retain {
        return Ok(command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?);
    }
    let log = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)?;
    let shared = Arc::new(Mutex::new((log, 0usize)));
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    fn drain(mut input: impl std::io::Read + Send + 'static, shared: Arc<Mutex<(File, usize)>>) {
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            while let Ok(n) = input.read(&mut buffer) {
                if n == 0 {
                    break;
                }
                let mut shared = shared.lock().unwrap();
                let remaining = (2 * 1024 * 1024usize).saturating_sub(shared.1);
                let keep = n.min(remaining);
                if shared.0.write_all(&buffer[..keep]).is_ok() {
                    shared.1 += keep;
                } else {
                    shared.1 = 2 * 1024 * 1024;
                }
            }
        });
    }
    if let Some(pipe) = child.stdout.take() {
        drain(pipe, shared.clone());
    }
    if let Some(pipe) = child.stderr.take() {
        drain(pipe, shared);
    }
    Ok(child)
}

fn overlay_allocation(path: &Path) -> Option<u64> {
    if validate_owned_path(path).is_err() {
        return None;
    }
    if !path.try_exists().ok()? {
        return Some(0);
    }
    directory_allocation(path).ok()?.0
}

fn mounted_root(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(format!("{}\\", path.display()))
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Prerequisite presence is distinct from a tested mount, never kernel approval proof.
pub fn readiness(report: &Value) -> Value {
    let available = report["mount_backend"]["available"].as_bool();
    let unsupported = report["os"]
        .as_str()
        .is_some_and(|os| !["macos", "windows", "linux"].contains(&os))
        || report["mount_diagnostic"]["status"] == "UNSUPPORTED_VERSION"
        || report["mount_backend"]["backend"] == "unsupported";
    let state = if unsupported {
        "Unsupported"
    } else if available == Some(false) {
        "Action required"
    } else if report["mount_test"]["status"] == "PASS" {
        "Ready"
    } else if report["mount_test"]["status"] == "FAIL"
        || (report["mount_test"]["status"] == "BLOCKED" && report["os"] != "windows")
    {
        "Action required"
    } else {
        "Unknown"
    };
    let guidance = match report["os"].as_str() {
        Some("macos") => {
            "Install official macFUSE 5.3.3+ in the 5.x series. Follow its kernel-backend approval and restart instructions in System Settings. FSKit is unsupported. Installation presence does not prove approval."
        }
        Some("windows") => {
            "Install official WinFsp 2.1 with its filesystem driver, and WebView2. Restart if the installer requests it. DLL detection alone does not prove the driver can mount."
        }
        Some("linux") => {
            "Install your distribution's FUSE 3 package. Ensure /dev/fuse exists and your account can open it, and fusermount3 is installed. Follow distribution permission guidance; do not make the device world-writable."
        }
        _ => "Open the native app to inspect this system. Mount capability is unknown.",
    };
    #[cfg(target_os = "linux")]
    let helpers: Vec<String> = ["fusermount3", "fusermount"]
        .iter()
        .filter(|name| {
            std::env::var_os("PATH")
                .is_some_and(|p| std::env::split_paths(&p).any(|dir| dir.join(name).is_file()))
        })
        .map(|s| (*s).into())
        .collect();
    #[cfg(not(target_os = "linux"))]
    let helpers: Vec<String> = vec![];
    json!({"state":state, "can_attempt_mount":available == Some(true) && report["mount_test"]["status"] != "FAIL" && !(report["mount_test"]["status"] == "BLOCKED" && report["os"] != "windows"), "guidance":guidance, "arch":report["arch"], "platform":report["os"], "driver":report["mount_backend"]["backend"], "mount_test":report["mount_test"]["status"], "helpers":helpers, "approval":"Unknown unless independently confirmed; no system settings are changed"})
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeState {
    Unoptimized,
    Analyzed,
    Optimizing,
    Ready,
    Mounting,
    Mounted,
    Launching,
    Running,
    Stopping,
    Unmounting,
    NeedsAttention,
    Error,
}
pub fn runtime_state(game: &Game, jobs: &[Job]) -> RuntimeState {
    if let Some(job) = jobs
        .iter()
        .find(|j| j.game_id == game.id && j.state == "running")
    {
        return match job.operation {
            Operation::Optimize => RuntimeState::Optimizing,
            Operation::Mount => RuntimeState::Mounting,
            Operation::Launch => RuntimeState::Launching,
            Operation::Stop => RuntimeState::Stopping,
            Operation::Unmount => RuntimeState::Unmounting,
            _ => RuntimeState::NeedsAttention,
        };
    }
    if let Some(session) = &game.session {
        return match session.state.as_str() {
            "mounted" => RuntimeState::Mounted,
            "running" => RuntimeState::Running,
            _ => RuntimeState::NeedsAttention,
        };
    }
    if game.error.is_some() {
        return RuntimeState::NeedsAttention;
    }
    if game.store.is_some() {
        return if game.verified {
            RuntimeState::Ready
        } else {
            RuntimeState::NeedsAttention
        };
    }
    if game.analysis.is_some() {
        RuntimeState::Analyzed
    } else {
        RuntimeState::Unoptimized
    }
}
pub fn validate_transition(game: &Game, operation: Operation) -> Result<()> {
    let state = runtime_state(game, &[]);
    let allowed = match operation {
        Operation::Mount => game.verified && game.session.is_none(),
        Operation::Launch => {
            state == RuntimeState::Mounted
                && game
                    .launch
                    .as_ref()
                    .is_some_and(|d| d.compatibility_confirmed)
        }
        Operation::Stop => state == RuntimeState::Running,
        Operation::Unmount | Operation::Recover => {
            game.session.is_some() && state != RuntimeState::Running
        }
        Operation::Analyze | Operation::Optimize | Operation::Verify => game.session.is_none(),
    };
    ensure!(
        allowed,
        "Cannot {operation:?} while runtime state is {state:?}; inspect the game details for recovery"
    );
    Ok(())
}
