/* CapTerm 原型样例数据。
 * 全部为虚构内容：IP 使用文档/私有网段（203.0.113.0/24、10.0.0.0/8、192.168.0.0/16），域名使用 example.org。
 * 终端行内样式标记：⟦类名|文字⟧，类名对应 styles.css 中的 ANSI 类（a1–a15、b 粗体、dim、inv、bg*）。
 */
(function () {
  'use strict';

  /* ---------- 确定性随机数，保证每次渲染与导出一致 ---------- */
  function rng(seed) {
    return function () {
      seed |= 0; seed = (seed + 0x6D2B79F5) | 0;
      var t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }
  function series(seed, n, base, jitter, spikes, last) {
    var r = rng(seed), out = [];
    for (var i = 0; i < n; i++) {
      var v = base + (r() - 0.5) * 2 * jitter;
      (spikes || []).forEach(function (s) {
        var d = Math.abs(i - s.at);
        if (d <= s.width) v += s.height * (1 - d / (s.width + 1));
      });
      out.push(Math.max(0, Math.round(v * 10) / 10));
    }
    if (last != null) out[n - 1] = last;
    return out;
  }

  /* ---------- 会话 ---------- */
  var groups = [
    { id: 'prod', name: '生产环境' },
    { id: 'test', name: '测试环境' },
    { id: 'cfg', name: '~/.ssh/config', linked: true, path: 'C:\\Users\\jack\\.ssh\\config', synced: '2 分钟前同步' }
  ];

  var sessions = [
    { id: 'prod-api-01', group: 'prod', name: 'prod-api-01', user: 'deploy', host: '10.20.3.21', port: 22, hops: ['edge-bastion', 'core-gw'], auth: '密钥 deploy_ed25519', lat: [12, 9, 17] },
    { id: 'prod-api-02', group: 'prod', name: 'prod-api-02', user: 'deploy', host: '10.20.3.22', port: 22, hops: ['edge-bastion', 'core-gw'], auth: '密钥 deploy_ed25519', lat: [12, 9, 16] },
    { id: 'prod-db-01', group: 'prod', name: 'prod-db-01', user: 'dba', host: '10.20.5.10', port: 22, hops: ['edge-bastion', 'core-gw'], auth: '密钥 dba_ed25519', lat: [12, 9, 21] },
    { id: 'core-gw', group: 'prod', name: 'core-gw', user: 'jump', host: '10.20.0.4', port: 22, hops: ['edge-bastion'], auth: 'SSH Agent', lat: [12, 9] },
    { id: 'edge-bastion', group: 'prod', name: 'edge-bastion', user: 'ops', host: '203.0.113.40', port: 2222, hops: [], auth: '密钥 id_ed25519', lat: [12] },
    { id: 'staging', group: 'test', name: 'staging', user: 'deploy', host: '192.168.50.12', port: 22, hops: [], auth: '密钥 id_ed25519', lat: [6] },
    { id: 'build-box', group: 'test', name: 'build-box', user: 'builder', host: '192.168.50.30', port: 22, hops: [], auth: '密码', lat: [5] },
    { id: 'gpu-a100', group: 'cfg', name: 'gpu-a100', user: 'ml', host: '10.8.0.21', port: 22, hops: ['lab-gw'], auth: '密钥 lab_ed25519', lat: [24, 3], source: 'config' },
    { id: 'lab-gw', group: 'cfg', name: 'lab-gw', user: 'me', host: 'lab.example.org', port: 22, hops: [], auth: '密钥 lab_ed25519', lat: [24], source: 'config' },
    { id: 'nas', group: 'cfg', name: 'nas', user: 'admin', host: '192.168.1.10', port: 5022, hops: [], auth: '密码', lat: [2], source: 'config' },
    { id: 'pi-garage', group: 'cfg', name: 'pi-garage', user: 'pi', host: '192.168.1.42', port: 22, hops: [], auth: '密钥 id_ed25519', lat: [3], source: 'config' }
  ];

  /* ---------- 远程文件 ---------- */
  function f(name, size, mtime, extra) { return Object.assign({ name: name, size: size, mtime: mtime }, extra || {}); }
  function d(name, mtime, children) { return { name: name, dir: true, mtime: mtime, children: children || [] }; }

  var files = {
    'prod-api-01': {
      root: '/srv/api/current',
      items: [
        d('config', '11-05 18:20', [f('app.toml', '2.8 KB', '11-05 18:20'), f('worker.toml', '0.6 KB', '11-05 18:20')]),
        d('logs', '今天 10:41', [f('access.log', '42.7 MB', '今天 10:42'), f('app.log', '18.4 MB', '今天 10:41'), f('worker.log', '6.1 MB', '今天 10:41')]),
        d('scripts', '10-28 09:12', [f('migrate.sh', '1.1 KB', '10-28 09:12'), f('rotate-logs.sh', '0.7 KB', '10-28 09:12')]),
        f('.env.production', '1.2 KB', '10-30 14:03', { text: true, hidden: true }),
        f('compose.yaml', '1.6 KB', '今天 09:58', { text: true }),
        f('Dockerfile', '0.9 KB', '10-12 11:40', { text: true }),
        f('Makefile', '2.1 KB', '10-12 11:40', { text: true }),
        f('nginx.conf', '3.0 KB', '10-12 11:40', { text: true }),
        f('README.md', '3.4 KB', '09-21 16:05', { text: true }),
        f('VERSION', '6 B', '今天 09:55', { text: true }),
        f('release-1.8.3.tar.gz', '48.2 MB', '今天 09:55', { archive: true })
      ]
    },
    'staging': {
      root: '/home/deploy',
      items: [
        d('apps', '11-02 16:44'), d('backups', '11-06 03:00'),
        f('.bashrc', '3.7 KB', '08-14 10:02', { text: true, hidden: true }),
        f('deploy.sh', '1.9 KB', '11-02 16:44', { text: true }),
        f('notes.md', '0.8 KB', '昨天 21:13', { text: true })
      ]
    },
    'prod-db-01': { root: '/home/dba', items: [] }
  };

  /* ---------- 终端记录 ---------- */
  var P = function (t, cmd, st, dur, extra) {
    return Object.assign({ k: 'p', t: t, cmd: cmd, st: st, dur: dur, user: 'deploy', host: 'prod-api-01', cwd: '/srv/api/current' }, extra || {});
  };
  var O = function (t, s) { return { k: 'o', t: t, s: s }; };

  var prodTranscript = {
    start: 1180,
    lines: [
      P('10:41:20', 'git log --oneline -3', 'ok', '0.1 秒'),
      O('10:41:20', '⟦a3|9f3c2d1⟧ (⟦a6 b|HEAD -> ⟧⟦a2 b|release/1.8.3⟧, ⟦a3 b|tag: v1.8.3⟧) fix: retry redis auth on boot'),
      O('10:41:20', '⟦a3|4b7a0e9⟧ chore: bump redis client to 5.2'),
      O('10:41:20', '⟦a3|e21d6c4⟧ feat(worker): configurable concurrency'),
      P('10:41:44', 'cat VERSION', 'ok', '0.0 秒'),
      O('10:41:44', '1.8.3'),
      P('10:41:52', 'docker compose ps', 'ok', '0.6 秒'),
      O('10:41:52', 'NAME       IMAGE                         SERVICE   STATUS'),
      O('10:41:52', 'api-1      registry.internal/api:1.8.3   api       Up 2 hours (healthy)'),
      O('10:41:52', 'redis-1    redis:7.4-alpine              redis     Up 2 hours'),
      O('10:41:52', 'worker-1   registry.internal/api:1.8.3   worker    ⟦a3|Restarting (1) 8 seconds ago⟧'),
      P('10:42:06', 'docker compose logs --tail 5 worker', 'ok', '0.9 秒'),
      O('10:42:07', '⟦a6|worker-1  |⟧ 10:41:58 ⟦a2|INFO ⟧ boot  queue=default concurrency=8'),
      O('10:42:07', '⟦a6|worker-1  |⟧ 10:41:58 ⟦a2|INFO ⟧ connecting to redis://redis:6379/2'),
      O('10:42:07', '⟦a6|worker-1  |⟧ 10:41:59 ⟦a1 b|ERROR⟧ redis: NOAUTH Authentication required.'),
      O('10:42:07', '⟦a6|worker-1  |⟧ 10:41:59 ⟦a1 b|ERROR⟧ worker exited with code 1'),
      O('10:42:07', '⟦a6|worker-1  |⟧ 10:42:00 ⟦a2|INFO ⟧ restarting in 5s (attempt 14)'),
      P('10:42:31', 'grep -n REDIS .env.production', 'err', '0.1 秒', { code: 1 }),
      P('10:42:48', 'grep -rn REDIS_URL compose.yaml config/', 'ok', '0.1 秒'),
      O('10:42:48', '⟦a5|compose.yaml⟧⟦a6|:⟧⟦a2|21⟧⟦a6|:⟧      ⟦a1 b|REDIS_URL⟧: redis://redis:6379/2'),
      O('10:42:48', '⟦a5|config/worker.toml⟧⟦a6|:⟧⟦a2|4⟧⟦a6|:⟧redis_url = "${⟦a1 b|REDIS_URL⟧}"'),
      { k: 'gap', label: '空闲 4 分 51 秒' },
      P('10:47:39', 'df -h / /data', 'ok', '0.1 秒'),
      O('10:47:39', 'Filesystem      Size  Used Avail Use% Mounted on'),
      O('10:47:39', '/dev/vda1        97G   61G   37G  62% /'),
      O('10:47:39', '/dev/vdb1       2.0T  1.6T  382G  81% /data'),
      P('10:48:02', 'vim compose.yaml', 'ok', '1 分 12 秒'),
      { k: 'tui', t: '10:49:14', s: '已退出全屏程序 vim，用时 1 分 12 秒（全屏内容不计入行号）' },
      P('10:49:20', 'docker compose up -d worker', 'ok', '2.4 秒'),
      O('10:49:21', '[+] Running 1/1'),
      O('10:49:23', ' ⟦a2|✔⟧ Container api-worker-1  ⟦a2|Started⟧                          ⟦a4|2.1s⟧'),
      P('10:49:31', 'docker compose ps worker', 'ok', '0.5 秒'),
      O('10:49:31', 'NAME       IMAGE                         SERVICE   STATUS'),
      O('10:49:31', 'worker-1   registry.internal/api:1.8.3   worker    ⟦a2|Up 8 seconds⟧'),
      P('10:49:40', '', 'idle', '')
    ]
  };

  /* Agent CLI（内联 TUI）：前半段已提交为历史，最后的输入框是“实时区”，会被反复重绘 */
  var agentTranscript = {
    start: 1213,
    agent: { name: 'Claude Code', since: '10:49:46' },
    lines: [
      P('10:49:46', 'claude', 'run', '运行中 6 分钟'),
      { k: 'box', t: '10:49:47', w: 80, rows: [
        '⟦cell a5|✻⟧ Welcome to ⟦b|Claude Code⟧!',
        '',
        '  ⟦dim|/help for help, /status for your current setup⟧',
        '',
        '  ⟦dim|cwd: /srv/api/current⟧'
      ], color: 'a5' },
      O('10:50:02', ''),
      O('10:50:02', '⟦a8|>⟧ worker 一直重启，帮我查一下原因并修复'),
      O('10:50:02', ''),
      O('10:50:05', '⟦a15|●⟧ 我先看看 worker 的日志和配置。'),
      O('10:50:05', ''),
      O('10:50:06', '⟦a2|●⟧ ⟦b|Bash⟧(docker compose logs --tail 50 worker)'),
      O('10:50:07', '  ⟦dim|⎿⟧  ⟦a1|ERROR redis: NOAUTH Authentication required.⟧'),
      O('10:50:07', '     ⟦dim|… +46 lines (ctrl+r to expand)⟧'),
      O('10:50:07', ''),
      O('10:50:12', '⟦a2|●⟧ ⟦b|Read⟧(compose.yaml)'),
      O('10:50:12', '  ⟦dim|⎿⟧  Read ⟦b|42⟧ lines'),
      O('10:50:12', ''),
      O('10:50:19', '⟦a15|●⟧ 原因找到了：Redis 在 10:31 开启了密码认证，但 worker 的 REDIS_URL'),
      O('10:50:19', '  没有带密码；api 服务读取的是 REDIS_PASSWORD，所以只有 worker 受影响。'),
      O('10:50:19', ''),
      O('10:50:24', '⟦a2|●⟧ ⟦b|Update⟧(compose.yaml)'),
      O('10:50:24', '  ⟦dim|⎿⟧  Updated ⟦b|compose.yaml⟧ with ⟦b|1⟧ addition and ⟦b|1⟧ removal'),
      O('10:50:24', '       ⟦dim|20⟧      environment:'),
      O('10:50:24', '       ⟦a1|21 -⟧ ⟦bg1|     REDIS_URL: redis://redis:6379/2                        ⟧'),
      O('10:50:24', '       ⟦a2|21 +⟧ ⟦bg2|     REDIS_URL: redis://:${REDIS_PASSWORD}@redis:6379/2     ⟧'),
      O('10:50:24', '       ⟦dim|22⟧        WORKER_CONCURRENCY: "8"'),
      O('10:50:24', ''),
      O('10:50:31', '⟦a15|●⟧ 改好了。需要我重启 worker 让配置生效吗？'),
      O('10:50:31', ''),
      { k: 'livebox', w: 80, rows: ['⟦a8|>⟧ 先别重启，等我确认一下⟦cursor| ⟧'] },
      { k: 'live', s: '  ⟦cell a5|⏵⟧⟦cell a5|⏵⟧⟦a5| accept edits on⟧ ⟦dim|(shift+tab to cycle)⟧' }
    ]
  };

  /* 全屏 TUI（备用屏幕）：htop */
  var htopScreen = [
    '    ⟦a4|0⟧⟦b|[⟧⟦a2||||||||||⟧⟦a1||||⟧                          ⟦b|23.4%]⟧   Tasks: ⟦a6 b|87⟧, ⟦a6|312 thr⟧; ⟦a2 b|2⟧ running',
    '    ⟦a4|1⟧⟦b|[⟧⟦a2|||||||⟧⟦a1|||⟧                            ⟦b|17.9%]⟧   Load average: ⟦b|0.84⟧ ⟦b|0.71⟧ 0.66',
    '    ⟦a4|2⟧⟦b|[⟧⟦a2|||||||||||||||⟧⟦a1||||||⟧                 ⟦b|41.2%]⟧   Uptime: ⟦b|12 days, 04:13:52⟧',
    '    ⟦a4|3⟧⟦b|[⟧⟦a2|||||⟧⟦a1||⟧                               ⟦b|11.0%]⟧',
    '  ⟦a4|Mem⟧⟦b|[⟧⟦a2||||||||||||||⟧⟦a4|||⟧⟦a3|||||⟧               ⟦b|3.12G/7.76G]⟧',
    '  ⟦a4|Swp⟧⟦b|[⟧⟦a1||⟧                                ⟦b|64.0M/2.00G]⟧',
    '',
    '⟦inv b|    PID USER      PRI  NI  VIRT   RES   SHR S  CPU% MEM%   TIME+  Command                ⟧',
    '⟦bgsel|   2381 deploy     20   0 1234M  412M 38212 S  18.2  5.2  3h12:41 node dist/server.js    ⟧',
    '   2417 deploy     20   0  986M  244M 30144 S   6.4  3.1  0:41.07 node dist/worker.js',
    '   1102 ⟦a8|root⟧       20   0 2310M  112M 52260 S   2.1  1.4  6h02:13 /usr/bin/dockerd',
    '   1388 ⟦a8|999⟧        20   0  98.1M 15.6M  4408 S   1.6  0.2  1h40:55 redis-server *:6379',
    '    911 ⟦a8|root⟧       20   0 1890M  71.4M 34120 S   0.8  0.9  2h11:07 /usr/bin/containerd',
    '   2290 deploy     20   0  12.1M  5.9M  3520 S   0.4  0.1  0:02.88 -bash',
    '   3561 deploy     20   0  10.4M  4.2M  3256 ⟦a2|R⟧   0.3  0.1  0:00.21 htop',
    '    748 ⟦a8|root⟧       20   0  15.2M  7.8M  6612 S   0.2  0.1  0:31.42 sshd: /usr/sbin/sshd',
    '      1 ⟦a8|root⟧       20   0  166M  12.9M  8388 S   0.1  0.2  0:52.10 /sbin/init',
    '    402 ⟦a8|root⟧       19  -1  50.6M 18.3M 17204 S   0.0  0.2  0:09.83 /lib/systemd/journald',
    '    656 ⟦a8|systemd-r⟧  20   0  25.4M 12.4M  9860 S   0.0  0.2  0:04.55 systemd-resolved',
    '    690 ⟦a8|root⟧       20   0  234M   7.6M  6408 S   0.0  0.1  0:00.92 /usr/libexec/udisks2',
    '    712 ⟦a8|root⟧       20   0  14.9M  6.1M  5288 S   0.0  0.1  0:01.37 cron -f',
    '    733 ⟦a8|syslog⟧     20   0  217M   5.6M  4480 S   0.0  0.1  0:02.06 rsyslogd -n',
    '   1544 ⟦a8|root⟧       20   0  1.2G  42.1M 30120 S   0.0  0.5  0:18.42 containerd-shim',
    '   1601 ⟦a8|root⟧       20   0  1.2G  41.8M 29876 S   0.0  0.5  0:17.96 containerd-shim',
    '   2011 deploy     20   0  17.0M  9.8M  8104 S   0.0  0.1  0:00.12 /lib/systemd/systemd --user',
    '   2288 deploy     20   0  17.4M  6.7M  4980 S   0.0  0.1  0:00.37 sshd: deploy@pts/0',
    '    681 ⟦a8|root⟧       20   0  17.9M  8.1M  7104 S   0.0  0.1  0:00.84 /lib/systemd/systemd-logind',
    '    642 ⟦a8|messagebu⟧  20   0   9.6M  5.0M  4152 S   0.0  0.1  0:01.12 @dbus-daemon --system',
    '    699 ⟦a8|polkitd⟧    20   0  301M   8.4M  6820 S   0.0  0.1  0:00.47 /usr/lib/polkit-1/polkitd',
    '    601 ⟦a8|root⟧       RT   0  282M  27.0M  8840 S   0.0  0.3  0:12.30 /sbin/multipathd -d -s',
    '    760 ⟦a8|root⟧       20   0  1.4G  31.6M 18204 S   0.0  0.4  0:24.18 /usr/lib/snapd/snapd',
    '    804 ⟦a8|root⟧       20   0  108M  21.4M 13016 S   0.0  0.3  0:00.36 /usr/bin/python3 /usr/share/unattended-upgrades',
    '    812 ⟦a8|root⟧       20   0   8.4M  1.9M  1772 S   0.0  0.0  0:00.01 /sbin/agetty -o -p -- \\u --noclear',
    '    671 ⟦a8|root⟧       20   0  32.8M 19.6M 10432 S   0.0  0.2  0:00.42 /usr/bin/networkd-dispatcher',
    '   3410 deploy     20   0  11.2M  5.1M  3360 S   0.0  0.1  0:00.06 tmux: server',
    '',
    '',
    '',
    '',
    '',
    '⟦inv|F1⟧Help  ⟦inv|F2⟧Setup ⟦inv|F3⟧Search⟦inv|F4⟧Filter⟦inv|F5⟧Tree  ⟦inv|F6⟧SortBy⟦inv|F7⟧Nice -⟦inv|F8⟧Nice +⟦inv|F9⟧Kill  ⟦inv|F10⟧Quit'
  ];

  var stagingTranscript = {
    start: 342,
    lines: [
      P('10:31:05', 'kubectl get pods -n web', 'ok', '0.4 秒', { host: 'staging', cwd: '~' }),
      O('10:31:05', 'NAME                   READY   STATUS    RESTARTS   AGE'),
      O('10:31:05', 'web-6f9c8d7b5-2kq8x    1/1     Running   0          3d4h'),
      O('10:31:05', 'web-6f9c8d7b5-v7n2m    1/1     Running   0          3d4h'),
      O('10:31:05', 'web-6f9c8d7b5-zt4lp    1/1     Running   ⟦a3|2⟧          3d4h'),
      P('10:33:40', 'codex "web 的第三个副本重启过两次，看下原因"', 'run', '等待你的确认', { host: 'staging', cwd: '~' }),
      O('10:33:52', '⟦a6|•⟧ 发现 web-6f9c8d7b5-zt4lp 因 OOMKilled 重启（limit 256Mi）。'),
      O('10:34:10', '⟦a3|▌⟧ 需要执行：⟦b|kubectl rollout restart deploy/web -n web⟧'),
      O('10:34:10', '⟦a3|▌⟧ 允许执行吗？ ⟦inv| y 是 ⟧  ⟦b| n 否 ⟧')
    ]
  };

  /* 首次连接 prod-db-01：逐跳建立，等待确认主机密钥 */
  var connectTranscript = {
    start: 1,
    lines: [
      { k: 'sys', t: '10:58:02', icon: 'route', s: '正在连接 edge-bastion（203.0.113.40:2222），第 1 跳，共 3 跳' },
      { k: 'sys', t: '10:58:02', icon: 'circle-check', tone: 'ok', s: 'edge-bastion 已认证：公钥 id_ed25519，用时 0.31 秒' },
      { k: 'sys', t: '10:58:02', icon: 'route', s: '经 edge-bastion 打开到 core-gw（10.20.0.4:22）的通道' },
      { k: 'sys', t: '10:58:03', icon: 'circle-check', tone: 'ok', s: 'core-gw 已认证：SSH Agent（jack@DESKTOP-J4CK），用时 0.22 秒' },
      { k: 'sys', t: '10:58:03', icon: 'route', s: '经 core-gw 打开到 prod-db-01（10.20.5.10:22）的通道' },
      { k: 'sys', t: '10:58:03', icon: 'shield', tone: 'caution', s: '首次连接 prod-db-01：等待你确认主机密钥' }
    ]
  };

  /* ---------- 编辑器文件 ---------- */
  var composeYaml = [
    '# /srv/api/current/compose.yaml',
    'name: api',
    '',
    'services:',
    '  api:',
    '    image: registry.internal/api:1.8.3',
    '    restart: unless-stopped',
    '    env_file: .env.production',
    '    ports:',
    '      - "8080:8080"',
    '    depends_on:',
    '      - redis',
    '    healthcheck:',
    '      test: ["CMD", "curl", "-fsS", "http://localhost:8080/healthz"]',
    '      interval: 10s',
    '',
    '  worker:',
    '    image: registry.internal/api:1.8.3',
    '    command: ["node", "dist/worker.js"]',
    '    environment:',
    '      REDIS_URL: redis://:${REDIS_PASSWORD}@redis:6379/2',
    '      WORKER_CONCURRENCY: "8"',
    '    env_file: .env.production',
    '    restart: unless-stopped',
    '',
    '  redis:',
    '    image: redis:7.4-alpine',
    '    command: ["redis-server", "--requirepass", "${REDIS_PASSWORD}"]',
    '    volumes:',
    '      - redis-data:/data',
    '',
    'volumes:',
    '  redis-data:',
    ''
  ];

  /* ---------- 监控 ---------- */
  var monitor = {
    'prod-api-01': {
      os: 'Ubuntu 24.04', kernel: '6.8', uptime: '已运行 32 天', cores: 4, interval: 5, end: '10:49:40',
      cpu: series(7, 60, 21, 5, [{ at: 40, width: 3, height: 34 }], 24),
      mem: { used: 5.1, total: 7.8, unit: 'GiB' },
      swap: { used: 0.2, total: 2.0, unit: 'GiB' },
      disks: [{ mount: '/', used: 61, total: 97, unit: 'GB', pct: 62 }, { mount: '/data', used: 1.6, total: 2.0, unit: 'TB', pct: 81 }],
      rx: series(11, 60, 10.6, 2.4, [{ at: 44, width: 4, height: 6 }], 12.4),
      tx: series(19, 60, 1.05, 0.3, [{ at: 41, width: 2, height: 1.2 }], 1.2),
      load: [0.42, 0.38, 0.30],
      procs: [
        { pid: 2381, cmd: 'node dist/server.js', cpu: 18.2, mem: 5.2 },
        { pid: 2417, cmd: 'node dist/worker.js', cpu: 6.4, mem: 3.1 },
        { pid: 1102, cmd: 'dockerd', cpu: 2.1, mem: 1.4 },
        { pid: 1388, cmd: 'redis-server', cpu: 1.6, mem: 2.0 },
        { pid: 911, cmd: 'containerd', cpu: 0.8, mem: 0.9 },
        { pid: 748, cmd: 'sshd', cpu: 0.2, mem: 0.1 },
        { pid: 1, cmd: 'systemd', cpu: 0.1, mem: 0.2 }
      ]
    },
    'staging': {
      os: 'Debian 12', kernel: '6.1', uptime: '已运行 12 天', cores: 4, interval: 5, end: '10:55:20',
      cpu: series(23, 60, 22, 6, [{ at: 52, width: 2, height: 14 }], 23),
      mem: { used: 3.1, total: 7.8, unit: 'GiB' },
      swap: { used: 0.1, total: 2.0, unit: 'GiB' },
      disks: [{ mount: '/', used: 18, total: 64, unit: 'GB' }, { mount: '/var/lib/docker', used: 41, total: 128, unit: 'GB' }],
      rx: series(29, 60, 2.1, 0.8, [], 1.9),
      tx: series(31, 60, 0.4, 0.15, [], 0.3),
      load: [0.84, 0.71, 0.66],
      procs: [
        { pid: 2381, cmd: 'node dist/server.js', cpu: 18.2, mem: 5.2 },
        { pid: 2417, cmd: 'node dist/worker.js', cpu: 6.4, mem: 3.1 },
        { pid: 1102, cmd: 'dockerd', cpu: 2.1, mem: 1.4 },
        { pid: 1388, cmd: 'redis-server', cpu: 1.6, mem: 0.2 },
        { pid: 3561, cmd: 'htop', cpu: 0.3, mem: 0.1 }
      ]
    }
  };
  /* Agent 场景：claude 进程占用明显 */
  monitor.agent = Object.assign({}, monitor['prod-api-01'], {
    end: '10:55:46',
    cpu: series(41, 60, 31, 7, [{ at: 18, width: 3, height: 22 }, { at: 47, width: 2, height: 18 }], 38),
    procs: [
      { pid: 3120, cmd: 'claude', cpu: 21.6, mem: 4.8 },
      { pid: 2381, cmd: 'node dist/server.js', cpu: 12.9, mem: 5.2 },
      { pid: 2417, cmd: 'node dist/worker.js', cpu: 1.2, mem: 3.0 },
      { pid: 1102, cmd: 'dockerd', cpu: 1.4, mem: 1.4 },
      { pid: 1388, cmd: 'redis-server', cpu: 0.9, mem: 2.0 },
      { pid: 911, cmd: 'containerd', cpu: 0.6, mem: 0.9 },
      { pid: 748, cmd: 'sshd', cpu: 0.2, mem: 0.1 },
      { pid: 1, cmd: 'systemd', cpu: 0.1, mem: 0.2 }
    ]
  });

  /* ---------- 传输队列 ---------- */
  var transfers = {
    idle: [
      { id: 't1', dir: 'up', name: 'build-artifacts.zip', to: '/srv/api/current', pct: 62, speed: '14.2 MB/s', eta: '剩余 4 秒', size: '86.0 MB' },
      { id: 't2', dir: 'down', name: 'access.log', to: 'C:\\Users\\jack\\Downloads', pct: 31, speed: '8.6 MB/s', eta: '剩余 11 秒', size: '42.7 MB' }
    ],
    upload: [
      { id: 't1', dir: 'up', name: 'build-artifacts.zip', to: '/srv/api/current', pct: 62, speed: '14.2 MB/s', eta: '剩余 4 秒', size: '86.0 MB' },
      { id: 't3', dir: 'up', name: 'app.log', to: '/srv/api/current/logs', conflict: '已存在同名文件（18.4 MB，10:41 修改）', size: '18.9 MB' },
      { id: 't2', dir: 'down', name: 'access.log', to: 'C:\\Users\\jack\\Downloads', pct: 31, speed: '8.6 MB/s', eta: '剩余 11 秒', size: '42.7 MB' }
    ]
  };

  /* ---------- SSH config 导入预览 ---------- */
  var importRows = [
    { on: true, alias: 'prod-web-01', host: '10.30.1.11', user: 'deploy', port: 22, jump: 'edge-bastion', key: '~/.ssh/id_ed25519', status: 'new' },
    { on: true, alias: 'prod-web-02', host: '10.30.1.12', user: 'deploy', port: 22, jump: 'edge-bastion', key: '~/.ssh/id_ed25519', status: 'new' },
    { on: true, alias: 'gpu-a100', host: '10.8.0.21', user: 'ml', port: 22, jump: 'lab-gw', key: '~/.ssh/lab_ed25519', status: 'update', note: 'IdentityFile 已变化' },
    { on: false, alias: 'lab-gw', host: 'lab.example.org', user: 'me', port: 22, jump: '', key: '~/.ssh/lab_ed25519', status: 'same' },
    { on: true, alias: 'nas', host: '192.168.1.10', user: 'admin', port: 5022, jump: '', key: '', status: 'new' },
    { on: true, alias: 'pi-garage', host: '192.168.1.42', user: 'pi', port: 22, jump: '', key: '~/.ssh/id_ed25519', status: 'new' },
    { on: true, alias: 'git.example.org', host: 'git.example.org', user: 'git', port: 22, jump: '', proxyCommand: 'cloudflared access ssh --hostname %h', key: '~/.ssh/id_ed25519', status: 'new' },
    { on: false, alias: '*.internal', host: '', user: 'deploy', port: '', jump: '', key: '~/.ssh/id_ed25519', status: 'wildcard', note: '作为默认值合并' },
    { on: false, alias: 'Match host *.lab', host: '', user: '', port: '', jump: '', key: '', status: 'unsupported', note: '暂不支持 Match 块' }
  ];

  /* ---------- 快捷键 ---------- */
  var shortcuts = [
    ['命令面板 / 快速连接', 'Ctrl+Shift+P'],
    ['新建会话', 'Ctrl+Shift+N'],
    ['复制当前会话到新标签', 'Ctrl+Shift+T'],
    ['关闭标签', 'Ctrl+Shift+W'],
    ['切换标签', 'Ctrl+Tab / Ctrl+Shift+Tab'],
    ['终端内搜索', 'Ctrl+Shift+F'],
    ['复制 / 粘贴', 'Ctrl+Shift+C / Ctrl+Shift+V'],
    ['聚焦输入栏', 'Ctrl+Shift+I'],
    ['显示/隐藏行号与时间', 'Ctrl+Shift+G'],
    ['显示/隐藏文件面板', 'Ctrl+Shift+E'],
    ['显示/隐藏会话与监控面板', 'Ctrl+Shift+B'],
    ['切换深色 / 浅色主题', 'Ctrl+Shift+L'],
    ['专注模式（只保留终端）', 'Ctrl+Shift+Enter'],
    ['保存文件（编辑器）', 'Ctrl+S'],
    ['设置', 'Ctrl+,']
  ];

  window.CT_DATA = {
    groups: groups, sessions: sessions, files: files,
    transcripts: { 'prod-api-01': prodTranscript, agent: agentTranscript, staging: stagingTranscript, connect: connectTranscript },
    htop: htopScreen, composeYaml: composeYaml, monitor: monitor,
    transfers: transfers, importRows: importRows, shortcuts: shortcuts
  };
})();
