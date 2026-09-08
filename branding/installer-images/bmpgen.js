// Rasterise the brand SVGs to installer BMPs. No image deps available on this machine, so the
// browser draws the SVG onto a canvas and POSTs raw RGBA here; this writes a 24-bit BMP.
const http = require('http'), fs = require('fs'), path = require('path');
const ROOT = 'D:/Users/DKboy/Documents/code-workspace/multi64';
const OUT = process.argv[2];
fs.mkdirSync(OUT, { recursive: true });

function writeBmp(file, w, h, rgba) {
  const rowRaw = w * 3;
  const pad = (4 - (rowRaw % 4)) % 4;
  const rowSize = rowRaw + pad;
  const pixels = Buffer.alloc(rowSize * h, 0);
  for (let y = 0; y < h; y++) {
    // BMP scanlines run bottom-up.
    const src = (h - 1 - y) * w * 4;
    let dst = y * rowSize;
    for (let x = 0; x < w; x++) {
      const i = src + x * 4;
      pixels[dst++] = rgba[i + 2]; // B
      pixels[dst++] = rgba[i + 1]; // G
      pixels[dst++] = rgba[i + 0]; // R
    }
  }
  const header = Buffer.alloc(54);
  header.write('BM', 0);
  header.writeUInt32LE(54 + pixels.length, 2);
  header.writeUInt32LE(54, 10);
  header.writeUInt32LE(40, 14);
  header.writeInt32LE(w, 18);
  header.writeInt32LE(h, 22);
  header.writeUInt16LE(1, 26);
  header.writeUInt16LE(24, 28);
  header.writeUInt32LE(pixels.length, 34);
  header.writeInt32LE(2835, 38);
  header.writeInt32LE(2835, 42);
  fs.writeFileSync(file, Buffer.concat([header, pixels]));
}

const types = { '.html':'text/html', '.svg':'image/svg+xml', '.js':'text/javascript' };
http.createServer((req, res) => {
  const u = new URL(req.url, 'http://x');
  if (req.method === 'POST' && u.pathname === '/bmp') {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', () => {
      const w = +u.searchParams.get('w'), h = +u.searchParams.get('h');
      const name = u.searchParams.get('name').replace(/[^a-z0-9_.-]/gi, '');
      const file = path.join(OUT, name);
      writeBmp(file, w, h, Buffer.concat(chunks));
      console.log(`wrote ${name}  ${w}x${h}  ${fs.statSync(file).size} bytes`);
      res.writeHead(200); res.end('ok');
    });
    return;
  }
  const file = u.pathname === '/' ? path.join(OUT, 'gen.html') : path.join(ROOT, u.pathname.slice(1));
  fs.readFile(file, (e, b) => {
    if (e) { res.writeHead(404); return res.end('nf ' + u.pathname); }
    res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' });
    res.end(b);
  });
}).listen(8792, () => console.log('bmpgen on 8792, out=' + OUT));
