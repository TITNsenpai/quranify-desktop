// Hide the console window in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod offline;
mod remind;
mod settings;
mod shortcuts;
mod single;
mod uninstall;
mod watch;

use std::{fs, path::PathBuf, process::Command};

use offline::{OFFLINE_URL, Offline, UserEvent, load_last_url, save_last_url};
use settings::{AUTOSTART_ARG, Settings};

use tao::{
    dpi::{LogicalSize, PhysicalPosition, PhysicalSize},
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget},
    platform::windows::{IconExtWindows, WindowBuilderExtWindows},
    window::{Icon, Theme, Window, WindowBuilder},
};
use webview2_com::{
    Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW, COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL,
        COREWEBVIEW2_WEB_ERROR_STATUS, COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT,
        COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED,
        COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET, COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED,
        COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED,
        COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE, COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT,
        ICoreWebView2_2, ICoreWebView2_3, ICoreWebView2_19, ICoreWebView2Environment5,
    },
    BrowserProcessExitedEventHandler, NavigationCompletedEventHandler, TrySuspendCompletedHandler,
};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
    hotkey::{Code, HotKey, Modifiers},
};
use tray_icon::{
    Icon as TrayImage, MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu},
};
use windows_core::Interface;
use wry::{
    NewWindowResponse, WebContext, WebView, WebViewBuilder, WebViewBuilderExtWindows,
    WebViewExtWindows,
};

/// While minimized: stop painting (pauses the site's animations and frees GPU work)
/// and ask WebView2 to trim memory. Audio keeps playing.
fn set_background_mode(webview: &WebView, background: bool) {
    let _ = webview.set_visible(!background);
    unsafe {
        if let Ok(core) = webview.controller().CoreWebView2() {
            if let Ok(core19) = core.cast::<ICoreWebView2_19>() {
                let level = if background {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_LOW
                } else {
                    COREWEBVIEW2_MEMORY_USAGE_TARGET_LEVEL_NORMAL
                };
                let _ = core19.SetMemoryUsageTargetLevel(level);
            }
        }
    }
}

/// When loading the site fails because there is no connection, switch to offline mode.
fn watch_navigation_errors(webview: &WebView, proxy: EventLoopProxy<UserEvent>) {
    let handler = NavigationCompletedEventHandler::create(Box::new(move |_sender, args| {
        let Some(args) = args else { return Ok(()) };
        let mut ok = Default::default();
        let mut status = COREWEBVIEW2_WEB_ERROR_STATUS::default();
        unsafe {
            args.IsSuccess(&mut ok)?;
            args.WebErrorStatus(&mut status)?;
        }
        let no_network = [
            COREWEBVIEW2_WEB_ERROR_STATUS_CANNOT_CONNECT,
            COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_ABORTED,
            COREWEBVIEW2_WEB_ERROR_STATUS_CONNECTION_RESET,
            COREWEBVIEW2_WEB_ERROR_STATUS_DISCONNECTED,
            COREWEBVIEW2_WEB_ERROR_STATUS_HOST_NAME_NOT_RESOLVED,
            COREWEBVIEW2_WEB_ERROR_STATUS_SERVER_UNREACHABLE,
            COREWEBVIEW2_WEB_ERROR_STATUS_TIMEOUT,
        ];
        if !ok.as_bool() && no_network.contains(&status) {
            offline::post(&proxy, UserEvent::GoOffline);
        }
        Ok(())
    }));
    unsafe {
        if let Ok(core) = webview.controller().CoreWebView2() {
            let mut token = Default::default();
            let _ = core.add_NavigationCompleted(&handler, &mut token);
        }
    }
}

const START_URL: &str = "https://quranify.pro/en";
/// Name of the folder created inside a download location the user picks.
const DL_FOLDER_NAME: &str = "Quranify Offline";

/// Runs on every page. On quranify.pro it adds a small "offline" button, a Ctrl+O
/// shortcut, and switches to offline mode when the connection drops.
const SITE_SCRIPT: &str = include_str!("../ui/site.js");
const APP_DIR: &str = "QuranifyDesktop";

const BROWSER_ARGS: &str = concat!(
    // Lets the tray, global shortcuts and the mini player start playback even before
    // you have clicked in the page (checked: the site does not start audio by itself).
    "--autoplay-policy=no-user-gesture-required ",
    "--enable-features=HardwareMediaKeyHandling ",
    "--disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,",
    "msEdgeTranslate,msShoppingExp,msEdgeSidebarV2 ",
    "--disable-background-networking ",
    "--disable-component-update ",
    "--disable-sync ",
    "--no-first-run"
);

