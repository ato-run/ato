import { strToU8, zipSync } from 'fflate';
import files from './source-manifest.json';
const status = document.querySelector<HTMLElement>('#status')!;
const root = document.querySelector<HTMLElement>('#files')!;
const content: Record<string, Uint8Array> = {};
for (const [path, text] of Object.entries(files)) {
  content[path] = strToU8(text);
  const section = document.createElement('details');
  const heading = document.createElement('summary');
  const pre = document.createElement('pre');
  heading.textContent = path;
  pre.textContent = text;
  section.append(heading, pre);
  root.append(section);
}
status.textContent = `${Object.keys(content).length} source files available.`;
document.querySelector<HTMLButtonElement>('#download')!.addEventListener('click', () => {
  const bytes = zipSync(content);
  const url = URL.createObjectURL(new Blob([bytes], {type:'application/zip'}));
  const a = document.createElement('a');
  a.href = url; a.download = 'openkitchen-browser-source.zip';
  document.body.append(a); a.click(); a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 60000);
});
