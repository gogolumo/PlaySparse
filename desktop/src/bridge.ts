import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type {
  Analysis,
  InstallationInspection,
  LaunchCandidate,
  Game,
  LaunchDescriptor,
  Operation,
  Settings,
  Snapshot,
} from "./models";

export const preview = !("__TAURI_INTERNALS__" in window);
const GiB = 1024 ** 3;
const analysis: Analysis = {
  logical_bytes: 9.93 * GiB,
  original_allocated_bytes: 9.93 * GiB,
  files: 18342,
  exact_duplicate_file_bytes: 0.46 * GiB,
  cdc_duplicate_reuse_bytes: 0.82 * GiB,
  unique_compressible_raw_bytes: 7.2 * GiB,
  unique_incompressible_raw_bytes: 1.9 * GiB,
  cdc_chunks: {
    logical_bytes: 9.93 * GiB,
    physical_bytes: 5.68 * GiB,
    allocated_bytes: 5.68 * GiB,
    files: 18342,
  },
  temporary_workspace: {
    required_estimated_bytes: 21.4 * GiB,
    location: "/Preview/Temporary",
    available_bytes: 120 * GiB,
  },
};
const example: Game = {
  id: "preview-zomboid",
  name: "Project Zomboid",
  source: "/Preview/Games/Project Zomboid",
  analysis,
  store: "/Preview/Stores/project-zomboid",
  store_stats: analysis.cdc_chunks,
  verified: true,
  overlay_allocated_bytes: 0,
  session: null,
  error: null,
  launch: null,
};
const params = new URLSearchParams(location.search);
const demo: Snapshot = {
  version: 1,
  games: params.has("empty") ? [] : [example],
  jobs: [],
  settings: {
    theme: params.get("theme") === "light" ? "light" : "dark",
    storage_dir: "/Preview/Stores",
    temp_dir: "/Preview/Temporary",
    cache_mib: 256,
    retain_logs: true,
  },
};
if (params.get("view") === "recovery") {
  demo.games[0].verified = false;
  demo.games[0].error =
    "Store is missing (simulated). Inspect the previous session before recovery.";
  demo.games[0].session = {
    state: "needs_attention",
    mountpoint: "/Preview/Runtime/mount",
    overlay: "/Preview/Runtime/overlay",
    error: "Interrupted session (simulated); no persisted PID is killed.",
  };
}
if (params.get("view") === "optimization")
  demo.jobs.push({
    id: "preview-job",
    game_id: example.id,
    operation: "optimize",
    state: "running",
    stage: "packing",
    bytes: 2.84 * GiB,
    files: 4820,
    started_at: Date.now() - 32000,
    finished_at: null,
    error: null,
    cancellable: true,
  });
const copy = () => structuredClone(demo);
export const getSnapshot = async (): Promise<Snapshot> =>
  preview ? copy() : invoke("get_snapshot");