/// Close WebView2 cleanly and quit once its browser process has exited, so cookies and
/// site storage (including the sign-in token) are written to disk first.
fn begin_shutdown(webview: &WebView, proxy: &EventLoopProxy<UserEvent>) {
    unsafe {
        let controller = webview.controller();
        if let Ok(core) = controller.CoreWebView2() {
            if let Ok(env) = core.cast::<ICoreWebView2_2>().and_then(|c| c.Environment()) {
                if let Ok(env5) = env.cast::<ICoreWebView2Environment5>() {
                    let p = proxy.clone();
                    let handler = BrowserProcessExitedEventHandler::create(Box::new(move |_, _| {
                        offline::post(&p, UserEvent::Exit);
                        Ok(())
                    }));
                    let mut token = Default::default();
                    let _ = env5.add_BrowserProcessExited(&handler, &mut token);
                }
            }
        }
        let _ = controller.Close();
    }
    // Never hang on exit: quit anyway after a few seconds.
    let p = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let _ = p.send_event(UserEvent::Exit);
    });
}

/// Popups that must stay real popups (sign-in flows rely on window.opener).
fn is_auth_popup(url: &str) -> bool {
    let host = host_of(url);
    url.starts_with("about:")
        || url.contains("/__/auth/")
        || ["accounts.google.com", "appleid.apple.com", "firebaseapp.com", "googleapis.com"]
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

fn host_of(url: &str) -> String {
    url.split("://")
        .nth(1)
        .unwrap_or("")
        .split(['/', '?', '#', ':'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Hosts that stay inside the app window. Everything else opens in the default browser.
fn is_internal(url: &str) -> bool {
    let host = host_of(url);
    const ALLOWED: &[&str] = &[
        "quranify.pro",
        // Built-in offline player.
        "qoff.localhost",
        // Sign-in providers, so logging in works inside the app.
        "accounts.google.com",
        "appleid.apple.com",
        "firebaseapp.com",
        "googleapis.com",
    ];
    url.starts_with("about:")
        || url.starts_with("data:")
        || url.starts_with("blob:")
        || ALLOWED
            .iter()
            .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

fn open_external(url: &str) {
    if url.starts_with("http://") || url.starts_with("https://") || url.starts_with("mailto:") {
        let _ = Command::new("explorer").arg(url).spawn();
    }
}

/// A plain Windows message box (right-to-left, for Arabic text).
fn alert(text: &str, flags: u32) -> i32 {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_RIGHT, MB_RTLREADING, MB_SETFOREGROUND, MessageBoxW};
    let wide = |s: &str| s.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (t, c) = (wide(text), wide("Quranify"));
    unsafe { MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), flags | MB_RTLREADING | MB_RIGHT | MB_SETFOREGROUND) }
}

/// Release builds have no console, so an unexpected error would close the app silently.
/// Write what happened to crash.txt and tell the user where it is.
fn install_crash_report(dir: PathBuf) {
    std::panic::set_hook(Box::new(move |info| {
        let path = dir.join("crash.txt");
        let report = format!(
            "Quranify Desktop {}
{}

{}
",
            env!("CARGO_PKG_VERSION"),
            info,
            std::backtrace::Backtrace::force_capture()
        );
        let _ = fs::write(&path, report);
        alert(
            &format!(
                "حصلت مشكلة غير متوقعة والبرنامج هيقفل.

التفاصيل اتحفظت في:
{}

لو بعتّ الملف ده للمطوّر هيساعد في حل المشكلة.",
                path.display()
            ),
            windows_sys::Win32::UI::WindowsAndMessaging::MB_ICONERROR,
        );
    }));
}

/// Is the window's title bar on some connected screen? (A monitor may have been unplugged.)
fn on_some_screen(window: &Window) -> bool {
    let Ok(pos) = window.outer_position() else { return true };
    let size = window.outer_size();
    // A point in the title bar, near the middle.
    let (x, y) = (pos.x + size.width as i32 / 2, pos.y + 15);
    window.available_monitors().any(|m| {
        let (mp, ms) = (m.position(), m.size());
        x >= mp.x && x < mp.x + ms.width as i32 && y >= mp.y && y < mp.y + ms.height as i32
    })
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join(APP_DIR);
    let _ = fs::create_dir_all(&dir);
    dir
}

/// Saved window geometry: "width height x y maximized".
struct WinState {
    size: PhysicalSize<u32>,
    pos: PhysicalPosition<i32>,
    maximized: bool,
}

fn load_state(path: &PathBuf) -> Option<WinState> {
    let text = fs::read_to_string(path).ok()?;
    let v: Vec<i64> = text.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    if v.len() != 5 || v[0] < 400 || v[1] < 300 {
        return None;
    }
    Some(WinState {
        size: PhysicalSize::new(v[0] as u32, v[1] as u32),
        pos: PhysicalPosition::new(v[2] as i32, v[3] as i32),
        maximized: v[4] != 0,
    })
}

fn save_state(path: &PathBuf, w: &Window) {
    let maximized = w.is_maximized();
    let size = w.inner_size();
    let pos = w.outer_position().unwrap_or_default();
    if w.is_minimized() || size.width == 0 {
        return;
    }
    let _ = fs::write(
        path,
        format!("{} {} {} {} {}", size.width, size.height, pos.x, pos.y, maximized as u8),
    );
}

const MINI_HTML: &str = include_str!("../ui/mini.html");

/// Put the hidden page fully to sleep (frees memory, zero CPU). WebView2 refuses while
/// audio is playing, so it only happens when nothing is being listened to.
fn try_suspend(webview: &WebView) -> bool {
    unsafe {
        let Ok(core) = webview.controller().CoreWebView2() else { return false };
        let Ok(core3) = core.cast::<ICoreWebView2_3>() else { return false };
        let handler = TrySuspendCompletedHandler::create(Box::new(|_hr, _ok| Ok(())));
        core3.TrySuspend(&handler).is_ok()
    }
}

fn resume(webview: &WebView) {
    unsafe {
        if let Ok(core3) = webview.controller().CoreWebView2().and_then(|c| c.cast::<ICoreWebView2_3>()) {
            let _ = core3.Resume();
        }
    }
}

/// Ask for a sleep check in 30 seconds (ignored if the window came back meanwhile).
fn schedule_sleep(proxy: &EventLoopProxy<UserEvent>, generation: u64) {
    let p = proxy.clone();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(30));
        let _ = p.send_event(UserEvent::TrySleep(generation));
    });
}

