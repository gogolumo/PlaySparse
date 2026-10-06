use playsparse_desktop::{LaunchDescriptor, Operation, Service, Settings, Snapshot};
use serde_json::Value;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};
use tauri_plugin_dialog::DialogExt;

type Backend = Arc<Service>;
type Reply<T> = Result<T, String>;

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

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.set_focus();
            }
        }))
        .plugin(tauri_plugin_dialog::init())
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
            let service = Service::open(&root, &engine)?;
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
            get_snapshot,
            select_folder,
            add_game,
            remove_game,
            update_settings,
            configure_launch,
            start_job,
            cancel_job,
            get_system_status,
            get_storage_statistics,
            runtime_action
        ])
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
