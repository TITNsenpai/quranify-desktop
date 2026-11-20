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
    // Never remember a broken page as "the last page" (the app would reopen it next time).
    if (location.href !== lastUrl && !isErrorPage()) {
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
    if (playing && tick % 5 === 0) saveResume(m);
  }, 1000);

  // ── Resume where you stopped ──
  // The site itself always opens on Al-Fatihah from the start. Remember the surah, reciter and
  // position here, and on the next start load that surah again, paused at the same minute.
  var RESUME = '__qd_resume', restoring = false;
  var nativePlay = HTMLMediaElement.prototype.play;
  HTMLMediaElement.prototype.play = function () {
    // The surah page starts playing by itself; keep it silent while we restore.
    if (restoring) this.muted = true;
    return nativePlay.apply(this, arguments);
  };
  function barInfo() {
    var bar = document.querySelector('[aria-label="Open full player"]');
    if (!bar) return null;
    var img = bar.querySelector('img'), src = '';
    try { src = decodeURIComponent(img ? (img.currentSrc || img.src || '') : ''); } catch (_) {}
    var id = src.match(/\/reciters\/(\d+)\//);
    var lines = bar.innerText.split(/\n/).map(function (x) { return x.trim(); }).filter(Boolean);
    return { title: lines[0] || '', name: lines[1] || '', rid: id ? id[1] : null };
  }
  // Also save the moment you pause, so quitting right after a pause keeps the exact spot.
  document.addEventListener('pause', function (e) { if (e.target && e.target.tagName === 'AUDIO') saveResume(e.target); }, true);
  function saveResume(m) {
    if (restoring) return;
    var b = barInfo();
    if (!b || !b.rid || !b.title || !isFinite(m.duration)) return;
    try {
      if (m.duration - m.currentTime < 15) { localStorage.removeItem(RESUME); return; }
      var lang = (location.pathname.match(/^\/([a-z]{2})(\/|$)/) || [0, 'en'])[1];
      localStorage.setItem(RESUME, JSON.stringify({ rid: b.rid, name: b.name, title: b.title, pos: Math.floor(m.currentTime), lang: lang, ts: Date.now() }));
    } catch (_) {}
  }
  function sleep(ms) { return new Promise(function (r) { setTimeout(r, ms); }); }
  async function waitFor(fn, ms) {
    for (var t = 0; t < ms; t += 250) { var v = fn(); if (v) return v; await sleep(250); }
    return null;
  }
  function fmt(s) { s = Math.floor(s); return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0'); }
  function note(text) {
    var n = document.createElement('div');
    n.textContent = text;
    n.setAttribute('dir', 'rtl');
    n.setAttribute('style', 'position:fixed;left:50%;bottom:96px;transform:translateX(-50%);z-index:2147483000;background:#fff;color:#000;padding:9px 18px;border-radius:999px;font:600 14px "Segoe UI",Tahoma,sans-serif;box-shadow:0 10px 30px rgba(0,0,0,.4);transition:opacity .3s');
    document.body.appendChild(n);
    setTimeout(function () { n.style.opacity = '0'; setTimeout(function () { n.remove(); }, 400); }, 4500);
  }
  async function restoreResume() {
    try { if (sessionStorage.getItem('__qd_restored')) return; sessionStorage.setItem('__qd_restored', '1'); } catch (_) { return; }
    var s = null;
    try { s = JSON.parse(localStorage.getItem(RESUME) || 'null'); } catch (_) {}
    if (!s || !s.rid || !s.pos || Date.now() - s.ts > 30 * 864e5) return;
    if (!(await waitFor(function () { return window.next && window.next.router && document.querySelector('[aria-label="Open full player"]'); }, 15000))) return;
    await sleep(1200);
    var m = media(), b = barInfo();
    if (m && !m.paused) return; // you already started something yourself
    if (b && b.title === s.title && b.name === s.name && m && m.currentTime > 5) return;
    var rpath = await reciterPath(s.lang, s.rid, s.name);
    if (!/\/reciters\/[a-z]/.test(rpath)) return;
    var link = null;
    try {
      var doc = new DOMParser().parseFromString(await (await fetch(rpath, { credentials: 'same-origin' })).text(), 'text/html');
      // Surah pages end with the surah's name, e.g. ".../al-baqarah" for "Al-Baqarah",
      // ".../ali-imran" for "Ali 'Imran". Compare letters only.
      var key = function (x) { return String(x).toLowerCase().normalize('NFKD').replace(/[^a-z0-9]/g, ''); };
      var want = key(s.title);
      link = Array.prototype.find.call(doc.querySelectorAll('a[href^="' + rpath + '/"]'), function (a) {
        return key(a.getAttribute('href').split('/').pop()) === want;
      });
    } catch (_) {}
    if (!link) return;
    var back = location.pathname + location.search;
    restoring = true;
    window.next.router.push(link.getAttribute('href'));
    var a = await waitFor(function () {
      var bi = barInfo(), x = media();
      return bi && bi.title === s.title && x && x.readyState >= 1 && isFinite(x.duration) && x.duration > 0 ? x : null;
    }, 20000);
    if (a) {
      // Pause through the site's own button so its player knows it's paused (pausing the
      // audio directly made the first press of Play do nothing).
      await waitFor(function () { return !a.paused || null; }, 3000);
      var pb = btn(/^Pause$/i);
      if (pb) pb.click();
      await waitFor(function () { return a.paused || null; }, 2000);
      a.pause();
      a.currentTime = Math.min(s.pos, Math.max(0, a.duration - 10));
      await sleep(400);
    }
    // Never leave it playing out loud by surprise (e.g. right after Windows starts).
    var cur = media();
    if (cur && !cur.paused) { var pb2 = btn(/^Pause$/i); if (pb2) pb2.click(); cur.pause(); }
    if (cur) cur.muted = false;
    restoring = false;
    if (back !== location.pathname + location.search && !/\/reciters\//.test(back)) window.next.router.push(back);
    if (a) note('رجعت لـ ' + s.title + ' عند ' + fmt(a.currentTime) + '. دوس تشغيل وكمّل.');
  }
  if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', restoreResume);
  else restoreResume();

  // ── Site bug workaround: reciter links by number (e.g. /en/reciters/92, the reciter's name in
  // the full player) get "Internal Server Error" from quranify.pro's server, in any browser.
  // The reciter's normal page (/en/reciters/yasser-al-dosari-sa) works, so open that instead.
  var RID = /^\/([a-z]{2})\/reciters\/(\d+)\/?$/;
  var CC = ['sa', 'eg', 'kw', 'ae', 'ma', 'ly', 'dz', 'tn', 'sd', 'jo', 'sy', 'iq', 'ye', 'om', 'qa', 'bh', 'ps', 'lb',
    'tr', 'ru', 'kz', 'uz', 'tj', 'az', 'pk', 'in', 'bd', 'id', 'my', 'ng', 'so', 'gb', 'us', 'ca', 'fr', 'de'];
  function slugify(n) {
    return String(n).toLowerCase().normalize('NFKD').replace(/[̀-ͯ]/g, '')
      .replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
  }
  function loadSlugs() { try { return JSON.parse(localStorage.getItem('__qd_rslug') || '{}'); } catch (_) { return {}; } }
  function saveSlugs(c) { try { localStorage.setItem('__qd_rslug', JSON.stringify(c)); } catch (_) {} }
  function exists(path) {
    return fetch(path, { method: 'HEAD', credentials: 'same-origin' }).then(function (r) { return r.ok; }, function () { return false; });
  }
  async function reciterPath(lang, id, name) {
    var cache = loadSlugs(), base = '/' + lang + '/reciters/';
    if (cache[id]) return base + cache[id];
    try {
      // The reciters page links each reciter by name and shows their picture by number.
      var html = await (await fetch('/' + lang + '/reciters', { credentials: 'same-origin' })).text(), m;
      var re = /href="\/[a-z]{2}\/reciters\/([a-z0-9-]+)"[\s\S]{0,1500}?reciters%2F(\d+)%2F/g;
      while ((m = re.exec(html))) if (m[1].indexOf('collections') !== 0 && !cache[m[2]]) cache[m[2]] = m[1];
      saveSlugs(cache);
    } catch (_) {}
    if (cache[id]) return base + cache[id];
    // Not on that page: the address is "name-country". Try the likely countries, a few at a time.
    if (name) {
      var stem = slugify(name);
      for (var i = 0; i < CC.length; i += 6) {
        var batch = CC.slice(i, i + 6);
        var ok = await Promise.all(batch.map(function (cc) { return exists(base + stem + '-' + cc); }));
        var k = ok.indexOf(true);
        if (k >= 0) { cache[id] = stem + '-' + batch[k]; saveSlugs(cache); return base + cache[id]; }
      }
    }
    return '/' + lang + '/reciters';
  }
  function goTo(path, replace) {
    // Client-side navigation keeps the current recitation playing.
    if (!replace && window.next && window.next.router && window.next.router.push) window.next.router.push(path);
    else if (replace) location.replace(path); else location.assign(path);
  }
  document.addEventListener('click', function (e) {
    if (e.defaultPrevented || e.button !== 0 || e.ctrlKey || e.shiftKey || e.metaKey) return;
    var a = e.target.closest && e.target.closest('a[href]');
    if (!a) return;
    var u;
    try { u = new URL(a.getAttribute('href'), location.href); } catch (_) { return; }
    var m = u.host === location.host && u.pathname.match(RID);
    if (!m) return;
    e.preventDefault();
    e.stopPropagation();
    document.documentElement.style.cursor = 'progress';
    reciterPath(m[1], m[2], a.textContent.trim()).then(function (p) {
      document.documentElement.style.cursor = '';
      goTo(p);
    });
  }, true);

  // A page the site's server failed on: explain it and offer a way out instead of a bare error.
  function isErrorPage() {
    if (document.getElementById('__qd_err')) return true;
    var t = document.body ? document.body.innerText.trim() : '';
    return /^(Internal Server Error|500[^\n]*Internal Server Error|Bad Gateway|Service Unavailable)\s*(أوفلاين)?$/i.test(t);
  }
  function handleErrorPage() {
    if (!isErrorPage() || document.getElementById('__qd_err')) return;
    var m = location.pathname.match(RID);
    if (m) {
      reciterPath(m[1], m[2], '').then(function (p) { goTo(p, true); });
      return;
    }
    var lang = (location.pathname.match(/^\/([a-z]{2})(\/|$)/) || [0, 'en'])[1];
    var box = document.createElement('div');
    box.id = '__qd_err';
    box.setAttribute('dir', 'rtl');
    box.setAttribute('style', 'position:fixed;inset:0;z-index:2147482000;display:grid;place-items:center;background:#000;color:#f5f5f7;font:15px/1.6 "Segoe UI",Tahoma,sans-serif;text-align:center');
    box.innerHTML = '<div style="max-width:440px;padding:24px"><div style="font-size:22px;font-weight:700;margin-bottom:8px">الصفحة دي مش شغّالة دلوقتي</div>' +
      '<div style="color:#9a9aa0;margin-bottom:20px">موقع quranify.pro رجّع خطأ للصفحة دي (Internal Server Error). المشكلة من الموقع نفسه، مش من جهازك ولا من البرنامج.</div>' +
      '<button id="__qd_back" style="height:40px;padding:0 20px;border-radius:999px;border:0;background:#fff;color:#000;font-weight:600;cursor:pointer;margin:0 4px">رجوع</button>' +
      '<button id="__qd_home" style="height:40px;padding:0 20px;border-radius:999px;border:1px solid rgba(255,255,255,.2);background:transparent;color:#fff;font-weight:600;cursor:pointer;margin:0 4px">الصفحة الرئيسية</button></div>';
    document.body.appendChild(box);
    document.getElementById('__qd_back').onclick = function () { if (history.length > 1) history.back(); else location.assign('/' + lang); };
    document.getElementById('__qd_home').onclick = function () { location.assign('/' + lang); };
  }
  document.addEventListener('DOMContentLoaded', handleErrorPage);
  setTimeout(handleErrorPage, 800);

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
