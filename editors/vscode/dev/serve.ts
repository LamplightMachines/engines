// The panel harness's server (ADR 0040): `npm run harness` → http://127.0.0.1:5179. Serves the
// webview's own media (tokens.css, panel.css, panel.js) under a page that stubs the VS Code API, and
// `/model?scenario=…&t=…` — a PanelModel from the real state fold and view model (scenarios.ts).

import * as fs from 'node:fs';
import * as http from 'node:http';
import * as path from 'node:path';
import { NAMES, scenario } from './scenarios';

const PORT = Number(process.env.PORT ?? 5179);
const ROOT = path.resolve(__dirname, '..', '..');
const TYPES: Record<string, string> = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript', '.woff2': 'font/woff2', '.json': 'application/json' };

http
  .createServer((req, res) => {
    const url = new URL(req.url ?? '/', `http://127.0.0.1:${PORT}`);
    if (url.pathname === '/model') {
      const m = scenario(url.searchParams.get('scenario') ?? 'song-playing', Number(url.searchParams.get('t') ?? 0));
      res.writeHead(m ? 200 : 404, { 'content-type': 'application/json' });
      res.end(JSON.stringify(m ?? { error: 'no such scenario' }));
      return;
    }
    if (url.pathname === '/scenarios') {
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify(NAMES));
      return;
    }
    const rel = url.pathname === '/' ? 'dev/harness.html' : url.pathname.slice(1);
    const file = path.resolve(ROOT, rel);
    // Only the harness page and the webview's media.
    if (!(file === path.join(ROOT, 'dev', 'harness.html') || file.startsWith(path.join(ROOT, 'media') + path.sep))) {
      res.writeHead(404);
      res.end();
      return;
    }
    fs.readFile(file, (err, data) => {
      if (err) {
        res.writeHead(404);
        res.end();
      return;
      }
      res.writeHead(200, { 'content-type': TYPES[path.extname(file)] ?? 'application/octet-stream' });
      res.end(data);
    });
  })
  .listen(PORT, '127.0.0.1', () => console.log(`panel harness: http://127.0.0.1:${PORT}`));
