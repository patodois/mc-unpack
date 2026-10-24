// Renders a glTF exported by mc-unpack to a PNG in headless Chromium (three.js, nearest filtering).
//
//   npm install            (in this folder)
//   node shot.cjs out.png path/to/model.gltf "fit=1.0&dir=0.7,0.6,1&look=mc" 800x700
//
// Query options (third argument): fit=<scale> frames the model automatically (dir=x,y,z gives the
// view direction), pos/tgt/fov for a fixed camera, bg=<hex>, look=mc to shade faces by direction
// the way the game does (top 1.0, bottom 0.5, north and south 0.8, east and west 0.6; a viewer
// styling, not data). REFS=a.png,b.png in the environment shows those images, enlarged and
// unfiltered, in a panel beside the render (the raw textures of the jar, as a reference).
// CHROMIUM=<path> picks a browser binary.
const http = require('http'), fs = require('fs'), path = require('path');
const { chromium } = require('playwright-core');
const here = __dirname;
const types = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.gltf': 'model/gltf+json', '.bin': 'application/octet-stream', '.png': 'image/png' };
const [out, model, extra, size] = process.argv.slice(2);
const modelDir = path.dirname(path.resolve(model));
const refs = (process.env.REFS || '').split(',').filter(Boolean).map((p) => path.resolve(p));
const server = http.createServer((req, res) => {
  const u = decodeURIComponent(req.url.split('?')[0]);
  if (u.startsWith('/r/')) {
    const f = refs[Number(u.slice(3))];
    if (!f || !fs.existsSync(f)) { res.writeHead(404); return res.end(); }
    res.writeHead(200, { 'content-type': 'image/png' });
    return fs.createReadStream(f).pipe(res);
  }
  // /m/... serves the model's own folder, everything else this folder (page and node_modules)
  const base = u.startsWith('/m/') ? modelDir : here;
  const p = path.join(base, u.startsWith('/m/') ? u.slice(3) : u);
  if (!p.startsWith(base) || !fs.existsSync(p) || fs.statSync(p).isDirectory()) { res.writeHead(404); return res.end(); }
  res.writeHead(200, { 'content-type': types[path.extname(p)] || 'application/octet-stream' });
  fs.createReadStream(p).pipe(res);
});
(async () => {
  await new Promise((r) => server.listen(0, r));
  const port = server.address().port;
  const exe = process.env.CHROMIUM;
  const browser = await chromium.launch({ executablePath: exe && fs.existsSync(exe) ? exe : undefined, args: ['--use-angle=metal', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist'] });
  const [w, h] = (size || '1280x720').split('x').map(Number);
  const page = await browser.newPage({ viewport: { width: w, height: h } });
  page.on('pageerror', (e) => console.log('pageerror', String(e).slice(0, 300)));
  await page.goto(`http://127.0.0.1:${port}/view.html?m=${encodeURIComponent('/m/' + path.basename(model))}&${extra || ''}&refs=${encodeURIComponent(refs.map((f, i) => i + ':' + path.basename(f)).join(','))}`);
  await page.waitForFunction('window.done === true', null, { timeout: 90000 });
  console.log(JSON.stringify(await page.evaluate('window.result')));
  await page.screenshot({ path: out });
  await browser.close(); server.close();
})();
