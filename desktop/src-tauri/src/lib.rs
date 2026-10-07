use playsparse_desktop::{LaunchDescriptor, Operation, Service, Settings, Snapshot};
use serde_json::Value;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

type Backend = Arc<Service>;
type Reply<T> = Result<T, String>;

#[tauri::command]
async fn export_diagnostics(service: State<'_, Backend>) -> Reply<String> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service
            .export_diagnostics()
            .map(|p| p.display().to_string())
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn get_snapshot(service: State<'_, Backend>) -> Snapshot {
    service.snapshot()
}

#[tauri::command]
async fn select_folder(app: tauri::AppHandle) -> Reply<Option<String>> {
    tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Select a folder")
            .blocking_pick_folder()
            .map(|p| p.to_string())
    })
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
async fn add_game(service: State<'_, Backend>, path: String) -> Reply<playsparse_desktop::Game> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service
            .add_game(std::path::Path::new(&path))
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn inspect_installation(
    service: State<'_, Backend>,
    path: String,
) -> Reply<playsparse_desktop::product::InstallationInspection> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service
            .inspect_installation(std::path::Path::new(&path))
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn discover_launch(
    service: State<'_, Backend>,
    id: String,
) -> Reply<Vec<playsparse_desktop::product::LaunchCandidate>> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.discover_launch(&id).map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn optimize_preflight(service: State<'_, Backend>, id: String) -> Reply<Value> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service
            .optimize_preflight(&id)
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn test_readiness(service: State<'_, Backend>) -> Reply<Value> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.test_readiness().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn game_locations(service: State<'_, Backend>, id: String) -> Reply<Value> {
    service.game_locations(&id).map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn inspect_location(service: State<'_, Backend>, id: String, kind: String) -> Reply<()> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service
            .inspect_location(&id, &kind)
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
fn forget_missing_store(service: State<'_, Backend>, id: String) -> Reply<()> {
    service
        .forget_missing_store(&id)
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn remove_game(service: State<'_, Backend>, id: String) -> Reply<()> {
    service.remove_game(&id).map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn update_settings(service: State<'_, Backend>, settings: Settings) -> Reply<()> {
    service
        .update_settings(settings)
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn configure_launch(
    service: State<'_, Backend>,
    id: String,
    descriptor: LaunchDescriptor,
) -> Reply<()> {
    service
        .configure_launch(&id, descriptor)
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn start_job(
    service: State<'_, Backend>,
    id: String,
    operation: Operation,
) -> Reply<playsparse_desktop::Job> {
    service
        .inner()
        .start_job(&id, operation)
        .map_err(|e| format!("{e:#}"))
}
#[tauri::command]
fn cancel_job(service: State<'_, Backend>, id: String) -> Reply<()> {
    service.cancel_job(&id).map_err(|e| format!("{e:#}"))
}
#[tauri::command]
async fn get_system_status(service: State<'_, Backend>) -> Reply<Value> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.system_status().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn get_storage_statistics(service: State<'_, Backend>) -> Reply<Value> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service.storage_statistics().map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
async fn runtime_action(
    service: State<'_, Backend>,
    id: String,
    action: String,
    processes_closed: bool,
) -> Reply<()> {
    let service = service.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        service
            .perform_runtime(&id, &action, processes_closed)
            .map_err(|e| format!("{e:#}"))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(feature = "acceptance")]
#[tauri::command]
fn acceptance_report(service: State<'_, Backend>, report: Value) -> Reply<()> {
    let root = std::env::var_os("PLAYSPARSE_DESKTOP_DATA_DIR")
        .ok_or("Acceptance requires an isolated data directory")?;
    let path = std::path::PathBuf::from(root).join("native-acceptance.json");
    if service.snapshot().games.len() > 1 {
        return Err("Acceptance requires a single generated fixture".into());
    }
    std::fs::write(
        path,
        serde_json::to_vec_pretty(&report).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())
}
#[cfg(feature = "acceptance")]
#[tauri::command]
fn acceptance_probe(service: State<'_, Backend>, id: String) -> Reply<Value> {
    let snapshot = service.snapshot();
    let game = snapshot
        .games
        .iter()
        .find(|g| g.id == id)
        .ok_or("Unknown fixture")?;
    if std::fs::read(game.source.join(".playsparse-generated-fixture"))
        .map_err(|e| e.to_string())?
        != b"desktop-acceptance-v1"
    {
        return Err("Only generated acceptance fixtures can be probed".into());
    }
    let session = game.session.as_ref().ok_or("No mounted fixture")?;
    let expected = std::fs::read(game.source.join("assets/data.bin")).map_err(|e| e.to_string())?;
    let actual =
        std::fs::read(session.mountpoint.join("assets/data.bin")).map_err(|e| e.to_string())?;
    if expected != actual {
        return Err("Mounted content differs from source".into());
    }
    std::fs::write(
        session.mountpoint.join("fixture-save.txt"),
        b"generated overlay save",
    )
    .map_err(|e| e.to_string())?;
    if game.source.join("fixture-save.txt").exists() {
        return Err("Overlay escaped into source".into());
    }
    Ok(serde_json::json!({"exact_bytes":expected.len(), "source_save_absent":true}))
}

pub fn run() {
    let builder = tauri::Builder::default();
    // Isolated acceptance libraries may coexist with the user's normal application.
    #[cfg(not(feature = "acceptance"))]
    let builder = builder.plugin(tauri_plugin_single_instance::init(|app, _, _| {
        if let Some(window) = app.get_webview_window("main") {
            let _ = window.set_focus();
        }
    }));
    let builder = builder.plugin(tauri_plugin_dialog::init())
        .on_page_load(|webview, payload| {
            #[cfg(feature = "acceptance")]
            if payload.event() == tauri::webview::PageLoadEvent::Finished
                && let Some(source) = std::env::var_os("PLAYSPARSE_DESKTOP_ACCEPTANCE_SOURCE")
            {
                let _ = webview.set_focus();
                let prefix = format!(
                    "window.__DESKTOP_ACCEPTANCE_SOURCE={};",
                    serde_json::to_string(&source.to_string_lossy()).unwrap()
                );
                let _ = webview.eval(&(prefix + include_str!("acceptance.js")));
            }
            #[cfg(not(feature = "acceptance"))]
            let _ = (webview, payload);
        })
        .setup(|app| {
            let root = std::env::var_os("PLAYSPARSE_DESKTOP_DATA_DIR")
                .map(std::path::PathBuf::from)
                .unwrap_or(app.path().app_data_dir()?);
            let extension = if cfg!(windows) { ".exe" } else { "" };
            let engine = if cfg!(debug_assertions) {
                let host = env!("ENGINE_TARGET");
                std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join(format!("binaries/playsparse-engine-{host}{extension}"))
            } else {
                std::env::current_exe()?
                    .parent()
                    .ok_or("No application directory")?
                    .join(format!("playsparse-engine{extension}"))
            };
            let service = match Service::open(&root, &engine) {
                Ok(service) => service,
                Err(error) => {
                    let inspect = app.dialog().message(format!("PlaySparse could not open its library at {}.\n\n{error:#}\n\nExisting data was retained. Close other instances and reopen PlaySparse. For a corrupt library, retain library.json and restore a known-good backup before retrying. No library is reset automatically.", root.display()))
                        .title("Library needs attention").kind(tauri_plugin_dialog::MessageDialogKind::Error)
                        .buttons(tauri_plugin_dialog::MessageDialogButtons::OkCancelCustom("Show library folder".into(), "Close".into())).blocking_show();
                    if inspect {
                        #[cfg(target_os = "macos")]
                        let opener = "open";
                        #[cfg(target_os = "windows")]
                        let opener = "explorer.exe";
                        #[cfg(target_os = "linux")]
                        let opener = "xdg-open";
                        #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
                        let _ = std::process::Command::new(opener).arg(&root).spawn();
                    }
                    return Err(error.into());
                }
            };
            app.manage(service.clone());
            let handle = app.handle().clone();
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(750));
                    let _ = service.reconcile();
                    let _ = handle.emit("desktop-state", service.snapshot());
                }
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                let state = window.state::<Backend>();
                if !state.can_close() {
                    api.prevent_close();
                    let _ = window.emit(
                        "close-blocked",
                        "Finish or cancel active jobs and unmount runtime sessions before closing.",
                    );
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            inspect_installation,
            discover_launch,
            optimize_preflight,
            test_readiness,
            game_locations,
            inspect_location,
            get_snapshot,
            export_diagnostics,
            select_folder,
            add_game,
            remove_game,
            forget_missing_store,
            update_settings,
            configure_launch,
            start_job,
            cancel_job,
            get_system_status,
            get_storage_statistics,
            runtime_action
        ]);
    #[cfg(feature = "acceptance")]
    let builder = builder.invoke_handler(tauri::generate_handler![
        inspect_installation,
        discover_launch,
        optimize_preflight,
        test_readiness,
        game_locations,
        inspect_location,
        get_snapshot,
        export_diagnostics,
        select_folder,
        add_game,
        remove_game,
        forget_missing_store,
        update_settings,
        configure_launch,
        start_job,
        cancel_job,
        get_system_status,
        get_storage_statistics,
        runtime_action,
        acceptance_report,
        acceptance_probe
    ]);
    builder
        .build(tauri::generate_context!())
        .expect("Unable to build PlaySparse desktop")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event
                && !app.state::<Backend>().can_close()
            {
                api.prevent_exit();
                let _ = app.emit(
                    "close-blocked",
                    "Finish or cancel jobs and unmount sessions before quitting.",
                );
            }
        });
}