/// Run a player command ("toggle", "next", "prev", "seek:<sec>") in whichever page is open.
fn control(webview: &WebView, action: &str) {
    let arg = serde_json::to_string(action).unwrap_or_default();
    let _ = webview.evaluate_script(&format!("window.__qctl&&window.__qctl({arg})"));
}

fn load_pos(path: &PathBuf) -> Option<PhysicalPosition<i32>> {
    let t = fs::read_to_string(path).ok()?;
    let v: Vec<i32> = t.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    (v.len() == 2).then(|| PhysicalPosition::new(v[0], v[1]))
}

struct Mini {
    window: Window,
    webview: WebView,
}

fn build_mini(
    target: &EventLoopWindowTarget<UserEvent>,
    context: &mut WebContext,
    proxy: &EventLoopProxy<UserEvent>,
    pos_file: &PathBuf,
    browser_args: &str,
) -> Option<Mini> {
    let mut b = WindowBuilder::new()
        .with_title("Quranify Mini")
        .with_decorations(false)
        .with_always_on_top(true)
        .with_resizable(false)
        .with_skip_taskbar(true)
        .with_theme(Some(Theme::Dark))
        .with_inner_size(LogicalSize::new(400.0, 96.0))
        .with_window_icon(Icon::from_resource(1, None).ok());
    if let Some(p) = load_pos(pos_file) {
        b = b.with_position(p);
    }
    let window = b.build(target).ok()?;
    if load_pos(pos_file).is_none() || !on_some_screen(&window) {
        // First time: bottom-right corner, above the taskbar.
        if let Some(m) = window.current_monitor() {
            let (ms, ws) = (m.size(), window.outer_size());
            window.set_outer_position(PhysicalPosition::new(
                m.position().x + ms.width as i32 - ws.width as i32 - 24,
                m.position().y + ms.height as i32 - ws.height as i32 - 72,
            ));
        }
    }
    let p = proxy.clone();
    let webview = WebViewBuilder::new_with_web_context(context)
        .with_html(MINI_HTML)
        // Must match the main webview's options: both share one WebView2 profile.
        .with_additional_browser_args(browser_args)
        .with_background_color((24, 24, 27, 255))
        .with_ipc_handler(move |req| offline::post(&p, UserEvent::Mini(req.body().clone())))
        .build(&window)
        .ok()?;
    Some(Mini { window, webview })
}

