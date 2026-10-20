import http from 'node:http';
import { readFile } from 'node:fs/promises';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { randomUUID } from 'node:crypto';
import { WebSocketServer, WebSocket } from 'ws';
import { Daemon, fault } from './daemon.js';
import { MAX_RECORD, MAX_QUEUE } from './framing.js';

const root = fileURLToPath(new URL('../', import.meta.url));
const publicDir = fileURLToPath(new URL('./public/', import.meta.url));
const isolationDefault = process.env.PRTS_ISOLATION_PROFILE;
if (isolationDefault && !['none', 'job', 'appcontainer', 'lpac'].includes(isolationDefault)) throw new Error('Invalid PRTS_ISOLATION_PROFILE');
let args;
try {
  args = JSON.parse(process.env.PRTSD_ARGS || '[]');
  if (!Array.isArray(args) || args.some(arg => typeof arg !== 'string')) throw new Error();
} catch { throw new Error('PRTSD_ARGS must be a JSON string array'); }
const daemon = new Daemon(process.env.PRTSD_PATH || path.join(root, 'prtsd/target/release/prtsd.exe'), args, root);
const clients = new Set();
const sessions = new Map();
const methods = new Set(['hello', 'instance.create', 'instance.close', 'instance.list',
  'target.create', 'target.close', 'target.navigate', 'target.list', 'target.resize', 'target.activate',
  'session.create', 'session.offer', 'session.answer', 'session.ice', 'session.close', 'network.set', 'stats.configure']);
const contentTypes = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8' };

// The sample trusts the local operator. DNS rebinding and cross-site browser access
// are excluded by loopback binding, an exact Host allowlist and same-origin upgrades.
function validHost(req) {
  if (process.env.ALLOW_PUBLIC === '1') return true; // testing only: disables Host allowlist
  const port = server.address()?.port;
  return ['127.0.0.1', 'localhost'].some(host => req.headers.host === `${host}:${port}` || (port === 80 && req.headers.host === host));
}
const server = http.createServer(async (req, res) => {
  res.setHeader('X-Content-Type-Options', 'nosniff');
  res.setHeader('Cache-Control', 'no-store');
  res.setHeader('Content-Security-Policy', "default-src 'self'; connect-src 'self'; media-src 'self' blob:; img-src 'self' data:; style-src 'self'; frame-ancestors 'none'; base-uri 'none'");
  if (!validHost(req)) { res.writeHead(403).end(); return; }
  if (req.method !== 'GET' && req.method !== 'HEAD') { res.writeHead(405).end(); return; }
  // Explicit static allowlist avoids traversal, symlinks and accidental source serving.
  const pathname = new URL(req.url, 'http://localhost').pathname;
  const files = new Set(['/index.html', '/app.js', '/input.js', '/stats.js', '/video-crop.js', '/style.css']);
  const name = pathname === '/' ? '/index.html' : pathname;
  if (!files.has(name)) { res.writeHead(404).end(); return; }
  try {
    const data = await readFile(path.join(publicDir, name.slice(1)));
    res.setHeader('Content-Type', contentTypes[path.extname(name)]);
    res.writeHead(200).end(req.method === 'HEAD' ? undefined : data);
  } catch { res.writeHead(500).end('Unable to load page'); }
});
const wss = new WebSocketServer({ noServer: true, maxPayload: MAX_RECORD, perMessageDeflate: false });
server.on('upgrade', (req, socket, head) => {
  if (req.url !== '/ws' || !validHost(req) || req.headers.origin !== new URL(`http://${req.headers.host}`).origin || clients.size >= 64) {
    socket.end('HTTP/1.1 403 Forbidden\r\nConnection: close\r\n\r\n');
    return;
  }
  wss.handleUpgrade(req, socket, head, ws => wss.emit('connection', ws));
});

function send(ws, message) {
  if (ws.readyState !== WebSocket.OPEN) return;
  const json = JSON.stringify(message);
  if (ws.bufferedAmount + Buffer.byteLength(json) > MAX_QUEUE) { ws.terminate(); return; }
  ws.send(json);
}
function errorResponse(ws, id, error) {
  const code = error.code || 'internal';
  send(ws, { v: 1, id, error: {
    code, message: code === 'internal' ? 'Control operation failed' : error.message,
    retryable: error.retryable === true,
    ...(error.details ? { details: error.details } : {}),
  } });
}
async function closeSession(id) {
  const session = sessions.get(id);
  if (!session) return;
  sessions.delete(id);
  session.owner.sessions.delete(id);
  await daemon.request('session.close', { session_id: id }).catch(() => {});
}

