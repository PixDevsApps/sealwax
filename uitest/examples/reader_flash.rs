//! Headless check for a flash when the reader moves from one message to the next: the samples
//! are opened one after another as the message list opens them (`Reader::show_next`), and every frame GTK paints from the `show` until the reader
//! has settled is read back. A frame whose message area is brighter than both the message it
//! left and the message it settles on is a flash; its frame is saved in `.local/reader/flash/`.
//!
//! ```sh
//! cargo build -p sealwax-uitest --example reader_flash
//! SEALWAX_HEADLESS_TIMEOUT=180 tools/headless.sh 37 target/debug/examples/reader_flash
//! ```

#[path = "reader_support/mod.rs"]
mod support;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib, graphene};
use libadwaita as adw;
use sealwax_app::reader::samples;
use sealwax_app::theme::{STARTUP_WAIT, ThemeController};
use sealwax_domain::theme::ThemeOptions;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};
use support::broadway;

/// The order the samples are opened in: styled and themed bodies after each other.
const ORDER: [&str; 12] = [
    "plain",
    "newsletter",
    "simple",
    "receipt",
    "dark-promo",
    "newsletter",
    "plain",
    "receipt",
    "cid",
    "newsletter",
    "short",
    "receipt",
];

/// How long after a `show` frames are read.
const WATCH: Duration = Duration::from_millis(600);

struct Frame {
    at: Duration,
    white: f64,
    texture: gdk::Texture,
}

/// The share of near-white pixels in the reader's area, and the frame.
fn grab(widget: &gtk::Widget) -> Option<(f64, gdk::Texture)> {
    let (width, height) = (widget.width(), widget.height());
    if width <= 0 || height <= 0 {
        return None;
    }
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(widget)).snapshot(
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    let node = snapshot.to_node()?;
    let renderer = widget.native()?.renderer()?;
    let viewport = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    let (w, h) = (texture.width() as usize, texture.height() as usize);
    let stride = w * 4;
    let mut data = vec![0; stride * h];
    texture.download(&mut data, stride);
    let white = data
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|pixel| pixel[0] >= 240 && pixel[1] >= 240 && pixel[2] >= 240)
        .count();
    Some((white as f64 / (w * h) as f64, texture))
}

fn main() -> glib::ExitCode {
    support::count_criticals();
    let _ = sealwax_infrastructure::logging::init();
    let input = match broadway::connect() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("run this through tools/headless.sh: {error}");
            return glib::ExitCode::FAILURE;
        }
    };
    let root = support::scratch("flash");
    let scratch = support::store(&root);
    let now = glib::real_time() / 1_000_000;
    let filled = samples::fill_store(&scratch.store, &scratch.blobs, samples::messages(now))
        .expect("the samples go into the scratch store");
    let app = adw::Application::builder()
        .application_id("io.github.pixdevsapps.Sealwax.ReaderFlash")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let passed = Rc::new(Cell::new(false));
    let scratch = Rc::new(scratch);
    app.connect_activate(glib::clone!(
        #[strong]
        passed,
        move |app| {
            let theme = ThemeController::start(ThemeOptions::default(), STARTUP_WAIT);
            let reader = support::reader(&scratch, &theme);
            let window = support::window(app, &reader, (1440, 900));
            window.present();
            let app = app.clone();
            let passed = passed.clone();
            let filled = filled.clone();
            let _input = input.clone();
            glib::spawn_future_local(async move {
                let ok = match run(&reader, &window, &filled).await {
                    Ok(flashes) => {
                        println!("FLASH frames brighter than both messages: {flashes}");
                        flashes == 0
                    }
                    Err(error) => {
                        println!("FLASH error {error}");
                        false
                    }
                };
                println!("FLASH GLib criticals: {}", support::criticals());
                passed.set(ok && support::criticals() == 0);
                app.quit();
            });
        }
    ));
    app.run_with_args::<&str>(&[]);
    if passed.get() {
        glib::ExitCode::SUCCESS
    } else {
        glib::ExitCode::FAILURE
    }
}

async fn run(
    reader: &sealwax_app::reader::Reader,
    window: &sealwax_app::window::Window,
    filled: &samples::Filled,
) -> Result<u32, String> {
    support::resize(window.window(), 1440, 900).await?;
    support::sleep(1500).await;
    let reports = support::shown_reports(reader);
    let area: gtk::Widget = reader.widget().upcast();
    let clock = window
        .window()
        .frame_clock()
        .ok_or("the window has no frame clock")?;
    let out = support::output("flash");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).map_err(|error| error.to_string())?;
    let mut before = grab(&area).map_or(0.0, |(white, _)| white);
    let mut flashes = 0;
    for (step, name) in ORDER.iter().enumerate() {
        let id = filled.id(name).ok_or("a sample is missing")?;
        support::frames(window.window(), 2).await?;
        support::sleep(300).await;
        let frames: Rc<RefCell<Vec<Frame>>> = Rc::default();
        let started = Instant::now();
        let handler = clock.connect_after_paint(glib::clone!(
            #[strong]
            frames,
            #[strong]
            area,
            move |_| {
                if let Some((white, texture)) = grab(&area) {
                    frames.borrow_mut().push(Frame {
                        at: started.elapsed(),
                        white,
                        texture,
                    });
                }
            }
        ));
        clock.begin_updating();
        reader.show_next(id);
        let shown = loop {
            let report = glib::future_with_timeout(Duration::from_secs(10), reports.recv())
                .await
                .map_err(|_| format!("message {} was never shown", id.0))?
                .map_err(|error| error.to_string())?;
            if report.id == id {
                break report;
            }
        };
        let left = WATCH.saturating_sub(started.elapsed());
        support::sleep(left.as_millis() as u64).await;
        clock.end_updating();
        clock.disconnect(handler);
        let frames = frames.take();
        let after = frames.last().map_or(0.0, |frame| frame.white);
        let line: Vec<String> = frames
            .iter()
            .map(|frame| {
                format!(
                    "{:.0}ms:{:.0}%",
                    frame.at.as_secs_f64() * 1000.0,
                    frame.white * 100.0
                )
            })
            .collect();
        println!(
            "FLASH {step:02} {name} (ready {:.0} ms) before {:.0}% after {:.0}%: {}",
            shown.ready.as_secs_f64() * 1000.0,
            before * 100.0,
            after * 100.0,
            line.join(" ")
        );
        for (index, frame) in frames.iter().enumerate() {
            if std::env::var_os("READER_FLASH_ALL").is_some()
                && (frame.white - before).abs() > 0.005
                && (frame.white - after).abs() > 0.005
            {
                let path = out.join(format!("{step:02}-{name}-{index:02}-between.png"));
                let _ = frame.texture.save_to_png(&path);
            }
            if frame.white > before.max(after) + 0.05 {
                flashes += 1;
                let path = out.join(format!("{step:02}-{name}-{index:02}.png"));
                let _ = frame.texture.save_to_png(&path);
                println!("FLASH   frame {index} is brighter: {}", path.display());
            }
        }
        before = after;
    }
    Ok(flashes)
}
