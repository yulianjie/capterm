// 用无头 Chrome / Edge 把舵轮图标栅格化为 PNG，打包 Windows ICO，并生成预览图。
// 用法：node design/tools/export-icon.mjs
// 16–48 px 使用小尺寸稿 capterm-helm-small.svg，64 px 及以上使用主稿 capterm-helm.svg。
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const iconDir = resolve(here, '..', 'icon', 'helm');
const pngDir = join(iconDir, 'png');
const icoSizes = [16, 20, 24, 32, 40, 48, 64, 96, 128, 256];
const pngSizes = [16, 24, 32, 48, 64, 128, 256, 512, 1024];
const smallMax = 48;

const candidates = [
  process.env.CHROME,
  'C:/Program Files/Google/Chrome/Application/chrome.exe',
  'C:/Program Files (x86)/Microsoft/Edge/Application/msedge.exe',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium'
].filter(Boolean);
const browser = candidates.find((p) => existsSync(p));
if (!browser) {
  console.error('找不到 Chrome 或 Edge，请用 CHROME 环境变量指定浏览器路径。');
  process.exit(1);
}

const tmp = mkdtempSync(join(tmpdir(), 'capterm-icon-'));

function shoot(html, name, [w, h], transparent) {
  const page = join(tmp, `${name}.html`);
  const out = join(tmp, `${name}.png`);
  writeFileSync(page, html);
  execFileSync(browser, [
    '--headless=new', '--disable-gpu', '--hide-scrollbars', '--allow-file-access-from-files',
    '--force-device-scale-factor=1', `--window-size=${w},${h}`, `--screenshot=${out}`,
    ...(transparent ? ['--default-background-color=00000000'] : []),
    pathToFileURL(page).href
  ], { stdio: 'ignore' });
  return readFileSync(out);
}

function render(size) {
  const svg = join(iconDir, size <= smallMax ? 'capterm-helm-small.svg' : 'capterm-helm.svg');
  const png = shoot(
    `<!doctype html><body style="margin:0;background:transparent"><img src="${pathToFileURL(svg).href}" style="display:block;width:${size}px;height:${size}px">`,
    `icon-${size}`, [size, size], true);
  const [w, h] = [png.readUInt32BE(16), png.readUInt32BE(20)];
  if (w !== size || h !== size) throw new Error(`${size} px 渲染结果为 ${w}×${h}`);
  return png;
}

// ICO 的每一帧直接存 PNG 数据（Windows Vista 起支持），256 px 在目录项里记为 0。
function ico(frames) {
  const dir = Buffer.alloc(6 + 16 * frames.length);
  dir.writeUInt16LE(1, 2);
  dir.writeUInt16LE(frames.length, 4);
  let offset = dir.length;
  frames.forEach(({ size, png }, i) => {
    const e = 6 + 16 * i;
    dir.writeUInt8(size % 256, e);
    dir.writeUInt8(size % 256, e + 1);
    dir.writeUInt16LE(1, e + 4);
    dir.writeUInt16LE(32, e + 6);
    dir.writeUInt32LE(png.length, e + 8);
    dir.writeUInt32LE(offset, e + 12);
    offset += png.length;
  });
  return Buffer.concat([dir, ...frames.map((f) => f.png)]);
}

try {
  mkdirSync(pngDir, { recursive: true });
  const frames = new Map();
  for (const size of [...new Set([...icoSizes, ...pngSizes])].sort((a, b) => a - b)) {
    frames.set(size, render(size));
  }
  for (const size of pngSizes) {
    writeFileSync(join(pngDir, `capterm-helm-${size}.png`), frames.get(size));
  }
  const icoFile = join(iconDir, 'capterm-helm.ico');
  writeFileSync(icoFile, ico(icoSizes.map((size) => ({ size, png: frames.get(size) }))));
  console.log(`png/  ${pngSizes.join('、')} px`);
  console.log(`capterm-helm.ico  ${icoSizes.length} 帧  ${Math.round(statSync(icoFile).size / 1024)} KB`);

  // 预览：上方是主稿，下方是各尺寸的实际像素，左浅右深。
  const ladder = [128, 64, 48, 32, 24, 16];
  const img = (size) => `data:image/png;base64,${frames.get(size).toString('base64')}`;
  const side = (theme) => `
    <section class="${theme}">
      <img class="hero" src="${img(256)}" width="256" height="256" alt="">
      <div class="ladder">${ladder.map((s) => `<figure><img src="${img(s)}" width="${s}" height="${s}" alt=""><figcaption>${s}</figcaption></figure>`).join('')}</div>
    </section>`;
  const preview = shoot(`<!doctype html><meta charset="utf-8"><style>
    body { margin: 0; display: grid; grid-template-columns: 1fr 1fr; width: 1120px; font: 12px/16px "Segoe UI", system-ui, sans-serif; }
    section { display: grid; justify-items: center; gap: 36px; padding: 48px 0 36px; }
    .light { background: #F3F5F8; color: #65717F; }
    .dark { background: #0F1317; color: #7A8695; }
    .hero { display: block; }
    .ladder { display: flex; align-items: flex-end; gap: 26px; }
    figure { margin: 0; display: grid; justify-items: center; gap: 10px; }
    figure img { display: block; }
    figcaption { font-variant-numeric: tabular-nums; }
  </style>${side('light')}${side('dark')}`, 'preview', [1120, 560], false);
  writeFileSync(join(iconDir, 'preview.png'), preview);
  console.log('preview.png');
} finally {
  rmSync(tmp, { recursive: true, force: true });
}
