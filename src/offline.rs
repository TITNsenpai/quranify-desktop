//! Offline mode: a bundled player page served from `http://qoff.localhost/`, a
//! download queue for full-surah MP3s from mp3quran.net, and a small JSON bridge
//! between that page and Rust.

use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use serde::Deserialize;
use serde_json::{Value, json};
use tao::event_loop::EventLoopProxy;

use crate::settings::{self, Settings};
use wry::{
    RequestAsyncResponder,
    http::{Request, Response, StatusCode, header},
};

pub const OFFLINE_URL: &str = "http://qoff.localhost/";
const PAGE: &str = include_str!("../ui/offline.html");
/// Amiri Quran (SIL Open Font License), Arabic subset.
const QURAN_FONT: &[u8] = include_bytes!("../ui/fonts/AmiriQuran-arabic.woff2");
const TEXT_API: &str = "https://api.alquran.cloud/v1/surah/";
const TIMING_READS: &str = "https://www.mp3quran.net/api/v3/ayat_timing/reads";
const TIMING_API: &str = "https://www.mp3quran.net/api/v3/ayat_timing";
const CHUNK: u64 = 2 * 1024 * 1024;
const API: &str = "https://www.mp3quran.net/api/v3/reciters?language=";

/// Messages sent to the event loop from other threads.
pub enum UserEvent {
    /// JavaScript to run in the current page.
    Js(String),
    GoOffline,
    GoOnline,
    /// Show a folder picker (starting at this folder) for the download location.
    PickFolder(PathBuf),
    /// Load this URL in the main window (internal links that asked for a new tab).
    Navigate(String),
    /// WebView2 has shut down (or the shutdown timed out): quit the app.
    Exit,
    /// Player state (JSON) reported by the page, for the tray tooltip and mini player.
    State(String),
    /// Bring the main window back (second launch, tray click, mini player).
    ShowMain,
    /// Tray menu item clicked (its id).
    Tray(String),
    /// Global shortcut pressed (its id).
    Hotkey(u32),
    /// Message from the mini player page.
    Mini(String),
    /// Hidden for a while: put the page to sleep if nothing is playing (generation check).
    TrySleep(u64),
    /// Start a sleep timer (minutes; 0 cancels).
    SleepTimer(u32),
    /// A sleep timer ran out (its generation).
    SleepFire(u64),
    /// Lock/unlock, sleep, audio output change or a call (see watch.rs).
    System(crate::watch::Sys),
}

/// Remember the last quranify.pro page so the app can reopen it.
pub fn save_last_url(data_dir: &Path, url: &str) {
    if url.starts_with("https://quranify.pro/") && !url.contains("/__/auth") {
        let _ = fs::write(data_dir.join("last_url.txt"), url);
    }
}

