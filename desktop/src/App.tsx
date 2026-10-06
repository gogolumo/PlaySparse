import { useCallback, useEffect, useRef, useState } from "react";
import {
  Activity,
  ArrowDown,
  ArrowRight,
  Check,
  ChevronRight,
  CircleHelp,
  FolderOpen,
  HardDrive,
  Layers3,
  LayoutGrid,
  LoaderCircle,
  MoreHorizontal,
  Play,
  Plus,
  Search,
  Settings2,
  ShieldCheck,
  Square,
  X,
} from "lucide-react";
import * as bridge from "./bridge";
import {
  bytes,
  savings,
  status,
  effectiveBytes,
  readinessOf,
  stageLabel,
} from "./models";
import type {
  Game,
  Operation,
  Settings,
  Snapshot,
  InstallationInspection,
  LaunchCandidate,
} from "./models";

type Page = "Library" | "Activity" | "Storage" | "Settings";
const sections = [
  { name: "Library" as Page, icon: LayoutGrid },
  { name: "Activity" as Page, icon: Activity },
  { name: "Storage" as Page, icon: HardDrive },
  { name: "Settings" as Page, icon: Settings2 },
];
function Brand() {
  return (
    <span className="brandmark" aria-hidden="true">
      <i />
      <i />
      <i />
      <i />
    </span>
  );
}
function Metric({
  label,
  value,
  green = false,
}: {
  label: string;
  value: string;
  green?: boolean;
}) {
  return (
    <div className="metric">
      <span>{label}</span>
      <strong className={green ? "green" : ""}>{value}</strong>
    </div>
  );
}
function Modal({
  title,
  children,
  close,
}: {
  title: string;
  children: React.ReactNode;
  close: () => void;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const dialog = ref.current;
    dialog?.showModal();
    return () => dialog?.close();
  }, []);
  return (
    <dialog ref={ref} onCancel={close} aria-label={title}>
      <div className="dialog-head">
        <h2>{title}</h2>
        <button aria-label="Close dialog" onClick={close}>
          <X size={18} />
        </button>
      </div>
      {children}
    </dialog>
  );
}
export default function App() {
  const [data, setData] = useState<Snapshot | null>(null);
  const initialView = new URLSearchParams(location.search).get("view");
  const [page, setPage] = useState<Page>(
    initialView === "settings"
      ? "Settings"
      : initialView === "optimization"
        ? "Activity"
        : "Library",
  );
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const [query, setQuery] = useState("");
  const [addPath, setAddPath] = useState<string | null>(
    initialView === "add" ? "/Preview/Games/New installation" : null,
  );
  const [detail, setDetail] = useState<string | null>(
    initialView === "analysis" ? "preview-zomboid" : null,
  );
  const [confirm, setConfirm] = useState<{
    title: string;
    text: string;
    run: () => Promise<unknown>;
  } | null>(null);
  const [system, setSystem] = useState<unknown>(null);
  const [storage, setStorage] = useState<unknown>(null);
  const [draft, setDraft] = useState<Settings | null>(null);
  const [inspection, setInspection] = useState<InstallationInspection | null>(
    null,
  );
  const [locations, setLocations] = useState<{ overlay: string } | null>(null);
  const [candidates, setCandidates] = useState<LaunchCandidate[]>([]);
  const [executable, setExecutable] = useState("");
  const [args, setArgs] = useState("");
  const [compatible, setCompatible] = useState(false);
  useEffect(() => {
    if (!detail) {
      setCandidates([]);
      return;
    }
    let current = true;
    bridge
      .getSnapshot()
      .then((state) => {
        const game = state.games.find((g) => g.id === detail);
        if (current) {
          setExecutable(game?.launch?.executable ?? "");
          setArgs((game?.launch?.args ?? []).join("\n"));
          setCompatible(game?.launch?.compatibility_confirmed ?? false);
        }
      })
      .catch((e) => setError(String(e)));
    bridge
      .gameLocations(detail)
      .then((result) => {
        if (current) setLocations(result);
      })
      .catch((e) => setError(String(e)));
    bridge
      .discoverLaunch(detail)
      .then((found) => {
        if (current) setCandidates(found);
      })
      .catch((e) => {
        if (current)
          setError(
            `Launch discovery: ${String(e)}. Restore the source folder or configure a target after recovery.`,
          );
      });
    return () => {
      current = false;
    };
  }, [detail]);
  useEffect(() => {
    setInspection(null);
    if (!addPath) return;
    let current = true;
    const timer = setTimeout(() => {
      bridge
        .inspectInstallation(addPath)
        .then((result) => {
          if (current) setInspection(result);
        })
        .catch((e) => {
          if (current) setError(String(e));
        });
    }, 200);
    return () => {
      current = false;
      clearTimeout(timer);
    };
  }, [addPath]);
  const refresh = useCallback(async () => {
    const state = await bridge.getSnapshot();
    setData(state);
  }, []);
  useEffect(() => {
    let active = true;
    let cleanup = () => {};
    bridge
      .getSnapshot()
      .then((state) => {
        if (active) {
          setData(state);
          setDraft(state.settings);
        }
      })
      .catch((e) => setError(String(e)));
    bridge
      .diagnostics()
      .then((result) => {
        if (active) setSystem(result);
      })
      .catch((e) => setError(String(e)));
    bridge
      .subscribe(
        (state) => {
          if (active) setData(state);
        },
        (e) => setError(e),
      )
      .then((fn) => {
        if (active) cleanup = fn;
        else fn();
      })
      .catch((e) => setError(String(e)));
    return () => {
      active = false;
      cleanup();
    };
  }, []);
  useEffect(() => {
    const theme = data?.settings.theme ?? "system";
    const mq = matchMedia("(prefers-color-scheme: dark)");
    const update = () => {
      document.documentElement.dataset.theme =
        theme === "system" ? (mq.matches ? "dark" : "light") : theme;
    };
    update();
    mq.addEventListener("change", update);
    return () => mq.removeEventListener("change", update);
  }, [data?.settings.theme]);
  const action = useCallback(
    async (run: () => Promise<unknown>) => {
      setError(null);
      setPending(true);
      try {
        await run();
        await refresh();
      } catch (e) {
        setError(String(e));
      } finally {
        setPending(false);
      }
    },
    [refresh],
  );
  const add = useCallback(() => {
    void action(async () => {
      const path = await bridge.selectFolder();
      if (path) setAddPath(path);
    });
  }, [action]);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "o") {
        e.preventDefault();
        add();
      }
      if ((e.metaKey || e.ctrlKey) && ["1", "2", "3", "4"].includes(e.key)) {
        e.preventDefault();
        setPage(sections[Number(e.key) - 1].name);
      }
    };
    window.addEventListener("keydown", key);
    return () => window.removeEventListener("keydown", key);
  }, [add]);
  if (!data)
    return (
      <main className="loading">
        <Brand />
        <h2>Opening your library</h2>
        {error ? (
          <p role="alert">{error}</p>
        ) : (
          <LoaderCircle className="spin" />
        )}
      </main>
    );
  const selected = data.games.find((g) => g.id === detail);
  const readiness = readinessOf(system);
  const canMount = bridge.preview || readiness.can_attempt_mount;
  const active = data.jobs.some((j) => j.state === "running");
  const busy = pending || active;
  const measured = data.games.filter(
    (g) =>
      g.verified &&
      effectiveBytes(g) != null &&
      g.analysis?.original_allocated_bytes != null,
  );
  const original = measured.reduce(
    (n, g) => n + g.analysis!.original_allocated_bytes!,
    0,
  );
  const optimized = measured.reduce((n, g) => n + effectiveBytes(g)!, 0);
  const runJob = (id: string, operation: Operation) =>
    action(() => bridge.startJob(id, operation));
  const remove = (game: Game) =>
    setConfirm({
      title: "Remove from library?",
      text: `${game.name} will be unregistered. Its original installation, store and overlay files stay on disk.`,
      run: () => bridge.removeGame(game.id),
    });
  const runtime = (game: Game, task: string) => {
    if (task === "unmount" || task === "stop")
      setConfirm({
        title:
          task === "unmount"
            ? "Safely unmount runtime?"
            : "Stop the tracked process?",
        text:
          task === "unmount"
            ? "Confirm every game, launcher and child process is closed. Ordinary unmount will preserve your overlay and refuse a busy runtime."
            : "The tracked process will be terminated. Save your game first. Launcher and child processes may still need to be closed manually.",
        run: () => bridge.runtimeAction(game.id, task, true),
      });
    else void action(() => bridge.runtimeAction(game.id, task));
  };
  const readinessPanel = (
    <section className="panel readiness-panel" aria-label="System readiness">
      <details
        open={
          page === "Settings" ||
          data.games.length === 0 ||
          ["Action required", "Unsupported"].includes(readiness.state)
        }
      >
        <summary className="section-title">
          System readiness{" "}
          <span
            className={`status ${readiness.state === "Ready" ? "success" : ""}`}
          >
            {readiness.state}
          </span>
        </summary>
        <p>
          {readiness.driver ?? "Native runtime"} ·{" "}
          {readiness.platform ?? "Browser preview"} ·{" "}
          {readiness.arch ?? "Unknown architecture"}
        </p>
        <p>{readiness.guidance}</p>
        <p className="footnote">
          Analysis and optimization work without a mount driver. Mounting is{" "}
          {readiness.can_attempt_mount
            ? "available to attempt; actual permission is checked when mounting"
            : "unavailable or unverified"}
          . Kernel approval and restart requirements are not inferred.
        </p>
        <div className="panel-actions">
          <button
            disabled={busy}
            onClick={() =>
              void action(async () => setSystem(await bridge.diagnostics()))
            }
          >
            Run diagnostics again
          </button>
          <button
            disabled={busy}
            onClick={() =>
              void action(async () => setSystem(await bridge.testReadiness()))
            }
          >
            Test mount readiness
          </button>
        </div>
        <details>
          <summary>Technical details</summary>
          <pre>{JSON.stringify(system, null, 2)}</pre>
        </details>
      </details>
    </section>
  );
  return (
    <div className="shell">
      <aside className="sidebar">
        <div className="brand">
          <Brand />
          <span>PlaySparse</span>
          <small>DESKTOP</small>
        </div>
        <div className="nav-label">WORKSPACE</div>
        <nav aria-label="Main navigation">
          {sections.map(({ name, icon: Icon }) => (
            <button
              key={name}
              aria-current={page === name ? "page" : undefined}
              onClick={() => setPage(name)}
            >
              <Icon size={18} />
              <span>{name}</span>
              {name === "Library" && <small>{data.games.length}</small>}
              {name === "Activity" && active && <span className="dot" />}
            </button>
          ))}
        </nav>
        <div className="sidebar-bottom">
          <div className="source-note">
            <ShieldCheck size={19} />
            <strong>Your originals stay yours</strong>
            <p>
              PlaySparse creates separate stores. Your installed games remain
              untouched.
            </p>
          </div>
          <div className="version">
            <span className="dot" />
            Rust storage engine<span>v0.1.0</span>
          </div>
        </div>
      </aside>
      <div className="workspace">
        <header className="topbar">
          <span>
            Workspace <ChevronRight size={13} /> <strong>{page}</strong>
          </span>
          <span className="topbar-right">
            {bridge.preview && (
              <b className="preview-badge">Preview Mode · simulated data</b>
            )}
            <button
              aria-label="About runtime compatibility"
              title="Compatibility and diagnostics"
              onClick={() => setPage("Settings")}
            >
              <CircleHelp size={17} />
            </button>
          </span>
        </header>
        <main className="content">
          {error && (
            <div className="error-banner" role="alert">
              <span>{error}</span>
              <button onClick={() => setError(null)} aria-label="Dismiss error">
                <X size={16} />
              </button>
            </div>
          )}
          <div className="page-heading">
            <div>
              <div className="eyebrow">
                {page === "Library"
                  ? "A LITTLE LESS SPACE. A LOT MORE ROOM."
                  : "YOUR PLAYSPARSE WORKSPACE"}
              </div>
              <h1>
                {page === "Library"
                  ? "Your games, lighter."
                  : page === "Storage"
                    ? "Every byte accounted for."
                    : page === "Activity"
                      ? "Work in progress."
                      : "Make it yours."}
              </h1>
              <p>
                {page === "Library"
                  ? "Optimize your library. Keep the games you love."
                  : page === "Storage"
                    ? "Measured storage representations, with originals preserved."
                    : page === "Activity"
                      ? "Real engine work, from the first scan to a verified store."
                      : "Preferences, storage locations and runtime readiness."}
              </p>
            </div>
            {page === "Library" && (
              <button className="primary" onClick={add} disabled={busy}>
                <Plus size={17} />
                Add Game
              </button>
            )}
          </div>
          {page === "Library" && (
            <>
              {readinessPanel}
              <div className="library-toolbar">
                <div className="section-title">
                  Library <span>{data.games.length}</span>
                </div>
                <label className="search">
                  <Search size={15} />
                  <input
                    aria-label="Search games"
                    placeholder="Search your games"
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                  />
                  <kbd>⌘ O</kbd>
                </label>
              </div>
              {!data.games.length ? (
                <div className="empty">
                  <div className="empty-icon">
                    <Layers3 size={32} />
                  </div>
                  <h2>A fresh start for your library.</h2>
                  <p>
                    Add an installed game to measure its storage.
                    <br />
                    Nothing in the original folder will be changed.
                  </p>
                  <button className="primary" disabled={busy} onClick={add}>
                    <Plus size={17} />
                    Add your first game
                  </button>
                  <small>
                    Start with analysis. Optimize when you’re ready.
                  </small>
                </div>
              ) : (
                <div className="game-grid">
                  {data.games
                    .filter((g) =>
                      g.name.toLowerCase().includes(query.toLowerCase()),
                    )
                    .map((game) => {
                      const label = status(game, data.jobs);
                      const source = game.analysis?.original_allocated_bytes;
                      const stored = effectiveBytes(game);
                      const percent = savings(source, stored);
                      const ownJob = data.jobs.find(
                        (j) => j.game_id === game.id && j.state === "running",
                      );
                      return (
                        <article className="game-card" key={game.id}>
                          <div className="game-top">
                            <div className="game-icon">
                              <Layers3 size={25} />
                            </div>
                            <div className="game-title">
                              <h2>{game.name}</h2>
                              <span>
                                {game.store
                                  ? "PlaySparse store"
                                  : "Installed folder"}
                              </span>
                            </div>
                            <span
                              className={`status ${["Ready", "Mounted", "Running"].includes(label) ? "success" : ""}`}
                            >
                              {label === "Ready" ? (
                                <Check size={12} />
                              ) : (
                                <span className="dot" />
                              )}
                              {label}
                            </span>
                          </div>
                          <div className="card-metrics">
                            <Metric
                              label="Original allocated"
                              value={bytes(source)}
                            />
                            <Metric
                              label="Effective allocated"
                              value={bytes(stored)}
                            />
                            <Metric
                              label="Representation saved"
                              green={percent != null && percent > 0}
                              value={
                                percent == null ? "—" : `${percent.toFixed(2)}%`
                              }
                            />
                          </div>
                          <div
                            className="storage-bar"
                            aria-label={
                              percent == null
                                ? "Analyze to measure storage"
                                : `Store is ${(100 - percent).toFixed(1)} percent of original allocation`
                            }
                          >
                            <span
                              style={{
                                width:
                                  percent == null
                                    ? "0%"
                                    : `${Math.min(100, Math.max(0, 100 - percent))}%`,
                              }}
                            />
                          </div>
                          <div className="bar-caption">
                            <span>
                              <i />{" "}
                              {stored == null
                                ? game.store
                                  ? "Measure runtime storage"
                                  : "No store yet"
                                : "Store + overlay"}
                            </span>
                            <span>
                              {source == null || stored == null
                                ? "Savings not measured"
                                : `${bytes(Math.abs(source - stored))} ${source >= stored ? "smaller" : "larger"}`}
                            </span>
                          </div>
                          {ownJob && (
                            <div className="inline-progress">
                              <LoaderCircle size={15} className="spin" />
                              {stageLabel(ownJob.stage)} · {bytes(ownJob.bytes)}{" "}
                              processed
                            </div>
                          )}
                          <div className="card-footer">
                            <button
                              className={
                                game.verified ? "launch-button" : "primary"
                              }
                              disabled={busy}
                              onClick={() => {
                                if (game.session?.state === "running")
                                  runtime(game, "stop");
                                else if (
                                  game.session?.state === "mounted" &&
                                  game.launch?.compatibility_confirmed
                                )
                                  runtime(game, "launch");
                                else if (game.session) setDetail(game.id);
                                else if (game.verified) {
                                  if (canMount) runtime(game, "mount");
                                  else setPage("Settings");
                                } else if (game.store)
                                  void runJob(game.id, "verify");
                                else if (game.analysis) setDetail(game.id);
                                else void runJob(game.id, "analyze");
                              }}
                            >
                              {game.session?.state === "running" ? (
                                <Square size={15} />
                              ) : game.verified ? (
                                <Play size={15} />
                              ) : (
                                <ArrowRight size={15} />
                              )}{" "}
                              {game.session?.state === "running"
                                ? "Stop Game"
                                : game.session?.state === "mounted" &&
                                    game.launch?.compatibility_confirmed
                                  ? "Launch Game"
                                  : game.session
                                    ? "Manage Runtime"
                                    : game.verified
                                      ? canMount
                                        ? "Mount Store"
                                        : "Diagnose Runtime"
                                      : game.store
                                        ? "Verify Store"
                                        : game.analysis
                                          ? "Review & Optimize"
                                          : "Analyze Game"}
                            </button>
                            <button
                              className="icon-button"
                              aria-label={`Manage ${game.name}`}
                              title="Analysis and options"
                              onClick={() => {
                                setDetail(game.id);
                                setExecutable(game.launch?.executable ?? "");
                                setArgs(
                                  JSON.stringify(game.launch?.args ?? []),
                                );
                                setCompatible(
                                  game.launch?.compatibility_confirmed ?? false,
                                );
                              }}
                            >
                              <MoreHorizontal size={20} />
                            </button>
                          </div>
                        </article>
                      );
                    })}
                </div>
              )}
              <section className="overview">
                <div className="section-title">
                  Storage overview <HardDrive size={16} />
                </div>
                <div className="overview-grid">
                  <Metric
                    label="Games managed"
                    value={String(data.games.length)}
                  />
                  <Metric
                    label="Verified stores"
                    value={String(data.games.filter((g) => g.verified).length)}
                  />
                  <Metric
                    label="Representation savings"
                    value={
                      measured.length
                        ? bytes(original - optimized)
                        : "Not measured"
                    }
                    green={original > optimized}
                  />
                  <div className="overview-note">
                    <ShieldCheck size={18} />
                    <p>
                      Originals remain installed.
                      <br />
                      <span>
                        These savings do not mean disk space was reclaimed.
                      </span>
                    </p>
                  </div>
                </div>
              </section>
            </>
          )}
          {page === "Activity" && (
            <div className="activity-list">
              {!data.jobs.length ? (
                <div className="empty">
                  <Activity size={32} />
                  <h2>All quiet here.</h2>
                  <p>Analysis, packing and verification jobs appear here.</p>
                </div>
              ) : (
                [...data.jobs].reverse().map((job) => (
                  <article key={job.id} className="activity-row">
                    <div
                      className={`activity-icon ${job.state === "completed" ? "green" : ""}`}
                    >
                      {job.state === "running" ? (
                        <LoaderCircle className="spin" size={20} />
                      ) : job.state === "completed" ? (
                        <Check size={20} />
                      ) : (
                        <Square size={18} />
                      )}
                    </div>
                    <div className="activity-details">
                      <h3>
                        {job.operation === "optimize"
                          ? "Create optimized store"
                          : job.operation === "analyze"
                            ? "Analyze installation"
                            : job.operation === "verify"
                              ? "Verify store"
                              : `${job.operation.charAt(0).toUpperCase()}${job.operation.slice(1)} runtime`}{" "}
                        <span>
                          {data.games.find((g) => g.id === job.game_id)?.name ??
                            job.game_id}
                        </span>
                      </h3>
                      <p>
                        {stageLabel(job.stage)} · {bytes(job.bytes)} processed ·{" "}
                        {job.files.toLocaleString()} files ·{" "}
                        {Math.max(
                          0,
                          Math.round(
                            ((job.finished_at ?? Date.now()) - job.started_at) /
                              1000,
                          ),
                        )}
                        s elapsed
                      </p>
                      {job.state === "running" && (
                        <div
                          className="indeterminate"
                          aria-label="Operation in progress"
                        />
                      )}
                      {job.error && (
                        <pre className="job-error">{job.error}</pre>
                      )}
                    </div>
                    <span className="status">{job.state}</span>
                    {job.state === "running" && job.cancellable && (
                      <button
                        onClick={() =>
                          void action(() => bridge.cancelJob(job.id))
                        }
                      >
                        Cancel
                      </button>
                    )}
                  </article>
                ))
              )}
            </div>
          )}
          {page === "Storage" && (
            <>
              <section className="panel">
                <div className="section-title">
                  Storage representations{" "}
                  <button
                    onClick={() =>
                      void action(async () =>
                        setStorage(await bridge.storageStatistics()),
                      )
                    }
                  >
                    <ArrowDown size={14} />
                    Measure now
                  </button>
                </div>
                <p className="muted">
                  Each store is independent. Runtime caches use memory.
                  Compatibility shadows are not automatically created by this
                  version.
                </p>
                <div className="storage-table">
                  <div className="table-row table-head">
                    <span>Installation</span>
                    <span>Original</span>
                    <span>Store + overlay</span>
                    <span>Difference</span>
                  </div>
                  {data.games.map((g) => (
                    <div className="table-row" key={g.id}>
                      <strong>{g.name}</strong>
                      <span>{bytes(g.analysis?.original_allocated_bytes)}</span>
                      <span>{bytes(effectiveBytes(g))}</span>
                      <span className="green">
                        {g.analysis?.original_allocated_bytes != null &&
                        effectiveBytes(g) != null
                          ? bytes(
                              g.analysis.original_allocated_bytes -
                                effectiveBytes(g)!,
                            )
                          : "—"}
                      </span>
                    </div>
                  ))}
                </div>
                <p className="footnote">
                  Card values are from the last completed analysis or pack.
                  Measure now includes current overlays and errors. Unknown
                  allocation is never substituted with logical size.
                  Cross-installation hardlinks and reflinks are not
                  deduplicated.
                </p>
              </section>
              {storage != null && (
                <section className="panel">
                  <h2>Current disk measurements</h2>
                  <pre>{JSON.stringify(storage, null, 2)}</pre>
                </section>
              )}
            </>
          )}
          {page === "Settings" && draft && (
            <>
              <section className="panel settings-panel">
                <div className="section-title">Preferences</div>
                <div className="setting-row">
                  <div>
                    <h3>Appearance</h3>
                    <p>A comfortable workspace, day or night.</p>
                  </div>
                  <div className="segmented">
                    {(["light", "dark", "system"] as const).map((theme) => (
                      <button
                        key={theme}
                        aria-pressed={draft.theme === theme}
                        onClick={() => {
                          const next = { ...draft, theme };
                          setDraft(next);
                          void action(() => bridge.updateSettings(next));
                        }}
                      >
                        {theme}
                      </button>
                    ))}
                  </div>
                </div>
                <label className="setting-field">
                  <span>
                    Store directory
                    <small>
                      Applies only to new stores. Existing stores are not
                      migrated.
                    </small>
                  </span>
                  <input
                    value={draft.storage_dir}
                    onChange={(e) =>
                      setDraft({ ...draft, storage_dir: e.target.value })
                    }
                  />
                  <button
                    aria-label="Choose store directory"
                    onClick={() =>
                      void action(async () => {
                        const p = await bridge.selectFolder();
                        if (p) setDraft({ ...draft, storage_dir: p });
                      })
                    }
                  >
                    <FolderOpen size={17} />
                  </button>
                </label>
                <label className="setting-field">
                  <span>
                    Temporary directory
                    <small>
                      Applies to new analysis jobs; two disposable stores are
                      required.
                    </small>
                  </span>
                  <input
                    value={draft.temp_dir}
                    onChange={(e) =>
                      setDraft({ ...draft, temp_dir: e.target.value })
                    }
                  />
                  <button
                    aria-label="Choose temporary directory"
                    onClick={() =>
                      void action(async () => {
                        const p = await bridge.selectFolder();
                        if (p) setDraft({ ...draft, temp_dir: p });
                      })
                    }
                  >
                    <FolderOpen size={17} />
                  </button>
                </label>
                <label className="setting-field">
                  <span>
                    Runtime memory cache
                    <small>MiB · applied to new mounts</small>
                  </span>
                  <input
                    type="number"
                    min={16}
                    max={16384}
                    value={draft.cache_mib}
                    onChange={(e) =>
                      setDraft({ ...draft, cache_mib: Number(e.target.value) })
                    }
                  />
                  <span>MiB</span>
                </label>
                <label className="setting-row">
                  <div>
                    <h3>Runtime logs</h3>
                    <p>
                      Retain up to 2 MiB per mount or launch. Applied to new
                      sessions.
                    </p>
                  </div>
                  <input
                    type="checkbox"
                    aria-label="Retain runtime logs"
                    checked={draft.retain_logs}
                    onChange={(e) =>
                      setDraft({ ...draft, retain_logs: e.target.checked })
                    }
                  />
                </label>
                <div className="panel-actions">
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() =>
                      void action(() => bridge.updateSettings(draft))
                    }
                  >
                    Save preferences
                  </button>
                </div>
              </section>
              {readinessPanel}
              <section className="about">
                <Brand />
                <div>
                  <h3>
                    PlaySparse <span>0.1.0 · Development build</span>
                  </h3>
                  <p>
                    Existing Rust engine. Independent stores. Verified bytes.
                  </p>
                  <p>
                    Signed macOS native code may need an APFS compatibility
                    shadow. Protected multiplayer and launcher compatibility
                    require separate validation.
                  </p>
                </div>
              </section>
            </>
          )}
          <footer>
            <span>
              <ShieldCheck size={13} />
              Source-preserving by design
            </span>
            <span>
              {bridge.preview
                ? "Browser preview · no filesystem operations"
                : "Native desktop · Rust backend"}
            </span>
          </footer>
        </main>
      </div>
      {addPath != null && (
        <Modal title="Add an installation" close={() => setAddPath(null)}>
          <div className="modal-body">
            <div className="modal-illustration">
              <FolderOpen size={28} />
            </div>
            <h3>Start with a safe, measured scan.</h3>
            <p>
              Choose your installed game folder. PlaySparse will register its
              path; analysis runs only when you ask.
            </p>
            <label>
              Installation folder
              <input
                value={addPath}
                onChange={(e) => {
                  setInspection(null);
                  setAddPath(e.target.value);
                }}
              />
            </label>
            {inspection?.discovery_truncated && (
              <p className="footnote">
                Inspection is limited to 12 directory levels. Sizes and file
                count below are partial; Analyze performs the full engine scan.
              </p>
            )}
            {inspection ? (
              <div className="analysis-grid">
                <Metric label="Detected title" value={inspection.title} />
                <Metric
                  label="Logical size"
                  value={bytes(inspection.logical_bytes)}
                />
                <Metric
                  label="Files inspected"
                  value={inspection.files.toLocaleString()}
                />
              </div>
            ) : (
              <p>Validating folder and inspecting installation…</p>
            )}
            <div className="info-note">
              <ShieldCheck size={18} />
              <span>
                Your installation will stay untouched. Optimized stores and
                writable runtime overlays are separate.
              </span>
            </div>
            {bridge.preview && (
              <p className="footnote">
                Preview Mode uses a simulated folder. Native folder selection is
                available in the desktop app.
              </p>
            )}
            <div className="modal-actions">
              <button onClick={() => setAddPath(null)}>Cancel</button>
              <button
                className="primary"
                disabled={pending || !inspection}
                onClick={() =>
                  void action(async () => {
                    await bridge.addGame(addPath);
                    setAddPath(null);
                  })
                }
              >
                Add to Library
                <ArrowRight size={16} />
              </button>
            </div>
          </div>
        </Modal>
      )}
      {selected && (
        <Modal title={selected.name} close={() => setDetail(null)}>
          <div className="modal-body details">
            <p className="path-label">
              Original installation <code>{selected.source}</code>
            </p>
            <div className="analysis-grid">
              <Metric
                label="Store allocated"
                value={bytes(selected.store_stats?.allocated_bytes)}
              />
              <Metric
                label="Overlay allocated"
                value={bytes(selected.overlay_allocated_bytes)}
              />
              <Metric
                label="Effective representation"
                value={bytes(effectiveBytes(selected))}
              />
              <Metric
                label="Verification"
                value={selected.verified ? "Verified" : "Required"}
              />
              <Metric label="Runtime readiness" value={readiness.state} />
              <Metric label="State" value={status(selected, data.jobs)} />
            </div>
            <p className="path-label">
              PlaySparse store <code>{selected.store ?? "Not created"}</code>
            </p>
            <p className="path-label">
              Writable overlay{" "}
              <code>
                {selected.session?.overlay ??
                  locations?.overlay ??
                  "Created in app-owned runtime folder on mount"}
              </code>
            </p>
            <div className="panel-actions">
              <button
                onClick={() =>
                  void action(() =>
                    bridge.inspectLocation(selected.id, "source"),
                  )
                }
              >
                Show Installation
              </button>
              {selected.store && (
                <button
                  onClick={() =>
                    void action(() =>
                      bridge.inspectLocation(selected.id, "store"),
                    )
                  }
                >
                  Inspect Store
                </button>
              )}
              {selected.session && (
                <button
                  onClick={() =>
                    void action(() =>
                      bridge.inspectLocation(selected.id, "mount"),
                    )
                  }
                >
                  Inspect Mount
                </button>
              )}
            </div>
            {selected.error && (
              <div role="alert" className="error-banner">
                {selected.error}
              </div>
            )}
            {data.jobs.filter((j) => j.game_id === selected.id).at(-1) && (
              <p>
                Last operation:{" "}
                {
                  data.jobs.filter((j) => j.game_id === selected.id).at(-1)
                    ?.operation
                }{" "}
                ·{" "}
                {
                  data.jobs.filter((j) => j.game_id === selected.id).at(-1)
                    ?.state
                }
                <br />
                {
                  data.jobs.filter((j) => j.game_id === selected.id).at(-1)
                    ?.error
                }
              </p>
            )}
            <p className="footnote">
              Representation savings compare store + overlay with the original
              allocation. Your original remains installed; no disk space has
              been reclaimed.
            </p>
            {selected.analysis ? (
              <>
                <h3>Measured analysis</h3>
                <div className="analysis-grid">
                  <Metric
                    label="Logical source"
                    value={bytes(selected.analysis.logical_bytes)}
                  />
                  <Metric
                    label="Allocated source"
                    value={bytes(selected.analysis.original_allocated_bytes)}
                  />
                  <Metric
                    label="Files"
                    value={selected.analysis.files.toLocaleString()}
                  />
                  <Metric
                    label="Estimated store allocation"
                    value={bytes(selected.analysis.cdc_chunks.allocated_bytes)}
                  />
                  <Metric
                    label="Exact duplicate files"
                    value={bytes(selected.analysis.exact_duplicate_file_bytes)}
                  />
                  <Metric
                    label="Reusable chunk bytes"
                    value={bytes(selected.analysis.cdc_duplicate_reuse_bytes)}
                  />
                  <Metric
                    label="Unique compressible bytes"
                    value={bytes(
                      selected.analysis.unique_compressible_raw_bytes,
                    )}
                  />
                  <Metric
                    label="Unique raw bytes"
                    value={bytes(
                      selected.analysis.unique_incompressible_raw_bytes,
                    )}
                  />
                  <Metric
                    label="Destination budget"
                    value={bytes(
                      selected.analysis.destination_required_estimated_bytes,
                    )}
                  />
                  <Metric
                    label="Temporary budget"
                    value={bytes(
                      selected.analysis.temporary_workspace
                        .required_estimated_bytes,
                    )}
                  />
                </div>
                <p className="footnote">
                  Full scan, two temporary verified stores. Codec choices are
                  measured; entropy and container attribution are unavailable.
                  Duplicate file and chunk counts overlap and must not be added.
                  Source changes require a new analysis.
                </p>
              </>
            ) : (
              <p>
                Analyze this folder to measure storage and preview an optimized
                representation.
              </p>
            )}
            {selected.analysis && !selected.store && (
              <div className="info-note">
                <span>
                  New store location
                  <code>
                    {data.settings.storage_dir}/{selected.id}
                  </code>
                  <button
                    disabled={busy}
                    onClick={() =>
                      void action(async () => {
                        const location = await bridge.selectFolder();
                        if (!location) return;
                        const settings = {
                          ...data.settings,
                          storage_dir: location,
                        };
                        await bridge.updateSettings(settings);
                        setDraft(settings);
                      })
                    }
                  >
                    <FolderOpen size={14} />
                    Choose store location
                  </button>
                </span>
              </div>
            )}
            <div className="modal-actions">
              <button
                disabled={busy || !!selected.session}
                onClick={() => void runJob(selected.id, "analyze")}
              >
                Analyze{selected.analysis ? " again" : " Game"}
              </button>
              {!selected.store && selected.analysis && (
                <button
                  className="primary"
                  disabled={busy}
                  onClick={() =>
                    void action(async () => {
                      const budget = await bridge.optimizePreflight(
                        selected.id,
                      );
                      setConfirm({
                        title: "Create a verified store?",
                        text: `Destination: ${budget.destination}. Conservative requirement: ${bytes(budget.required_estimated_bytes)}; available: ${bytes(budget.available_bytes)}. The original stays installed. Capacity is checked again before packing. Only verified data is published atomically.`,
                        run: () => bridge.startJob(selected.id, "optimize"),
                      });
                    })
                  }
                >
                  Optimize Game
                  <ArrowRight size={16} />
                </button>
              )}
              {selected.store && (
                <button
                  disabled={busy || !!selected.session}
                  onClick={() => void runJob(selected.id, "verify")}
                >
                  <ShieldCheck size={15} />
                  Verify Store
                </button>
              )}
            </div>
            {selected.verified && (
              <section className="runtime-config">
                <h3>Runtime & launch</h3>
                <p className="muted">
                  Mount the verified store with a separate writable overlay.
                  Launch requires a relative executable and compatibility you
                  have tested for this installation.
                </p>
                {selected.session && (
                  <div className="info-note">
                    <span>
                      {selected.session.state}
                      <code>{selected.session.mountpoint}</code>
                      {selected.session.error && (
                        <p>{selected.session.error}</p>
                      )}
                    </span>
                  </div>
                )}
                <label>
                  Launch target
                  <select
                    aria-label="Discovered launch target"
                    value={
                      candidates.some((c) => c.executable === executable)
                        ? executable
                        : ""
                    }
                    onChange={(e) => {
                      setExecutable(e.target.value);
                      setCompatible(false);
                    }}
                  >
                    <option value="">Configure manually</option>
                    {candidates.map((candidate, index) => (
                      <option
                        key={candidate.executable}
                        value={candidate.executable}
                      >
                        {candidate.label}
                        {index === 0 ? " · First candidate" : ""}
                      </option>
                    ))}
                  </select>
                </label>
                {candidates.find((c) => c.executable === executable)?.note && (
                  <p className="footnote">
                    {candidates.find((c) => c.executable === executable)?.note}
                  </p>
                )}
                <label>
                  Executable relative to mount
                  <input
                    placeholder="bin/game"
                    value={executable}
                    onChange={(e) => setExecutable(e.target.value)}
                  />
                </label>
                <label>
                  Arguments (one literal argument per line)
                  <textarea
                    rows={3}
                    value={args}
                    onChange={(e) => setArgs(e.target.value)}
                  />
                </label>
                <label className="checkbox">
                  <input
                    type="checkbox"
                    checked={compatible}
                    onChange={(e) => setCompatible(e.target.checked)}
                  />
                  I confirm this launch target and understand compatibility is
                  untested until I test this installation.
                </label>
                <button
                  disabled={busy || !!selected.session || !executable}
                  onClick={() =>
                    void action(async () => {
                      const parsed = args === "" ? [] : args.split("\n");
                      await bridge.configureLaunch(selected.id, {
                        executable,
                        args: parsed,
                        compatibility_confirmed: compatible,
                      });
                    })
                  }
                >
                  Save launch configuration
                </button>
                <div className="modal-actions">
                  {!selected.session && (
                    <button
                      disabled={busy || !canMount}
                      onClick={() => runtime(selected, "mount")}
                    >
                      <Play size={15} />
                      Mount Store
                    </button>
                  )}
                  {selected.session?.state === "mounted" &&
                    selected.launch?.compatibility_confirmed && (
                      <button
                        disabled={pending}
                        onClick={() => runtime(selected, "launch")}
                      >
                        Launch Game
                      </button>
                    )}
                  {selected.session?.state === "running" &&
                    (selected.launch?.executable.endsWith(".app") ? (
                      <p className="info-note">
                        Quit the game using its own Quit command. PlaySparse
                        waits for Launch Services; it cannot safely stop the app
                        by the helper PID.
                      </p>
                    ) : (
                      <button onClick={() => runtime(selected, "stop")}>
                        Stop Game
                      </button>
                    ))}
                  {selected.session && selected.session.state !== "running" && (
                    <button onClick={() => runtime(selected, "unmount")}>
                      Unmount
                    </button>
                  )}
                  {selected.session?.state === "needs_attention" && (
                    <button
                      disabled={pending}
                      onClick={() => runtime(selected, "recover")}
                    >
                      Reconcile stale session
                    </button>
                  )}
                </div>
                <p className="footnote">
                  Automatic APFS shadows and case translation are not
                  implemented. Keep original launcher paths intact. No DRM,
                  Gatekeeper or anti-cheat bypass is provided.
                </p>
              </section>
            )}
            {selected.store && !selected.verified && (
              <button
                disabled={busy || !!selected.session}
                onClick={() =>
                  setConfirm({
                    title: "Forget missing store metadata?",
                    text: "Only permitted when the registered store path is absent. No files are deleted. Restore the original and analyze again before building a new store.",
                    run: () => bridge.forgetMissingStore(selected.id),
                  })
                }
              >
                Forget Missing Store
              </button>
            )}
            <div className="details-bottom">
              <span>Removing an entry never deletes its files.</span>
              <button
                className="danger-text"
                disabled={busy || !!selected.session}
                onClick={() => remove(selected)}
              >
                Remove from Library
              </button>
            </div>
          </div>
        </Modal>
      )}
      {confirm && (
        <Modal title={confirm.title} close={() => setConfirm(null)}>
          <div className="modal-body">
            <p>{confirm.text}</p>
            <div className="modal-actions">
              <button onClick={() => setConfirm(null)}>Cancel</button>
              <button
                className="primary"
                disabled={pending}
                onClick={() => {
                  const run = confirm.run;
                  setConfirm(null);
                  void action(async () => {
                    await run();
                    if (confirm.title.includes("Remove")) setDetail(null);
                  });
                }}
              >
                Continue
              </button>
            </div>
          </div>
        </Modal>
      )}
    </div>
  );
}
