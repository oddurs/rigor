// After `next build`: every class the page uses must have a rule behind it.
//
// This is the failure this site has already had once. StyleX's webpack plugin
// put its CSS into a compilation whose asset Next did not serve, so the HTML
// carried atomic class names with nothing behind them — the build reported
// success and the page rendered unstyled. A green build proves nothing here.
import { readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const out = new URL('../out/', import.meta.url).pathname;
const html = readFileSync(join(out, 'index.html'), 'utf8');
const cssDir = join(out, '_next/static/css');
const css = readdirSync(cssDir)
  .filter((f) => f.endsWith('.css'))
  .map((f) => readFileSync(join(cssDir, f), 'utf8'))
  .join('\n');

const used = new Set(
  [...html.matchAll(/class="([^"]+)"/g)]
    .flatMap((m) => m[1].split(/\s+/))
    .filter(Boolean),
);
const escape = (s) => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
const missing = [...used].filter((c) => !new RegExp(`\\.${escape(c)}[,{:\\s]`).test(css));

if (used.size === 0) {
  console.error('check-css: the page uses no classes; is out/index.html the real build?');
  process.exit(1);
}
if (missing.length > 0) {
  console.error(`check-css: ${missing.length} of ${used.size} classes have no CSS rule:`);
  console.error(`  ${missing.slice(0, 12).join(' ')}`);
  process.exit(1);
}
console.log(`check-css: all ${used.size} classes have rules`);
