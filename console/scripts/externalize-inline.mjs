// Post-processes the static export so it runs under the console's Content-Security-Policy
// (`script-src 'self'; style-src 'self'`): every inline <script> the framework emits becomes a
// hashed file under _next/static/inline/, loaded by a plain (blocking, in-order) <script src>, which
// keeps the inline scripts' execution order. The build then fails if any inline script, <style>
// element or style attribute remains, or if development-only mock code reached the output.
import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join, relative } from 'node:path';

const outDir = process.argv[2] ?? 'out';
const uiPath = (process.env.ANTIRION_UI_PATH ?? '/ui').replace(/\/+$/, '');
const inlineDir = join(outDir, '_next', 'static', 'inline');

function walk(dir) {
  const files = [];
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) files.push(...walk(path));
    else files.push(path);
  }
  return files;
}

const SCRIPT = /<script(?![^>]*\bsrc=)([^>]*)>([\s\S]*?)<\/script>/g;

mkdirSync(inlineDir, { recursive: true });
let moved = 0;
const problems = [];

for (const file of walk(outDir).filter((f) => f.endsWith('.html'))) {
  const html = readFileSync(file, 'utf8');
  const rewritten = html.replace(SCRIPT, (whole, attrs, body) => {
    if (/type=["']application\/(ld\+)?json["']/.test(attrs)) return whole;
    const hash = createHash('sha256').update(body).digest('hex').slice(0, 20);
    const target = join(inlineDir, `${hash}.js`);
    if (!existsSync(target)) writeFileSync(target, body);
    moved += 1;
    const kept = attrs.replace(/\s+(async|defer)(=["'][^"']*["'])?/g, '');
    return `<script${kept} src="${uiPath}/_next/static/inline/${hash}.js"></script>`;
  });
  if (rewritten !== html) writeFileSync(file, rewritten);
  if (SCRIPT.test(rewritten)) problems.push(`${relative(outDir, file)}: inline <script> left`);
  SCRIPT.lastIndex = 0;
  if (/<style[\s>]/.test(rewritten)) problems.push(`${relative(outDir, file)}: inline <style> element`);
  if (/\sstyle="/.test(rewritten)) problems.push(`${relative(outDir, file)}: style attribute`);
}

const worker = join(outDir, 'mockServiceWorker.js');
if (existsSync(worker)) rmSync(worker);
for (const file of walk(join(outDir, '_next'))) {
  if (file.endsWith('.js') && /mockServiceWorker|msw\/browser/.test(readFileSync(file, 'utf8'))) {
    problems.push(`${relative(outDir, file)}: contains development mock code`);
  }
}

let bytes = 0;
for (const file of walk(outDir)) bytes += statSync(file).size;

if (problems.length > 0) {
  console.error(`CSP check failed:\n  ${problems.join('\n  ')}`);
  process.exit(1);
}
console.log(`externalized ${moved} inline scripts; ${outDir}/ is ${(bytes / 1024 / 1024).toFixed(2)} MB`);
