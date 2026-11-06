/*
 * ato-puter-shim: a small `window.puter` for Puter.js apps running on ato.run.
 *
 * It replaces <script src="https://js.puter.com/v2/"> and keeps app data in
 * the ato.run Instance through `window.storage` (shared scope) instead of a
 * Puter account. Only the parts of Puter.js that the HeyPuter/everything apps
 * use for local data are provided:
 *
 *   puter.env, puter.auth (always signed in as the Instance), puter.kv,
 *   puter.fs (small files, stored as base64), puter.workers (unsupported).
 *
 * Limits, stated so callers do not rely on Puter semantics this cannot give:
 *   - kv.incr is read-modify-write, not atomic across devices.
 *   - fs keeps each file in one storage value; large files are refused.
 *   - Everyone who can open the Instance shares the same data (shared scope).
 */
(function () {
  'use strict';

  const NS = 'puter1:';
  const KV = NS + 'kv:';
  const FS = NS + 'fs:';
  const MAX_FILE_BYTES = 4 * 1024 * 1024;

  function storage() {
    const s = globalThis.window && globalThis.window.storage;
    if (!s || typeof s.get !== 'function') {
      throw new Error('ato.run storage is not available on this page');
    }
    return s;
  }

  // ato.run storage keys may not contain whitespace, '/', '\', "'" or '"'.
  // Only those characters (and '%') are escaped, one by one, so keys that were
  // already valid are stored unchanged and prefix listing still works.
  const escKey = (k) => k.replace(/[%\s\\/'"]/g, (c) => '%' + c.charCodeAt(0).toString(16).padStart(2, '0'));
  const unescKey = (k) => k.replace(/%([0-9a-f]{2})/g, (_, h) => String.fromCharCode(parseInt(h, 16)));

  async function rawGet(key) {
    try {
      const r = await storage().get(escKey(key), true);
      return r && typeof r.value === 'string' ? r.value : null;
    } catch (e) {
      if (e && (e.code === 'not_found' || /not.?found/i.test(String(e.message)))) return null;
      throw e;
    }
  }
  async function rawSet(key, value) {
    await storage().set(escKey(key), value, true);
  }
  async function rawDel(key) {
    try {
      await storage().delete(escKey(key), true);
    } catch (e) {
      if (!(e && (e.code === 'not_found' || /not.?found/i.test(String(e.message))))) throw e;
    }
  }
  async function rawList(prefix) {
    const r = await storage().list(escKey(prefix), true);
    return (Array.isArray(r) ? r : (r && r.keys) || []).map(unescKey);
  }

  const decode = (s) => (s == null ? null : JSON.parse(s));
  const encode = (v) => JSON.stringify(v === undefined ? null : v);

  // Puter KV patterns are prefixes, optionally ending in '*'.
  const patternPrefix = (p) => String(p || '').replace(/\*+$/, '');

  // Paths like 'a', '["tx.2026-10"]' or 'a.b' (nested), as Puter KV reads them.
  function parsePath(path) {
    const parts = [];
    const re = /\[("(?:[^"\\]|\\.)*")\]|([^.[\]]+)/g;
    let m;
    while ((m = re.exec(path))) parts.push(m[1] ? JSON.parse(m[1]) : m[2]);
    return parts;
  }

  const kv = {
    async get(key) {
      return decode(await rawGet(KV + key));
    },
    async set(keyOrItems, value) {
      if (Array.isArray(keyOrItems)) {
        for (const it of keyOrItems) await rawSet(KV + it.key, encode(it.value));
        return true;
      }
      await rawSet(KV + keyOrItems, encode(value));
      return true;
    },
    async del(key) {
      await rawDel(KV + key);
      return true;
    },
    async list(arg, returnValuesArg) {
      const opts = typeof arg === 'object' && arg !== null ? arg : { pattern: arg, returnValues: returnValuesArg };
      const prefix = patternPrefix(opts.pattern);
      const keys = (await rawList(KV + prefix)).map((k) => k.slice(KV.length)).sort();
      if (!opts.returnValues) return keys;
      // Values are fetched a few at a time; one request per key would make a
      // cold load of a few dozen records take tens of seconds.
      const items = new Array(keys.length);
      let next = 0;
      const worker = async () => {
        while (next < keys.length) {
          const i = next++;
          items[i] = { key: keys[i], value: await kv.get(keys[i]) };
        }
      };
      await Promise.all(Array.from({ length: Math.min(8, keys.length) }, worker));
      return items;
    },
    async incr(key, amount) {
      const cur = await kv.get(key);
      if (amount === undefined || typeof amount === 'number') {
        const next = (Number(cur) || 0) + (amount === undefined ? 1 : amount);
        await kv.set(key, next);
        return next;
      }
      const obj = cur && typeof cur === 'object' ? cur : {};
      for (const [path, by] of Object.entries(amount)) {
        const parts = parsePath(path);
        let node = obj;
        for (let i = 0; i < parts.length - 1; i++) {
          if (!node[parts[i]] || typeof node[parts[i]] !== 'object') node[parts[i]] = {};
          node = node[parts[i]];
        }
        const last = parts[parts.length - 1];
        node[last] = (Number(node[last]) || 0) + Number(by || 0);
      }
      await kv.set(key, obj);
      return obj;
    },
    async decr(key, amount) {
      if (amount === undefined || typeof amount === 'number') return kv.incr(key, -(amount === undefined ? 1 : amount));
      const neg = {};
      for (const [p, by] of Object.entries(amount)) neg[p] = -Number(by || 0);
      return kv.incr(key, neg);
    },
    async flush() {
      for (const k of await rawList(KV)) await rawDel(k);
      return true;
    },
  };

  const normPath = (p) => '/' + String(p || '').replace(/^~?\/*/, '').replace(/\/+$/, '');

  async function toBytes(data) {
    if (data == null) return new Uint8Array();
    if (typeof data === 'string') return new TextEncoder().encode(data);
    if (data instanceof Uint8Array) return data;
    if (data instanceof ArrayBuffer) return new Uint8Array(data);
    if (typeof Blob !== 'undefined' && data instanceof Blob) return new Uint8Array(await data.arrayBuffer());
    return new TextEncoder().encode(String(data));
  }
  function b64(bytes) {
    let s = '';
    for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
    return btoa(s);
  }
  function unb64(s) {
    const bin = atob(s);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  const fileInfo = (path, rec) => ({
    path,
    name: path.split('/').pop(),
    is_dir: false,
    size: rec.size,
    type: rec.type,
    modified: rec.modified,
    uid: path,
    id: path,
  });

  const fs = {
    async write(path, data, opts) {
      const p = normPath(typeof path === 'object' && path ? path.path || path.name : path);
      const bytes = await toBytes(data);
      if (bytes.length > MAX_FILE_BYTES) {
        throw new Error(`File is larger than ${MAX_FILE_BYTES / 1024 / 1024} MB, the limit on ato.run for this app`);
      }
      if (opts && opts.overwrite === false && (await rawGet(FS + p)) != null) {
        throw new Error('A file already exists at ' + p);
      }
      const rec = {
        size: bytes.length,
        type: (data && data.type) || 'application/octet-stream',
        modified: Date.now(),
        data: b64(bytes),
      };
      await rawSet(FS + p, JSON.stringify(rec));
      return fileInfo(p, rec);
    },
    async read(path) {
      const p = normPath(path);
      const raw = await rawGet(FS + p);
      if (raw == null) throw Object.assign(new Error('File not found: ' + p), { code: 'subject_does_not_exist' });
      const rec = JSON.parse(raw);
      return new Blob([unb64(rec.data)], { type: rec.type });
    },
    async stat(path) {
      const p = normPath(path);
      const raw = await rawGet(FS + p);
      if (raw == null) throw Object.assign(new Error('File not found: ' + p), { code: 'subject_does_not_exist' });
      return fileInfo(p, JSON.parse(raw));
    },
    async delete(path) {
      const paths = Array.isArray(path) ? path : [path];
      for (const one of paths) {
        const p = normPath(one);
        for (const k of await rawList(FS + p)) {
          const rest = k.slice((FS + p).length);
          if (rest === '' || rest.startsWith('/')) await rawDel(k);
        }
      }
      return true;
    },
    async readdir(path) {
      const p = normPath(path);
      const base = p === '/' ? '/' : p + '/';
      const seen = new Map();
      for (const k of await rawList(FS + base)) {
        const rest = k.slice((FS + base).length);
        const name = rest.split('/')[0];
        if (!name) continue;
        const isDir = rest.includes('/');
        if (!seen.has(name)) seen.set(name, { path: base + name, name, is_dir: isDir });
      }
      return [...seen.values()];
    },
    async mkdir(path) {
      return { path: normPath(path), name: normPath(path).split('/').pop(), is_dir: true };
    },
    async rename(from, to) {
      const blob = await fs.read(from);
      const info = await fs.write(to, blob);
      await fs.delete(from);
      return info;
    },
    async copy(from, to) {
      return fs.write(to, await fs.read(from));
    },
    async move(from, to) {
      return fs.rename(from, to);
    },
  };

  const instanceName = (() => {
    try {
      return location.hostname.split('.')[0] || 'ato';
    } catch {
      return 'ato';
    }
  })();
  const user = Object.freeze({ username: 'ato', uuid: 'ato:' + instanceName, email_confirmed: true, is_temp: false });

  const auth = {
    isSignedIn: () => true,
    async signIn() {
      return { success: true };
    },
    signOut() {},
    async getUser() {
      return user;
    },
    async getProfilePicture() {
      return undefined;
    },
    async getMonthlyUsage() {
      return {};
    },
  };

  const unsupported = (what) => async () => {
    throw new Error(`${what} is not available on ato.run`);
  };
  const workers = {
    create: unsupported('Puter workers'),
    exec: unsupported('Puter workers'),
    delete: unsupported('Puter workers'),
    list: async () => [],
    get: async () => null,
  };

  globalThis.puter = {
    env: 'web',
    quiet: true,
    ato: { shim: 'ato-puter-shim', version: 1 },
    auth,
    kv,
    fs,
    workers,
    ai: { chat: unsupported('Puter AI'), img: unsupported('Puter AI'), img2txt: unsupported('Puter AI'), txt2img: unsupported('Puter AI') },
    hosting: { create: unsupported('Puter hosting'), delete: unsupported('Puter hosting'), list: async () => [] },
    net: { fetch: (...a) => fetch(...a) },
    print: (...a) => console.log(...a),
    randName: () => Math.random().toString(36).slice(2, 10),
    setAuthToken() {},
    authToken: 'ato',
  };
})();
