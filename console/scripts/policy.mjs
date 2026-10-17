// The repository's text policy (ci/policy.txt) applied to the console, the way `cargo xtask policy`
// applies it to the rest of the repository: no open-work markers.
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

const repo = resolve(import.meta.dirname, '..', '..');
const patterns = [];
for (const raw of readFileSync(resolve(repo, 'ci/policy.txt'), 'utf8').split('\n')) {
  const line = raw.trim();
  if (!line || line.startsWith('#')) continue;
  if (!line.startsWith('regex:'))
    throw new Error(`ci/policy.txt: cannot read the line ${JSON.stringify(line)}`);
  patterns.push(new RegExp(line.slice(6).trim(), 'u'));
}

const files = execFileSync('git', ['ls-files', '--cached', '--others', '--exclude-standard', 'console'], {
  cwd: repo,
  encoding: 'utf8',
})
  .split('\n')
  .filter((f) => f && !f.endsWith('pnpm-lock.yaml') && !/\.(png|jpe?g|webp|woff2?|ico)$/.test(f));

const problems = [];
for (const file of files) {
  const path = resolve(repo, file);
  if (!existsSync(path)) continue;
  readFileSync(path, 'utf8')
    .split('\n')
    .forEach((line, i) => {
      for (const p of patterns) {
        const m = p.exec(line);
        if (m) problems.push(`${file}:${i + 1}: matches /${p.source}/: ${JSON.stringify(m[0])}`);
      }
    });
}
if (problems.length > 0) {
  console.error(`policy: ${problems.length} problem(s)\n${problems.slice(0, 50).join('\n')}`);
  process.exit(1);
}
console.log(`policy: ${files.length} files clean`);