fn main() -> wry::Result<()> {
    let _ = offline::MAIN_THREAD.set(std::thread::current().id());
    let dir = data_dir();
    let state_file = dir.join("window.txt");
    let mini_file = dir.join("mini.txt");
    let settings_path = dir.join("settings.json");
    install_crash_report(dir.clone());
    let settings = Settings::load(&settings_path);
    let autostarted = std::env::args().any(|a| a == AUTOSTART_ARG);
    if std::env::args().any(|a| a == "--quit") {
        single::request_quit();
        return Ok(());
    }
    if std::env::args().any(|a| a == "--diag") {
        let report = format!("Quranify {}
{}", env!("CARGO_PKG_VERSION"), watch::diagnostics());
        let _ = fs::write(dir.join("diag.txt"), report);
        return Ok(());
    }
    if std::env::args().any(|a| a == "--uninstall") {
        uninstall::run(&dir);
        return Ok(());
    }

    // One copy only: a second launch just brings the running one to the front.
    if !single::acquire(autostarted) {
        return Ok(());
    }
    settings::refresh_autostart_path();

    let start_hidden = autostarted && settings.start_minimized;
    // With the tray icon, "start minimized" means waiting quietly in the tray.
    let start_in_tray = start_hidden && settings.close_to_tray;

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let mut builder = WindowBuilder::new()
        .with_title("Quranify")
        .with_theme(Some(Theme::Dark))
        .with_min_inner_size(LogicalSize::new(960.0, 600.0))
        .with_inner_size(LogicalSize::new(1280.0, 800.0))
        .with_window_icon(Icon::from_resource(1, None).ok())
        .with_visible(!start_hidden);

    match load_state(&state_file) {
        Some(s) => {
            builder = builder
                .with_inner_size(s.size)
                .with_position(s.pos)
                .with_maximized(s.maximized);
        }
        None => builder = builder.with_maximized(false),
    }
    let window = builder.build(&event_loop).expect("failed to create window");
    if load_state(&state_file).is_none() || !on_some_screen(&window) {
        // Centre on first launch.
        if let Some(m) = window.current_monitor() {
            let ms = m.size();
            let ws = window.outer_size();
            window.set_outer_position(PhysicalPosition::new(
                m.position().x + (ms.width as i32 - ws.width as i32) / 2,
                m.position().y + (ms.height as i32 - ws.height as i32) / 2,
            ));
        }
    }

    if start_hidden && !start_in_tray {
        // Started with Windows: sit minimized on the taskbar instead of popping up.
        window.set_minimized(true);
        window.set_visible(true);
    }

    // Persistent profile so sign-in, settings and the last reciter survive restarts.
    let mut context = WebContext::new(Some(dir.join("WebView2")));

    let mut args = String::from(BROWSER_ARGS);
    // Software rendering roughly halves memory for this site at the same CPU cost.
    // GPU acceleration can be turned on in the settings screen (or with QURANIFY_GPU=1).
    if !settings.gpu && std::env::var_os("QURANIFY_GPU").is_none() {
        args.push_str(" --disable-gpu");
    }
    // Optional extra Chromium flags, e.g. QURANIFY_ARGS="--disable-gpu".
    if let Ok(extra) = std::env::var("QURANIFY_ARGS") {
        args.push(' ');
        args.push_str(&extra);
    }

    // `Quranify.exe --offline` (or the start-page setting) opens straight into offline mode.
    let start_offline =
        std::env::args().any(|a| a == "--offline") || settings.start_page == "offline";
    let proxy = event_loop.create_proxy();
    single::listen(proxy.clone());
    watch::start(proxy.clone());
    let nav_proxy = proxy.clone();
    let exit_proxy = proxy.clone();
    let offline = Offline::new(dir.clone(), settings_path.clone(), proxy.clone());
    let ipc_offline = offline.clone();
    let proto_offline = offline.clone();
    // Reopen the page that was open last time.
    let site_url = load_last_url(&dir).unwrap_or_else(|| START_URL.to_string());

    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(if start_offline { OFFLINE_URL } else { site_url.as_str() })
        .with_initialization_script(SITE_SCRIPT)
        .with_ipc_handler(move |req| ipc_offline.ipc(req.body()))
        .with_asynchronous_custom_protocol("qoff".into(), move |_id, req, responder| {
            proto_offline.serve(req, responder)
        })
        .with_background_color((0, 0, 0, 255))
        .with_theme(wry::Theme::Dark)
        .with_devtools(cfg!(debug_assertions))
        .with_browser_accelerator_keys(true)
        .with_additional_browser_args(&args)
        .with_navigation_handler(|url| {
            if is_internal(&url) {
                true
            } else {
                open_external(&url);
                false
            }
        })
        .with_new_window_req_handler(move |url, _features| {
            if is_auth_popup(&url) {
                // Sign-in popups need window.opener, so let WebView2 open them.
                NewWindowResponse::Allow
            } else if is_internal(&url) {
                // Links like "Help Center" ask for a new tab: show them in the main window.
                offline::post(&nav_proxy, UserEvent::Navigate(url));
                NewWindowResponse::Deny
            } else {
                open_external(&url);
                NewWindowResponse::Deny
            }
        })
        .build(&window);
    let webview = match webview {
        Ok(w) => w,
        Err(e) => {
            // Almost always: the WebView2 runtime is missing or broken (some Windows 10 setups).
            let _ = fs::write(dir.join("crash.txt"), format!("WebView2 error: {e}
"));
            use windows_sys::Win32::UI::WindowsAndMessaging::{IDYES, MB_ICONWARNING, MB_YESNO};
            if alert(
                "البرنامج محتاج «Microsoft Edge WebView2 Runtime» علشان يشتغل، ومش لاقيه على جهازك أو فيه مشكلة.

ده برنامج مجاني من Microsoft وبيتسطّب في أقل من دقيقة. تحب تحمّله دلوقتي؟

بعد ما يتسطّب افتح البرنامج تاني.",
                MB_YESNO | MB_ICONWARNING,
            ) == IDYES
            {
                open_external("https://go.microsoft.com/fwlink/p/?LinkId=2124703");
            }
            return Ok(());
        }
    };

    watch_navigation_errors(&webview, proxy.clone());
    if start_in_tray {
        set_background_mode(&webview, true);
    }

    // ── tray icon ──
    let item_show = MenuItem::with_id("show", "إظهار البرنامج", true, None);
    let item_toggle = MenuItem::with_id("toggle", "تشغيل / إيقاف", true, None);
    let item_next = MenuItem::with_id("next", "السورة التالية", true, None);
    let item_prev = MenuItem::with_id("prev", "السورة السابقة", true, None);
    let item_mini = MenuItem::with_id("mini", "المشغّل الصغير", true, None);
    let item_offline = MenuItem::with_id("offline", "الأوفلاين والتحميلات", true, None);
    let item_khatma = MenuItem::with_id("khatma", "ورد الختمة", true, None);
    let item_quit = MenuItem::with_id("quit", "خروج", true, None);
    let sleep_items: Vec<MenuItem> = [15, 30, 45, 60, 90]
        .iter()
        .map(|m| MenuItem::with_id(format!("sleep{m}"), format!("بعد {m} دقيقة"), true, None))
        .collect();
    let sleep_cancel = MenuItem::with_id("sleep0", "إلغاء المؤقّت", true, None);
    let sleep_menu = Submenu::new("مؤقّت النوم", true);
    for it in &sleep_items {
        let _ = sleep_menu.append(it);
    }
    let _ = sleep_menu.append(&PredefinedMenuItem::separator());
    let _ = sleep_menu.append(&sleep_cancel);
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &item_show,
        &PredefinedMenuItem::separator(),
        &item_toggle,
        &item_next,
        &item_prev,
        &PredefinedMenuItem::separator(),
        &sleep_menu,
        &item_mini,
        &item_khatma,
        &item_offline,
        &PredefinedMenuItem::separator(),
        &item_quit,
    ]);
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .with_tooltip("Quranify")
        .with_icon(TrayImage::from_resource(1, None).expect("tray icon"))
        .build()
        .ok();
    if let Some(t) = &tray {
        remind::start(proxy.clone(), t.window_handle(), dir.clone(), settings_path.clone());
    }
    let p = proxy.clone();
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        offline::post(&p, UserEvent::Tray(e.id.0.clone()));
    }));
    let p = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            offline::post(&p, UserEvent::ShowMain);
        }
    }));

    // ── global shortcuts ──
    let hk_toggle = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::Space);
    let hk_next = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::ArrowRight);
    let hk_prev = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::ArrowLeft);
    let hk_mini = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyM);
    let hotkeys = GlobalHotKeyManager::new().ok();
    if let (Some(m), true) = (&hotkeys, settings.hotkeys) {
        for hk in [hk_toggle, hk_next, hk_prev, hk_mini] {
            let _ = m.register(hk);
        }
    }
    let p = proxy.clone();
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state == HotKeyState::Pressed {
            offline::post(&p, UserEvent::Hotkey(e.id));
        }
    }));

    let mut in_background = start_in_tray;
    let mut closing = false;
    let mut mini: Option<Mini> = None;
    let mut last_state = String::from("null");
    let mut sleeping = false;
    let mut missed_updates = false;
    let mut sleep_gen: u64 = 0;
    let mut timer_gen: u64 = 0;
    // Why we paused playback ourselves (so we know whether to resume it).
    let mut paused_by: Option<watch::Sys> = None;
    if start_in_tray {
        schedule_sleep(&proxy, sleep_gen);
    }
    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        // Keep these alive for the life of the loop.
        let _ = (&tray, &hotkeys, &context);

        // Anything that needs the page awake calls this first.
        let wake = |sleeping: &mut bool, missed: &mut bool| {
            if *sleeping {
                *sleeping = false;
                resume(&webview);
                if *missed {
                    *missed = false;
                    let _ = webview.evaluate_script("window.__qrefresh&&window.__qrefresh()");
                }
            }
        };
        let is_playing =
            |state: &str| state.contains("\"playing\":true") || state.contains("\"busy\":true");
        let mut timer: Option<u32> = None;

        let mut action: Option<&str> = None;
        let mut toggle_mini = false;
        let mut quit = false;
        let mut show = false;
        let mut go_offline_page = false;
        let mut go_khatma = false;

        match event {
            Event::WindowEvent { window_id, event: WindowEvent::CloseRequested, .. } => {
                if mini.as_ref().is_some_and(|m| m.window.id() == window_id) {
                    toggle_mini = true;
                } else if Settings::load(&settings_path).close_to_tray && tray.is_some() {
                    // Keep running in the tray; audio continues.
                    save_state(&state_file, &window);
                    window.set_visible(false);
                    if !in_background {
                        in_background = true;
                        set_background_mode(&webview, true);
                        sleep_gen += 1;
                        schedule_sleep(&proxy, sleep_gen);
                    }
                } else {
                    quit = true;
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::Resized(_), .. }
                if window_id == window.id() =>
            {
                let hidden = window.is_minimized() || !window.is_visible();
                if hidden != in_background {
                    in_background = hidden;
                    sleep_gen += 1;
                    if hidden {
                        set_background_mode(&webview, true);
                        schedule_sleep(&proxy, sleep_gen);
                    } else {
                        wake(&mut sleeping, &mut missed_updates);
                        set_background_mode(&webview, false);
                    }
                }
            }
            Event::WindowEvent { window_id, event: WindowEvent::Moved(pos), .. }
                if mini.as_ref().is_some_and(|m| m.window.id() == window_id) =>
            {
                let _ = fs::write(&mini_file, format!("{} {}", pos.x, pos.y));
            }
            Event::UserEvent(UserEvent::Exit) => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::TrySleep(generation)) => {
                if generation == sleep_gen
                    && in_background
                    && !sleeping
                    && mini.is_none()
                    && !is_playing(&last_state)
                {
                    sleeping = try_suspend(&webview);
                }
            }
            Event::UserEvent(UserEvent::Navigate(url)) => {
                wake(&mut sleeping, &mut missed_updates);
                let _ = webview.load_url(&url);
            }
            Event::UserEvent(UserEvent::Js(script)) => {
                if sleeping {
                    // Don't wake the page for download progress; it refreshes on wake.
                    missed_updates = true;
                } else {
                    let _ = webview.evaluate_script(&script);
                }
            }
            Event::UserEvent(UserEvent::GoOffline) => {
                wake(&mut sleeping, &mut missed_updates);
                let on_offline_page = webview
                    .url()
                    .map(|u| u.starts_with(OFFLINE_URL))
                    .unwrap_or(false);
                if !on_offline_page {
                    let _ = webview.load_url(OFFLINE_URL);
                }
            }
            Event::UserEvent(UserEvent::PickFolder(current)) => {
                if let Some(folder) = rfd::FileDialog::new()
                    .set_title("اختار مكان تحميل السور")
                    .set_directory(&current)
                    .set_parent(&window)
                    .pick_folder()
                {
                    // Keep our files together in their own subfolder.
                    let target = if folder.ends_with(DL_FOLDER_NAME) {
                        folder
                    } else {
                        folder.join(DL_FOLDER_NAME)
                    };
                    offline.change_dir(target);
                }
            }
            Event::UserEvent(UserEvent::GoOnline) => {
                let url = load_last_url(&dir).unwrap_or_else(|| START_URL.to_string());
                let _ = webview.load_url(&url);
            }
            Event::UserEvent(UserEvent::ShowMain) => show = true,
            Event::UserEvent(UserEvent::State(json)) => {
                if json != last_state {
                    last_state = json;
                    let v: serde_json::Value =
                        serde_json::from_str(&last_state).unwrap_or_default();
                    let title = v["title"].as_str().unwrap_or("");
                    let sub = v["sub"].as_str().unwrap_or("");
                    let playing = v["playing"].as_bool().unwrap_or(false);
                    if let Some(t) = &tray {
                        let tip = if title.is_empty() {
                            "Quranify".to_string()
                        } else {
                            format!("{} {} — {}", if playing { "▶" } else { "⏸" }, title, sub)
                        };
                        let tip: String = tip.chars().take(120).collect();
                        let _ = t.set_tooltip(Some(tip));
                    }
                    item_toggle.set_text(if playing { "إيقاف مؤقت" } else { "تشغيل" });
                    if playing {
                        // You pressed play yourself: we no longer own the pause.
                        paused_by = None;
                    }
                    watch::ACTIVE.store(
                        is_playing(&last_state) || paused_by.is_some(),
                        std::sync::atomic::Ordering::Relaxed,
                    );
                    if let Some(m) = &mini {
                        let _ = m.webview.evaluate_script(&format!("window.__m&&window.__m({last_state})"));
                    }
                }
            }
            Event::UserEvent(UserEvent::System(sys)) => {
                use watch::Sys;
                let st = Settings::load(&settings_path);
                let playing = is_playing(&last_state);
                let mut do_pause = |why: Option<Sys>, note: &str| {
                    wake(&mut sleeping, &mut missed_updates);
                    control(&webview, "pause");
                    paused_by = why;
                    if !note.is_empty() {
                        let msg = serde_json::to_string(note).unwrap_or_default();
                        let _ = webview.evaluate_script(&format!(
                            "window.__qo&&window.__qo.on({{type:'toast',message:{msg}}})"
                        ));
                    }
                };
                match sys {
                    Sys::Lock if st.pause_on_lock && playing => do_pause(Some(Sys::Lock), ""),
                    Sys::Suspend if playing => do_pause(None, ""),
                    Sys::OutputChanged if st.pause_on_output && playing => {
                        do_pause(None, "التلاوة وقفت علشان مخرج الصوت اتغيّر (السماعة اتشالت؟)")
                    }
                    Sys::CallStarted if st.pause_on_call && playing => {
                        do_pause(Some(Sys::CallStarted), "التلاوة وقفت علشان فيه مكالمة. هتكمّل لما تخلص")
                    }
                    Sys::Unlock | Sys::CallEnded
                        if paused_by == Some(if sys == Sys::Unlock { Sys::Lock } else { Sys::CallStarted }) =>
                    {
                        paused_by = None;
                        if st.auto_resume {
                            wake(&mut sleeping, &mut missed_updates);
                            control(&webview, "play");
                        }
                    }
                    _ => {}
                }
                watch::ACTIVE.store(is_playing(&last_state) || paused_by.is_some(), std::sync::atomic::Ordering::Relaxed);
            }
            Event::UserEvent(UserEvent::SleepTimer(m)) => timer = Some(m),
            Event::UserEvent(UserEvent::SleepFire(generation)) => {
                if generation == timer_gen {
                    wake(&mut sleeping, &mut missed_updates);
                    control(&webview, "sleepStop");
                    let _ = sleep_menu.set_text("مؤقّت النوم");
                    let _ = webview.evaluate_script("window.__qo&&window.__qo.on({type:'sleep',until:0})");
                }
            }
            Event::UserEvent(UserEvent::Tray(id)) if id.starts_with("sleep") => {
                timer = id[5..].parse().ok();
            }
            Event::UserEvent(UserEvent::Tray(id)) => match id.as_str() {
                "show" => show = true,
                "toggle" => action = Some("toggle"),
                "next" => action = Some("next"),
                "prev" => action = Some("prev"),
                "mini" => toggle_mini = true,
                "offline" => {
                    show = true;
                    go_offline_page = true;
                }
                "khatma" => {
                    show = true;
                    go_khatma = true;
                }
                "quit" => quit = true,
                _ => {}
            },
            Event::UserEvent(UserEvent::Hotkey(id)) => {
                if id == hk_toggle.id() {
                    action = Some("toggle");
                } else if id == hk_next.id() {
                    action = Some("next");
                } else if id == hk_prev.id() {
                    action = Some("prev");
                } else if id == hk_mini.id() {
                    toggle_mini = true;
                }
            }
            Event::UserEvent(UserEvent::Mini(body)) => {
                let v: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                match v["cmd"].as_str().unwrap_or("") {
                    "ctl" => {
                        if let Some(a) = v["action"].as_str() {
                            wake(&mut sleeping, &mut missed_updates);
                            control(&webview, a);
                        }
                    }
                    "ready" => {
                        if let Some(m) = &mini {
                            let _ = m.webview.evaluate_script(&format!("window.__m&&window.__m({last_state})"));
                        }
                    }
                    "drag" => {
                        if let Some(m) = &mini {
                            let _ = m.window.drag_window();
                        }
                    }
                    "show" => show = true,
                    "close" => toggle_mini = true,
                    _ => {}
                }
            }
            _ => {}
        }

        if let Some(minutes) = timer {
            timer_gen += 1;
            let until_ms = if minutes > 0 {
                let p = proxy.clone();
                let generation = timer_gen;
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_secs(minutes as u64 * 60));
                    let _ = p.send_event(UserEvent::SleepFire(generation));
                });
                let _ = sleep_menu.set_text(format!("مؤقّت النوم (بعد {minutes} د)"));
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64 + minutes as u64 * 60_000)
                    .unwrap_or(0)
            } else {
                let _ = sleep_menu.set_text("مؤقّت النوم");
                0
            };
            if !sleeping {
                let _ = webview.evaluate_script(&format!(
                    "window.__qo&&window.__qo.on({{type:'sleep',until:{until_ms}}})"
                ));
            }
        }
        if show {
            window.set_visible(true);
            if window.is_minimized() {
                window.set_minimized(false);
            }
            window.set_focus();
            sleep_gen += 1;
            wake(&mut sleeping, &mut missed_updates);
            if in_background {
                in_background = false;
                set_background_mode(&webview, false);
            }
        }
        if go_offline_page {
            let _ = webview.load_url(OFFLINE_URL);
        }
        if go_khatma {
            let on_offline_page = webview.url().map(|u| u.starts_with(OFFLINE_URL)).unwrap_or(false);
            if on_offline_page {
                let _ = webview.evaluate_script("window.__qgo&&window.__qgo('khatma')");
            } else {
                let _ = webview.load_url(&format!("{OFFLINE_URL}#khatma"));
            }
        }
        if let Some(a) = action {
            wake(&mut sleeping, &mut missed_updates);
            control(&webview, a);
        }
        if toggle_mini {
            if let Some(m) = mini.take() {
                if let Ok(p) = m.window.outer_position() {
                    let _ = fs::write(&mini_file, format!("{} {}", p.x, p.y));
                }
                // Dropping the window and webview frees their memory.
            } else {
                wake(&mut sleeping, &mut missed_updates);
                mini = build_mini(target, &mut context, &proxy, &mini_file, &args);
            }
        }
        if quit && !closing {
            closing = true;
            if window.is_visible() {
                save_state(&state_file, &window);
            }
            if let Ok(url) = webview.url() {
                save_last_url(&dir, &url);
            }
            if let Some(m) = mini.take() {
                if let Ok(p) = m.window.outer_position() {
                    let _ = fs::write(&mini_file, format!("{} {}", p.x, p.y));
                }
            }
            window.set_visible(false);
            begin_shutdown(&webview, &exit_proxy);
        }
    });
}
