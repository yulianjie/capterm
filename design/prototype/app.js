/* CapTerm 交互原型。
 * URL 参数：
 *   theme=dark|light           主题
 *   scene=workbench|search|agent|tui|editor|upload|session|import|connect|settings
 *   static=1                   关闭动画（导出截图用）
 *   hover=cpu:40               固定某个图表的悬停位置（导出截图用）
 */
(function () {
  'use strict';

  const D = window.CT_DATA;
  const params = new URLSearchParams(location.search);
  const $ = (s, r) => (r || document).querySelector(s);
  const $$ = (s, r) => Array.from((r || document).querySelectorAll(s));
  const esc = (s) => String(s == null ? '' : s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
  const ic = (name, cls) => `<svg class="i ${cls || ''}" aria-hidden="true"><use href="#i-${name}"/></svg>`;
  const sessionById = (id) => D.sessions.find((s) => s.id === id);

  /* ------------------------------------------------------------------ 状态 */
  const S = {
    theme: params.get('theme') === 'light' ? 'light' : 'dark',
    scene: params.get('scene') || 'workbench',
    tabs: [],
    active: null,
    gutter: { num: true, time: true },
    monTab: 'overview',
    mode: 'line',
    broadcast: false,
    draft: '',
    openDirs: new Set(),
    selFile: null,
    modifiedFiles: new Set(),
    showHidden: true,
    follow: true,
    find: null,
    transfers: [],
    trOpen: true,
    drag: null,
    collapsed: new Set(),
    filter: '',
    dialog: null,
    settingsSec: 'appearance',
    toasts: [],
    hover: null,
    menu: null
  };
  if (params.has('static')) document.documentElement.setAttribute('data-static', '');
  if (params.get('hover')) {
    const [key, idx] = params.get('hover').split(':');
    S.hover = { key, idx: Number(idx) };
  }

  function termTab(id, session, trKey, extra) {
    const tr = D.transcripts[trKey || session];
    return Object.assign({
      id, kind: 'term', session, title: sessionById(session).name, status: 'connected',
      start: tr ? tr.start : 1, lines: tr ? tr.lines.slice() : [], monKey: session
    }, extra || {});
  }
  function editorTab(file, extra) {
    return Object.assign({ id: 'ed:' + file, kind: 'editor', session: 'prod-api-01', title: file, file, status: 'connected' }, extra || {});
  }
  const activeTab = () => S.tabs.find((t) => t.id === S.active) || null;
  const activeSession = () => { const t = activeTab(); return t ? sessionById(t.session) : null; };

  /* ------------------------------------------------------------------ 场景 */
  function baseScene() {
    S.tabs = [termTab('prod-api-01', 'prod-api-01'), termTab('staging', 'staging')];
    S.active = 'prod-api-01';
    S.transfers = D.transfers.idle.map((x) => Object.assign({}, x));
  }
  const SCENES = {
    workbench() { baseScene(); },
    search() { baseScene(); S.find = { q: 'worker', cur: 5 }; },
    agent() {
      S.tabs = [
        termTab('prod-api-01', 'prod-api-01', 'agent', { agent: { name: 'Claude Code', since: '运行 6 分钟' }, monKey: 'agent' }),
        termTab('staging', 'staging', 'staging', { agent: { name: 'Codex' }, badge: '待确认' })
      ];
      S.active = 'prod-api-01';
      S.monTab = 'procs';
      S.mode = 'paste';
      S.draft = 'worker 的修改先别重启。再帮我确认两件事：\n1. api 服务是否也读取了 REDIS_URL\n2. .env.production 里有没有 REDIS_PASSWORD';
      S.transfers = [];
      S.toasts = [{ icon: 'bell-ring', title: 'staging：Codex 等待你的确认', body: '要执行 <span class="mono">kubectl rollout restart deploy/web -n web</span>', time: '10:55', acts: ['切换到标签', '稍后'] }];
    },
    tui() {
      S.tabs = [termTab('prod-api-01', 'prod-api-01'), termTab('staging', 'staging', null, { alt: { app: 'htop', since: '10:55:12' } })];
      S.active = 'staging';
      S.transfers = [];
    },
    editor() {
      S.tabs = [
        termTab('prod-api-01', 'prod-api-01'),
        editorTab('compose.yaml', { modified: true }),
        editorTab('nginx.conf', { preview: true })
      ];
      S.active = 'ed:compose.yaml';
      S.selFile = 'compose.yaml';
      S.modifiedFiles.add('compose.yaml');
      S.transfers = [];
    },
    upload() {
      baseScene();
      S.openDirs = new Set(['logs']);
      S.transfers = D.transfers.upload.map((x) => Object.assign({}, x));
      S.drag = { over: 'logs', names: ['deploy.sh', 'hotfix.patch'], frozen: true };
    },
    session() { baseScene(); S.dialog = 'session'; },
    import() { baseScene(); S.dialog = 'import'; },
    connect() {
      baseScene();
      S.tabs.push(termTab('prod-db-01', 'prod-db-01', 'connect', { status: 'connecting', conn: { reached: 2 } }));
      S.active = 'prod-db-01';
      S.dialog = 'hostkey';
    },
    settings() { baseScene(); S.dialog = 'settings'; S.settingsSec = 'appearance'; }
  };
  (SCENES[S.scene] || SCENES.workbench)();

  /* ------------------------------------------------------------------ 终端文本工具 */
  // 东亚宽字符占 2 列（与 Maple Mono NF CN 的 2:1 宽度一致）
  function wcw(cp) {
    if ((cp >= 0x1100 && cp <= 0x115F) || (cp >= 0x2E80 && cp <= 0xA4CF && cp !== 0x303F) ||
        (cp >= 0xAC00 && cp <= 0xD7A3) || (cp >= 0xF900 && cp <= 0xFAFF) || (cp >= 0xFE30 && cp <= 0xFE4F) ||
        (cp >= 0xFF00 && cp <= 0xFF60) || (cp >= 0xFFE0 && cp <= 0xFFE6) || (cp >= 0x20000 && cp <= 0x3FFFD)) return 2;
    return 1;
  }
  const strW = (s) => { let w = 0; for (const ch of s) w += wcw(ch.codePointAt(0)); return w; };
  function parseMarkup(s) {
    const out = []; const re = /⟦([^|⟧]*)\|([^⟧]*)⟧/g; let i = 0; let m;
    while ((m = re.exec(s))) {
      if (m.index > i) out.push({ cls: '', text: s.slice(i, m.index) });
      out.push({ cls: m[1], text: m[2] });
      i = re.lastIndex;
    }
    if (i < s.length) out.push({ cls: '', text: s.slice(i) });
    return out;
  }
  const plain = (s) => parseMarkup(s).map((x) => x.text).join('');

  let hitCounter = 0;
  function hl(text) {
    const q = S.find && S.find.q;
    if (!q) return esc(text);
    const lower = text.toLowerCase(); const ql = q.toLowerCase();
    let out = ''; let i = 0; let j;
    while ((j = lower.indexOf(ql, i)) !== -1) {
      hitCounter += 1;
      out += esc(text.slice(i, j)) + `<mark class="hit${hitCounter === S.find.cur ? ' cur' : ''}">${esc(text.slice(j, j + q.length))}</mark>`;
      i = j + q.length;
    }
    return out + esc(text.slice(i));
  }
  const renderSegs = (s) => parseMarkup(s).map((x) => (x.cls ? `<span class="${esc(x.cls)}">${hl(x.text)}</span>` : hl(x.text))).join('');

  function boxLines(rows, w, color) {
    const inner = w - 4;
    const pad = (s) => s + ' '.repeat(Math.max(0, inner - strW(plain(s))));
    const out = [`⟦${color}|╭${'─'.repeat(w - 2)}╮⟧`];
    rows.forEach((r) => out.push(`⟦${color}|│⟧ ${pad(r)} ⟦${color}|│⟧`));
    out.push(`⟦${color}|╰${'─'.repeat(w - 2)}╯⟧`);
    return out;
  }

  /* ------------------------------------------------------------------ 终端渲染 */
  function promptSegs(ln) {
    return `<span class="p-user">${esc(ln.user)}@${esc(ln.host)}</span>:<span class="p-path">${esc(ln.cwd)}</span>$ ${hl(ln.cmd)}${ln.st === 'idle' ? '<span class="cursor"></span>' : ''}`;
  }

  function renderTerm(t) {
    hitCounter = 0;
    const rows = [];
    const marks = [];
    let n = t.start;
    let prevTime = null;
    let block = null;
    const timeCell = (time) => {
      const dup = time && time === prevTime;
      prevTime = time || prevTime;
      return `<span class="g-time${dup ? ' dup' : ''}">${esc(time || '')}</span>`;
    };

    if (t.alt) {
      const vp = $('#viewport');
      const avail = Math.max(24, Math.floor((vp.clientHeight - 8) / 20));
      const screen = D.htop.slice(0, -1);
      while (screen.length && !screen[screen.length - 1]) screen.pop();
      while (screen.length < avail - 1) screen.push('');
      screen.length = avail - 1;
      screen.push(D.htop[D.htop.length - 1]);
      screen.forEach((line, i) => {
        rows.push(`<div class="tl"><span class="g-num">${i + 1}</span><span class="g-time"></span><span class="g-mark"></span><span class="t-text">${renderSegs(line)}</span><span></span></div>`);
      });
      return `<div class="term alt${S.gutter.num ? '' : ' no-num'}${S.gutter.time ? '' : ' no-time'}" tabindex="0" aria-label="终端：全屏程序 htop">${rows.join('')}</div>`;
    }

    // 展开 box / livebox 为普通行
    const lines = [];
    t.lines.forEach((ln) => {
      if (ln.k === 'box' || ln.k === 'livebox') {
        boxLines(ln.rows, ln.w, ln.color || 'a8').forEach((s, i) => lines.push({ k: ln.k === 'box' ? 'o' : 'live', t: i === 0 ? ln.t : ln.t, s }));
      } else lines.push(ln);
    });

    lines.forEach((ln, i) => {
      const next = lines[i + 1];
      if (ln.k === 'p') {
        block = ln.st;
        const meta = ln.st === 'ok' ? `<span class="t-meta ok">${ic('check')}${esc(ln.dur)}</span>`
          : ln.st === 'err' ? `<span class="t-meta err">${ic('circle-x')}退出码 ${ln.code}<span class="muted">&nbsp;&nbsp;${esc(ln.dur)}</span></span>`
          : ln.st === 'run' ? `<span class="t-meta run">${ic('loader-circle')}${esc(ln.dur)}</span>` : '<span></span>';
        const hasOut = next && next.k === 'o';
        marks.push({ at: rows.length, kind: ln.st === 'err' ? 'err' : 'cmd' });
        rows.push(`<div class="tl prompt ${ln.st}" data-n="${n}"><span class="g-num">${n}</span>${timeCell(ln.t)}<span class="g-mark">${ln.st === 'idle' ? '' : ic(hasOut ? 'chevron-down' : 'chevron-right', 'fold')}</span><span class="t-text">${promptSegs(ln)}</span>${meta}</div>`);
        n += 1;
      } else if (ln.k === 'o') {
        const end = !next || next.k !== 'o';
        const cls = block && block !== 'idle' ? ` inblock ${block === 'err' ? 'err' : ''}${end ? ' blk-end' : ''}` : '';
        rows.push(`<div class="tl out${cls}"><span class="g-num">${n}</span>${timeCell(ln.t)}<span class="g-mark"></span><span class="t-text">${renderSegs(ln.s)}</span><span></span></div>`);
        n += 1;
      } else if (ln.k === 'gap') {
        block = null;
        rows.push(`<div class="tl gap"><span class="g-num"></span><span class="g-time"></span><span class="g-mark"></span><span class="gap-line">${ic('clock', 'xs')}${esc(ln.label)}</span></div>`);
      } else if (ln.k === 'sys' || ln.k === 'tui') {
        block = null;
        const icon = ln.k === 'tui' ? 'app-window' : ln.icon || 'info';
        rows.push(`<div class="tl ${ln.k} ${ln.tone || ''}"><span class="g-num">${ic(icon)}</span>${timeCell(ln.t)}<span class="g-mark"></span><span class="t-text">${esc(ln.s)}</span><span></span></div>`);
      } else if (ln.k === 'live') {
        const prevLive = lines[i - 1] && lines[i - 1].k === 'live';
        const nextLive = next && next.k === 'live';
        rows.push(`<div class="tl live${prevLive ? '' : ' live-first'}${nextLive ? '' : ' live-last'}" title="实时区：正在被程序重绘的行，滚入历史后定格时间"><span class="g-num">${n}</span><span class="g-time">${prevLive ? '' : '实时'}</span><span class="g-mark"></span><span class="t-text">${renderSegs(ln.s)}</span><span></span></div>`);
        n += 1;
      }
    });

    const total = n - 1;
    const cls = `term${S.gutter.num ? '' : ' no-num'}${S.gutter.time ? '' : ' no-time'}`;
    return `<div class="${cls}" tabindex="0" aria-label="终端：${esc(t.title)}">${rows.join('')}</div>${renderRuler(t, rows.length, marks, total)}`;
  }

  // 滚动概览：位置 + 命令（青）/ 失败（红）/ 搜索命中（琥珀）
  function renderRuler(t, visible, marks, total) {
    const history = Math.max(total, visible);
    const frac = (row) => ((history - visible + row) / history) * 100;
    let html = `<div class="ruler" aria-hidden="true"><span class="thumb" style="top:${(100 - (visible / history) * 100).toFixed(2)}%;height:max(18px, ${((visible / history) * 100).toFixed(2)}%)"></span>`;
    // 历史中的命令标记（确定性分布）
    let seed = 17;
    const rnd = () => { seed = (seed * 9301 + 49297) % 233280; return seed / 233280; };
    for (let i = 0; i < 26; i++) {
      const top = rnd() * 92;
      const kind = rnd() > 0.88 ? 'err' : 'cmd';
      html += `<i class="${kind}" style="top:${top.toFixed(2)}%"></i>`;
    }
    marks.forEach((m) => { html += `<i class="${m.kind}" style="top:${frac(m.at).toFixed(2)}%"></i>`; });
    if (S.find) {
      [12, 31, 47, 63, 78].forEach((p) => { html += `<i class="hit" style="top:${p}%"></i>`; });
    }
    return html + '</div>';
  }

  /* ------------------------------------------------------------------ 编辑器渲染 */
  function hlVal(v) {
    const t = v.trim();
    const lead = v.slice(0, v.length - v.trimStart().length);
    if (!t) return esc(v);
    if (/^(\d+(\.\d+)?|true|false|null)$/.test(t)) return esc(lead) + `<span class="s-num">${esc(t)}</span>`;
    let out = esc(lead); const re = /("(?:[^"\\]|\\.)*")|(\$\{[^}]+\})|([[\],])/g; let i = 0; let m;
    while ((m = re.exec(t))) {
      if (m.index > i) out += `<span class="s-str">${esc(t.slice(i, m.index))}</span>`;
      if (m[1]) out += `<span class="s-str">${esc(m[1]).replace(/\$\{[^}]+\}/g, (x) => `<span class="s-var">${x}</span>`)}</span>`;
      else if (m[2]) out += `<span class="s-var">${esc(m[2])}</span>`;
      else out += `<span class="s-punc">${esc(m[3])}</span>`;
      i = re.lastIndex;
    }
    if (i < t.length) out += `<span class="s-str">${esc(t.slice(i))}</span>`;
    return out;
  }
  function hlYaml(line) {
    if (/^\s*#/.test(line)) return `<span class="s-com">${esc(line)}</span>`;
    let m = line.match(/^(\s*)(- )?([A-Za-z0-9_.-]+)(:)(.*)$/);
    if (m) return esc(m[1]) + (m[2] ? '<span class="s-punc">- </span>' : '') + `<span class="s-key">${esc(m[3])}</span><span class="s-punc">:</span>` + hlVal(m[5]);
    m = line.match(/^(\s*)(- )(.*)$/);
    if (m) return esc(m[1]) + '<span class="s-punc">- </span>' + hlVal(m[3]);
    return esc(line);
  }
  function renderEditor(t) {
    const conflict = t.file === 'compose.yaml';
    const lines = t.file === 'compose.yaml' ? D.composeYaml : ['# nginx.conf 预览标签：单击文件打开为预览（标题为斜体），', '# 修改内容或双击后转为固定标签。', '', 'server {', '    listen 80;', '    server_name api.example.org;', '}'];
    const body = lines.map((ln, i) => {
      const no = i + 1;
      const cls = conflict && no === 21 ? ' modified active' : '';
      const caret = conflict && no === 21 ? '<span class="ed-caret"></span>' : '';
      return `<div class="el${cls}"><span class="ln">${no}</span><span class="mk"></span><span class="code">${t.file === 'compose.yaml' ? hlYaml(ln) : `<span class="${ln.startsWith('#') ? 's-com' : ''}">${esc(ln)}</span>`}${caret}</span></div>`;
    }).join('');
    return `<div class="editor">
      <div class="ed-toolbar">
        ${ic('file-code', 'sm')}
        <span class="ed-path">/srv/api/current/<b>${esc(t.file)}</b></span>
        <span class="tag outline">${ic('server')}prod-api-01</span>
        <span class="grow"></span>
        <div class="ed-meta">
          <button title="语言">${t.file === 'compose.yaml' ? 'YAML' : 'Nginx'}</button>
          <button title="编码">UTF-8</button>
          <button title="换行符">LF</button>
          <button title="缩进">空格：2</button>
        </div>
        <span class="divider-v"></span>
        <button class="btn ghost xs" title="下载到本机">${ic('download', 'sm')}下载</button>
        <button class="btn primary xs" title="保存到远端 (Ctrl+S)">${ic('save', 'sm')}保存<kbd>Ctrl+S</kbd></button>
      </div>
      ${conflict ? `<div class="ed-alert" role="alert">${ic('triangle-alert')}<div class="msg"><b>远端文件已更新。</b><span>10:51 有其他会话写入（1.6 KB → 1.7 KB），直接保存会覆盖对方的修改。</span></div><button class="btn xs">比较差异</button><button class="btn xs ghost">重新加载</button><button class="btn xs ghost">仍然保存</button></div>` : '<div></div>'}
      <div class="ed-body" tabindex="0" aria-label="编辑器：${esc(t.file)}">${body}</div>
    </div>`;
  }

  /* ------------------------------------------------------------------ 标签栏 */
  function fileIcon(name) {
    if (/\.(ya?ml|toml|json|conf)$|^Dockerfile$|^Makefile$|\.sh$/.test(name)) return 'file-code';
    if (/^\.env/.test(name)) return 'file-key';
    if (/\.(tar\.gz|zip|tgz)$/.test(name)) return 'file-archive';
    if (/\.(md|txt|log)$|^VERSION$/.test(name)) return 'file-text';
    return 'file';
  }
  function renderTabs() {
    const html = S.tabs.map((t) => {
      const sel = t.id === S.active;
      const lead = t.kind === 'editor' ? ic(fileIcon(t.file), 'ticon') : `<span class="dot ${t.status}" aria-hidden="true"></span>`;
      const agent = t.agent ? `<span title="检测到 ${esc(t.agent.name)}">${ic('bot', 'agent-ic')}</span>` : '';
      const badge = t.badge ? `<span class="tbadge">${ic('bell-ring')}${esc(t.badge)}</span>` : '';
      const tail = t.modified ? '<span class="mod-dot" title="有未保存的修改"></span>' : `<span class="close" data-close="${esc(t.id)}" title="关闭 (Ctrl+Shift+W)">${ic('x', 'xs')}</span>`;
      const tip = t.kind === 'editor' ? `prod-api-01:/srv/api/current/${t.file}` : `${t.title}（${statusText(t.status)}）`;
      return `<div class="tab${t.preview ? ' preview' : ''}" role="tab" aria-selected="${sel}" data-tab="${esc(t.id)}" title="${esc(tip)}">${lead}<span class="label">${esc(t.title)}</span>${agent}${badge}${tail}</div>`;
    }).join('');
    $('#tabbar').innerHTML = `<div class="tabs">${html}</div><button class="icon-btn tab-add" id="tab-add" title="新建连接 (Ctrl+Shift+T)">${ic('plus')}</button>
      <div class="tab-tools"><button class="icon-btn sm" title="向右分屏">${ic('columns-2', 'sm')}</button><button class="icon-btn sm" title="全部标签">${ic('chevron-down', 'sm')}</button></div>`;
  }
  const statusText = (st) => ({ connected: '已连接', connecting: '连接中', failed: '连接失败', idle: '未连接' }[st] || st);

  /* ------------------------------------------------------------------ 航线条 */
  function routeHtml(s, conn) {
    const chain = s.hops.map(sessionById).concat([s]);
    let html = `<span class="stop origin">${ic('laptop')}本机</span>`;
    chain.forEach((h, i) => {
      const last = i === chain.length - 1;
      let segCls = ''; let lat = conn && conn.quiet ? '' : `${s.lat[i] != null ? s.lat[i] : '—'} ms`; let stopCls = last ? 'target' : ''; let note = '';
      if (conn) {
        if (i < conn.reached) { /* 已建立 */ } else if (i === conn.reached) { segCls = 'active'; lat = conn.quiet ? '' : '…'; stopCls += ' pending'; if (!conn.quiet) note = `<span class="note">${ic('shield', 'xs')}等待确认主机密钥</span>`; } else { segCls = 'pending'; lat = ''; stopCls += ' pending'; }
      }
      html += `<span class="seg ${segCls}">${lat ? `<span class="lat">${lat}</span>` : ''}</span><span class="stop ${stopCls}" title="${esc(h.user)}@${esc(h.host)}:${h.port}"><span class="node"></span>${esc(h.name)}${note}</span>`;
    });
    return html;
  }
  function renderRoute() {
    const t = activeTab(); const s = activeSession();
    if (!t || !s) { $('#routebar').innerHTML = '<span class="muted">没有打开的会话</span>'; return; }
    const total = s.lat.reduce((a, b) => a + b, 0);
    let chips = '';
    if (t.agent && t.kind === 'term') chips += `<span class="route-chip agent" title="来自终端标题（OSC 0/2）">${ic('bot', 'sm')}${esc(t.agent.name)}${t.agent.since ? `<span style="opacity:.75">${esc(t.agent.since)}</span>` : ''}</span>`;
    if (t.alt) chips += `<span class="route-chip alt" title="程序切换到了备用屏幕">${ic('app-window', 'sm')}全屏程序 ${esc(t.alt.app)}</span>`;
    const tools = t.kind === 'term' ? `
      <button class="icon-btn sm" data-gutter="num" aria-pressed="${S.gutter.num}" title="行号 (Ctrl+Shift+G)">${ic('hash', 'sm')}</button>
      <button class="icon-btn sm" data-gutter="time" aria-pressed="${S.gutter.time}" title="时间戳">${ic('clock', 'sm')}</button>
      <button class="icon-btn sm" id="find-toggle" aria-pressed="${!!S.find}" title="搜索 (Ctrl+Shift+F)">${ic('text-search', 'sm')}</button>
      <button class="icon-btn sm" id="route-menu" title="更多：复制 SSH 命令、重新连接、导出日志">${ic('ellipsis-vertical', 'sm')}</button>` : '';
    $('#routebar').innerHTML = `<div class="route" title="${t.conn ? '正在逐跳建立连接' : `全链路往返 ${total} ms`}">${routeHtml(s, t.conn)}</div>${chips}<div class="route-tools">${tools}</div>`;
  }

  /* ------------------------------------------------------------------ 视口 */
  function renderViewport() {
    const t = activeTab();
    const vp = $('#viewport');
    if (!t) { vp.innerHTML = `<div class="empty"><div class="ic">${ic('square-terminal')}</div><strong>没有打开的会话</strong>双击右侧会话即可连接，或按 Ctrl+Shift+P 快速连接。</div>`; return; }
    vp.innerHTML = t.kind === 'editor' ? renderEditor(t) : renderTerm(t);
    if (t.kind === 'term' && S.find) {
      const count = $$('mark.hit', vp).length;
      vp.insertAdjacentHTML('beforeend', `<div class="find" role="search">
        <label class="field">${ic('search', 'sm')}<input id="find-input" value="${esc(S.find.q)}" aria-label="在终端中搜索"></label>
        <button class="opt" aria-pressed="false" title="区分大小写">Aa</button><button class="opt" aria-pressed="false" title="正则表达式">.*</button>
        <span class="count">${count ? `${Math.min(S.find.cur, count)} / ${count}` : '无结果'}</span>
        <button class="icon-btn sm" title="上一个 (Shift+Enter)">${ic('chevron-up', 'sm')}</button><button class="icon-btn sm" title="下一个 (Enter)">${ic('chevron-down', 'sm')}</button>
        <button class="icon-btn sm" id="find-close" title="关闭 (Esc)">${ic('x', 'sm')}</button></div>`);
    }
    fitTerm();
  }
  // 终端按整行显示：可视高度取行高的整数倍，滚到底部时顶部不会出现半行
  function fitTerm() {
    const term = $('#viewport .term');
    if (!term) return;
    term.style.bottom = '0px';
    term.style.bottom = ((term.clientHeight - 8) % 20) + 'px';
    term.scrollTop = term.scrollHeight;
    const cur = $('#viewport mark.hit.cur');
    if (cur) cur.scrollIntoView({ block: 'nearest' });
  }

  /* ------------------------------------------------------------------ 输入栏 */
  function renderComposer() {
    const t = activeTab(); const s = activeSession(); const c = $('#composer');
    if (t && t.alt) {
      c.className = 'composer collapsed';
      c.innerHTML = `<div class="bar">${ic('keyboard', 'sm')}<span>键盘输入直接发送给 <b class="mono">${esc(t.alt.app)}</b>，输入栏已收起以留出空间。</span><span class="grow"></span><span class="kbds"><kbd>Ctrl</kbd><kbd>Shift</kbd><kbd>I</kbd></span><button class="btn ghost xs" id="composer-expand">展开输入栏</button></div>`;
      return;
    }
    c.className = 'composer';
    const name = s ? s.name : '—';
    const connected = S.tabs.filter((x) => x.kind === 'term' && x.status === 'connected').length;
    const target = S.broadcast
      ? `<button class="send-target broadcast" id="target" title="发送目标">${ic('radio-tower', 'sm')}<span>广播到</span><b>${connected} 个会话</b>${ic('chevrons-up-down', 'xs')}</button>`
      : `<button class="send-target" id="target" title="发送目标">${ic('square-terminal', 'sm')}<span>发送到</span><span class="nm">${esc(name)}</span>${ic('chevrons-up-down', 'xs')}</button>`;
    const paste = S.mode === 'paste';
    const hint = t && t.agent && paste
      ? `<div class="composer-hint">${ic('bot')}<span>检测到 ${esc(t.agent.name)}，已切换为整段粘贴：内容通过括号粘贴（bracketed paste）一次发出，Agent 会把它当作一条消息。</span><span class="sp"></span><span class="keys"><span><kbd>Enter</kbd> 发送</span><span><kbd>Shift</kbd>+<kbd>Enter</kbd> 换行</span></span></div>`
      : '';
    const ph = paste ? '输入要整段发送的内容，例如给 Agent 的长提示词' : '输入命令，Enter 发送，↑ 调出历史';
    c.innerHTML = `${hint}<div class="composer-row">${target}
      <div class="cmd-input"><span class="pr">${paste ? '¶' : '❯'}</span><textarea id="draft" rows="1" spellcheck="false" placeholder="${esc(ph)}" aria-label="命令输入">${esc(S.draft)}</textarea>
        <span class="inline-tools"><button class="icon-btn sm" title="命令片段">${ic('scroll-text', 'sm')}</button><button class="icon-btn sm" title="历史 (Ctrl+R)">${ic('clock-arrow-up', 'sm')}</button></span></div>
      <div class="seg-ctl" role="group" aria-label="发送方式" style="height:32px;align-items:center"><button data-mode="line" aria-pressed="${!paste}" title="每一行都作为命令执行">逐行执行</button><button data-mode="paste" aria-pressed="${paste}" title="作为一次粘贴发送，适合 Agent CLI 和多行脚本">整段粘贴</button></div>
      <button class="btn primary lg" id="send">${ic('corner-down-left', 'sm')}发送</button></div>`;
    autosize($('#draft'));
  }
  function autosize(ta) {
    if (!ta) return;
    ta.style.height = '20px';
    ta.style.height = Math.min(120, ta.scrollHeight) + 'px';
  }

  /* ------------------------------------------------------------------ 左侧文件 */
  function fileRows(items, depth, parent) {
    return items.filter((it) => S.showHidden || !it.hidden).map((it) => {
      const path = parent ? `${parent}/${it.name}` : it.name;
      const open = it.dir && S.openDirs.has(path);
      const cls = ['frow', it.dir ? 'dir' : '', open ? 'open' : '', S.selFile === path ? 'selected' : '', it.hidden ? 'hidden-file' : '', S.drag && S.drag.over === path ? 'drop-target' : ''].join(' ');
      const icon = it.dir ? (open ? 'folder-open' : 'folder') : fileIcon(it.name);
      const mod = S.modifiedFiles.has(path) ? '<span class="mod" title="编辑器中有未保存的修改"></span>' : '';
      const acts = it.dir ? '' : `<span class="row-acts">${it.text ? `<button class="icon-btn sm" data-edit="${esc(path)}" title="编辑">${ic('pencil', 'sm')}</button>` : ''}<button class="icon-btn sm" title="下载到本机">${ic('download', 'sm')}</button></span>`;
      let html = `<div class="${cls}" style="--depth:${depth}" data-path="${esc(path)}" data-dir="${it.dir ? 1 : 0}" data-text="${it.text ? 1 : 0}" draggable="true" title="${esc(it.dir ? path : `${path}  ${it.size}`)}">
        <span class="name">${it.dir ? ic('chevron-right', 'chev') : '<span class="chev-sp"></span>'}${ic(icon, 'ficon')}<span class="nm">${esc(it.name)}</span>${mod}</span>
        <span class="size">${it.dir ? '' : esc(it.size)}</span>${acts}<span class="mtime" title="修改于 ${esc(it.mtime)}">${esc(shortTime(it.mtime))}</span></div>`;
      if (open) html += fileRows(it.children || [], depth + 1, path);
      return html;
    }).join('');
  }
  // 今天的文件显示时间，其余显示月-日；完整时间放在悬停提示里
  function shortTime(s) {
    if (s.startsWith('今天 ')) return s.slice(3);
    if (s.startsWith('昨天')) return '昨天';
    return s.split(' ')[0];
  }
  function renderTransfers() {
    if (!S.transfers.length) return '';
    const running = S.transfers.filter((x) => !x.done && !x.conflict && x.pct != null).length;
    const waiting = S.transfers.filter((x) => x.conflict).length;
    const items = S.transfers.map((x) => {
      const dirIc = ic(x.dir === 'up' ? 'arrow-up-from-line' : 'arrow-down-to-line', `sm dir ${x.dir}`);
      if (x.done) return `<div class="tr-item done">${ic('check', 'sm ok-text')}<span class="nm">${esc(x.name)}</span><span class="acts"><button class="icon-btn sm" title="在文件中显示">${ic('folder-open', 'sm')}</button></span><div class="meta"><span>已上传 ${esc(x.size)}</span><span class="ellipsis">到 ${esc(x.to)}</span></div></div>`;
      if (x.conflict) return `<div class="tr-item conflict">${dirIc}<span class="nm">${esc(x.name)}</span><span class="acts"><button class="icon-btn sm" title="取消">${ic('x', 'sm')}</button></span>
        <div class="conf-msg">${ic('triangle-alert', 'xs')}<span>${esc(x.conflict)}</span></div>
        <div class="conf-acts"><button class="btn xs">覆盖</button><button class="btn xs">跳过</button><button class="btn xs">保留两者</button><label><span class="check" aria-checked="true" role="checkbox"></span>其余冲突同样处理</label></div></div>`;
      return `<div class="tr-item">${dirIc}<span class="nm">${esc(x.name)}</span><span class="acts"><button class="icon-btn sm" title="暂停">${ic('pause', 'sm')}</button><button class="icon-btn sm" title="取消">${ic('x', 'sm')}</button></span>
        <div class="bar" role="progressbar" aria-valuenow="${x.pct}" aria-valuemin="0" aria-valuemax="100"><i style="width:${x.pct}%"></i></div>
        <div class="meta"><span class="pct">${x.pct}%</span><span>${esc(x.speed)}</span><span>${esc(x.eta)}</span><span class="ellipsis">${x.dir === 'up' ? '到' : '存到'} ${esc(x.to)}</span></div></div>`;
    }).join('');
    const parts = [];
    if (running) parts.push(`${running} 个进行中`);
    if (waiting) parts.push(`<span class="caution-text">${waiting} 个待处理</span>`);
    return `<div class="transfers"><div class="tr-head" id="tr-toggle">${ic(S.trOpen ? 'chevron-down' : 'chevron-right', 'xs')}<span class="t">传输</span><span class="sum grow ellipsis">${parts.join('，') || '全部完成'}</span><button class="icon-btn sm" title="全部暂停">${ic('pause', 'sm')}</button></div>${S.trOpen ? `<div class="tr-list">${items}</div>` : ''}</div>`;
  }
  function renderFiles() {
    const t = activeTab(); const s = activeSession();
    const fs = s && D.files[s.id];
    const head = `<div class="pane-head"><div class="pane-title">文件<span class="tag outline">SFTP</span><span class="sub">${s ? esc(s.name) : ''}</span></div>
      <button class="icon-btn sm" title="刷新 (F5)">${ic('refresh-cw', 'sm')}</button><button class="icon-btn sm" title="更多">${ic('ellipsis', 'sm')}</button></div>`;
    if (!t || !fs || t.status !== 'connected' || !fs.items.length) {
      const connecting = t && t.status === 'connecting';
      $('#pane-left').innerHTML = head + `<div class="empty"><div class="ic">${ic(connecting ? 'loader-circle' : 'folder')}</div><strong>${connecting ? `正在连接 ${esc(s.name)}` : '还没有可浏览的文件'}</strong>${connecting ? '连接建立后会自动打开登录目录。' : '连接会话后，这里显示远程主机的文件。'}</div>` + renderTransfers();
      return;
    }
    const parts = fs.root.split('/').filter(Boolean);
    const crumbs = parts.map((p, i) => `<button title="/${esc(parts.slice(0, i + 1).join('/'))}">${esc(p)}</button>${i < parts.length - 1 ? '<span class="sep">/</span>' : ''}`).join('');
    const drop = S.drag ? `<div class="drop-hint"><span class="ic">${ic('upload')}</span><div><strong>松开以上传 ${S.drag.names.length} 个文件到 <span class="path">${esc(fs.root)}/${esc(S.drag.over || '')}</span></strong><span>拖到空白处则上传到当前目录；从列表拖到桌面可下载到本机。</span></div></div>` : '';
    $('#pane-left').innerHTML = head + `
      <div class="pathbar"><button class="icon-btn sm" title="主目录">${ic('house', 'sm')}</button><div class="crumbs"><span class="sep">/</span>${crumbs}</div>
        <button class="follow" id="follow" aria-pressed="${S.follow}" title="跟随终端当前目录（依赖 OSC 7，未上报时回退为 pwd 查询）">${ic('locate-fixed')}跟随终端</button></div>
      <div class="ftoolbar">
        <button class="icon-btn sm" title="上一级">${ic('arrow-up', 'sm')}</button>
        <button class="icon-btn sm" title="新建文件">${ic('file-plus', 'sm')}</button>
        <button class="icon-btn sm" title="新建文件夹">${ic('folder-plus', 'sm')}</button>
        <button class="icon-btn sm" title="上传文件…">${ic('upload', 'sm')}</button>
        <button class="icon-btn sm" title="下载所选">${ic('download', 'sm')}</button>
        <span class="sp"></span>
        <button class="icon-btn sm" id="toggle-hidden" aria-pressed="${S.showHidden}" title="显示隐藏文件">${ic(S.showHidden ? 'eye' : 'eye-off', 'sm')}</button>
        <button class="icon-btn sm" title="筛选">${ic('list-filter', 'sm')}</button>
      </div>
      <div class="file-cols"><span>名称</span><span>大小</span><span>修改</span></div>
      <div class="file-list" id="file-list">${fileRows(fs.items, 0, '')}${drop}</div>` + renderTransfers();
    if (S.drag && S.drag.frozen) placeGhost();
  }
  function placeGhost() {
    let g = $('#drag-ghost');
    if (!g) {
      document.body.insertAdjacentHTML('beforeend', `<div class="drag-ghost" id="drag-ghost"><span class="stack">${ic('file-code')}${ic('file-code')}<span class="plus">+</span></span>${esc(S.drag.names[0])} 等 ${S.drag.names.length} 个文件</div>`);
      g = $('#drag-ghost');
    }
    const row = $(`.frow[data-path="${S.drag.over}"]`);
    if (row) { const r = row.getBoundingClientRect(); g.style.left = (r.left + 118) + 'px'; g.style.top = (r.top + 20) + 'px'; }
  }

  /* ------------------------------------------------------------------ 右上：会话 */
  function miniRoute(n) {
    const w = 8 + n * 9;
    let s = `<svg class="mini-route" width="${w + 2}" height="8" viewBox="0 0 ${w + 2} 8" aria-hidden="true"><line x1="3" y1="4" x2="${3 + n * 9 - 3}" y2="4" style="stroke:var(--route)" stroke-width="1.5"/>`;
    for (let i = 0; i < n; i++) s += `<circle cx="${3 + i * 9}" cy="4" r="2.3" style="fill:var(--route)"/>`;
    return s + `<circle cx="${3 + n * 9}" cy="4" r="2.8" fill="none" style="stroke:var(--route)" stroke-width="1.5"/></svg>`;
  }
  function sessionStatus(id) {
    const tabs = S.tabs.filter((t) => t.kind === 'term' && t.session === id);
    if (tabs.some((t) => t.status === 'connected')) return 'connected';
    if (tabs.some((t) => t.status === 'connecting')) return 'connecting';
    return 'idle';
  }
  function renderSessions() {
    const s = activeSession(); const q = S.filter.trim().toLowerCase();
    const groups = D.groups.map((g) => {
      const items = D.sessions.filter((x) => x.group === g.id && (!q || `${x.name} ${x.user}@${x.host}`.toLowerCase().includes(q)));
      if (q && !items.length) return '';
      const open = !S.collapsed.has(g.id) || !!q;
      const rows = open ? items.map((x) => {
        const st = sessionStatus(x.id);
        const hops = x.hops.length ? `<span class="hopinfo" title="经 ${esc(x.hops.join('、'))} 连接">${miniRoute(x.hops.length)}${x.hops.length} 跳</span>` : '';
        return `<div class="srow${s && s.id === x.id ? ' active' : ''}" data-session="${esc(x.id)}" title="双击连接">
          <span class="dot ${st}" title="${statusText(st)}"></span>
          <div style="min-width:0"><div class="nm">${esc(x.name)}</div><div class="addr">${esc(x.user)}@${esc(x.host)}${x.port !== 22 ? `:${x.port}` : ''}</div></div>
          <div class="right">${hops}<span class="acts"><button class="icon-btn sm" data-connect="${esc(x.id)}" title="连接">${ic('play', 'sm')}</button><button class="icon-btn sm" data-edit-session="${esc(x.id)}" title="编辑">${ic('pencil', 'sm')}</button></span></div></div>`;
      }).join('') : '';
      const note = open && g.linked ? `<div class="sync-note">${ic('circle-check')}已链接，${esc(g.synced)}，文件变化时自动更新</div>` : '';
      return `<div class="grp${open ? ' open' : ''}" data-group="${g.id}" title="${g.linked ? esc(g.path) : ''}">${ic('chevron-right', 'chev')}${g.linked ? ic('link', 'link') : ''}<span class="gname${g.linked ? ' mono' : ''}">${esc(g.name)}</span><span class="count">${items.length}</span></div>${note}${rows}`;
    }).join('');
    $('#sessions').innerHTML = `<div class="pane-head"><div class="pane-title">会话<span class="sub">${D.sessions.length} 个</span></div>
      <button class="icon-btn sm" id="new-session" title="新建会话 (Ctrl+Shift+N)">${ic('plus', 'sm')}</button>
      <button class="icon-btn sm" id="import-config" title="从 SSH 配置导入">${ic('import', 'sm')}</button>
      <button class="icon-btn sm" title="更多：新建分组、导出">${ic('ellipsis', 'sm')}</button></div>
      <div class="sess-search"><label class="field">${ic('search', 'sm')}<input id="sess-filter" placeholder="搜索名称、主机或用户" value="${esc(S.filter)}" aria-label="搜索会话"></label></div>
      <div class="sess-list" role="tree">${groups || '<div class="empty">没有匹配的会话</div>'}</div>`;
  }

  /* ------------------------------------------------------------------ 右下：监控 */
  function meterRow(label, used, total, unit, extraLbl, pctOverride) {
    const pct = pctOverride != null ? pctOverride : Math.round((used / total) * 100);
    const sev = pct >= 95 ? 'danger' : pct >= 80 ? 'caution' : '';
    const sevTxt = sev === 'danger' ? `<span class="sev danger-text">${ic('circle-alert')}告急</span>` : sev === 'caution' ? `<span class="sev">${ic('triangle-alert')}偏高</span>` : '';
    return `<div class="meter-row"><span class="lbl">${label}${extraLbl || ''}${sevTxt}</span><span class="num"><small>${used} / ${total} ${unit}</small><b>${pct}%</b></span><div class="meter ${sev}" role="meter" aria-valuenow="${pct}" aria-valuemin="0" aria-valuemax="100" aria-label="${esc(label.replace(/<[^>]+>/g, ''))}"><i style="width:${pct}%"></i></div></div>`;
  }
  function renderMonitor() {
    const t = activeTab(); const s = activeSession();
    const m = t && t.status === 'connected' ? D.monitor[t.monKey] || D.monitor[t.session] : null;
    const head = `<div class="pane-head"><div class="pane-title">监控<span class="sub">${s ? esc(s.name) : ''}</span></div>
      <div class="seg-ctl sm" role="tablist"><button data-mon="overview" aria-pressed="${S.monTab === 'overview'}">概览</button><button data-mon="procs" aria-pressed="${S.monTab === 'procs'}">进程</button></div>
      <button class="icon-btn sm" title="暂停采样">${ic('pause', 'sm')}</button></div>`;
    if (!m) {
      const connecting = t && t.status === 'connecting';
      $('#monitor').innerHTML = head + `<div class="empty"><div class="ic">${ic('activity')}</div><strong>${connecting ? '连接建立后开始采样' : '没有监控数据'}</strong>${connecting ? '通过已认证的 SSH 连接执行只读采集命令，无需在服务器上安装 agent。' : '打开一个已连接的会话即可查看 CPU、内存、磁盘和网络。'}</div>`;
      return;
    }
    let body;
    if (S.monTab === 'procs') {
      const max = Math.max.apply(null, m.procs.map((p) => p.cpu));
      body = `<div class="mon-sec-title"><span>按 CPU 排序，共 ${m.procs.length} 个进程</span><span class="muted">${esc(m.end)}</span></div>
        <table class="ptable"><thead><tr><th>PID</th><th>命令</th><th class="r">CPU</th><th class="r">内存</th></tr></thead><tbody>
        ${m.procs.map((p, i) => `<tr class="${i === 0 && t.agent ? 'hl' : ''}"><td class="pid">${p.pid}</td><td class="cmd" title="${esc(p.cmd)}">${esc(p.cmd)}</td><td class="r">${p.cpu.toFixed(1)}%<span class="cpu-bar"><i style="width:${(p.cpu / max) * 100}%"></i></span></td><td class="r">${p.mem.toFixed(1)}%</td></tr>`).join('')}
        </tbody></table>
        <div class="sysline"><span>右键进程可复制 PID、发送信号或在终端中查看详情。</span></div>`;
    } else {
      const cpu = m.cpu[m.cpu.length - 1];
      const peakRx = Math.max.apply(null, m.rx).toFixed(1); const peakTx = Math.max.apply(null, m.tx).toFixed(1);
      body = `<div class="sysline" title="每 ${m.interval} 秒采样"><span><b>${esc(m.os)}</b></span><span>内核 ${esc(m.kernel)}</span><span>${esc(m.uptime)}</span></div>
        <div class="mrow"><div><div class="lbl">CPU</div><div class="val">${Math.round(cpu)}%</div><div class="cap">${m.cores} 核，5 分钟</div></div><div class="spark" data-chart="cpu" tabindex="0" aria-label="CPU 使用率，近 5 分钟，当前 ${Math.round(cpu)}%"></div></div>
        <div class="mgroup">${meterRow('内存', m.mem.used, m.mem.total, m.mem.unit)}${meterRow('交换', m.swap.used, m.swap.total, m.swap.unit)}</div>
        <div class="mgroup">${m.disks.map((dk) => meterRow('磁盘', dk.used, dk.total, dk.unit, ` <span class="mono">${esc(dk.mount)}</span>`, dk.pct)).join('')}</div>
        <div class="net">
          <div><div class="h">${ic('arrow-down', 'xs')}下行 <b>${m.rx[m.rx.length - 1]}</b><small>MB/s</small></div><div class="spark" data-chart="rx" tabindex="0" aria-label="下行速率"></div><div class="cap">峰值 ${peakRx} MB/s</div></div>
          <div><div class="h">${ic('arrow-up', 'xs')}上行 <b>${m.tx[m.tx.length - 1]}</b><small>MB/s</small></div><div class="spark" data-chart="tx" tabindex="0" aria-label="上行速率"></div><div class="cap">峰值 ${peakTx} MB/s</div></div>
        </div>
        <div class="load"><span>负载</span><b>${m.load[0].toFixed(2)}</b><b>${m.load[1].toFixed(2)}</b><b>${m.load[2].toFixed(2)}</b><small>1 / 5 / 15 分钟</small></div>`;
    }
    $('#monitor').innerHTML = head + `<div class="mon-body">${body}</div>`;
    $$('.spark[data-chart]', $('#monitor')).forEach((el) => drawSpark(el, m, el.dataset.chart));
  }

  function sampleTime(m, idx) {
    const [h, mi, se] = m.end.split(':').map(Number);
    let secs = h * 3600 + mi * 60 + se - (m.cpu.length - 1 - idx) * m.interval;
    const p = (v) => String(v).padStart(2, '0');
    return `${p(Math.floor(secs / 3600))}:${p(Math.floor((secs % 3600) / 60))}:${p(secs % 60)}`;
  }
  function drawSpark(el, m, key) {
    const data = m[key];
    const w = el.clientWidth; const h = el.clientHeight;
    if (!w || !h) return;
    const max = key === 'cpu' ? 100 : Math.max.apply(null, data) * 1.18;
    const unit = key === 'cpu' ? '%' : ' MB/s';
    const label = { cpu: 'CPU', rx: '下行', tx: '上行' }[key];
    const x = (i) => 1 + (i / (data.length - 1)) * (w - 7);
    const y = (v) => h - 1 - (v / max) * (h - 7);
    const pts = data.map((v, i) => `${x(i).toFixed(1)} ${y(v).toFixed(1)}`);
    const line = 'M' + pts.join(' L');
    const area = `${line} L${x(data.length - 1).toFixed(1)} ${h - 1} L${x(0).toFixed(1)} ${h - 1} Z`;
    const lx = x(data.length - 1); const ly = y(data[data.length - 1]);
    el.innerHTML = `<svg viewBox="0 0 ${w} ${h}" aria-hidden="true">
      <line x1="0" x2="${w}" y1="${h - 0.5}" y2="${h - 0.5}" style="stroke:var(--chart-grid)" stroke-width="1"/>
      <path d="${area}" style="fill:var(--chart-area)"/>
      <path d="${line}" fill="none" style="stroke:var(--chart-line)" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
      <circle cx="${lx}" cy="${ly}" r="4" style="fill:var(--chart-line);stroke:var(--bg-panel)" stroke-width="2"/>
      <g class="hover" style="display:none"><line class="xh" y1="0" y2="${h}"/><circle r="4" style="fill:var(--chart-line);stroke:var(--bg-panel)" stroke-width="2"/></g>
    </svg>`;
    const g = $('.hover', el);
    const show = (idx) => {
      idx = Math.max(0, Math.min(data.length - 1, idx));
      el.dataset.idx = idx;
      const cx = x(idx); const cy = y(data[idx]);
      g.style.display = '';
      $('line', g).setAttribute('x1', cx); $('line', g).setAttribute('x2', cx);
      $('circle', g).setAttribute('cx', cx); $('circle', g).setAttribute('cy', cy);
      let tip = $('.chart-tip', el);
      if (!tip) { el.insertAdjacentHTML('beforeend', '<div class="chart-tip" role="tooltip"></div>'); tip = $('.chart-tip', el); }
      tip.innerHTML = `<b>${data[idx]}${unit}</b><span>${label}　${sampleTime(m, idx)}</span>`;
      tip.hidden = false;
      const box = el.closest('.mon-body').getBoundingClientRect(); const r = el.getBoundingClientRect();
      const tw = tip.offsetWidth; const th = tip.offsetHeight;
      let left = cx > w / 2 ? cx - tw - 10 : cx + 10;
      left = Math.max(box.left + 6 - r.left, Math.min(left, box.right - 6 - r.left - tw));
      tip.style.left = left + 'px';
      tip.style.top = ((h - th) / 2) + 'px';
    };
    const hide = () => { g.style.display = 'none'; const tip = $('.chart-tip', el); if (tip) tip.hidden = true; };
    el.onpointermove = (e) => { const r = el.getBoundingClientRect(); show(Math.round(((e.clientX - r.left - 1) / (w - 7)) * (data.length - 1))); };
    el.onpointerleave = hide;
    el.onfocus = () => show(data.length - 1);
    el.onblur = hide;
    el.onkeydown = (e) => {
      if (e.key === 'ArrowLeft') show(Number(el.dataset.idx || data.length - 1) - 1);
      if (e.key === 'ArrowRight') show(Number(el.dataset.idx || data.length - 1) + 1);
    };
    if (S.hover && S.hover.key === key) {
      if (document.fonts.status === 'loaded') show(S.hover.idx);
      else document.fonts.ready.then(() => setTimeout(() => show(S.hover.idx), 120));
    }
  }

  /* ------------------------------------------------------------------ 状态栏 */
  function renderStatus() {
    const t = activeTab(); const s = activeSession(); const sb = $('#statusbar');
    if (!t || !s) { sb.innerHTML = '<span class="sb">就绪</span>'; return; }
    const total = s.lat.reduce((a, b) => a + b, 0);
    const state = t.status === 'connected' ? '' : t.status === 'connecting' ? ' caution' : ' failed';
    let left = `<span class="sb state${state}"><span class="dot ${t.status}"></span><b>${statusText(t.status)}</b>${esc(s.user)}@${esc(s.name)}</span>`;
    if (t.status === 'connected') left += `<span class="sb" title="密钥交换 curve25519-sha256，主机密钥 ED25519">${ic('lock')}chacha20-poly1305</span><span class="sb">${ic('activity')}往返 ${total} ms</span>`;
    else if (t.status === 'connecting') left += `<span class="sb">${ic('route')}已建立 ${t.conn.reached} / ${s.hops.length + 1} 跳</span>`;
    let right;
    if (t.kind === 'editor') right = `<span class="sb">行 21，列 47</span><span class="sb">YAML</span><span class="sb">UTF-8</span><span class="sb">LF</span><span class="sb caution-text">未保存</span>`;
    else right = `<span class="sb">xterm-256color</span><span class="sb" id="sb-size">—</span><span class="sb">UTF-8</span>`;
    const tr = S.transfers.filter((x) => !x.done).length;
    if (tr) right += `<span class="sb click" id="sb-transfers">${ic('arrow-up-from-line')}${tr} 个传输</span>`;
    sb.innerHTML = left + '<span class="sb-sp"></span>' + right;
    updateSize();
  }
  function updateSize() {
    const el = $('#sb-size'); const term = $('.term');
    if (!el || !term) return;
    const probe = document.createElement('span');
    probe.className = 'mono'; probe.style.cssText = 'position:absolute;visibility:hidden;font-size:13px'; probe.textContent = '0'.repeat(50);
    document.body.appendChild(probe);
    const cw = probe.getBoundingClientRect().width / 50; probe.remove();
    const gutter = (S.gutter.num ? 46 : 0) + (S.gutter.time ? 64 : 0) + (S.gutter.num || S.gutter.time ? 16 : 10);
    const cols = Math.floor((term.clientWidth - gutter - 18) / cw);
    const rows = Math.floor(term.clientHeight / 20);
    el.textContent = `${cols}×${rows}`;
    el.title = `终端尺寸：${cols} 列 × ${rows} 行（行号与时间列不占用终端列数）`;
  }

  /* ------------------------------------------------------------------ 对话框 */
  function dlgHead(title, desc) {
    return `<div class="dlg-head"><div class="tt"><div class="dlg-title" id="dlg-title">${title}</div>${desc ? `<div class="dlg-desc">${desc}</div>` : ''}</div><button class="icon-btn" data-close-dialog title="关闭 (Esc)">${ic('x')}</button></div>`;
  }
  function dlgSession() {
    const s = sessionById('prod-api-01');
    const hop = (h, idx, isTarget) => `
      <div class="rte-row${isTarget ? ' last' : ''}"><div class="rte-rail"><span class="node"></span></div>
        <div class="hop${isTarget ? ' target' : ''}">
          <div class="top"><span class="idx">${isTarget ? '目标' : `第 ${idx} 跳`}</span><span class="hn">${esc(h.name)}</span>${isTarget ? '' : '<span class="tag outline">已保存会话</span>'}</div>
          <div class="tools">${isTarget ? '' : `<button class="icon-btn sm grip" title="拖动调整顺序">${ic('grip-vertical', 'sm')}</button>`}<button class="icon-btn sm" title="更多">${ic('ellipsis', 'sm')}</button></div>
          <div class="addr">${esc(h.user)}@${esc(h.host)}:${h.port}</div>
          <div class="meta"><span>${ic(h.auth.includes('Agent') ? 'key' : 'key-round')}${esc(h.auth)}</span><span class="ok-text">${ic('circle-check')}已验证，${s.lat[idx - 1]} ms</span>${isTarget ? '<span>主机密钥 ED25519，已记录在 known_hosts</span>' : ''}</div>
        </div></div>`;
    return `<div class="dialog" style="width:880px;height:780px" role="dialog" aria-modal="true" aria-labelledby="dlg-title">
      ${dlgHead('编辑会话 prod-api-01', '保存在「生产环境」分组，上次连接于今天 10:41')}
      <div class="dlg-body" style="display:grid;grid-template-columns:184px minmax(0,1fr)">
        <nav class="dlg-nav">
          <button>${ic('server')}基本信息</button><button>${ic('key-round')}认证</button>
          <button aria-current="true">${ic('waypoints')}跳板与代理</button>
          <button>${ic('square-terminal')}终端</button><button>${ic('arrow-left-right')}端口转发</button><button>${ic('settings')}高级</button>
        </nav>
        <div style="overflow:auto">
          <div class="form-sec">
            <div class="sec-title">跳板链路</div>
            <div class="sec-desc">按顺序经过下列跳板机到达目标主机。每一跳可以引用已保存的会话（沿用它的认证方式），也可以单独填写。</div>
            <div class="rte">
              <div class="rte-row first"><div class="rte-rail"><span class="node"></span></div><div class="rte-origin">${ic('laptop', 'sm')}本机<span class="mono">DESKTOP-J4CK</span></div></div>
              ${hop(sessionById('edge-bastion'), 1)}
              ${hop(sessionById('core-gw'), 2)}
              <div class="rte-row"><div class="rte-rail"></div><button class="rte-add">${ic('plus', 'sm')}在目标前添加跳板<span class="muted" style="margin-left:auto;font-size:11px">可选已保存会话，或输入 user@host:port</span></button></div>
              ${hop(s, 3, true)}
            </div>
          </div>
          <div class="form-sec">
            <div class="sec-title">连接方式</div>
            <div class="opt-cards" style="margin:10px 0 6px">
              <button class="opt-card" role="radio" aria-checked="true"><span class="radio" aria-checked="true"></span><span><b>ProxyJump（推荐）</b><span>CapTerm 逐跳建立 SSH 隧道，每一跳单独认证，跳板机上无需安装任何东西。</span></span></button>
              <button class="opt-card" role="radio" aria-checked="false"><span class="radio"></span><span><b>ProxyCommand</b><span>运行自定义命令建立连接，例如 cloudflared、nc 或公司内部的接入工具。</span></span></button>
            </div>
            <div class="set-line"><div class="txt"><b>复用跳板连接</b><span>多个会话经过同一跳板时共用一条 SSH 连接，后续会话无需重复认证。</span></div><span class="switch" role="switch" aria-checked="true"></span></div>
            <div class="set-line"><div class="txt"><b>网络代理</b><span>连接第 1 跳之前先经过 SOCKS5 或 HTTP CONNECT 代理。</span></div><div class="field select" style="width:150px"><span>不使用</span>${ic('chevrons-up-down', 'xs')}</div></div>
          </div>
          <div class="form-sec" style="padding-bottom:20px">
            <div class="sec-title">等效 OpenSSH 命令</div>
            <div class="sec-desc">可以直接在其他终端里使用；导出为 SSH 配置时会写成 ProxyJump。</div>
            <div class="cmd-preview"><code>ssh -J ops@203.0.113.40:2222,jump@10.20.0.4 deploy@10.20.3.21</code><button class="icon-btn sm" title="复制">${ic('copy', 'sm')}</button></div>
          </div>
        </div>
      </div>
      <div class="dlg-foot"><button class="btn">${ic('plug-zap', 'sm')}测试连接</button><span class="note ok-text">${ic('circle-check', 'sm')}全链路可达，往返 38 ms</span><span class="sp"></span><button class="btn ghost" data-close-dialog>取消</button><button class="btn">保存</button><button class="btn primary">保存并连接</button></div>
    </div>`;
  }
  function dlgImport() {
    const st = { new: ['accent', '新增'], update: ['', '更新'], same: ['', '无变化'], wildcard: ['', '跳过'], unsupported: ['caution', '跳过'] };
    const rows = D.importRows.map((r) => {
      const [tone, txt] = st[r.status];
      const off = !r.on;
      const dis = r.status === 'wildcard' || r.status === 'unsupported';
      const jump = r.jump ? `<span class="jump">${miniRoute(1)}${esc(r.jump)}</span>`
        : r.proxyCommand ? `<span class="tag outline">ProxyCommand</span> <span class="pc" title="${esc(r.proxyCommand)}">${esc(r.proxyCommand)}</span>` : '<span class="muted">—</span>';
      const key = r.key ? `<span class="mono">${esc(r.key)}</span>` : (r.status === 'new' ? '<span class="muted">无，连接时询问密码</span>' : '<span class="muted">—</span>');
      return `<tr class="${off ? 'off' : ''}"><td class="cb"><span class="check" role="checkbox" aria-checked="${r.on}" ${dis ? 'aria-disabled="true"' : ''}></span></td>
        <td class="mono">${esc(r.alias)}</td><td class="mono">${esc(r.host) || '<span class="muted">—</span>'}</td><td class="mono">${esc(r.user) || '—'}</td><td class="mono tnum">${r.port || '—'}</td>
        <td>${jump}</td><td>${key}</td><td class="res"><span class="tag ${tone}">${txt}</span>${r.note ? `<span class="note">${esc(r.note)}</span>` : ''}</td></tr>`;
    }).join('');
    return `<div class="dialog" style="width:1060px;height:720px" role="dialog" aria-modal="true" aria-labelledby="dlg-title">
      ${dlgHead('导入 SSH 配置', '读取 OpenSSH 配置文件中的 Host 条目生成会话；ProxyJump 会自动连成跳板链路。')}
      <div class="dlg-body" style="padding:16px 20px 18px;display:flex;flex-direction:column;gap:14px">
        <div class="frm" style="grid-template-columns:88px minmax(0,1fr);margin:0;align-items:start">
          <label style="padding-top:7px">配置文件</label>
          <div class="src-files">
            <div class="src-row"><label class="field lg grow">${ic('file-text', 'sm')}<input class="mono" value="C:\\Users\\jack\\.ssh\\config" aria-label="配置文件路径" style="font-family:var(--font-mono);font-size:12px"></label><button class="btn lg">选择文件…</button><button class="btn lg ghost">${ic('plus', 'sm')}再添加一个</button></div>
            <div class="inc">${ic('link-2', 'xs')}已展开 Include：<span class="tag outline">~/.ssh/config.d/work.conf</span><span class="tag outline">~/.ssh/config.d/lab.conf</span><span>共 9 个条目</span></div>
          </div>
          <label style="padding-top:12px">导入方式</label>
          <div class="opt-cards">
            <button class="opt-card" role="radio" aria-checked="true"><span class="radio" aria-checked="true"></span><span><b>链接并自动同步</b><span>配置文件变化后自动更新会话。会话只读，需要改动时可「转为本地会话」。</span></span></button>
            <button class="opt-card" role="radio" aria-checked="false"><span class="radio"></span><span><b>复制为本地会话</b><span>只导入一次，之后在 CapTerm 中自由编辑，不再跟随文件变化。</span></span></button>
          </div>
          <label style="padding-top:5px">放入分组</label>
          <div class="inline-fields" style="gap:16px"><div class="field select" style="width:230px"><span class="mono" style="font-size:12px">~/.ssh/config</span>${ic('chevrons-up-down', 'xs')}</div>
            <label style="display:inline-flex;gap:8px;align-items:center;font-size:12.5px;color:var(--text-2)"><span class="check" role="checkbox" aria-checked="true"></span>通配符 Host 只作为默认值合并，不单独生成会话</label></div>
        </div>
        <div class="tbl-wrap"><table class="tbl">
          <thead><tr><th class="cb"><span class="check" role="checkbox" aria-checked="mixed" title="全选"></span></th><th>Host</th><th>HostName</th><th>User</th><th>Port</th><th>跳板 / 代理</th><th>IdentityFile</th><th>结果</th></tr></thead>
          <tbody>${rows}</tbody></table></div>
      </div>
      <div class="dlg-foot"><span class="note">${ic('info', 'sm')}已选 6 个：新增 5 个，更新 1 个；另有 1 个无变化、2 个跳过</span><span class="sp"></span><button class="btn ghost" data-close-dialog>取消</button><button class="btn primary">导入 6 个会话</button></div>
    </div>`;
  }
  function routeInline(s) {
    return `<span class="route compact">${routeHtml(s, { reached: s.hops.length, quiet: true })}</span>`;
  }
  function dlgHostKey() {
    const s = sessionById('prod-db-01');
    return `<div class="dialog" style="width:620px" role="alertdialog" aria-modal="true" aria-labelledby="dlg-title">
      <div class="dlg-head" style="border:0;padding:20px 16px 6px 20px;gap:14px"><div class="hk-icon">${ic('shield', 'lg')}</div><div class="tt"><div class="dlg-title" id="dlg-title">确认 prod-db-01 的主机密钥</div><div class="dlg-desc" style="font-size:12.5px;line-height:19px;margin-top:4px">首次连接这台主机，known_hosts 里还没有它的记录。请通过可信渠道（运维文档、云控制台）核对下面的指纹，一致后再继续。</div></div></div>
      <div class="dlg-body" style="padding:12px 20px 18px">
        <dl class="kv">
          <dt>主机</dt><dd class="mono">dba@10.20.5.10:22</dd>
          <dt>链路</dt><dd style="overflow:hidden">${routeInline(s)}</dd>
          <dt>密钥类型</dt><dd>ED25519（256 位）</dd>
          <dt>SHA256 指纹</dt><dd class="fp">SHA256:3vQm1rT8w0kJxGd7nH5bYc2pLz9eFaRq4uW6sK1oXyE</dd>
          <dt>写入位置</dt><dd class="mono">C:\\Users\\jack\\.ssh\\known_hosts</dd>
        </dl>
      </div>
      <div class="dlg-foot"><button class="btn ghost" data-close-dialog>取消连接</button><span class="sp"></span><button class="btn">仅本次信任</button><button class="btn primary">信任并保存</button></div>
    </div>`;
  }
  function miniPreview(theme) {
    return `<div class="mini" data-theme="${theme}"><div style="background:var(--bg-app);border-bottom:1px solid var(--line)"></div>
      <div class="mb"><div class="mp"><i style="width:80%"></i><i class="s" style="width:60%"></i><i style="width:70%"></i><i style="width:55%"></i></div>
      <div class="mt"><i class="r" style="width:70%"></i><i class="a" style="width:46%"></i><i style="width:84%"></i><i style="width:62%"></i><i style="width:74%"></i><i class="a" style="width:38%"></i><i style="width:58%"></i></div>
      <div class="mp"><i style="width:70%"></i><i style="width:80%"></i><i class="s" style="width:50%"></i></div></div>
      <div style="background:var(--bg-app);border-top:1px solid var(--line)"></div></div>`;
  }
  function setGrid(title, desc, ctl) { return `<div class="set-grid"><div class="txt"><b>${title}</b>${desc ? `<span>${desc}</span>` : ''}</div><div class="ctl">${ctl}</div></div>`; }
  const sel = (v, w) => `<div class="field select" style="width:${w || 200}px"><span>${v}</span>${ic('chevrons-up-down', 'xs')}</div>`;
  const sw = (on) => `<span class="switch" role="switch" aria-checked="${on}"></span>`;
  function settingsBody(sec) {
    if (sec === 'appearance') {
      return `<div class="form-sec"><div class="sec-title">主题</div><div class="sec-desc">深色、浅色两套主题等价设计；终端配色、图表和代码高亮随主题一起切换。快捷键 Ctrl+Shift+L。</div>
        <div class="theme-cards" role="radiogroup" aria-label="主题">
          <button class="theme-card" role="radio" aria-checked="false">${miniPreview('dark')}<span class="cap"><span class="radio"></span>深色<small>夜航</small></span></button>
          <button class="theme-card" role="radio" aria-checked="false">${miniPreview('light')}<span class="cap"><span class="radio"></span>浅色<small>白昼</small></span></button>
          <button class="theme-card system" role="radio" aria-checked="true"><div class="sys-split"><div>${miniPreview('dark')}</div><div>${miniPreview('light')}</div></div><span class="cap"><span class="radio" aria-checked="true"></span>跟随系统<small>当前为${S.theme === 'dark' ? '深色' : '浅色'}</small></span></button>
        </div></div>
        <div class="form-sec"><div class="sec-title">字体</div>
          ${setGrid('界面字体', '标签、菜单与表单。', sel('Noto Sans SC'))}
          ${setGrid('终端与编辑器字体', '建议使用中英文 2:1 等宽的字体，TUI 边框与表格才不会错位。', sel('Maple Mono NF CN'))}
          ${setGrid('终端字号与行高', '', `${sel('13', 80)}${sel('1.5 倍', 100)}`)}
        </div>
        <div class="form-sec" style="padding-bottom:16px"><div class="sec-title">行号与时间</div>
          ${setGrid('显示行号', '按会话累计编号，全屏程序（vim、htop）期间显示屏幕行号。', sw(true))}
          ${setGrid('显示时间戳', '每一行记录写入终端的时间；被重绘的行在滚入历史后定格。', sw(true))}
          ${setGrid('时间格式', '', sel('时:分:秒（10:42:07）', 200))}
          ${setGrid('弱化同一秒内重复的时间', '让命令的开始时间更容易被扫到。', sw(true))}
          ${setGrid('空闲间隔线', '两行之间超过设定时长时插入一条带时长的分隔线。', `<label class="field" style="width:120px"><input value="60" aria-label="秒"><span class="muted">秒</span></label>`)}
        </div>`;
    }
    if (sec === 'terminal') {
      return `<div class="form-sec"><div class="sec-title">终端兼容</div><div class="sec-desc">面向 vim、htop、tmux 以及 Claude Code、Codex、Gemini CLI 等 Agent CLI。</div>
          ${setGrid('终端类型 TERM', '同时设置 COLORTERM=truecolor（需服务器 AcceptEnv 放行）。', sel('xterm-256color'))}
          ${setGrid('Shift+Enter', 'Agent CLI 用它换行。程序开启 Kitty 键盘协议时按协议编码。', sel('发送 ESC + Enter', 200))}
          ${setGrid('Kitty 键盘协议', '允许程序请求更完整的按键编码（CSI u）。', sw(true))}
          ${setGrid('同步输出（DEC 2026）', '整帧绘制，避免 Agent CLI 刷新时闪烁。', sw(true))}
          ${setGrid('允许程序写入剪贴板（OSC 52）', '', sel('每次询问', 160))}
          ${setGrid('桌面通知（OSC 9 / 777、响铃）', '后台标签需要你确认时提醒。', sel('仅后台标签', 160))}
          ${setGrid('粘贴多行内容时确认', '整段粘贴模式和括号粘贴不受影响。', sw(true))}
          ${setGrid('回滚行数', '', sel('100 000 行', 160))}
        </div>`;
    }
    if (sec === 'ssh') {
      return `<div class="form-sec"><div class="sec-title">SSH 配置文件</div><div class="sec-desc">按顺序读取，后面的文件不会覆盖前面已定义的 Host（与 OpenSSH 规则一致）。</div>
          <div class="path-list">
            <div class="path-item">${ic('file-text', 'sm')}<div style="min-width:0"><div class="p">C:\\Users\\jack\\.ssh\\config</div><div class="s">已链接，11 个主机，2 分钟前同步</div></div><div style="display:flex;gap:2px"><button class="icon-btn sm" title="立即同步">${ic('refresh-cw', 'sm')}</button><button class="icon-btn sm" title="移除">${ic('x', 'sm')}</button></div></div>
            <div class="path-item">${ic('file-text', 'sm')}<div style="min-width:0"><div class="p">D:\\work\\infra\\ssh_config</div><div class="s">已链接，5 个主机，今天 09:12 同步</div></div><div style="display:flex;gap:2px"><button class="icon-btn sm" title="立即同步">${ic('refresh-cw', 'sm')}</button><button class="icon-btn sm" title="移除">${ic('x', 'sm')}</button></div></div>
          </div>
          <div style="display:flex;gap:8px;margin:10px 0 14px"><button class="btn">${ic('plus', 'sm')}添加配置文件…</button><button class="btn ghost">从文件导入一次…</button></div>
          ${setGrid('监听文件变化', '文件保存后自动重新解析并更新链接的会话。', sw(true))}
        </div>
        <div class="form-sec" style="padding-bottom:16px"><div class="sec-title">认证与安全</div>
          ${setGrid('known_hosts 文件', '', `<span class="mono muted" style="font-size:12px">~/.ssh/known_hosts</span>`)}
          ${setGrid('首次连接新主机', '', sel('显示指纹并询问', 180))}
          ${setGrid('SSH Agent', '', sel('Windows OpenSSH Agent', 200))}
          ${setGrid('保活间隔', 'ServerAliveInterval', sel('30 秒', 120))}
          ${setGrid('连接超时', '每一跳单独计时。', sel('15 秒', 120))}
        </div>`;
    }
    if (sec === 'transfer') {
      return `<div class="form-sec" style="padding-bottom:16px"><div class="sec-title">文件传输</div>
          ${setGrid('默认下载位置', '', `<span class="mono muted" style="font-size:12px">C:\\Users\\jack\\Downloads</span><button class="btn xs">更改…</button>`)}
          ${setGrid('同时传输', '', sel('3 个文件', 120))}
          ${setGrid('目标已存在时', '', sel('在队列中询问', 160))}
          ${setGrid('保留修改时间与权限', '', sw(true))}
          ${setGrid('点击打开文本文件的大小上限', '更大的文件以只读方式打开前 1 MB，或下载后用本机程序打开。', sel('5 MB', 120))}
          ${setGrid('保存方式', '先写入临时文件再原子替换，失败时远端文件保持不变。', sel('原子替换', 140))}
        </div>`;
    }
    if (sec === 'monitor') {
      return `<div class="form-sec" style="padding-bottom:16px"><div class="sec-title">主机监控</div><div class="sec-desc">复用当前 SSH 连接执行只读命令（读取 /proc、df），不在服务器上安装任何程序。</div>
          ${setGrid('采样间隔', '', sel('5 秒', 120))}
          ${setGrid('CPU 偏高 / 告急', '', `${sel('80%', 90)}${sel('95%', 90)}`)}
          ${setGrid('磁盘偏高 / 告急', '', `${sel('80%', 90)}${sel('95%', 90)}`)}
          ${setGrid('跳板机上也采集', '默认只采集链路的目标主机。', sw(false))}
          ${setGrid('窗口在后台时暂停采样', '', sw(true))}
        </div>`;
    }
    return `<div class="form-sec" style="padding-bottom:16px"><div class="sec-title">快捷键</div><div class="sec-desc">全局快捷键统一使用 Ctrl+Shift，避免抢走终端程序需要的 Ctrl 组合键。</div>
      <table class="kbd-table">${D.shortcuts.map(([a, k]) => `<tr><td>${esc(a)}</td><td>${k.split(' / ').map((g) => `<span class="kbds">${g.split('+').map((x) => `<kbd>${esc(x)}</kbd>`).join('')}</span>`).join('<span class="muted"> 或 </span>')}</td></tr>`).join('')}</table></div>`;
  }
  function dlgSettings() {
    const nav = [['appearance', 'sun-moon', '外观'], ['terminal', 'square-terminal', '终端'], ['ssh', 'key-round', 'SSH 与会话'], ['transfer', 'arrow-up-from-line', '文件传输'], ['monitor', 'activity', '主机监控'], ['keys', 'keyboard', '快捷键']];
    return `<div class="dialog" style="width:980px;height:700px" role="dialog" aria-modal="true" aria-labelledby="dlg-title">
      ${dlgHead('设置', '修改即时生效，保存在 %APPDATA%\\CapTerm\\settings.json')}
      <div class="dlg-body" style="display:grid;grid-template-columns:200px minmax(0,1fr)">
        <nav class="dlg-nav">${nav.map(([id, icon, label]) => `<button data-set="${id}" aria-current="${S.settingsSec === id}">${ic(icon)}${label}</button>`).join('')}</nav>
        <div style="overflow:auto">${settingsBody(S.settingsSec)}</div>
      </div>
    </div>`;
  }
  function dlgPalette() {
    const q = (S.palQ || '').toLowerCase();
    const items = D.sessions.filter((s) => !q || `${s.name} ${s.user}@${s.host}`.toLowerCase().includes(q)).slice(0, 7);
    const direct = /^[\w.-]+@[\w.-]+/.test(q) ? `<div class="pal-group">快速连接</div><div class="pal-item" aria-selected="true">${ic('plug-zap')}<span>连接到 <span class="mono">${esc(q)}</span></span><span class="sub">Enter</span></div>` : '';
    return `<div class="dialog palette" role="dialog" aria-modal="true" aria-label="命令面板">
      <div class="pal-input">${ic('search')}<input id="pal-input" placeholder="搜索会话、命令，或输入 user@host 快速连接" value="${esc(S.palQ || '')}"></div>
      <div class="pal-list">${direct}<div class="pal-group">会话</div>${items.map((s, i) => `<div class="pal-item" data-connect="${esc(s.id)}" aria-selected="${!direct && i === 0}">${ic('server')}<span>${esc(s.name)} <span class="sub">${esc(s.user)}@${esc(s.host)}</span></span>${s.hops.length ? `<span class="hopinfo">${miniRoute(s.hops.length)}${s.hops.length} 跳</span>` : '<span></span>'}</div>`).join('')}
        <div class="pal-group">命令</div>
        <div class="pal-item">${ic('sun-moon')}<span>切换深色 / 浅色主题</span><span class="sub">Ctrl+Shift+L</span></div>
        <div class="pal-item">${ic('import')}<span>从 SSH 配置导入…</span><span></span></div>
        <div class="pal-item">${ic('hash')}<span>显示 / 隐藏行号与时间</span><span class="sub">Ctrl+Shift+G</span></div>
      </div></div>`;
  }
  function renderLayer() {
    const layer = $('#layer');
    if (!S.dialog) { layer.className = 'layer'; layer.innerHTML = ''; return; }
    const html = { session: dlgSession, import: dlgImport, hostkey: dlgHostKey, settings: dlgSettings, palette: dlgPalette }[S.dialog]();
    layer.className = 'layer open' + (S.dialog === 'palette' ? ' top' : '');
    layer.innerHTML = html;
    const pal = $('#pal-input');
    if (pal) { pal.focus(); pal.setSelectionRange(pal.value.length, pal.value.length); }
  }
  function renderToasts() {
    $('#toasts').innerHTML = S.toasts.map((t, i) => `<div class="toast" role="status"><span class="ic">${ic(t.icon)}</span><div><b>${esc(t.title)}</b><p>${t.body}</p><div class="acts">${t.acts.map((a, j) => `<button class="btn xs ${j ? 'ghost' : ''}" data-toast="${i}" data-act="${j}">${esc(a)}</button>`).join('')}</div></div><span class="muted" style="font-size:11px">${esc(t.time)}</span></div>`).join('');
  }

  /* ------------------------------------------------------------------ 渲染入口 */
  function renderTheme() {
    document.documentElement.dataset.theme = S.theme;
    const use = $('#theme-toggle use');
    if (use) use.setAttribute('href', S.theme === 'dark' ? '#i-sun' : '#i-moon');
    $('#theme-toggle').title = S.theme === 'dark' ? '切换到浅色主题 (Ctrl+Shift+L)' : '切换到深色主题 (Ctrl+Shift+L)';
  }
  function renderAll() {
    renderTheme(); renderTabs(); renderRoute(); renderComposer(); renderViewport();
    renderFiles(); renderSessions(); renderMonitor(); renderStatus(); renderLayer(); renderToasts();
    fitTerm(); updateSize();
    document.fonts.ready.then(() => { fitTerm(); updateSize(); if (S.drag && S.drag.frozen) placeGhost(); });
  }
  function refreshCenter() { renderTabs(); renderRoute(); renderComposer(); renderViewport(); renderStatus(); }
  function refreshContext() { refreshCenter(); renderFiles(); renderSessions(); renderMonitor(); }

  /* ------------------------------------------------------------------ 交互 */
  function openEditor(path, pin) {
    const name = path.split('/').pop();
    let tab = S.tabs.find((t) => t.kind === 'editor' && t.file === name);
    if (!tab) {
      const preview = S.tabs.findIndex((t) => t.preview);
      tab = editorTab(name, { preview: !pin });
      if (preview >= 0) S.tabs.splice(preview, 1, tab); else S.tabs.push(tab);
    } else if (pin) tab.preview = false;
    S.active = tab.id; S.selFile = path;
    refreshCenter(); renderFiles();
  }
  function connect(id) {
    const existing = S.tabs.find((t) => t.kind === 'term' && t.session === id);
    if (existing) { S.active = existing.id; refreshContext(); return; }
    const s = sessionById(id);
    const tab = termTab(id, id, D.transcripts[id] ? id : 'connect', { status: 'connecting', conn: { reached: 0 } });
    if (!D.transcripts[id]) {
      tab.start = 1;
      tab.lines = [{ k: 'sys', t: now(), icon: 'route', s: `正在连接 ${s.name}（${s.host}:${s.port}）${s.hops.length ? `，共 ${s.hops.length + 1} 跳` : ''}` }];
    }
    S.tabs.push(tab); S.active = tab.id;
    refreshContext();
    let step = 0;
    const timer = setInterval(() => {
      step += 1;
      if (step <= s.hops.length) {
        tab.conn.reached = step;
        tab.lines.push({ k: 'sys', t: now(), icon: 'circle-check', tone: 'ok', s: `${sessionById(s.hops[step - 1]).name} 已认证` });
      } else {
        clearInterval(timer);
        delete tab.conn; tab.status = 'connected';
        tab.lines.push({ k: 'sys', t: now(), icon: 'circle-check', tone: 'ok', s: `已连接 ${s.name}（原型演示：未真正建立 SSH 连接）` });
        tab.lines.push({ k: 'p', t: now(), cmd: '', st: 'idle', user: s.user, host: s.name, cwd: '~' });
      }
      if (S.active === tab.id) refreshContext(); else renderTabs();
    }, 650);
  }
  const now = () => new Date().toTimeString().slice(0, 8);
  function send() {
    const ta = $('#draft'); const t = activeTab();
    if (!ta || !t || t.kind !== 'term' || t.status !== 'connected') return;
    const text = ta.value.trim(); if (!text) return;
    const idle = t.lines.findIndex((l) => l.k === 'p' && l.st === 'idle');
    const tail = idle >= 0 ? t.lines.splice(idle, 1)[0] : null;
    if (S.mode === 'paste' || t.agent) {
      const at = t.lines.findIndex((l) => l.k === 'live' || l.k === 'livebox');
      const add = text.split('\n').map((l, i) => ({ k: 'o', t: now(), s: `${i ? '  ' : '⟦a8|>⟧ '}${l.replace(/[⟦⟧]/g, '')}` }));
      add.push({ k: 'o', t: now(), s: '⟦dim|（原型演示：内容未真正发送）⟧' });
      t.lines.splice(at >= 0 ? at : t.lines.length, 0, ...add);
    } else {
      text.split('\n').forEach((cmd) => {
        t.lines.push(Object.assign({}, tail || { user: 'deploy', host: t.title, cwd: '~' }, { k: 'p', t: now(), cmd, st: 'ok', dur: '0.0 秒' }));
        t.lines.push({ k: 'o', t: now(), s: '⟦dim|（原型演示：命令未真正发送）⟧' });
      });
    }
    if (tail) t.lines.push(Object.assign({}, tail, { t: now() }));
    S.draft = '';
    renderViewport(); renderComposer();
    const d = $('#draft'); if (d) d.focus();
  }
  function showMenu(anchor, items) {
    closeMenu();
    const r = anchor.getBoundingClientRect();
    document.body.insertAdjacentHTML('beforeend', `<div class="menu" id="menu" role="menu">${items.map((it) => (it === '-' ? '<div class="sep"></div>' : `<button role="menuitem" data-menu="${esc(it.id || '')}">${it.check ? ic('check', 'check-ic') : ic(it.icon || 'circle-dot', 'sm')}${esc(it.label)}${it.sub ? `<span class="sub">${esc(it.sub)}</span>` : ''}</button>`)).join('')}</div>`);
    const m = $('#menu');
    let top = r.bottom + 4;
    if (top + m.offsetHeight > window.innerHeight - 8) top = r.top - m.offsetHeight - 4;
    m.style.top = top + 'px';
    m.style.left = Math.min(r.left, window.innerWidth - m.offsetWidth - 8) + 'px';
  }
  function closeMenu() { const m = $('#menu'); if (m) m.remove(); }
  function toggleTheme() { S.theme = S.theme === 'dark' ? 'light' : 'dark'; renderTheme(); renderMonitor(); if (S.dialog === 'settings') renderLayer(); }
  function setPane(which, on) {
    S[which === 'left' ? 'leftOff' : 'rightOff'] = !on;
    $('#app').dataset[which] = on ? 'on' : 'off';
    $(`[data-toggle-pane="${which}"]`).setAttribute('aria-pressed', on);
    setTimeout(renderMonitor, 0);
  }

  document.addEventListener('click', (e) => {
    const el = e.target.closest('button, #tr-toggle, [data-tab], [data-path], [data-session], [data-group], .close, .check, .switch, .opt-card, .theme-card, .pal-item');
    if (!e.target.closest('#menu')) closeMenu();
    if (!el) { if (e.target.id === 'layer' && S.dialog) { S.dialog = null; renderLayer(); } return; }

    if (el.matches('[data-close]')) { e.stopPropagation(); const id = el.dataset.close; const i = S.tabs.findIndex((t) => t.id === id); S.tabs.splice(i, 1); if (S.active === id) S.active = (S.tabs[Math.max(0, i - 1)] || {}).id || null; refreshContext(); return; }
    if (el.matches('[data-tab]')) { S.active = el.dataset.tab; const t = activeTab(); if (t.badge) delete t.badge; refreshContext(); return; }
    if (el.matches('[data-close-dialog]')) { S.dialog = null; renderLayer(); return; }
    if (el.id === 'theme-toggle') { toggleTheme(); return; }
    if (el.id === 'open-settings') { S.dialog = 'settings'; renderLayer(); return; }
    if (el.id === 'quick' || el.id === 'tab-add') { S.dialog = 'palette'; S.palQ = ''; renderLayer(); return; }
    if (el.id === 'new-session' || el.matches('[data-edit-session]')) { S.dialog = 'session'; renderLayer(); return; }
    if (el.id === 'import-config' || el.dataset.rail === 'import') { S.dialog = 'import'; renderLayer(); return; }
    if (el.dataset.rail) { $('#app').classList.toggle('rail-open'); setTimeout(renderMonitor, 0); return; }
    if (el.dataset.togglePane) { const w = el.dataset.togglePane; setPane(w, $('#app').dataset[w] === 'off'); return; }
    if (el.dataset.set) { S.settingsSec = el.dataset.set; renderLayer(); return; }
    if (el.matches('.pal-item[data-connect]')) { S.dialog = null; renderLayer(); connect(el.dataset.connect); return; }
    if (el.matches('[data-connect]')) { connect(el.dataset.connect); return; }
    if (el.dataset.gutter) { S.gutter[el.dataset.gutter] = !S.gutter[el.dataset.gutter]; renderRoute(); renderViewport(); updateSize(); return; }
    if (el.id === 'find-toggle') { S.find = S.find ? null : { q: '', cur: 1 }; renderRoute(); renderViewport(); const fi = $('#find-input'); if (fi) fi.focus(); return; }
    if (el.id === 'find-close') { S.find = null; renderRoute(); renderViewport(); return; }
    if (el.id === 'route-menu') { showMenu(el, [{ icon: 'copy', label: '复制 SSH 命令' }, { icon: 'clipboard-copy', label: '复制会话日志（含行号与时间）' }, { icon: 'download', label: '导出会话日志…' }, '-', { icon: 'rotate-cw', label: '重新连接', id: 'reconnect' }, { icon: 'unplug', label: '断开连接' }]); return; }
    if (el.dataset.menu === 'reconnect') { const t = activeTab(); closeMenu(); if (t) { const id = t.session; S.tabs = S.tabs.filter((x) => x !== t); connect(id); } return; }
    if (el.id === 'target') { showMenu(el, [{ label: `当前会话 ${activeSession() ? activeSession().name : ''}`, check: !S.broadcast, id: 'tgt-one' }, { label: '广播到所有已连接会话', check: S.broadcast, id: 'tgt-all', icon: 'radio-tower' }, { label: '选择会话…', icon: 'list-filter' }]); return; }
    if (el.dataset.menu === 'tgt-one' || el.dataset.menu === 'tgt-all') { S.broadcast = el.dataset.menu === 'tgt-all'; closeMenu(); renderComposer(); return; }
    if (el.dataset.mode) { S.mode = el.dataset.mode; S.draft = ($('#draft') || {}).value || ''; renderComposer(); return; }
    if (el.id === 'send') { send(); return; }
    if (el.id === 'composer-expand') { const t = activeTab(); if (t) t.alt = null; refreshCenter(); return; }
    if (el.dataset.mon) { S.monTab = el.dataset.mon; renderMonitor(); return; }
    if (el.id === 'follow') { S.follow = !S.follow; renderFiles(); return; }
    if (el.id === 'toggle-hidden') { S.showHidden = !S.showHidden; renderFiles(); return; }
    if (el.id === 'tr-toggle' || el.closest('#tr-toggle')) { S.trOpen = !S.trOpen; renderFiles(); return; }
    if (el.matches('[data-edit]')) { e.stopPropagation(); openEditor(el.dataset.edit, true); return; }
    if (el.matches('[data-path]')) {
      const p = el.dataset.path;
      if (el.dataset.dir === '1') { if (S.openDirs.has(p)) S.openDirs.delete(p); else S.openDirs.add(p); renderFiles(); return; }
      if (el.dataset.text === '1') { openEditor(p, false); return; }
      S.selFile = p; renderFiles(); return;
    }
    if (el.matches('[data-group]')) { const g = el.dataset.group; if (S.collapsed.has(g)) S.collapsed.delete(g); else S.collapsed.add(g); renderSessions(); return; }
    if (el.matches('[data-session]')) { $$('.srow').forEach((r) => r.classList.toggle('active', r === el)); return; }
    if (el.matches('[data-toast]')) { const t = S.toasts[Number(el.dataset.toast)]; S.toasts.splice(Number(el.dataset.toast), 1); renderToasts(); if (el.dataset.act === '0' && t) { S.active = 'staging'; const tab = activeTab(); if (tab) delete tab.badge; refreshContext(); } return; }
    if (el.matches('.check') && el.getAttribute('aria-disabled') !== 'true') { el.setAttribute('aria-checked', el.getAttribute('aria-checked') !== 'true'); return; }
    if (el.matches('.switch')) { el.setAttribute('aria-checked', el.getAttribute('aria-checked') !== 'true'); return; }
    if (el.matches('.opt-card, .theme-card')) {
      $$(el.matches('.opt-card') ? '.opt-card' : '.theme-card', el.parentElement).forEach((c) => { c.setAttribute('aria-checked', c === el); const r = $('.radio', c); if (r) r.setAttribute('aria-checked', c === el); });
      if (el.matches('.theme-card')) { const idx = $$('.theme-card', el.parentElement).indexOf(el); if (idx < 2) { S.theme = idx === 0 ? 'dark' : 'light'; renderTheme(); renderMonitor(); } }
    }
  });

  document.addEventListener('dblclick', (e) => {
    const row = e.target.closest('[data-session]');
    if (row) { connect(row.dataset.session); return; }
    const f = e.target.closest('[data-path]');
    if (f && f.dataset.text === '1') openEditor(f.dataset.path, true);
  });

  document.addEventListener('input', (e) => {
    if (e.target.id === 'sess-filter') { S.filter = e.target.value; const pos = e.target.selectionStart; renderSessions(); const inp = $('#sess-filter'); inp.focus(); inp.setSelectionRange(pos, pos); }
    if (e.target.id === 'draft') { S.draft = e.target.value; autosize(e.target); }
    if (e.target.id === 'find-input') { S.find.q = e.target.value; S.find.cur = 1; const pos = e.target.selectionStart; renderViewport(); const fi = $('#find-input'); fi.focus(); fi.setSelectionRange(pos, pos); }
    if (e.target.id === 'pal-input') { S.palQ = e.target.value; renderLayer(); }
  });

  document.addEventListener('keydown', (e) => {
    const k = e.key.toLowerCase();
    if (e.key === 'Escape') { if (S.dialog) { S.dialog = null; renderLayer(); } else if (S.find) { S.find = null; renderRoute(); renderViewport(); } closeMenu(); return; }
    if (e.target.id === 'draft' && e.key === 'Enter' && !e.shiftKey) { e.preventDefault(); send(); return; }
    if (e.target.id === 'pal-input' && e.key === 'Enter') { const sel = $('.pal-item[aria-selected="true"][data-connect]'); if (sel) { S.dialog = null; renderLayer(); connect(sel.dataset.connect); } return; }
    if (e.ctrlKey && e.key === ',') { e.preventDefault(); S.dialog = 'settings'; renderLayer(); return; }
    if (!(e.ctrlKey && e.shiftKey)) return;
    const map = {
      l: toggleTheme,
      p: () => { S.dialog = 'palette'; S.palQ = ''; renderLayer(); },
      f: () => { if (!S.find) S.find = { q: '', cur: 1 }; renderRoute(); renderViewport(); const fi = $('#find-input'); if (fi) fi.focus(); },
      g: () => { const on = !(S.gutter.num && S.gutter.time); S.gutter.num = on; S.gutter.time = on; renderRoute(); renderViewport(); updateSize(); },
      e: () => setPane('left', $('#app').dataset.left === 'off'),
      b: () => setPane('right', $('#app').dataset.right === 'off'),
      n: () => { S.dialog = 'session'; renderLayer(); },
      i: () => { const d = $('#draft'); if (d) d.focus(); }
    };
    if (map[k]) { e.preventDefault(); map[k](); }
  });

  /* 拖拽：从系统拖入文件到文件面板上传；拖动文件行到终端插入远程路径 */
  document.addEventListener('dragover', (e) => {
    const list = e.target.closest('#file-list');
    if (list && e.dataTransfer.types.includes('Files')) {
      e.preventDefault();
      const row = e.target.closest('.frow[data-dir="1"]');
      const over = row ? row.dataset.path : '';
      if (!S.drag || S.drag.over !== over) { S.drag = { over, names: ['拖入的文件'] }; renderFiles(); }
    } else if (e.target.closest('#viewport')) e.preventDefault();
  });
  document.addEventListener('dragleave', (e) => {
    if (S.drag && !S.drag.frozen && !e.relatedTarget) { S.drag = null; renderFiles(); }
  });
  document.addEventListener('drop', (e) => {
    if (e.target.closest('#file-list') && e.dataTransfer.files.length) {
      e.preventDefault();
      const s = activeSession(); const root = D.files[s.id].root;
      const to = S.drag && S.drag.over ? `${root}/${S.drag.over}` : root;
      Array.from(e.dataTransfer.files).forEach((f, i) => {
        const tr = { id: 'u' + Date.now() + i, dir: 'up', name: f.name, to, pct: 0, speed: '—', eta: '排队中', size: `${(f.size / 1048576).toFixed(1)} MB` };
        S.transfers.unshift(tr);
        const timer = setInterval(() => {
          tr.pct = Math.min(100, tr.pct + 9 + Math.round(Math.random() * 12)); tr.speed = `${(8 + Math.random() * 9).toFixed(1)} MB/s`; tr.eta = `剩余 ${Math.max(1, Math.round((100 - tr.pct) / 12))} 秒`;
          if (tr.pct >= 100) { clearInterval(timer); tr.done = true; }
          renderFiles(); renderStatus();
        }, 400);
      });
      S.drag = null; S.trOpen = true; renderFiles(); renderStatus();
      return;
    }
    if (e.target.closest('#viewport')) {
      e.preventDefault();
      const p = e.dataTransfer.getData('text/x-capterm-path');
      const d = $('#draft');
      if (p && d) { d.value = `${d.value}${d.value && !d.value.endsWith(' ') ? ' ' : ''}'${p}'`; S.draft = d.value; d.focus(); autosize(d); }
    }
  });
  document.addEventListener('dragstart', (e) => {
    const row = e.target.closest && e.target.closest('.frow[data-path]');
    if (!row) return;
    const s = activeSession();
    e.dataTransfer.setData('text/x-capterm-path', `${D.files[s.id].root}/${row.dataset.path}`);
    e.dataTransfer.setData('text/plain', `${D.files[s.id].root}/${row.dataset.path}`);
    e.dataTransfer.effectAllowed = 'copy';
  });

  /* 分隔条拖动调整面板尺寸 */
  document.addEventListener('pointerdown', (e) => {
    const sp = e.target.closest('[data-split]');
    if (!sp) return;
    e.preventDefault();
    sp.classList.add('dragging');
    const root = document.documentElement.style;
    const kind = sp.dataset.split;
    const onMove = (ev) => {
      if (kind === 'left') root.setProperty('--w-left', Math.max(200, Math.min(480, ev.clientX)) + 'px');
      if (kind === 'right') root.setProperty('--w-right', Math.max(260, Math.min(440, window.innerWidth - ev.clientX)) + 'px');
      if (kind === 'monitor') root.setProperty('--h-monitor', Math.max(220, Math.min(window.innerHeight - 220, window.innerHeight - 24 - ev.clientY)) + 'px');
    };
    const onUp = () => { sp.classList.remove('dragging'); window.removeEventListener('pointermove', onMove); window.removeEventListener('pointerup', onUp); renderMonitor(); updateSize(); };
    window.addEventListener('pointermove', onMove);
    window.addEventListener('pointerup', onUp);
  });

  let resizeTimer;
  window.addEventListener('resize', () => {
    // 导出截图时视口会在截图前被调整，必须同步重绘
    if (document.documentElement.hasAttribute('data-static')) { $('#drag-ghost') && $('#drag-ghost').remove(); renderAll(); return; }
    clearTimeout(resizeTimer); resizeTimer = setTimeout(() => { renderMonitor(); renderViewport(); updateSize(); if (S.drag && S.drag.frozen) placeGhost(); }, 80); });

  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', renderAll);
  else renderAll();
})();