export async function subscribe(
  onState: (state: Snapshot) => void,
  onError: (error: string) => void,
): Promise<() => void> {
  if (preview) return () => {};
  const state = await listen<Snapshot>("desktop-state", (e) =>
    onState(e.payload),
  );
  const close = await listen<string>("close-blocked", (e) =>
    onError(e.payload),
  );
  return () => {
    state();
    close();
  };
}
export async function selectFolder(): Promise<string | null> {
  return preview ? "/Preview/Games/New installation" : invoke("select_folder");
}
export async function addGame(path: string): Promise<void> {
  if (!preview) {
    await invoke("add_game", { path });
    return;
  }
  demo.games.push({
    ...structuredClone(example),
    id: `preview-${Date.now()}`,
    name: path.split("/").at(-1) || "Installation",
    source: path,
    analysis: null,
    store: null,
    store_stats: null,
    verified: false,
  });
}
export async function removeGame(id: string) {
  if (preview) demo.games = demo.games.filter((g) => g.id !== id);
  else await invoke("remove_game", { id });
}
export async function updateSettings(settings: Settings) {
  if (preview) demo.settings = settings;
  else await invoke("update_settings", { settings });
}
export async function configureLaunch(
  id: string,
  descriptor: LaunchDescriptor,
) {
  if (preview) {
    const g = demo.games.find((g) => g.id === id);
    if (g) g.launch = descriptor;
  } else await invoke("configure_launch", { id, descriptor });
}
export async function startJob(id: string, operation: Operation) {
  if (!preview) return invoke("start_job", { id, operation });
  const game = demo.games.find((g) => g.id === id);
  if (!game) throw Error("Unknown preview installation");
  if (operation === "analyze") game.analysis = structuredClone(analysis);
  if (operation === "optimize") {
    game.store = `/Preview/Stores/${id}`;
    game.store_stats = analysis.cdc_chunks;
    game.verified = true;
  }
  if (operation === "verify") game.verified = true;
  demo.jobs.push({
    id: `preview-${Date.now()}`,
    game_id: id,
    operation,
    state: "completed",
    stage: "complete",
    bytes: analysis.logical_bytes,
    files: analysis.files,
    started_at: Date.now(),
    finished_at: Date.now(),
    error: null,
    cancellable: false,
  });
}
export async function cancelJob(id: string) {
  if (!preview) return invoke("cancel_job", { id });
  const job = demo.jobs.find((j) => j.id === id);
  if (job) {
    job.state = "cancelled";
    job.finished_at = Date.now();
  }
}
export async function runtimeAction(
  id: string,
  action: string,
  processesClosed = false,
) {
  if (preview)
    throw Error(
      "Runtime operations require the native PlaySparse app. Preview mode never mounts or launches a game.",
    );
  await invoke("runtime_action", { id, action, processesClosed });
}
export async function diagnostics(): Promise<unknown> {
  return preview
    ? {
        mode: "Preview Mode",
        engine: "Simulated; native diagnostics unavailable",
        platform: "Browser",
        runtime:
          "Open the desktop application to check macFUSE, WinFsp or FUSE",
      }
    : invoke("get_system_status");
}
export async function storageStatistics(): Promise<unknown> {
  return preview
    ? {
        mode: "Preview Mode",
        originals_retained: true,
        note: "Reference representations are simulated. No disk space was reclaimed.",
      }
    : invoke("get_storage_statistics");
}

export async function inspectInstallation(
  path: string,
): Promise<InstallationInspection> {
  return preview
    ? {
        source: path,
        title: path.split("/").at(-1) || "Installation",
        logical_bytes: analysis.logical_bytes,
        files: analysis.files,
        candidates: [
          {
            executable: "bin/game",
            label: "Game executable",
            kind: "executable",
            rank: 80,
            note: "Simulated candidate; not compatibility evidence.",
          },
        ],
        discovery_truncated: false,
      }
    : invoke("inspect_installation", { path });
}
export async function discoverLaunch(id: string): Promise<LaunchCandidate[]> {
  return preview
    ? (await inspectInstallation("/Preview/Game")).candidates
    : invoke("discover_launch", { id });
}
export async function optimizePreflight(id: string): Promise<{
  destination: string;
  required_estimated_bytes: number;
  available_bytes: number;
}> {
  return preview
    ? {
        destination: `${demo.settings.storage_dir}/${id}`,
        required_estimated_bytes: 11 * GiB,
        available_bytes: 120 * GiB,
      }
    : invoke("optimize_preflight", { id });
}
export async function testReadiness(): Promise<unknown> {
  return preview ? diagnostics() : invoke("test_readiness");
}

export async function forgetMissingStore(id: string): Promise<void> {
  if (preview) throw Error("Preview store still exists (simulated).");
  await invoke("forget_missing_store", { id });
}

export async function gameLocations(id: string): Promise<{
  source: string;
  store: string | null;
  overlay: string;
  mount: string | null;
}> {
  if (preview) {
    const game = demo.games.find((g) => g.id === id);
    return {
      source: game?.source ?? "",
      store: game?.store ?? null,
      overlay: `/Preview/Runtime/${id}/overlay`,
      mount: null,
    };
  }
  return invoke("game_locations", { id });
}
export async function inspectLocation(id: string, kind: string): Promise<void> {
  if (preview)
    throw Error(
      "Inspect folders in the native app; preview has no filesystem access.",
    );
  await invoke("inspect_location", { id, kind });
}

export async function exportDiagnostics(): Promise<string> {
  if (preview) throw Error("Diagnostic export requires the native app.");
  return invoke("export_diagnostics");
}
