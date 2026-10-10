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
