// Compiled only into the opt-in native acceptance build. Real Tauri IPC;
// there is no browser mock, HTTP bridge or replacement engine here.
(async () => {
  if (window.__DESKTOP_ACCEPTANCE_STARTED) return;
  window.__DESKTOP_ACCEPTANCE_STARTED = true;
  const invoke = window.__TAURI_INTERNALS__.invoke;
  const steps = [];
  const record = (stage, value) => steps.push({ stage, value });
  const pause = ms => new Promise(resolve => setTimeout(resolve, ms));
  async function job(id, operation) {
    const submitted = await invoke('start_job', { id, operation });
    const deadline = Date.now() + 60000;
    while (Date.now() < deadline) {
      const snapshot = await invoke('get_snapshot');
      const current = snapshot.jobs.find(j => j.id === submitted.id);
      if (current.state !== 'running') {
        if (current.state !== 'completed') throw Error(JSON.stringify(current));
        record(operation, current); return;
      }
      await pause(100);
    }
    throw Error('Native job timed out');
  }
  try {
    while (!document.querySelector('nav')) await pause(100);
    if (document.body.textContent.includes('Preview Mode')) throw Error('Native app entered simulated mode');
    for (const name of ['Activity', 'Storage', 'Settings', 'Library']) {
      const button = Array.from(document.querySelectorAll('nav button')).find(b => b.textContent.trim().startsWith(name));
      if (!button) throw Error(`Missing navigation: ${name}`);
      button.click(); await pause(150);
    }
    record('native_navigation', 'PASS');
    const inspection = await invoke('inspect_installation', { path: window.__DESKTOP_ACCEPTANCE_SOURCE });
    if (!inspection.candidates.some(c => c.executable === 'fixture-game')) throw Error('Fixture executable was not discovered');
    record('native_installation_inspection', inspection);
    const diagnostics = await invoke('get_system_status');
    if (!diagnostics.readiness) throw Error('Missing structured readiness');
    record('readiness', diagnostics.readiness);
    const tested = await invoke('test_readiness');
    if (tested.readiness.state !== 'Ready' || tested.mount_test.cleanup !== 'PASS') throw Error('Readiness mount probe failed: ' + JSON.stringify(tested));
    record('actual_readiness_probe', tested);
    const game = await invoke('add_game', { path: window.__DESKTOP_ACCEPTANCE_SOURCE });
    record('register_actual_folder', game.source);
    await job(game.id, 'analyze');
    record('destination_preflight', await invoke('optimize_preflight', {id:game.id}));
    await job(game.id, 'optimize');
    await job(game.id, 'verify');
    record('discovered_launch', await invoke('discover_launch', {id:game.id}));
    await invoke('configure_launch', { id:game.id, descriptor:{ executable:'fixture-game', args:[], compatibility_confirmed:true } });
    await invoke('runtime_action', { id:game.id, action:'mount', processesClosed:false });
    record('mount', 'PASS');
    record('exact_bytes_and_overlay_separation', await invoke('acceptance_probe', {id:game.id}));
    await invoke('runtime_action', {id:game.id, action:'launch', processesClosed:false});
    record('launch', (await invoke('get_snapshot')).games[0].session.state);
    await invoke('runtime_action', {id:game.id, action:'stop', processesClosed:false});
    record('stop', 'PASS');
    await invoke('runtime_action', {id:game.id, action:'unmount', processesClosed:true});
    record('unmount', 'PASS');
    await job(game.id, 'verify');
    record('verify_after_overlay', 'PASS');
    const snapshot = await invoke('get_snapshot');
    if (!snapshot.games[0].verified || snapshot.games[0].session) throw Error('Unexpected final state');
    await invoke('acceptance_report', { report:{ status:'PASS', scope:'Native WKWebView + real Tauri commands + Rust engine + installed macFUSE; generated fixture only', steps, snapshot }});
  } catch (error) {
    await invoke('acceptance_report', {report:{status:'FAIL', error:String(error), steps}});
  }
})();
