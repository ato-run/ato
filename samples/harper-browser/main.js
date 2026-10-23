import { WorkerLinter, createBinaryModuleFromUrl } from 'harper.js';

const input = document.querySelector('textarea');
const status = document.querySelector('[role=status]');
const results = document.querySelector('ol');
let linter;
let generation = 0;

// Document text stays in browser storage. localStorage can be mirrored by
// an embedding host, so it must never contain documents.
const database = await new Promise((resolve, reject) => {
  const request = indexedDB.open('harper-documents', 1);
  request.onupgradeneeded = () => request.result.createObjectStore('documents');
  request.onsuccess = () => resolve(request.result);
  request.onerror = () => reject(request.error);
});
function documentOperation(mode, operation) {
  return new Promise((resolve, reject) => {
    const transaction = database.transaction('documents', mode);
    const request = operation(transaction.objectStore('documents'));
    transaction.oncomplete = () => resolve(request.result);
    transaction.onabort = () => reject(transaction.error ?? request.error);
    transaction.onerror = () => reject(transaction.error ?? request.error);
  });
}
const saved = await documentOperation('readonly', store => store.get('text'));
if (typeof saved === 'string') input.value = saved;

async function check() {
  if (!linter) return;
  const id = ++generation;
  const text = input.value;
  status.textContent = 'Checking…';
  try {
    const lints = await linter.lint(text);
    if (id !== generation) return;
    results.replaceChildren();
    for (const lint of lints) {
      const item = document.createElement('li');
      const message = document.createElement('p');
      message.textContent = lint.message();
      item.append(message);
      for (const suggestion of lint.suggestions()) {
        const button = document.createElement('button');
        button.textContent = 'Apply: ' + suggestion.get_replacement_text();
        button.addEventListener('click', async () => {
          if (text !== input.value) return;
          input.value = await linter.applySuggestion(text, lint, suggestion);
          await check();
        });
        item.append(button);
      }
      results.append(item);
    }
    status.textContent = lints.length + ' issues';
  } catch {
    status.textContent = 'Grammar check failed';
  }
}
document.querySelector('#check').addEventListener('click', check);
document.querySelector('#save').addEventListener('click', async () => {
  try {
    await documentOperation('readwrite', store => store.put(input.value, 'text'));
    status.textContent = 'Saved in this browser';
  } catch {
    status.textContent = 'Saving failed';
  }
});
document.querySelector('#download').addEventListener('click', () => {
  const url = URL.createObjectURL(new Blob([input.value], { type: 'text/plain;charset=utf-8' }));
  const anchor = document.createElement('a');
  anchor.href = url;
  anchor.download = 'harper-text.txt';
  anchor.click();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
});

try {
  // Fetch the immutable library binary once. Both Harper's UI serializer
  // and its Worker can reuse this browser-owned copy without concurrent
  // network/cache writes for the same large resource.
  const response = await fetch(new URL('./harper.wasm', import.meta.url));
  if (!response.ok) throw new Error('WASM download failed');
  const binaryUrl = URL.createObjectURL(new Blob([await response.arrayBuffer()], { type: 'application/wasm' }));
  linter = new WorkerLinter({ binary: createBinaryModuleFromUrl(binaryUrl) });
  await linter.setup();
  status.textContent = 'Ready';
} catch {
  status.textContent = 'Grammar checker could not start';
}
