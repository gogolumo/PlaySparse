export type Operation = "analyze" | "optimize" | "verify";
export interface Settings {
  theme: "light" | "dark" | "system";
  storage_dir: string;
  temp_dir: string;
  cache_mib: number;
  retain_logs: boolean;
}
export interface PackStats {
  logical_bytes: number;
  physical_bytes: number;
  allocated_bytes: number | null;
  files: number;
  metadata_bytes?: number;
}
export interface Analysis {
  logical_bytes: number;
  files: number;
  original_allocated_bytes: number | null;
  exact_duplicate_file_bytes: number;
  cdc_duplicate_reuse_bytes: number;
  unique_compressible_raw_bytes: number;
  unique_incompressible_raw_bytes: number;
  cdc_chunks: PackStats;
  temporary_workspace: {
    required_estimated_bytes: number;
    location: string;
    available_bytes: number;
  };
}
export interface LaunchDescriptor {
  executable: string;
  args: string[];
  compatibility_confirmed: boolean;
}
export interface Game {
  id: string;
  name: string;
  source: string;
  analysis: Analysis | null;
  store: string | null;
  verified: boolean;
  store_stats: PackStats | null;
  overlay_allocated_bytes: number | null;
  error: string | null;
  launch: LaunchDescriptor | null;
  session: {
    state: string;
    mountpoint: string;
    overlay: string;
    error: string | null;
  } | null;
}
export interface Job {
  id: string;
  game_id: string;
  operation: Operation | "mount" | "launch" | "stop" | "unmount" | "recover";
  state: string;
  stage: string;
  bytes: number;
  files: number;
  started_at: number;
  finished_at: number | null;
  error: string | null;
  cancellable: boolean;
}
export interface Snapshot {
  version: number;
  games: Game[];
  jobs: Job[];
  settings: Settings;
}
export function bytes(value: number | null | undefined): string {
  if (value == null) return "Unavailable";
  if (value === 0) return "0 B";
  const index = Math.min(
    4,
    Math.floor(Math.log(Math.abs(value)) / Math.log(1024)),
  );
  return `${(value / 1024 ** Math.max(0, index)).toFixed(index > 1 ? 2 : 0)} ${["B", "KiB", "MiB", "GiB", "TiB"][Math.max(0, index)]}`;
}
export function savings(
  original: number | null | undefined,
  optimized: number | null | undefined,
): number | null {
  return original != null && optimized != null && original > 0
    ? ((original - optimized) / original) * 100
    : null;
}
export function status(game: Game, jobs: Job[]): string {
  const active = jobs.find(
    (j) => j.game_id === game.id && j.state === "running",
  );
  if (
    active &&
    ["mount", "launch", "stop", "unmount", "recover"].includes(active.operation)
  )
    return (
      {
        mount: "Mounting",
        launch: "Launching",
        stop: "Stopping",
        unmount: "Unmounting",
        recover: "Reconciling",
      } as Record<string, string>
    )[active.operation];
  if (active)
    return active.operation === "analyze"
      ? "Analyzing"
      : active.stage.startsWith("verif")
        ? "Verifying"
        : active.operation === "optimize"
          ? "Optimizing"
          : "Verifying";
  if (game.session)
    return (
      {
        running: "Running",
        mounted: "Mounted",
        preparing: "Preparing",
        needs_attention: "Needs attention",
      }[game.session.state] ?? "Needs attention"
    );
  const last = jobs.filter((j) => j.game_id === game.id).at(-1);
  if (game.error || last?.state === "failed" || last?.state === "interrupted")
    return "Needs attention";
  if (game.store) return game.verified ? "Ready" : "Verify required";
  return game.analysis ? "Ready to optimize" : "Not analyzed";
}

export function effectiveBytes(game: Game): number | null {
  const store = game.store_stats?.allocated_bytes;
  return store != null && game.overlay_allocated_bytes != null
    ? store + game.overlay_allocated_bytes
    : null;
}
