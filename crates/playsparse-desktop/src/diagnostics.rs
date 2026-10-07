//! Allowlisted metadata only: no launch arguments, raw logs, tokens or arbitrary file reads.
use super::*;
impl Service {
    pub fn export_diagnostics(&self) -> Result<PathBuf> {
        let snapshot = self.snapshot();
        let games: Vec<Value> = snapshot.games.iter().map(|g| json!({
            "id":g.id,"verified":g.verified,"last_verified":g.last_verified,
            "compatibility":g.compatibility.status,"runtime":g.session.as_ref().map(|s| &s.state),
            "lifecycle":g.session.as_ref().map(|s| &s.processes.lifecycle),
            "active_process_count":g.session.as_ref().map(|s| s.processes.active_processes.len()),
            "has_error":g.error.is_some(),"store_stats":g.store_stats,
            "available_actions":available_actions(g)
        })).collect();
        let jobs: Vec<Value> = snapshot.jobs.iter().map(|j| json!({"operation":j.operation,"state":j.state,
            "stage":j.stage,"started_at":j.started_at,"finished_at":j.finished_at,"has_error":j.error.is_some()})).collect();
        let system = self.system_status().ok();
        // Driver fields selected from the structured report; no raw environment or helper paths.
        let value = json!({"schema":1,"version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,
            "architecture":std::env::consts::ARCH,"created_at":now(),"games":games,"jobs":jobs,
            "readiness":system.as_ref().map(|s| &s["readiness"]["state"]),
            "provider":system.as_ref().map(|s| &s["readiness"]["driver"]),
            "privacy":"Paths, names, argv, raw errors, logs, process IDs and ownership tokens omitted. No user file content included."});
        let mut file = tempfile::Builder::new()
            .prefix("diagnostics-")
            .suffix(".json")
            .tempfile_in(&self.root)?;
        serde_json::to_writer_pretty(&mut file, &value)?;
        file.as_file().sync_all()?;
        let (_, path) = file.keep()?;
        Ok(path)
    }
    /// Explicit offline restoration. Retain corrupt metadata before atomically publishing a validated backup.
    pub fn restore_library_backup(root: &Path) -> Result<PathBuf> {
        let root = root.canonicalize()?;
        validate_metadata_file(&root.join("library.lock"))?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(root.join("library.lock"))?;
        acquire_library_lock(&lock).context("Close PlaySparse before restoring metadata")?;
        let backup_path = root.join("library.backup.json");
        validate_metadata_file(&backup_path)?;
        ensure!(
            fs::metadata(&backup_path)?.len() <= 16 * 1024 * 1024,
            "Backup exceeds bound"
        );
        let bytes = fs::read(&backup_path)?;
        let backup: Snapshot = serde_json::from_slice(&bytes)?;
        ensure!(
            backup.version == 1 && backup.games.len() <= 1000 && backup.jobs.len() <= 200,
            "Invalid backup"
        );
        for game in &backup.games {
            ensure!(
                game.id.starts_with("g-")
                    && game
                        .id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    && game.source.is_absolute()
                    && !overlap(&game.source, &root),
                "Unsafe backup metadata"
            );
        }
        let library = root.join("library.json");
        validate_metadata_file(&library)?;
        ensure!(
            fs::metadata(&library)?.len() <= 16 * 1024 * 1024,
            "Corrupt metadata exceeds bound; retain manually"
        );
        let mut retained = tempfile::Builder::new()
            .prefix("library-corrupt-")
            .suffix(".json")
            .tempfile_in(&root)?;
        retained.write_all(&fs::read(&library)?)?;
        retained.as_file().sync_all()?;
        let (_, retained_path) = retained.keep()?;
        let mut replacement = tempfile::NamedTempFile::new_in(&root)?;
        replacement.write_all(&bytes)?;
        replacement.as_file().sync_all()?;
        replacement.persist(&library)?;
        #[cfg(unix)]
        File::open(&root)?.sync_all()?;
        Ok(retained_path)
    }
}
/// UI consumes backend decisions; all actions are revalidated when invoked.
pub fn available_actions(game: &Game) -> Vec<&'static str> {
    let mut actions = vec![];
    for (operation, name) in [
        (Operation::Mount, "mount"),
        (Operation::Launch, "launch"),
        (Operation::Stop, "stop"),
        (Operation::Unmount, "unmount"),
        (Operation::Recover, "recover"),
    ] {
        if validate_transition(game, operation).is_ok() {
            actions.push(name);
        }
    }
    actions
}
