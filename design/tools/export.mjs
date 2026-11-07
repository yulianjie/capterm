// 用无头 Chrome / Edge 把原型的各个场景导出为 PNG 画板（2 倍分辨率）。
// 用法：node design/tools/export.mjs [场景名…]
// 不带参数时导出全部画板到 design/exports/。
import { execFileSync } from 'node:child_process';
import { existsSync, mkdirSync, statSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const designDir = resolve(here, '..');
const outDir = join(designDir, 'exports');
const page = pathToFileURL(join(designDir, 'prototype', 'index.html')).href;

export const boards = [
  { file: '01-workbench-dark', query: 'scene=workbench&theme=dark&hover=cpu:40' },
  { file: '02-workbench-light-search', query: 'scene=search&theme=light' },
  { file: '03-agent-cli-dark', query: 'scene=agent&theme=dark' },
  { file: '04-fullscreen-tui-light', query: 'scene=tui&theme=light' },
  { file: '05-file-editor-light', query: 'scene=editor&theme=light' },
  { file: '06-drag-upload-dark', query: 'scene=upload&theme=dark' },
  { file: '07-session-jump-dark', query: 'scene=session&theme=dark' },
  { file: '08-import-ssh-config-light', query: 'scene=import&theme=light' },
  { file: '09-first-connect-dark', query: 'scene=connect&theme=dark' },
  { file: '10-settings-light', query: 'scene=settings&theme=light' },
  { file: '11-compact-dark', query: 'scene=workbench&theme=dark', size: [1120, 760] }
];

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

mkdirSync(outDir, { recursive: true });
const only = process.argv.slice(2);
for (const b of boards.filter((x) => !only.length || only.some((o) => x.file.includes(o)))) {
  const [w, h] = b.size || [1440, 900];
  const out = join(outDir, `${b.file}.png`);
  execFileSync(browser, [
    '--headless=new', '--disable-gpu', '--hide-scrollbars',
    '--force-device-scale-factor=2', `--window-size=${w},${h}`,
    '--virtual-time-budget=4000', `--screenshot=${out}`,
    `${page}?${b.query}&static=1`
  ], { stdio: 'ignore' });
  console.log(`${b.file}.png  ${Math.round(statSync(out).size / 1024)} KB`);
}
