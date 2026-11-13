// Hide the console window on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod glass;
mod intro;
mod media;
mod settings;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use slint::{ComponentHandle, Timer, TimerMode};
use unitn_core::audio::Player;
use unitn_core::library::Library;

slint::include_modules!();

/// Logs to stderr and to `unitn.log` in the data folder (recreated on each start).
fn init_logging(path: &std::path::Path) {
    use std::io::Write;
    use std::sync::Mutex;
    let file = std::fs::File::create(path).ok().map(Mutex::new);
    let started = std::time::Instant::now();
    unitn_core::log::set_sink(move |msg| {
        let line = format!("[{:>8.3}] {msg}", started.elapsed().as_secs_f64());
        eprintln!("{line}");
        if let Some(f) = &file {
            if let Ok(mut f) = f.lock() {
                let _ = writeln!(f, "{line}");
                let _ = f.flush();
            }
        }
    });
    std::panic::set_hook(Box::new(|info| {
        let bt = std::backtrace::Backtrace::force_capture();
        unitn_core::log::log(format!("PANIC: {info}\n{bt}"));
        if std::thread::current().name() == Some("main") {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("Unitn crashed")
                .set_description(format!("{info}\n\nDetails are in unitn.log in the Unitn data folder."))
                .show();
        }
    }));
    unitn_core::log::log(format!(
        "Unitn {} on {} {}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    ));
}

fn main() {
    if let Err(e) = run() {
        // Release builds on Windows have no console: make failures visible.
        unitn_core::log::log(format!("fatal: {e:#}"));
        rfd::MessageDialog::new()
            .set_level(rfd::MessageLevel::Error)
            .set_title("Unitn could not start")
            .set_description(format!("{e:#}\n\nDetails are in unitn.log in the Unitn data folder."))
            .show();
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let paths = settings::Paths::new()?;
    init_logging(&paths.log);
    let settings = settings::Settings::load(&paths.settings);
    let library = Library::open(&paths.database)?;

    configure_window_style()?;
    unitn_core::log::log("startup: creating window");
    let ui = AppWindow::new()?;
    if cfg!(target_os = "macos") {
        // Our content extends under the (transparent) macOS title bar.
        ui.set_titlebar_inset(28.0);
    }
    let (player, player_events) = Player::new();

    // The window opens at the size it had when the app was closed.
    if let (Some(w), Some(h)) = (settings.window_width, settings.window_height) {
        let (w, h) = (w.max(MIN_WINDOW.0 as f32), h.max(MIN_WINDOW.1 as f32));
        if w.is_finite() && h.is_finite() && w < 10_000.0 && h < 10_000.0 {
            ui.window().set_size(slint::LogicalSize::new(w, h));
        }
    }
    let start_maximized = settings.window_maximized;

    // Startup animation in its own splash window (test hook: UNITN_NO_INTRO=1
    // skips it); the main window appears when it is over.
    let intro_enabled = std::env::var("UNITN_NO_INTRO").map_or(true, |v| v.is_empty());
    let intro_sound = settings.startup_sound.then_some(settings.volume);
    let (main_shown_tx, main_shown_rx) = mpsc::channel::<()>();
    let show_main = {
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else { return };
            if let Err(e) = ui.show() {
                unitn_core::log::log(format!("startup: cannot show the window: {e}"));
            }
            if start_maximized {
                ui.window().set_maximized(true);
            }
            unitn_core::log::log("startup: window shown");
            let _ = main_shown_tx.send(());
        }
    };
    let _splash = if intro_enabled {
        let splash = SplashWindow::new()?;
        let _intro = intro::start(&splash, intro_sound, show_main);
        Some(splash)
    } else {
        show_main();
        None
    };
    let (media_tx, media_rx) = mpsc::channel();
    // Media controls are attached once the event loop runs (on Windows the
    // native window, which they need, only exists from then on).
    let media = media::Media::disabled();

    let app = Rc::new(RefCell::new(app::App::new(
        &ui,
        paths.clone(),
        settings,
        library,
        player,
        player_events,
        media,
        media_rx,
    )));
    app::connect(&ui, &app);
    // Load the library once the event loop runs, so the window (or the
    // startup animation) shows right away.
    {
        let app = app.clone();
        Timer::single_shot(Duration::ZERO, move || app.borrow_mut().start());
    }

    // Test hook: UNITN_AUTOPLAY="<file or URL>|<file or URL>|…" plays each
    // target for a few seconds in turn.
    let mut autoplay_timers = Vec::new();
    if let Ok(targets) = std::env::var("UNITN_AUTOPLAY") {
        for (i, target) in targets.split('|').filter(|t| !t.is_empty()).enumerate() {
            let app = app.clone();
            let target = target.to_string();
            let timer = Timer::default();
            timer.start(TimerMode::SingleShot, Duration::from_millis(200 + 8000 * i as u64), move || {
                app.borrow_mut().autoplay(&target);
            });
            autoplay_timers.push(timer);
        }
    }

    // Files dragged onto the window (several files arrive as separate events).
    {
        use slint::winit_030::winit::event::WindowEvent;
        use slint::winit_030::{WinitWindowAccessor, WinitWindowEventResult};
        let app = app.clone();
        let weak = ui.as_weak();
        let dropped: Rc<RefCell<Vec<std::path::PathBuf>>> = Rc::default();
        let timer = Rc::new(Timer::default());
        let resize_weak = ui.as_weak();
        let resize_app = app.clone();
        let resize_save = Rc::new(Timer::default());
        ui.window().on_winit_window_event(move |_, event| {
            match event {
                WindowEvent::Resized(_) => {
                    // Re-check after the event: the window must not get
                    // smaller than the size the player still works at.
                    let weak = resize_weak.clone();
                    let app = resize_app.clone();
                    let save_timer = resize_save.clone();
                    Timer::single_shot(Duration::ZERO, move || {
                        if let Some(ui) = weak.upgrade() {
                            keep_min_size(&ui);
                            // Remember the size for the next start (not the
                            // full-screen visualizer's).
                            let window = ui.window();
                            if !window.is_fullscreen() && !window.is_minimized() {
                                let size = window.size().to_logical(window.scale_factor());
                                if let Ok(mut app) = app.try_borrow_mut() {
                                    app.remember_window(size.width, size.height, window.is_maximized());
                                }
                                // Save shortly after resizing stops: quitting
                                // with Cmd+Q on macOS skips the save at exit.
                                let app = app.clone();
                                save_timer.start(TimerMode::SingleShot, Duration::from_millis(800), move || {
                                    if let Ok(app) = app.try_borrow() {
                                        app.save_settings();
                                    }
                                });
                            }
                        }
                    });
                }
                WindowEvent::HoveredFile(_) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_drop_hover(true);
                    }
                }
                WindowEvent::HoveredFileCancelled => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_drop_hover(false);
                    }
                }
                WindowEvent::DroppedFile(path) => {
                    if let Some(ui) = weak.upgrade() {
                        ui.set_drop_hover(false);
                    }
                    dropped.borrow_mut().push(path.clone());
                    let (app, dropped) = (app.clone(), dropped.clone());
                    timer.start(TimerMode::SingleShot, Duration::from_millis(200), move || {
                        let paths = std::mem::take(&mut *dropped.borrow_mut());
                        if let Ok(mut app) = app.try_borrow_mut() {
                            app.handle_drop(paths);
                        }
                    });
                }
                _ => {}
            }
            WinitWindowEventResult::Propagate
        });
    }

    // Media controls attach to the native window, so only once it is shown
    // (after the startup animation).
    let media_timer = Timer::default();
    {
        let app = app.clone();
        let weak = ui.as_weak();
        let mut attached = false;
        media_timer.start(TimerMode::Repeated, Duration::from_millis(400), move || {
            if attached || main_shown_rx.try_recv().is_err() {
                return;
            }
            attached = true;
            let Some(ui) = weak.upgrade() else { return };
            let media = media::Media::new(window_handle(&ui), media_tx.clone());
            if let Ok(mut app) = app.try_borrow_mut() {
                app.set_media(media);
                app.apply_glass();
            }
            keep_min_size(&ui);
        });
    }

    // Pump events from the audio thread, background jobs and media keys.
    let timer = Timer::default();
    {
        let app = app.clone();
        timer.start(TimerMode::Repeated, Duration::from_millis(40), move || {
            if let Ok(mut app) = app.try_borrow_mut() {
                app.poll();
            }
        });
    }

    // Visualizer animation at ~30 fps.
    let viz_timer = Timer::default();
    {
        let app = app.clone();
        viz_timer.start(TimerMode::Repeated, Duration::from_millis(33), move || {
            if let Ok(mut app) = app.try_borrow_mut() {
                app.tick_visualizer();
            }
        });
    }

    unitn_core::log::log("startup: ready");
    slint::run_event_loop()?;
    ui.hide()?;
    app.borrow().save_settings();
    Ok(())
}

