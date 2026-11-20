(function () {
  if (!/(^|\.)quranify\.pro$/.test(location.hostname) || window.top !== window) return;
  var post = function (cmd) { window.ipc.postMessage(JSON.stringify({ cmd: cmd })); };
  var post2 = function (msg) { window.ipc.postMessage(JSON.stringify(msg)); };

  function addButton() {
    if (!document.body || document.getElementById('__qo_btn')) return;
    var b = document.createElement('button');
    b.id = '__qo_btn';
    b.type = 'button';
    b.title = 'السور المحمّلة والاستماع بدون إنترنت (Ctrl+O)';
    b.innerHTML =
      '<svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 4v11"/><path d="m7 10 5 5 5-5"/><path d="M5 20h14"/></svg><span>أوفلاين</span>';
    b.setAttribute('style', [
      'position:fixed', 'top:14px', 'right:18px', 'z-index:2147483000',
      'display:flex', 'align-items:center', 'gap:6px', 'padding:7px 13px',
      'border-radius:999px', 'border:1px solid rgba(255,255,255,.14)',
      'background:rgba(30,30,32,.55)', 'backdrop-filter:blur(14px)', '-webkit-backdrop-filter:blur(14px)',
      'color:#fff', 'font:600 13px/1 "Segoe UI",system-ui,sans-serif', 'cursor:pointer',
      'opacity:.85', 'transition:opacity .15s,background .15s'
    ].join(';'));
    b.onmouseenter = function () { b.style.opacity = '1'; b.style.background = 'rgba(60,60,64,.7)'; };
    b.onmouseleave = function () { b.style.opacity = '.85'; b.style.background = 'rgba(30,30,32,.55)'; };
    b.onclick = function () { post('goOffline'); };
    document.body.appendChild(b);
  }

  // React can replace <body> children while hydrating, so re-check now and then.
  document.addEventListener('DOMContentLoaded', addButton);
  setInterval(addButton, 2000);

  window.addEventListener('keydown', function (e) {
    if (e.ctrlKey && !e.shiftKey && !e.altKey && (e.key === 'o' || e.key === 'O')) {
      e.preventDefault();
      post('goOffline');
    }
  }, true);

  // ── Player bridge: tray icon, global shortcuts and the mini player use these. ──
  function btn(re) {
    var all = document.querySelectorAll('button[aria-label]');
    for (var i = 0; i < all.length; i++) {
      var b = all[i];
      if (re.test(b.getAttribute('aria-label')) && b.getClientRects().length) return b;
    }
    return null;
  }
  function media() {
    var list = document.querySelectorAll('audio');
    for (var i = 0; i < list.length; i++) if (list[i].currentSrc && list[i].currentSrc.indexOf('blob:') === 0) return list[i];
    return list[0] || null;
  }
  window.__qctl = function (a) {
    var b = null;
    if (a === 'pause' || a === 'sleepStop') b = btn(/^Pause$/i);
    else if (a === 'play') b = btn(/^Play$/i);
    else if (a === 'toggle') b = btn(/^(Play|Pause)$/i);
    else if (a === 'next') b = btn(/^Next surah$/i);
    else if (a === 'prev') b = btn(/^Previous surah$/i);
    else if (a.indexOf('seek:') === 0) { var m = media(); if (m && isFinite(m.duration)) m.currentTime = +a.slice(5); return; }
    if (b) b.click();
  };

  // Report the current page and what is playing. Cheap: one small check per second while
  // playing, every 3 seconds otherwise, and only sent when something changed.
  var lastUrl = '', lastState = '', tick = 0;
  setInterval(function () {
    tick++;
    var m = media();
    var playing = !!(m && !m.paused);
    if (!playing && tick % 3) return;
    if (location.href !== lastUrl) {
      lastUrl = location.href;
      post2({ cmd: 'route', url: lastUrl });
    }
    var bar = document.querySelector('[aria-label="Open full player"]');
    var lines = bar ? bar.innerText.split(/\n/).map(function (x) { return x.trim(); }).filter(Boolean) : [];
    var st = {
      title: lines[0] || '', sub: lines[1] || '', initial: lines[1] || lines[0] || '', playing: playing,
      pos: m ? Math.floor(m.currentTime || 0) : 0,
      dur: m && isFinite(m.duration) ? Math.floor(m.duration) : 0, source: 'site'
    };
    var json = JSON.stringify(st);
    if (json !== lastState) { lastState = json; post2({ cmd: 'state', data: st }); }
  }, 1000);

  // Internal links that ask for a new tab open in this window instead (no bare popups).
  document.addEventListener('click', function (e) {
    if (e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.shiftKey || e.metaKey) return;
    var a = e.target.closest && e.target.closest('a[target="_blank"][href]');
    if (!a) return;
    var u;
    try { u = new URL(a.getAttribute('href'), location.href); } catch (_) { return; }
    if (/(^|\.)quranify\.pro$/.test(u.hostname) && u.pathname.indexOf('/__/auth') !== 0) {
      e.preventDefault();
      e.stopPropagation();
      location.assign(u.href);
    }
  }, true);

  var timer;
  window.addEventListener('offline', function () {
    clearTimeout(timer);
    timer = setTimeout(function () { if (!navigator.onLine) post('goOffline'); }, 4000);
  });
  window.addEventListener('online', function () { clearTimeout(timer); });
})();
