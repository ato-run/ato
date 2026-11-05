import { lockedDatabase, withDatabase } from "@/lib/browser-sql";
import { makeBackup } from "@/lib/full-backup";

export interface BackupFileInfo { name: string; isAuto: boolean }
const openStore = new Promise<IDBDatabase>((resolve, reject) => {
  const request = indexedDB.open("wakerun-browser-backups-v1", 1);
  request.onupgradeneeded = () => request.result.createObjectStore("backups");
  request.onsuccess = () => resolve(request.result);
  request.onerror = () => reject(request.error);
  request.onblocked = () => reject(new Error("Backup storage is blocked"));
});

async function names(): Promise<string[]> {
  const store = await openStore;
  return new Promise((resolve, reject) => {
    const transaction = store.transaction("backups", "readonly");
    const request = transaction.objectStore("backups").getAllKeys();
    request.onsuccess = () => resolve(request.result.map(String).sort().reverse());
    request.onerror = () => reject(request.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

async function save(name: string, json: string): Promise<void> {
  const store = await openStore;
  return new Promise((resolve, reject) => {
    const transaction = store.transaction("backups", "readwrite");
    transaction.objectStore("backups").put(json, name);
    transaction.oncomplete = () => resolve();
    transaction.onerror = () => reject(transaction.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

async function backup(): Promise<string> {
  return withDatabase(async () => makeBackup(await lockedDatabase()));
}

export async function readBackup(name: string): Promise<string> {
  const store = await openStore;
  return new Promise((resolve, reject) => {
    const transaction = store.transaction("backups", "readonly");
    const request = transaction.objectStore("backups").get(name);
    request.onsuccess = () => typeof request.result === "string"
      ? resolve(request.result) : reject(new Error("Backup not found"));
    request.onerror = () => reject(request.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

async function saveAuto(name: string, json: string): Promise<boolean> {
  const store = await openStore;
  return new Promise((resolve, reject) => {
    const transaction = store.transaction("backups", "readwrite");
    const objectStore = transaction.objectStore("backups");
    const request = objectStore.getAllKeys();
    let created = false;
    request.onsuccess = () => {
      const keys = request.result.map(String);
      if (keys.includes(name)) return;
      objectStore.put(json, name);
      created = true;
      const old = keys.filter((key) => key.startsWith("wakerun-auto-")).sort().reverse();
      for (const key of old.slice(2)) objectStore.delete(key);
    };
    transaction.oncomplete = () => resolve(created);
    transaction.onerror = () => reject(transaction.error);
    transaction.onabort = () => reject(transaction.error);
  });
}

export async function listBackupFiles(): Promise<BackupFileInfo[]> {
  return (await names()).map((name) => ({ name, isAuto: name.startsWith("wakerun-auto-") }));
}

export async function createManualBackup(): Promise<{ filename: string }> {
  const filename = `wakerun-manual-${new Date().toISOString().replace(/[:.]/g, "-")}.wakerun`;
  await save(filename, await backup());
  return { filename };
}

export async function runAutoBackup(): Promise<{ created: boolean; filename?: string; error?: string }> {
  try {
    const filename = `wakerun-auto-${new Date().toISOString().slice(0, 10)}.wakerun`;
    if ((await names()).includes(filename)) return { created: false, filename };
    return { created: await saveAuto(filename, await backup()), filename };
  } catch (error) {
    return { created: false, error: error instanceof Error ? error.message : String(error) };
  }
}
