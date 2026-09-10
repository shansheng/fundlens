#!/usr/bin/env node
'use strict';
/*
 * FundLens 多设备同步 relay（零依赖，Node 18+）
 *
 * 协议与 src-tauri/src/cloud.rs 的 HttpTransport 一一对应：
 *   所有请求 POST {endpoint}?action=<list|put|get>，头 x-sync-token: <令牌>
 *     list → 无正文；响应 JSON {"ok":true,"items":[{key,device,at,size,kind}]}
 *     put  → 查询串 key/device/at/kind，**正文即原始字节**；响应 JSON {"ok":true,"item":{...}}
 *     get  → 查询串 key；响应正文即原始字节（缺键 → 404 + {"ok":false,"error":"..."}）
 *
 * 设计取舍：
 *   - 零依赖（只用 node 内置模块）→ 目标机器不需要 npm install，直接 node server.js 即可。
 *   - 正文走原始字节而非 base64 → 4MB 级快照/11MB 备份不做 33% 膨胀，也不多一次内存拷贝。
 *   - 落盘布局 {DATA_DIR}/snapshots/{device}/{stamp}.jsonl，与快照键一一对应；
 *     "list" 就是一次目录遍历，无需数据库、无需索引文件。
 *
 * 环境变量：
 *   SYNC_TOKEN  必填。同步令牌，客户端配置里填同一个值。
 *   PORT        监听端口，默认 8787。
 *   DATA_DIR    数据目录，默认 ./data。
 *   MAX_BODY    单次请求正文上限（字节），默认 64MB。
 */
const http = require('node:http');
const fsp = require('node:fs/promises');
const path = require('node:path');

const PORT = Number(process.env.PORT || 8787);
const DATA_DIR = path.resolve(process.env.DATA_DIR || './data');
const TOKEN = process.env.SYNC_TOKEN || '';
const SNAP_DIR = path.join(DATA_DIR, 'snapshots');
const MAX_BODY = Number(process.env.MAX_BODY || 64 * 1024 * 1024);

/** 时间戳：YYYYMMDD-HHMMSSmmm（18 位，毫秒精度），与 cloud.rs 的 STAMP_LEN 一致。 */
const STAMP_RE = /^\d{8}-\d{9}$/;
const STAMP_LEN = 18;

if (!TOKEN) {
  console.error('[fundlens-relay] 缺少 SYNC_TOKEN 环境变量，拒绝启动（令牌是唯一的访问边界）');
  process.exit(1);
}
if (TOKEN.length < 16) {
  console.error('[fundlens-relay] SYNC_TOKEN 太短（至少 16 位）；它同时是鉴权与访问控制的唯一凭据');
  process.exit(1);
}

/** 校验快照键 `{device}/{stamp}.jsonl`，返回 {device, stamp}；不合规返回 null。 */
function parseKey(key) {
  if (typeof key !== 'string' || key.length === 0) return null;
  if (key.includes('\\') || key.includes('..') || key.startsWith('/')) return null;
  const parts = key.split('/');
  if (parts.length !== 2) return null;
  const [device, file] = parts;
  if (!device || device.includes('.')) return null;
  if (!file.endsWith('.jsonl')) return null;
  const stamp = file.slice(0, -'.jsonl'.length);
  if (stamp.length !== STAMP_LEN || !STAMP_RE.test(stamp)) return null;
  return { device, stamp };
}

/** 键 → 绝对路径；拒绝一切逃出 SNAP_DIR 的情况（双保险：正则 + 前缀核对）。 */
function resolvePath(key) {
  if (!parseKey(key)) return null;
  const abs = path.join(SNAP_DIR, key);
  if (abs !== SNAP_DIR && !abs.startsWith(SNAP_DIR + path.sep)) return null;
  return abs;
}

/** `20260910-221500123` → `2026-09-10 22:15:00`（与 Rust 侧 stamp_to_at 同口径）。 */
function stampToAt(stamp) {
  if (stamp.length < 15 || stamp[8] !== '-') return stamp;
  const d = stamp.slice(0, 8);
  const t = stamp.slice(9, 15);
  return `${d.slice(0, 4)}-${d.slice(4, 6)}-${d.slice(6, 8)} ${t.slice(0, 2)}:${t.slice(2, 4)}:${t.slice(4, 6)}`;
}

function send(res, status, contentType, body) {
  res.writeHead(status, {
    'content-type': contentType,
    'content-length': Buffer.byteLength(body),
    'cache-control': 'no-store',
  });
  res.end(body);
}

function sendJson(res, status, obj) {
  send(res, status, 'application/json; charset=utf-8', JSON.stringify(obj));
}