wss.on('connection', ws => {
  clients.add(ws);
  ws.sessions = new Set();
  ws.seen = new Set();
  ws.viewerId = randomUUID();
  ws.inflight = 0;
  ws.sessionCreating = false;
  ws.on('error', () => ws.terminate());
  ws.on('close', () => {
    clients.delete(ws);
    for (const id of ws.sessions) void closeSession(id);
  });
  ws.on('message', async (bytes, binary) => {
    let message;
    try {
      if (binary) throw new Error();
      message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes));
      if (!message || message.v !== 1 || typeof message.id !== 'string' ||
          Buffer.byteLength(message.id) < 1 || Buffer.byteLength(message.id) > 128 ||
          typeof message.method !== 'string' || !message.params || Array.isArray(message.params) || typeof message.params !== 'object') throw new Error();
    } catch { ws.close(1008, 'Invalid request'); return; }
    const { id, method } = message;
    let params = message.params;
    if (ws.seen.has(id) || ws.seen.size >= 100000) { ws.close(1008, 'Duplicate ID or request limit'); return; }
    ws.seen.add(id);
    if (ws.inflight >= 64) { errorResponse(ws, id, fault('resource_exhausted', 'Too many pending requests')); return; }
    ws.inflight++;
    let offering;
    let creating = false;
    try {
      if (!methods.has(method)) throw fault('unsupported', 'Unknown method');
      if (daemon.dead) throw fault('invalid_state', 'prtsd is unavailable; restart the server');
      await daemon.ready;
      if (method === 'instance.create' && isolationDefault && params.options && !Object.hasOwn(params.options, 'isolation_profile')) {
        params = { ...params, options: { ...params.options, isolation_profile: isolationDefault } };
      }
      if (daemon.dead) throw fault('invalid_state', 'prtsd is unavailable; restart the server');
      if (method === 'hello') {
        if (!Array.isArray(params.versions) || !params.versions.includes(1)) throw fault('unsupported', 'Version 1 required');
        send(ws, { v: 1, id, result: daemon.hello });
        return;
      }
      if (method === 'session.create') {
        if (ws.sessions.size || ws.sessionCreating) throw fault('invalid_state', 'Close the existing viewer session first');
        ws.sessionCreating = creating = true;
        // Never accept a browser-supplied viewer identity as authorization.
        params = { ...params, viewer_id: ws.viewerId };
      } else if (method.startsWith('session.')) {
        const session = sessions.get(params.session_id);
        if (!session || session.owner !== ws) throw fault('permission_denied', 'Session belongs to another viewer');
        if (method === 'session.offer') {
          if (session.offering) throw fault('invalid_state', 'Offer already pending');
          session.offering = true;
          offering = session;
        }
      } else if (method === 'target.activate') {
        if (![...ws.sessions].some(sid => sessions.get(sid)?.control && sessions.get(sid)?.instance === params.instance_id)) {
          throw fault('permission_denied', 'A controlling viewer session is required');
        }
      } else if (method === 'target.create' && params.activate !== false &&
          [...ws.sessions].some(sid => sessions.get(sid)?.instance === params.instance_id && !sessions.get(sid)?.control)) {
        throw fault('permission_denied', 'A view-only session cannot activate a new target');
      }
      const result = await daemon.request(method, params);
      if (method === 'session.create') {
        const session = { owner: ws, instance: params.instance_id, control: result.control, ice: [], offering: false };
        sessions.set(result.session_id, session);
        ws.sessions.add(result.session_id);
        if (ws.readyState !== WebSocket.OPEN) { await closeSession(result.session_id); return; }
      }
      send(ws, { v: 1, id, result });
      if (offering) {
        offering.offering = false;
        for (const event of offering.ice.splice(0)) send(ws, event);
      }
      if (method === 'session.close') {
        sessions.delete(params.session_id);
        ws.sessions.delete(params.session_id);
      }
    } catch (error) {
      errorResponse(ws, id, error);
      if (offering) { offering.offering = false; offering.ice.length = 0; }
    } finally {
      if (creating) ws.sessionCreating = false;
      ws.inflight--;
    }
  });
});

daemon.on('event', event => {
  const sid = event.data.session_id;
  if (sid) {
    const session = sessions.get(sid);
    if (!session) return;
    if (event.event === 'session.ice' && session.offering) {
      if (session.ice.length >= 256) { session.owner.terminate(); return; }
      session.ice.push(event);
    } else send(session.owner, event);
    if (event.event === 'session.closed') {
      sessions.delete(sid);
      session.owner.sessions.delete(sid);
    }
  } else {
    for (const ws of clients) {
      // Other viewers' transport/feedback details do not leave their connection.
      const outgoing = event.event === 'stats'
        ? { ...event, data: { ...event.data, sessions: (event.data.sessions || []).filter(s => ws.sessions.has(s.session_id)) } }
        : event;
      send(ws, outgoing);
    }
  }
});
daemon.on('unavailable', () => {
  for (const ws of clients) ws.close(1011, 'prtsd unavailable; restart the server');
  sessions.clear();
});
daemon.ready.then(() => daemon.request('stats.configure', { interval_ms: 1000 })).catch(() => {});

const port = Number(process.env.PORT ?? 8080);
if (!Number.isInteger(port) || port < 0 || port > 65535) throw new Error('Invalid PORT');
server.listen(port, () => {
  const url = `http://127.0.0.1:${server.address().port}`;
  console.log(`PRTS listening at ${url}`);
  process.send?.({ url, daemonPid: daemon.child.pid });
});
server.on('error', error => { console.error(error.message); daemon.stop(); process.exitCode = 1; });
let shuttingDown = false;
function shutdown() {
  if (shuttingDown) return;
  shuttingDown = true;
  for (const ws of clients) ws.terminate();
  wss.close();
  server.close();
  daemon.stop();
}
process.on('SIGINT', shutdown);
process.on('SIGTERM', shutdown);
// A supervising Node process can close its IPC channel for graceful Windows shutdown.
process.on('disconnect', shutdown);
