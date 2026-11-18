import { readFileSync, writeFileSync, readdirSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';
export const desktop = fileURLToPath(new URL('../', import.meta.url));
export const stage = resolve(desktop, 'dist/public');
export function stagePackage() {
  // NodeNext consumers must resolve emitted declaration references, not source aliases.
  function patch(dir) {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const file = join(dir, entry.name);
      if (entry.isDirectory()) patch(file);
      else if (entry.name.endsWith('.d.ts')) {
        const text = readFileSync(file, 'utf8').replace(/(from\s+["'])(\.[^"']+)(["'])/g,
          (_, before, path, after) => before + (/\.(js|json)$/.test(path) ? path : path + '.js') + after);
        writeFileSync(file, text);
      }
    }
  }
  patch(join(stage, 'types'));
  writeFileSync(join(stage, 'package.json'), readFileSync(join(desktop, 'public-package.json')));
}
if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) stagePackage();
