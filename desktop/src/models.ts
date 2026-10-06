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
  destination_required_estimated_bytes?: number;
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

export interface LaunchCandidate {
  executable: string;
  label: string;
  kind: string;
  rank: number;
  note: string;
}
export interface InstallationInspection {
  source: string;
  title: string;
  logical_bytes: number;
  files: number;
  candidates: LaunchCandidate[];
  discovery_truncated: boolean;
}
export interface Readiness {
  state: "Ready" | "Action required" | "Unsupported" | "Unknown";
  can_attempt_mount: boolean;
  guidance: string;
  arch: string | null;
  platform: string | null;
  driver: string | null;
  mount_test: string | null;
  helpers: string[];
}
export function readinessOf(system: unknown): Readiness {
  if (system && typeof system === "object" && "readiness" in system)
    return system.readiness as Readiness;
  return {
    state: "Unknown",
    can_attempt_mount: false,
    guidance:
      "Open the desktop application to check macFUSE, WinFsp or FUSE. Driver presence does not prove mount capability.",
    arch: null,
    platform: null,
    driver: null,
    mount_test: null,
    helpers: [],
  };
}
export function stageLabel(stage: string): string {
  return (
    (
      {
        measuring_source: "Measuring source and temporary requirements",
        fixed_scanning: "Scanning files for fixed chunks",
        testing_fixed_chunks: "Testing fixed chunks and compression",
        fixed_verifying: "Verifying fixed-chunk measurements",
        fixed_finalizing: "Finalizing fixed-chunk measurements",
        cdc_scanning: "Scanning files for content-defined chunks",
        testing_cdc_chunks: "Testing content-defined chunks and compression",
        cdc_verifying: "Verifying content-defined measurements",
        cdc_finalizing: "Finalizing content-defined measurements",
        finalizing_analysis: "Finalizing analysis",
        scanning: "Scanning files",
        packing: "Building compressed store",
        verifying: "Verifying exact bytes",
        publishing: "Publishing atomically",
      } as Record<string, string>
    )[stage] ?? stage.replaceAll("_", " ")
  );
}
