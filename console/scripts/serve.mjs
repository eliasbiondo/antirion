// Serves the static export the way the gateway's admin listener does: the files of out/ under the
// console path, with the console's security headers, and /admin/v1/* proxied to a gateway's admin
// listener on the same origin (so the session cookie and the Origin check behave as in production).
//
//   ANTIRION_ADMIN_URL=http://127.0.0.1:20481 PORT=20482 node scripts/serve.mjs
import { createReadStream, existsSync, statSync } from 'node:fs';
import { createServer, request as httpRequest } from 'node:http';
import { extname, join, normalize, resolve } from 'node:path';

const root = resolve(process.env.OUT_DIR ?? 'out');
const uiPath = (process.env.ANTIRION_UI_PATH ?? '/ui').replace(/\/+$/, '');
const admin = new URL(process.env.ANTIRION_ADMIN_URL ?? 'http://127.0.0.1:20481');
const port = Number(process.env.PORT ?? 20482);
const host = process.env.HOST ?? '127.0.0.1';

const CSP =
  "default-src 'self'; img-src 'self' data: https:; style-src 'self'; script-src 'self'; connect-src 'self'; " +
  "frame-ancestors 'none'; base-uri 'none'; form-action 'self'";

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json',
  '.txt': 'text/plain; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.woff2': 'font/woff2',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
};

function securityHeaders(type) {
  const headers = {
    'content-security-policy': CSP,
    'x-frame-options': 'DENY',
    'x-content-type-options': 'nosniff',
    'referrer-policy': 'no-referrer',
  };
  if (type.startsWith('text/html')) headers['cache-control'] = 'no-store';
  return headers;
}

function sendFile(res, file, status = 200) {
  const type = TYPES[extname(file)] ?? 'application/octet-stream';
  const immutable = file.includes(`${join('_next', 'static')}`);
  res.writeHead(status, {
    'content-type': type,
    ...securityHeaders(type),
    ...(immutable ? { 'cache-control': 'public, max-age=31536000, immutable' } : {}),
  });
  createReadStream(file).pipe(res);
}

function resolveFile(pathname) {
  const rel = normalize(decodeURIComponent(pathname)).replace(/^(\.\.[/\\])+/, '');
  const candidate = join(root, rel);
  if (!candidate.startsWith(root)) return null;
  if (existsSync(candidate) && statSync(candidate).isFile()) return candidate;
  const index = join(candidate, 'index.html');
  if (existsSync(index)) return index;
  return null;
}

function proxy(req, res) {
  const upstream = httpRequest(
    {
      hostname: admin.hostname,
      port: admin.port,
      path: req.url,
      method: req.method,
      headers: req.headers,
    },
    (answer) => {
      res.writeHead(answer.statusCode ?? 502, answer.headers);
      answer.pipe(res);
    },
  );
  upstream.on('error', () => {
    res.writeHead(502, { 'content-type': 'application/json' });
    res.end(
      JSON.stringify({
        error: { code: 'bad_gateway', message: 'The gateway is not reachable.', request_id: '' },
      }),
    );
  });
  req.pipe(upstream);
}

createServer((req, res) => {
  const url = new URL(req.url ?? '/', `http://${req.headers.host ?? 'localhost'}`);
  if (url.pathname.startsWith('/admin/v1/')) return proxy(req, res);
  if (url.pathname === '/' || url.pathname === uiPath) {
    res.writeHead(308, { location: `${uiPath}/${url.search}` });
    return res.end();
  }
  if (!url.pathname.startsWith(`${uiPath}/`)) {
    res.writeHead(404, { 'content-type': 'text/plain' });
    return res.end('not found');
  }
  const inner = url.pathname.slice(uiPath.length);
  const file = resolveFile(inner);
  if (file) {
    if (file.endsWith('index.html') && !inner.endsWith('/') && extname(inner) === '') {
      res.writeHead(308, { location: `${url.pathname}/${url.search}` });
      return res.end();
    }
    return sendFile(res, file);
  }
  return sendFile(res, join(root, '404.html'), 404);
}).listen(port, host, () => {
  console.log(`console on http://${host}:${port}${uiPath}/ → admin ${admin.origin}`);
});
