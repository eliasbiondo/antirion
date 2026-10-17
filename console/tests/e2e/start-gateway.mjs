// Starts a test-mode gateway for the end-to-end suite with a fresh embedded store on the e2e ports and
// local accounts on, so invitations can be accepted with a password.
import { spawn } from 'node:child_process';
import { mkdirSync, rmSync, writeFileSync } from 'node:fs';
import { homedir } from 'node:os';
import { join, resolve } from 'node:path';

const bin = resolve(process.env.ANTIRION_BIN ?? '../target/debug/antirion');
const dir = resolve(
  process.env.ANTIRION_E2E_DIR ?? join(homedir(), '.cache', 'antirion-console', 'e2e-gateway'),
);
const data = Number(process.env.ANTIRION_E2E_DATA_PORT ?? 20480);
const admin = Number(process.env.ANTIRION_E2E_ADMIN_PORT ?? 20481);

rmSync(join(dir, 'data'), { recursive: true, force: true });
mkdirSync(dir, { recursive: true });
const config = join(dir, 'gateway.yaml');
writeFileSync(
  config,
  [
    'version: 1',
    'server:',
    `  listen: "127.0.0.1:${data}"`,
    'admin:',
    `  listen: "127.0.0.1:${admin}"`,
    'store:',
    '  backend: embedded',
    '  embedded:',
    `    path: "${join(dir, 'data')}"`,
    'auth:',
    '  admin:',
    '    bootstrap_token: "${env:ANTIRION_BOOTSTRAP_TOKEN}"',
    '    local:',
    '      enabled: true',
    '',
  ].join('\n'),
);

const child = spawn(bin, ['serve', '--test-mode', '--config', config], {
  stdio: 'inherit',
  env: {
    ...process.env,
    ANTIRION_BOOTSTRAP_TOKEN:
      process.env.ANTIRION_BOOTSTRAP_TOKEN ?? 'dev-bootstrap-token-0123456789abcdef0123',
  },
});
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => child.kill(signal));
child.on('exit', (code) => process.exit(code ?? 0));