pub fn load_last_url(data_dir: &Path) -> Option<String> {
    fs::read_to_string(data_dir.join("last_url.txt"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|u| u.starts_with("https://quranify.pro/") && !u.contains("/__/auth"))
}

/// The UI thread; set once at startup.
pub static MAIN_THREAD: std::sync::OnceLock<thread::ThreadId> = std::sync::OnceLock::new();

/// Send an event to the event loop. From the UI thread (inside WebView2 callbacks) a direct
/// send can sit unprocessed until the next window message, so hand it to a helper thread.
pub fn post(proxy: &EventLoopProxy<UserEvent>, event: UserEvent) {
    if MAIN_THREAD.get() == Some(&thread::current().id()) {
        let proxy = proxy.clone();
        thread::spawn(move || {
            let _ = proxy.send_event(event);
        });
    } else {
        let _ = proxy.send_event(event);
    }
}

/// Deliver an event to the offline page (a no-op on any other page).
fn emit(proxy: &EventLoopProxy<UserEvent>, data: Value) {
    post(proxy, UserEvent::Js(format!("window.__qo&&window.__qo.on({data})")));
}

/// Is quranify.pro reachable right now? Blocks for up to ~3 s.
pub fn online() -> bool {
    let Ok(addrs) = ("quranify.pro", 443).to_socket_addrs() else {
        return false;
    };
    addrs
        .into_iter()
        .take(2)
        .any(|a| TcpStream::connect_timeout(&a, Duration::from_millis(2500)).is_ok())
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() < 12 && key.bytes().all(|b| b.is_ascii_digit())
}

fn valid_server(server: &str) -> bool {
    let host = server
        .strip_prefix("https://")
        .and_then(|s| s.split('/').next())
        .unwrap_or("");
    host == "mp3quran.net" || host.ends_with(".mp3quran.net")
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .http_status_as_error(false)
        .user_agent("QuranifyDesktop/0.2")
        .build()
        .into()
}

// ───────────────────────────── catalog ─────────────────────────────

fn fetch_json(agent: &ureq::Agent, url: &str) -> Result<Value, String> {
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status()));
    }
    let text = resp
        .body_mut()
        .with_config()
        .limit(20 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// Compact reciter catalog: `[{id, ar, en, m:[{id, ar, en, server, list:[n..]}]}]`.
fn download_catalog() -> Result<Value, String> {
    let agent = agent();
    let ar = fetch_json(&agent, &format!("{API}ar"))?;
    let en = fetch_json(&agent, &format!("{API}eng")).unwrap_or(Value::Null);

    let mut en_names: HashMap<i64, &str> = HashMap::new();
    let mut en_moshaf: HashMap<i64, &str> = HashMap::new();
    for r in en["reciters"].as_array().into_iter().flatten() {
        en_names.insert(r["id"].as_i64().unwrap_or(-1), r["name"].as_str().unwrap_or(""));
        for m in r["moshaf"].as_array().into_iter().flatten() {
            en_moshaf.insert(m["id"].as_i64().unwrap_or(-1), m["name"].as_str().unwrap_or(""));
        }
    }

    // Recitations that have verse-by-verse timings (matched by their audio folder).
    let mut timing_reads: HashMap<String, i64> = HashMap::new();
    if let Ok(reads) = fetch_json(&agent, TIMING_READS) {
        for r in reads.as_array().into_iter().flatten() {
            if let (Some(url), Some(id)) = (r["folder_url"].as_str(), r["id"].as_i64()) {
                timing_reads.insert(url.trim_end_matches('/').to_string(), id);
            }
        }
    }

    let mut out = Vec::new();
    for r in ar["reciters"].as_array().ok_or("bad catalog")? {
        let rid = r["id"].as_i64().unwrap_or(-1);
        let mut moshafs = Vec::new();
        for m in r["moshaf"].as_array().into_iter().flatten() {
            let mid = m["id"].as_i64().unwrap_or(-1);
            let mut server = m["server"].as_str().unwrap_or("").to_string();
            if !valid_server(&server) {
                continue;
            }
            if !server.ends_with('/') {
                server.push('/');
            }
            let list: Vec<u16> = m["surah_list"]
                .as_str()
                .unwrap_or("")
                .split(',')
                .filter_map(|s| s.trim().parse().ok())
                .filter(|n| (1..=114).contains(n))
                .collect();
            if list.is_empty() {
                continue;
            }
            let timing = timing_reads.get(server.trim_end_matches('/')).copied();
            moshafs.push(json!({
                "id": mid,
                "ar": m["name"].as_str().unwrap_or(""),
                "en": en_moshaf.get(&mid).copied().unwrap_or(""),
                "server": server,
                "list": list,
                "t": timing,
            }));
        }
        if moshafs.is_empty() {
            continue;
        }
        out.push(json!({
            "id": rid,
            "ar": r["name"].as_str().unwrap_or("").trim(),
            "en": en_names.get(&rid).copied().unwrap_or("").trim(),
            "m": moshafs,
        }));
    }
    Ok(Value::Array(out))
}

fn load_catalog(dir: &Path, refresh: bool) -> Result<Value, String> {
    let cache = dir.join("catalog2.json");
    let cached = fs::read_to_string(&cache)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if !refresh {
        if let Some(v) = cached {
            return Ok(v);
        }
    }
    match download_catalog() {
        Ok(v) => {
            let _ = fs::write(&cache, v.to_string());
            Ok(v)
        }
        // Offline on first run of this version: fall back to the older cache file.
        Err(e) => cached
            .or_else(|| {
                fs::read_to_string(dir.join("catalog.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            })
            .ok_or(e),
    }
}

// ───────────────────────────── library ─────────────────────────────

fn library(dir: &Path) -> Value {
    let mut items = Vec::new();
    let mut total: u64 = 0;
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let key = entry.file_name().to_string_lossy().to_string();
        if !path.is_dir() || !valid_key(&key) {
            continue;
        }
        let Some(meta) = fs::read_to_string(path.join("meta.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        else {
            continue;
        };
        let mut surahs = Vec::new();
        let mut bytes = 0;
        for f in fs::read_dir(&path).into_iter().flatten().flatten() {
            let name = f.file_name().to_string_lossy().to_string();
            if let Some(n) = name.strip_suffix(".mp3").and_then(|s| s.parse::<u16>().ok()) {
                surahs.push(n);
                bytes += f.metadata().map(|m| m.len()).unwrap_or(0);
            }
        }
        surahs.sort_unstable();
        total += bytes;
        items.push(json!({ "key": key, "meta": meta, "surahs": surahs, "bytes": bytes }));
    }
    json!({ "type": "library", "items": items, "total": total })
}

// ───────────────────────────── downloads ─────────────────────────────

#[derive(Clone, PartialEq, serde::Serialize, Deserialize)]
struct Job {
    key: String,
    surah: u16,
    server: String,
}

struct Queue {
    jobs: VecDeque<Job>,
    current: Option<Job>,
}

pub struct Offline {
    /// App data folder (catalog cache, default download location).
    data_dir: PathBuf,
    /// Where surahs are downloaded; can be changed in the settings screen.
    dir: RwLock<PathBuf>,
    moving: AtomicBool,
    settings_path: PathBuf,
    proxy: EventLoopProxy<UserEvent>,
    queue: Mutex<Queue>,
    wake: Condvar,
    cancel: AtomicBool,
}

impl Offline {
    pub fn new(
        data_dir: PathBuf,
        settings_path: PathBuf,
        proxy: EventLoopProxy<UserEvent>,
    ) -> Arc<Self> {
        let default_dir = data_dir.join("offline");
        // Older builds kept the catalog cache inside the download folder.
        let old_catalog = default_dir.join("catalog.json");
        if old_catalog.exists() && !data_dir.join("catalog.json").exists() {
            let _ = fs::rename(&old_catalog, data_dir.join("catalog.json"));
        }
        let dir = Settings::load(&settings_path)
            .download_dir
            .map(PathBuf::from)
            .filter(|p| fs::create_dir_all(p).is_ok())
            .unwrap_or(default_dir);
        let _ = fs::create_dir_all(&dir);
        let this = Arc::new(Self {
            data_dir,
            dir: RwLock::new(dir),
            moving: AtomicBool::new(false),
            settings_path,
            proxy,
            queue: Mutex::new(Queue { jobs: VecDeque::new(), current: None }),
            wake: Condvar::new(),
            cancel: AtomicBool::new(false),
        });
        // Downloads that were still queued when the app closed carry on.
        if let Some(saved) = fs::read_to_string(this.data_dir.join("queue.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Vec<Job>>(&t).ok())
        {
            let mut q = this.queue.lock().unwrap();
            for job in saved {
                if valid_key(&job.key) && valid_server(&job.server) && (1..=114).contains(&job.surah) {
                    q.jobs.push_back(job);
                }
            }
        }
        let worker = this.clone();
        thread::spawn(move || worker.run());
        this
    }

    fn save_queue(&self) {
        let q = self.queue.lock().unwrap();
        let jobs: Vec<&Job> = q.current.iter().chain(q.jobs.iter()).collect();
        let text = serde_json::to_string(&jobs).unwrap_or_else(|_| "[]".into());
        drop(q);
        let _ = fs::write(self.data_dir.join("queue.json"), text);
    }

    fn dir(&self) -> PathBuf {
        self.dir.read().unwrap().clone()
    }

    pub fn default_dir(&self) -> PathBuf {
        self.data_dir.join("offline")
    }

    /// Move every downloaded reciter folder to `new` and download there from now on.
    pub fn change_dir(self: &Arc<Self>, new: PathBuf) {
        let old = self.dir();
        if new == old {
            emit(&self.proxy, json!({ "type": "toast", "message": "ده نفس المكان الحالي" }));
            return;
        }
        if self.moving.swap(true, Ordering::SeqCst) {
            return;
        }
        self.cancel(None, None);
        emit(&self.proxy, json!({ "type": "moving", "on": true }));
        let this = self.clone();
        thread::spawn(move || {
            // Let a cancelled download stop writing first.
            thread::sleep(Duration::from_millis(500));
            let result = move_downloads(&old, &new);
            let message = match &result {
                Ok(0) => "اتغيّر مكان التحميلات".to_string(),
                Ok(n) => format!("اتنقلت التحميلات ({n} قارئ) للمكان الجديد"),
                Err(e) => format!("تعذّر النقل، المكان القديم زي ما هو: {e}"),
            };
            if result.is_ok() {
                *this.dir.write().unwrap() = new.clone();
                let mut s = Settings::load(&this.settings_path);
                s.download_dir = (new != this.default_dir()).then(|| new.display().to_string());
                s.save(&this.settings_path);
            }
            this.moving.store(false, Ordering::SeqCst);
            emit(&this.proxy, json!({ "type": "moving", "on": false }));
            emit(&this.proxy, json!({ "type": "toast", "message": message }));
            emit(&this.proxy, library(&this.dir()));
            this.emit_settings(None);
        });
    }

    fn emit_queue(&self) {
        self.save_queue();
        let q = self.queue.lock().unwrap();
        let pending: Vec<Value> = q.jobs.iter().map(|j| json!([j.key, j.surah])).collect();
        let current = q.current.as_ref().map(|j| json!([j.key, j.surah]));
        drop(q);
        emit(&self.proxy, json!({ "type": "queue", "pending": pending, "current": current }));
    }

    fn run(self: Arc<Self>) {
        let agent = agent();
        loop {
            let job = {
                let mut q = self.queue.lock().unwrap();
                while q.jobs.is_empty() {
                    q = self.wake.wait(q).unwrap();
                }
                let job = q.jobs.pop_front().unwrap();
                q.current = Some(job.clone());
                self.cancel.store(false, Ordering::SeqCst);
                job
            };
            self.emit_queue();
            let result = self.download(&agent, &job);
            if let Err(e) = &result {
                if e != "cancelled" && !online() {
                    // No internet: keep the job and try again once the connection is back.
                    {
                        let mut q = self.queue.lock().unwrap();
                        q.current = None;
                        q.jobs.push_front(job.clone());
                    }
                    emit(&self.proxy, json!({ "type": "queueWaiting" }));
                    self.emit_queue();
                    while !online() {
                        thread::sleep(Duration::from_secs(15));
                    }
                    continue;
                }
            }
            self.queue.lock().unwrap().current = None;
            match result {
                Ok(()) => emit(&self.proxy, json!({ "type": "done", "key": job.key, "surah": job.surah })),
                Err(e) if e == "cancelled" => {}
                Err(e) => emit(
                    &self.proxy,
                    json!({ "type": "error", "key": job.key, "surah": job.surah, "message": e }),
                ),
            }
            self.emit_queue();
        }
    }

    fn download(&self, agent: &ureq::Agent, job: &Job) -> Result<(), String> {
        let folder = self.dir().join(&job.key);
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        let target = folder.join(format!("{:03}.mp3", job.surah));
        if target.exists() {
            return Ok(());
        }
        let part = folder.join(format!("{:03}.mp3.part", job.surah));
        let url = format!("{}{:03}.mp3", job.server, job.surah);

        // Resume a partial download when possible.
        let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
        let mut req = agent.get(&url);
        if have > 0 {
            req = req.header("Range", format!("bytes={have}-"));
        }
        let mut resp = req.call().map_err(|e| e.to_string())?;
        let status = resp.status().as_u16();
        let (mut file, mut got) = match status {
            206 => (
                fs::OpenOptions::new().append(true).open(&part).map_err(|e| e.to_string())?,
                have,
            ),
            200 => (File::create(&part).map_err(|e| e.to_string())?, 0),
            _ => return Err(format!("HTTP {status}")),
        };
        let len: u64 = resp
            .headers()
            .get("content-length")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let total = if len > 0 { got + len } else { 0 };

        let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
        let mut buf = vec![0u8; 64 * 1024];
        let mut last = Instant::now();
        loop {
            if self.cancel.load(Ordering::SeqCst) {
                return Err("cancelled".into());
            }
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            got += n as u64;
            if last.elapsed() >= Duration::from_millis(250) {
                last = Instant::now();
                emit(
                    &self.proxy,
                    json!({ "type": "progress", "key": job.key, "surah": job.surah, "got": got, "total": total }),
                );
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        if total > 0 && got < total {
            return Err("انقطع التحميل".into());
        }
        fs::rename(&part, &target).map_err(|e| e.to_string())
    }

    fn enqueue(&self, meta: &Value, surahs: &[u16]) -> Result<(), String> {
        let key = meta["key"].as_str().unwrap_or("").to_string();
        let server = meta["server"].as_str().unwrap_or("").to_string();
        if !valid_key(&key) || !valid_server(&server) || !server.ends_with('/') {
            return Err("invalid download request".into());
        }
        if self.moving.load(Ordering::SeqCst) {
            return Err("استنى لحد ما نقل التحميلات يخلص".into());
        }
        let folder = self.dir().join(&key);
        fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
        fs::write(folder.join("meta.json"), meta.to_string()).map_err(|e| e.to_string())?;
        {
            let mut q = self.queue.lock().unwrap();
            for &surah in surahs {
                if !(1..=114).contains(&surah) || folder.join(format!("{surah:03}.mp3")).exists() {
                    continue;
                }
                let job = Job { key: key.clone(), surah, server: server.clone() };
                if !q.jobs.contains(&job) && q.current.as_ref() != Some(&job) {
                    q.jobs.push_back(job);
                }
            }
        }
        self.wake.notify_one();
        self.emit_queue();
        Ok(())
    }

    /// Cancel queued and running downloads; `key`/`surah` narrow it down.
    fn cancel(&self, key: Option<&str>, surah: Option<u16>) {
        let matches = |j: &Job| {
            key.is_none_or(|k| j.key == k) && surah.is_none_or(|s| j.surah == s)
        };
        let mut q = self.queue.lock().unwrap();
        q.jobs.retain(|j| !matches(j));
        if q.current.as_ref().is_some_and(matches) {
            self.cancel.store(true, Ordering::SeqCst);
        }
        drop(q);
        self.emit_queue();
    }

    fn delete(&self, key: &str, surah: Option<u16>) {
        if !valid_key(key) {
            return;
        }
        self.cancel(Some(key), surah);
        let folder = self.dir().join(key);
        match surah {
            Some(n) => {
                let _ = fs::remove_file(folder.join(format!("{n:03}.mp3")));
                let _ = fs::remove_file(folder.join(format!("{n:03}.mp3.part")));
            }
            None => {
                // Give a running download a moment to stop writing.
                thread::sleep(Duration::from_millis(300));
                let _ = fs::remove_dir_all(&folder);
            }
        }
    }

    // ───────────────────────────── page bridge ─────────────────────────────

    pub fn ipc(self: &Arc<Self>, body: &str) {
        #[derive(Deserialize)]
        struct Msg {
            cmd: String,
            #[serde(default)]
            meta: Value,
            #[serde(default)]
            key: Option<String>,
            #[serde(default)]
            surah: Option<u16>,
            #[serde(default)]
            surahs: Vec<u16>,
            #[serde(default)]
            refresh: bool,
            #[serde(default)]
            settings: Option<Settings>,
            #[serde(default)]
            autostart: Option<bool>,
            #[serde(default)]
            url: Option<String>,
            #[serde(default)]
            data: Value,
            #[serde(default)]
            read: Option<u32>,
            #[serde(default)]
            minutes: Option<u32>,
        }
        let Ok(msg) = serde_json::from_str::<Msg>(body) else {
            return;
        };
        let this = self.clone();
        match msg.cmd.as_str() {
            "goOffline" => {
                post(&self.proxy, UserEvent::GoOffline);
            }
            "goOnline" => {
                thread::spawn(move || {
                    if online() {
                        post(&this.proxy, UserEvent::GoOnline);
                    } else {
                        emit(&this.proxy, json!({ "type": "toast", "message": "مفيش اتصال بالإنترنت دلوقتي" }));
                    }
                });
            }
            "status" => {
                thread::spawn(move || {
                    emit(&this.proxy, json!({ "type": "status", "online": online() }));
                });
            }
            "catalog" => {
                thread::spawn(move || {
                    let data = match load_catalog(&this.data_dir, msg.refresh) {
                        Ok(v) => json!({ "type": "catalog", "reciters": v }),
                        Err(e) => json!({ "type": "catalog", "error": e }),
                    };
                    emit(&this.proxy, data);
                });
            }
            "library" => emit(&self.proxy, library(&self.dir())),
            "download" => {
                if let Err(e) = self.enqueue(&msg.meta, &msg.surahs) {
                    emit(&self.proxy, json!({ "type": "toast", "message": e }));
                }
            }
            "cancel" => self.cancel(msg.key.as_deref(), msg.surah),
            "delete" => {
                if let Some(key) = msg.key {
                    thread::spawn(move || {
                        this.delete(&key, msg.surah);
                        emit(&this.proxy, library(&this.dir()));
                    });
                }
            }
            "state" => post(&self.proxy, UserEvent::State(msg.data.to_string())),
            "showMain" => post(&self.proxy, UserEvent::ShowMain),
            "queueState" => self.emit_queue(),
            "toggleMini" => post(&self.proxy, UserEvent::Tray("mini".into())),
            "getUser" => {
                let data = fs::read_to_string(self.data_dir.join("user.json"))
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .unwrap_or(Value::Null);
                emit(&self.proxy, json!({ "type": "user", "data": data }));
            }
            "saveUser" => {
                if msg.data.is_object() {
                    let tmp = self.data_dir.join("user.json.tmp");
                    if fs::write(&tmp, msg.data.to_string()).is_ok() {
                        let _ = fs::rename(&tmp, self.data_dir.join("user.json"));
                    }
                }
            }
            "text" => {
                if let Some(n) = msg.surah.filter(|n| (1..=114).contains(n)) {
                    thread::spawn(move || {
                        let data = match surah_text(&this.data_dir, n) {
                            Ok(ayahs) => json!({ "type": "text", "surah": n, "ayahs": ayahs }),
                            Err(e) => json!({ "type": "text", "surah": n, "error": e }),
                        };
                        emit(&this.proxy, data);
                    });
                }
            }
            "timing" => {
                if let (Some(read), Some(n)) = (msg.read, msg.surah.filter(|n| (1..=114).contains(n))) {
                    thread::spawn(move || {
                        let data = match surah_timing(&this.data_dir, read, n) {
                            Ok(t) => json!({ "type": "timing", "read": read, "surah": n, "t": t }),
                            Err(e) => json!({ "type": "timing", "read": read, "surah": n, "error": e }),
                        };
                        emit(&this.proxy, data);
                    });
                }
            }
            "sleepTimer" => post(&self.proxy, UserEvent::SleepTimer(msg.minutes.unwrap_or(0))),
            "textAll" => {
                thread::spawn(move || {
                    let mut failed = 0;
                    for n in 1..=114u16 {
                        if surah_text(&this.data_dir, n).is_err() {
                            failed += 1;
                        }
                        emit(&this.proxy, json!({ "type": "textProgress", "done": n, "failed": failed }));
                    }
                });
            }
            "textStatus" => {
                let have = (1..=114u16).filter(|n| text_path(&self.data_dir, *n).exists()).count();
                emit(&self.proxy, json!({ "type": "textStatus", "have": have }));
            }
            "route" => {
                if let Some(url) = msg.url {
                    save_last_url(&self.data_dir, &url);
                }
            }
            "getSettings" => self.emit_settings(None),
            "testReminder" => {
                if !crate::remind::test(&self.data_dir) {
                    emit(&self.proxy, json!({ "type": "toast", "message": "تعذّر إظهار الإشعار" }));
                }
            }
            "setSettings" => {
                let mut error = None;
                if let Some(mut s) = msg.settings {
                    // The download folder is only changed through change_dir().
                    s.download_dir = Settings::load(&self.settings_path).download_dir;
                    s.save(&self.settings_path);
                }
                if let Some(on) = msg.autostart {
                    if let Err(e) = settings::set_autostart(on) {
                        error = Some(format!("تعذّر تغيير التشغيل التلقائي: {e}"));
                    }
                }
                self.emit_settings(error);
            }
            "openFolder" => {
                let _ = std::process::Command::new("explorer").arg(self.dir()).spawn();
            }
            "pickFolder" => {
                post(&self.proxy, UserEvent::PickFolder(self.dir()));
            }
            "resetFolder" => {
                let dir = self.default_dir();
                self.change_dir(dir);
            }
            _ => {}
        }
    }

    fn emit_settings(&self, error: Option<String>) {
        let s = Settings::load(&self.settings_path);
        emit(
            &self.proxy,
            json!({
                "type": "settings",
                "settings": s,
                "autostart": settings::autostart_enabled(),
                "downloadDir": self.dir().display().to_string(),
                "isDefaultDir": self.dir() == self.default_dir(),
                "error": error,
            }),
        );
    }

    // ───────────────────────────── page + audio server ─────────────────────────────

    pub fn serve(self: &Arc<Self>, req: Request<Vec<u8>>, responder: RequestAsyncResponder) {
        let dir = self.dir();
        thread::spawn(move || responder.respond(handle(&dir, &req)));
    }
}

/// Move the numbered reciter folders from `old` into `new`. Returns how many were moved.
fn move_downloads(old: &Path, new: &Path) -> std::io::Result<usize> {
    fs::create_dir_all(new)?;
    // Make sure we can actually write there before touching anything.
    let probe = new.join(".write-test");
    fs::write(&probe, b"ok")?;
    fs::remove_file(&probe)?;
    let mut moved = 0;
    for entry in fs::read_dir(old).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() && valid_key(&name) {
            move_dir(&entry.path(), &new.join(&name))?;
            moved += 1;
        }
    }
    Ok(moved)
}

/// Rename when possible (same drive), otherwise copy then delete.
fn move_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    if !dst.exists() && fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dst.join(entry.file_name());
        if entry.path().is_dir() {
            move_dir(&entry.path(), &target)?;
        } else if fs::rename(entry.path(), &target).is_err() {
            fs::copy(entry.path(), &target)?;
            fs::remove_file(entry.path())?;
        }
    }
    fs::remove_dir(src)
}

/// Verse timings `[[ayah, start_ms, end_ms], ...]` for one recitation, cached on disk.
fn surah_timing(data_dir: &Path, read: u32, n: u16) -> Result<Value, String> {
    let path = data_dir.join("timing").join(read.to_string()).join(format!("{n:03}.json"));
    if let Some(v) = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()) {
        return Ok(v);
    }
    let raw = fetch_json(&agent(), &format!("{TIMING_API}?surah={n}&read={read}"))?;
    let rows: Vec<Value> = raw
        .as_array()
        .ok_or("no timings")?
        .iter()
        .map(|a| json!([a["ayah"].as_i64().unwrap_or(0), a["start_time"].as_i64().unwrap_or(0), a["end_time"].as_i64().unwrap_or(0)]))
        .collect();
    if rows.is_empty() {
        return Err("no timings".into());
    }
    let v = Value::Array(rows);
    let _ = fs::create_dir_all(path.parent().unwrap());
    let _ = fs::write(&path, v.to_string());
    Ok(v)
}

fn text_path(data_dir: &Path, n: u16) -> PathBuf {
    data_dir.join("text").join(format!("{n:03}.json"))
}

/// Verse 1 of most surahs comes with the Basmala in front (its four words); drop them.
/// The spelling of the diacritics varies, so match on the first word only.
fn strip_basmala(text: &str) -> Option<String> {
    let words: Vec<&str> = text.split_whitespace().collect();
    (words.len() > 4 && words[0].starts_with("بِسْ")).then(|| words[4..].join(" "))
}

/// `[[uthmani, tafsir (Muyassar), English (Sahih Intl)], ...]`, cached on disk.
fn surah_text(data_dir: &Path, n: u16) -> Result<Value, String> {
    let path = text_path(data_dir, n);
    if let Some(mut v) = fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
        // Fix caches written before the Basmala was split off correctly.
        if n != 1 {
            if let Some(first) = v.get_mut(0).and_then(|a| a.get_mut(0)) {
                if let Some(fixed) = first.as_str().and_then(strip_basmala) {
                    *first = Value::String(fixed);
                    let _ = fs::write(&path, v.to_string());
                }
            }
        }
        return Ok(v);
    }
    let raw = fetch_json(
        &agent(),
        &format!("{TEXT_API}{n}/editions/quran-uthmani,ar.muyassar,en.sahih"),
    )?;
    let eds = raw["data"].as_array().ok_or("bad text response")?;
    if eds.len() < 3 {
        return Err("bad text response".into());
    }
    let ayahs = |i: usize| -> Vec<String> {
        eds[i]["ayahs"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|a| a["text"].as_str().unwrap_or("").to_string())
            .collect()
    };
    let (mut ar, tf, en) = (ayahs(0), ayahs(1), ayahs(2));
    // The API puts the Basmala inside verse 1; the page shows it as a header instead.
    if n != 1 {
        if let Some(first) = ar.first_mut() {
            if let Some(rest) = strip_basmala(first) {
                *first = rest;
            }
        }
    }
    let out: Vec<Value> = (0..ar.len())
        .map(|i| json!([ar[i], tf.get(i).cloned().unwrap_or_default(), en.get(i).cloned().unwrap_or_default()]))
        .collect();
    let v = Value::Array(out);
    let _ = fs::create_dir_all(path.parent().unwrap());
    let _ = fs::write(&path, v.to_string());
    Ok(v)
}

fn respond(status: StatusCode, mime: &str, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Cow::Owned(body))
        .unwrap()
}

fn handle(dir: &Path, req: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let path = req.uri().path();
    if path == "/font/quran.woff2" {
        return Response::builder()
            .header(header::CONTENT_TYPE, "font/woff2")
            .header(header::CACHE_CONTROL, "max-age=31536000")
            .body(Cow::Borrowed(&QURAN_FONT[..]))
            .unwrap();
    }
    if path == "/" || path == "/index.html" {
        return Response::builder()
            .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
            .header(header::CACHE_CONTROL, "no-store")
            .body(Cow::Borrowed(PAGE.as_bytes()))
            .unwrap();
    }

    // /audio/{key}/{n}.mp3
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    let file = match parts.as_slice() {
        ["audio", key, name] if valid_key(key) => name
            .strip_suffix(".mp3")
            .and_then(|n| n.parse::<u16>().ok())
            .filter(|n| (1..=114).contains(n))
            .map(|n| dir.join(key).join(format!("{n:03}.mp3"))),
        _ => None,
    };
    let Some(file) = file else {
        return respond(StatusCode::NOT_FOUND, "text/plain", b"not found".to_vec());
    };
    let Ok(mut f) = File::open(&file) else {
        return respond(StatusCode::NOT_FOUND, "text/plain", b"not downloaded".to_vec());
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);

    let range = req
        .headers()
        .get(header::RANGE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("bytes="))
        .and_then(|v| {
            let (a, b) = v.split_once('-')?;
            let start: u64 = a.trim().parse().ok()?;
            let end: Option<u64> = b.trim().parse().ok();
            Some((start, end))
        });

    let (status, start, end) = match range {
        Some((start, _)) if start >= len => {
            return Response::builder()
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{len}"))
                .body(Cow::Borrowed(&[][..]))
                .unwrap();
        }
        Some((start, end)) => {
            let end = end.unwrap_or(len - 1).min(len - 1).min(start + CHUNK - 1);
            (StatusCode::PARTIAL_CONTENT, start, end)
        }
        None if len == 0 => (StatusCode::OK, 0, 0),
        None => (StatusCode::OK, 0, len - 1),
    };

    let size = if len == 0 { 0 } else { end - start + 1 };
    let mut body = vec![0u8; size as usize];
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_exact(&mut body).is_err() {
        return respond(StatusCode::INTERNAL_SERVER_ERROR, "text/plain", b"read error".to_vec());
    }

    let mut resp = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "audio/mpeg")
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CONTENT_LENGTH, size.to_string());
    if status == StatusCode::PARTIAL_CONTENT {
        resp = resp.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"));
    }
    resp.body(Cow::Owned(body)).unwrap()
}
