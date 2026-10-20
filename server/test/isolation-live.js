// Standalone real-daemon acceptance; intentionally excluded from npm test.
import assert from 'node:assert/strict';
import { setTimeout as delay } from 'node:timers/promises';
import { access } from 'node:fs/promises';
import { execFileSync } from 'node:child_process';
import { once } from 'node:events';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { Daemon } from '../daemon.js';

const root = fileURLToPath(new URL('../../', import.meta.url));
const profile = process.env.PRTS_ISOLATION_PROFILE || 'job';
assert.notEqual(profile, 'none', 'kill-on-close requires an outer job');
const events = [];
let daemon;
function start() {
  const daemon = new Daemon(path.join(root, 'prtsd/target/release/prtsd.exe'), [], root);
  daemon.on('event', event => events.push(event));
  return daemon;
}
async function until(predicate, message, timeout = 60000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const result = await predicate();
    if (result) return result;
    await delay(100);
  }
  throw new Error(message);
}
async function exists(file) { try { await access(file); return true; } catch { return false; } }
function alive(pid) { try { process.kill(pid, 0); return true; } catch { return false; } }
try {
  daemon = start(); await daemon.ready;
  for (const unavailable of ['appcontainer', 'lpac', 'invalid'].filter(p => !daemon.hello.capabilities.isolation_profiles.includes(p))) {
    await assert.rejects(daemon.request('instance.create', { options: { isolation_profile: unavailable } }), error => error.code === 'isolation_error');
  }
  await daemon.request('stats.configure', { interval_ms: 250 });
  const instances = [];
  for (let n = 0; n < 2; n++) {
    const result = await daemon.request('instance.create', { options: { source: 'chromium', isolation_profile: profile, encoder: 'qsv' } });
    const id = result.instance.instance_id;
    const event = await until(() => events.find(e => ['instance.ready', 'instance.failed'].includes(e.event) && e.data.instance_id === id), 'browser readiness timeout');
    assert.equal(event.event, 'instance.ready', JSON.stringify(event));
    const stats = await until(() => events.find(e => e.event === 'stats' && e.data.instance_id === id && e.data.isolation?.pids?.length >= 5), 'job membership snapshot timeout');
    instances.push(stats.data.isolation);
  }
  const pids = instances.flatMap(i => i.pids);
  console.log('I1 daemon crash snapshots', JSON.stringify(instances));
  const command = `Get-CimInstance Win32_Process | Where-Object { $_.ProcessId -in @(${pids.join(',')}) } | Select-Object ProcessId,ParentProcessId,CommandLine | ConvertTo-Json -Depth 3`;
  const snapshot = execFileSync('powershell.exe', ['-NoProfile', '-Command', command], { windowsHide: true, encoding: 'utf8' });
  console.log('I1 process commands', snapshot);
  assert(!/--no-sandbox|--disable-gpu-sandbox|--ignore-gpu-blocklist|--enable-unsafe-swiftshader/.test(snapshot));
  const exited = once(daemon.child, 'exit');
  daemon.child.kill(); await exited;
  await until(() => pids.every(pid => !alive(pid)), 'job descendants survived daemon crash', 10000);
  for (const instance of instances) assert(await exists(instance.state_path), 'crash should leave state for startup recovery');
  events.length = 0;
  daemon = start(); await daemon.ready;
  for (const instance of instances) assert(!await exists(instance.state_path), 'stale state not removed at daemon startup');
  console.log('I1 PASS actual daemon kill-on-close, all sampled descendants gone, startup stale cleanup and fail-closed protocol');
  const exited2 = once(daemon.child, 'exit'); daemon.stop(); await exited2;
} finally {
  if (daemon && !daemon.stopping && !daemon.dead) daemon.stop();
}