/** 读取请求正文（带上限，超限直接断开）。 */
function readBody(req) {
  return new Promise((resolve, reject) => {
    const chunks = [];
    let size = 0;
    req.on('data', (c) => {
      size += c.length;
      if (size > MAX_BODY) {
        reject(new Error(`请求正文超过上限 ${MAX_BODY} 字节`));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on('end', () => resolve(Buffer.concat(chunks)));
    req.on('error', reject);
  });
}

async function handleList(res) {
  let devices;
  try {
    devices = await fsp.readdir(SNAP_DIR, { withFileTypes: true });
  } catch {
    sendJson(res, 200, { ok: true, items: [] }); // 目录不存在 = 远端还没有任何快照
    return;
  }
  const items = [];
  for (const d of devices) {
    if (!d.isDirectory()) continue;
    let files;
    try {
      files = await fsp.readdir(path.join(SNAP_DIR, d.name), { withFileTypes: true });
    } catch {
      continue;
    }
    for (const f of files) {
      if (!f.isFile()) continue;
      const key = `${d.name}/${f.name}`;
      const info = parseKey(key);
      if (!info) continue;
      let size = 0;
      try {
        size = (await fsp.stat(path.join(SNAP_DIR, key))).size;
      } catch {
        continue;
      }
      items.push({
        key,
        device: info.device,
        at: stampToAt(info.stamp),
        size,
        kind: 'snapshot',
      });
    }
  }
  items.sort((a, b) => (a.key < b.key ? -1 : a.key > b.key ? 1 : 0));
  sendJson(res, 200, { ok: true, items });
}

async function handlePut(res, url, body) {
  const key = url.searchParams.get('key');
  const device = url.searchParams.get('device') || '';
  const kind = url.searchParams.get('kind') || 'snapshot';
  const info = parseKey(key);
  if (!info) {
    sendJson(res, 400, { ok: false, error: `非法快照键: ${key}` });
    return;
  }
  if (kind !== 'snapshot') {
    sendJson(res, 400, { ok: false, error: `当前仅支持 snapshot 类型，收到: ${kind}` });
    return;
  }
  const abs = resolvePath(key);
  if (!abs) {
    sendJson(res, 400, { ok: false, error: `非法快照键: ${key}` });
    return;
  }
  if (body.length === 0) {
    sendJson(res, 400, { ok: false, error: '快照正文为空' });
    return;
  }
  await fsp.mkdir(path.dirname(abs), { recursive: true });
  // 先写临时文件再原子改名：避免客户端中断留下半截快照被当成可用版本
  const tmp = `${abs}.tmp-${process.pid}-${Date.now()}`;
  await fsp.writeFile(tmp, body);
  await fsp.rename(tmp, abs);
  sendJson(res, 200, {
    ok: true,
    item: {
      key,
      device,
      at: stampToAt(info.stamp),
      size: body.length,
      kind: 'snapshot',
    },
  });
}

async function handleGet(res, url) {
  const key = url.searchParams.get('key');
  const abs = resolvePath(key);
  if (!abs) {
    sendJson(res, 400, { ok: false, error: `非法快照键: ${key}` });
    return;
  }
  try {
    const buf = await fsp.readFile(abs);
    send(res, 200, 'application/octet-stream', buf);
  } catch {
    sendJson(res, 404, { ok: false, error: 'not found' });
  }
}

const server = http.createServer(async (req, res) => {
  let url;
  try {
    url = new URL(req.url, 'http://relay.invalid');
  } catch {
    sendJson(res, 400, { ok: false, error: 'bad url' });
    return;
  }

  if (req.method === 'GET' && url.pathname === '/healthz') {
    sendJson(res, 200, { ok: true, service: 'fundlens-relay' });
    return;
  }
  if (req.method !== 'POST') {
    sendJson(res, 405, { ok: false, error: 'only POST is supported' });
    return;
  }
  // 令牌用定长比较，避免因返回时间差被逐字节猜解
  const got = req.headers['x-sync-token'];
  if (typeof got !== 'string' || !timingSafeEqual(got, TOKEN)) {
    sendJson(res, 401, { ok: false, error: 'bad token' });
    return;
  }

  let body;
  try {
    body = await readBody(req);
  } catch (e) {
    sendJson(res, 413, { ok: false, error: String(e.message || e) });
    return;
  }

  try {
    switch (url.searchParams.get('action')) {
      case 'list':
        await handleList(res);
        break;
      case 'put':
        await handlePut(res, url, body);
        break;
      case 'get':
        await handleGet(res, url);
        break;
      default:
        sendJson(res, 400, { ok: false, error: 'unknown action' });
    }
  } catch (e) {
    console.error('[fundlens-relay] 处理失败:', e);
    sendJson(res, 500, { ok: false, error: String(e.message || e) });
  }
});

/** 恒定时间字符串比较（长度不同直接判否，长度本身不是秘密）。 */
function timingSafeEqual(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i += 1) diff |= a.charCodeAt(i) ^ b.charCodeAt(i);
  return diff === 0;
}

server.listen(PORT, () => {
  console.log(`[fundlens-relay] 监听 :${PORT}，数据目录 ${SNAP_DIR}`);
});