/// Smallest window size (logical pixels) at which the sidebar, the pages
/// and the now playing bar all still fit. Same as `min-width`/`min-height`
/// in app.slint; set on the native window as well, because not every
/// platform picked up the Slint value.
const MIN_WINDOW: (f64, f64) = (960.0, 620.0);

fn keep_min_size(ui: &AppWindow) {
    use slint::winit_030::winit::dpi::LogicalSize;
    use slint::winit_030::WinitWindowAccessor;
    ui.window().with_winit_window(|w| {
        w.set_min_inner_size(Some(LogicalSize::new(MIN_WINDOW.0, MIN_WINDOW.1)));
        if w.fullscreen().is_some() {
            return;
        }
        let size = w.inner_size().to_logical::<f64>(w.scale_factor());
        if size.width < MIN_WINDOW.0 - 1.0 || size.height < MIN_WINDOW.1 - 1.0 {
            unitn_core::log::log(format!("window: {:.0}×{:.0} is below the minimum, enlarging", size.width, size.height));
            let _ = w.request_inner_size(LogicalSize::new(size.width.max(MIN_WINDOW.0), size.height.max(MIN_WINDOW.1)));
        }
    });
}

/// On macOS, make the title bar transparent and let the app draw underneath
/// it, so it always matches the theme (and the transparency setting).
#[cfg(target_os = "macos")]
fn configure_window_style() -> Result<()> {
    use slint::winit_030::winit::platform::macos::WindowAttributesExtMacOS;
    slint::BackendSelector::new()
        .with_winit_window_attributes_hook(|attributes| {
            attributes
                .with_titlebar_transparent(true)
                .with_fullsize_content_view(true)
                .with_title_hidden(true)
        })
        .select()?;
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn configure_window_style() -> Result<()> {
    Ok(())
}

#[cfg(target_os = "windows")]
fn window_handle(ui: &AppWindow) -> Option<*mut std::ffi::c_void> {
    use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use slint::winit_030::WinitWindowAccessor;
    let hwnd = ui
        .window()
        .with_winit_window(|w| match w.window_handle().ok()?.as_raw() {
            RawWindowHandle::Win32(h) => Some(h.hwnd.get() as *mut std::ffi::c_void),
            _ => None,
        })
        .flatten();
    unitn_core::log::log(format!("startup: window handle {}", if hwnd.is_some() { "found" } else { "missing" }));
    hwnd
}

#[cfg(not(target_os = "windows"))]
fn window_handle(_ui: &AppWindow) -> Option<*mut std::ffi::c_void> {
    None
}
