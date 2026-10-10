// Local-only persistence: the canvas snapshot and asset blobs live in IndexedDB.
// (With a host attached, the host owns the project on disk and this becomes a cache.)

const DB = 'falos';
const VERSION = 1;

export interface StoredAsset { id: string; blob: Blob; mime: string; name: string }

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, VERSION);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (!db.objectStoreNames.contains('docs')) db.createObjectStore('docs');
      if (!db.objectStoreNames.contains('assets')) db.createObjectStore('assets', { keyPath: 'id' });
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error);
  });
}

let dbp: Promise<IDBDatabase> | null = null;
const db = () => (dbp ??= open());

function tx<T>(store: string, mode: IDBTransactionMode, f: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  return db().then(
    (d) =>
      new Promise<T>((resolve, reject) => {
        const t = d.transaction(store, mode);
        const req = f(t.objectStore(store));
        req.onsuccess = () => resolve(req.result);
        req.onerror = () => reject(req.error);
      }),
  );
}

export const saveSnapshot = (canvasId: string, bytes: Uint8Array) =>
  tx('docs', 'readwrite', (s) => s.put(bytes, canvasId));

export const loadSnapshot = (canvasId: string) =>
  tx<Uint8Array | undefined>('docs', 'readonly', (s) => s.get(canvasId));

export const saveAsset = (a: StoredAsset) => tx('assets', 'readwrite', (s) => s.put(a));

export const loadAssets = () => tx<StoredAsset[]>('assets', 'readonly', (s) => s.getAll());

export async function clearAll(canvasId: string) {
  await tx('docs', 'readwrite', (s) => s.delete(canvasId));
  await tx('assets', 'readwrite', (s) => s.clear());
}

// ---- portable bundle (.falos.json): snapshot + assets, base64 ----

function b64(bytes: Uint8Array): string {
  let s = '';
  for (let i = 0; i < bytes.length; i += 0x8000) s += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(s);
}

function unb64(s: string): Uint8Array {
  const bin = atob(s);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

export async function exportBundle(snapshot: Uint8Array, assets: StoredAsset[]): Promise<Blob> {
  const list = await Promise.all(
    assets.map(async (a) => ({ id: a.id, mime: a.mime, name: a.name, data: b64(new Uint8Array(await a.blob.arrayBuffer())) })),
  );
  return new Blob([JSON.stringify({ format: 'falos-canvas', version: 1, snapshot: b64(snapshot), assets: list })], {
    type: 'application/json',
  });
}

export async function importBundle(file: Blob): Promise<{ snapshot: Uint8Array; assets: StoredAsset[] }> {
  const v = JSON.parse(await file.text());
  if (v.format !== 'falos-canvas') throw new Error('Not a fal-os canvas file');
  return {
    snapshot: unb64(v.snapshot),
    assets: v.assets.map((a: { id: string; mime: string; name: string; data: string }) => ({
      id: a.id,
      mime: a.mime,
      name: a.name,
      blob: new Blob([unb64(a.data)], { type: a.mime }),
    })),
  };
}
