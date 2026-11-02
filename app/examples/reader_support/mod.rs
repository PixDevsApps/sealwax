//! What the reader examples share: a scratch folder inside the repository, a scratch store
//! filled through `ingest_raw`, the real window around a reader, and waiting helpers. Run the
//! examples through `tools/headless.sh`.

#![allow(dead_code)]

pub mod broadway;

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib, graphene};
use libadwaita as adw;
use sealwax_app::reader::{Reader, Shown};
use sealwax_app::theme::ThemeController;
use sealwax_app::window::{
    AccountStatus, FolderRole, Pane, SidebarAccount, SidebarFolder, SidebarModel, Window,
};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::theme::Rgb;
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{Store, StoreOptions};
use sealwax_infrastructure::xdg::SealwaxDirs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static CRITICALS: AtomicU32 = AtomicU32::new(0);

/// Counts GLib criticals from every library (GTK's failed assertions among them); they are
/// still written as GLib writes them. Call it first thing, once.
pub fn count_criticals() {
    glib::log_set_writer_func(|level, fields| {
        if matches!(level, glib::LogLevel::Critical) {
            CRITICALS.fetch_add(1, Ordering::Relaxed);
        }
        glib::log_writer_default(level, fields)
    });
}

/// The criticals written since [`count_criticals`].
pub fn criticals() -> u32 {
    CRITICALS.load(Ordering::Relaxed)
}

/// A scratch folder under the repository's git-ignored `.local/`, emptied first.
pub fn scratch(name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.local/reader-examples")
        .join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("a scratch folder in .local");
    std::fs::canonicalize(&root).unwrap_or(root)
}

pub struct Scratch {
    pub dirs: SealwaxDirs,
    pub store: Rc<Store>,
    pub blobs: BlobStore,
}

/// A store and blob store under `root`. Opened before the GTK main loop runs.
pub fn store(root: &Path) -> Scratch {
    let dirs = SealwaxDirs::under(root);
    let blobs = BlobStore::open(&dirs.blobs_dir()).expect("a scratch blob store");
    let store = Store::open(
        &dirs.store_file(),
        StoreOptions {
            blobs: Some(blobs.clone()),
            ..StoreOptions::default()
        },
    )
    .expect("a scratch store");
    Scratch {
        dirs,
        store: Rc::new(store),
        blobs,
    }
}

/// The real window with one account, showing `reader` in its reader pane.
pub fn window(app: &adw::Application, reader: &Reader, size: (i32, i32)) -> Window {
    let window = Window::new(app, true);
    window.set_sidebar(SidebarModel {
        preferred: None,
        accounts: vec![SidebarAccount {
            key: "personal".into(),
            name: "Personal".into(),
            color: None,
            collapsed: false,
            more_open: None,
            status: AccountStatus::Synced,
            folders: vec![SidebarFolder {
                key: "personal/inbox".into(),
                name: "Inbox".into(),
                role: Some(FolderRole::Inbox),
                count: 0,
                children: Vec::new(),
                folded: false,
            }],
            more: Vec::new(),
        }],
    });
    window.set_reader(&reader.widget());
    window.set_reader_toolbar(&reader.toolbar());
    window.window().set_default_size(size.0, size.1);
    // A live run's window otherwise gets whatever tile the compositor gives it.
    if std::env::var_os("SEALWAX_START_FULLSCREEN").is_some_and(|value| value == "1") {
        window.window().fullscreen();
    }
    window.show_pane(Pane::Reader);
    window
}

pub fn reader(scratch: &Scratch, theme: &Rc<ThemeController>) -> Reader {
    Reader::new(
        scratch.store.clone(),
        theme.clone(),
        &Keymap::default(),
        &scratch.dirs.cache,
    )
}

/// Every `Shown` report of `reader`, in order.
pub fn shown_reports(reader: &Reader) -> async_channel::Receiver<Shown> {
    let (sender, receiver) = async_channel::unbounded();
    reader.connect_shown(move |shown| {
        let _ = sender.try_send(shown.clone());
    });
    receiver
}

/// Shows `id` and waits for its report (10 s at most).
pub async fn show(
    reader: &Reader,
    reports: &async_channel::Receiver<Shown>,
    id: sealwax_domain::model::MessageId,
) -> Result<Shown, String> {
    reader.show(id);
    loop {
        let report = glib::future_with_timeout(Duration::from_secs(10), reports.recv())
            .await
            .map_err(|_| format!("message {} was never shown", id.0))?
            .map_err(|error| error.to_string())?;
        if report.id == id {
            return Ok(report);
        }
    }
}

pub async fn sleep(ms: u64) {
    glib::timeout_future(Duration::from_millis(ms)).await;
}

/// Waits until `check` holds, polling every 10 ms, for at most `limit`.
pub async fn wait_until(limit: Duration, mut check: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while !check() {
        if started.elapsed() > limit {
            return false;
        }
        sleep(10).await;
    }
    true
}

/// Waits for `count` painted frames of `widget`'s window.
pub async fn frames(widget: &impl IsA<gtk::Widget>, count: usize) -> Result<(), String> {
    let clock = widget
        .as_ref()
        .frame_clock()
        .ok_or("the widget has no frame clock")?;
    let (sender, receiver) = async_channel::unbounded();
    let handler = clock.connect_after_paint(move |_| {
        let _ = sender.try_send(Instant::now());
    });
    clock.begin_updating();
    let mut result = Ok(());
    for _ in 0..count {
        widget.as_ref().queue_draw();
        if glib::future_with_timeout(Duration::from_secs(5), receiver.recv())
            .await
            .is_err()
        {
            result = Err("no frame was painted".to_owned());
            break;
        }
    }
    clock.end_updating();
    clock.disconnect(handler);
    result
}

/// Resizes a mapped window and waits until it has the size (Broadway ignores a size asked
/// for before the window maps, as the self-test harness found).
pub async fn resize(window: &impl IsA<gtk::Window>, width: i32, height: i32) -> Result<(), String> {
    let window = window.as_ref();
    window.set_default_size(width, height);
    let fitted = wait_until(Duration::from_secs(10), || {
        (window.width(), window.height()) == (width, height)
    })
    .await;
    if !fitted {
        return Err(format!(
            "the window is {}×{}, not {width}×{height}",
            window.width(),
            window.height()
        ));
    }
    frames(window, 2).await
}

/// The window's size on the live session, where the compositor decides it (a tiling Hyprland
/// tiles the window): `width`×`height` is asked for, then the size is taken once it has held
/// for half a second (5 s at most). Headless runs use [`resize`], which insists on the size.
pub async fn settled_size(window: &impl IsA<gtk::Window>, width: i32, height: i32) -> (i32, i32) {
    let window = window.as_ref();
    window.set_default_size(width, height);
    let started = Instant::now();
    let mut held_since = Instant::now();
    let mut size = (window.width(), window.height());
    while started.elapsed() < Duration::from_secs(5)
        && held_since.elapsed() < Duration::from_millis(500)
    {
        sleep(50).await;
        let now = (window.width(), window.height());
        if now != size {
            size = now;
            held_since = Instant::now();
        }
    }
    size
}

/// [`pixel`], retried over a few frames while the widget hasn't drawn yet.
pub async fn painted_pixel(widget: &impl IsA<gtk::Widget>, x: i32, y: i32) -> Result<Rgb, String> {
    let mut last = Err("never tried".to_owned());
    for _ in 0..20 {
        last = pixel(widget, x, y);
        if last.is_ok() {
            break;
        }
        frames(widget, 1).await?;
    }
    last
}

/// Where `x, y` of `widget` is in its window's surface, for Broadway input.
pub fn surface_point(widget: &impl IsA<gtk::Widget>, x: f64, y: f64) -> Option<(f64, f64)> {
    let widget = widget.as_ref();
    let native = widget.native()?;
    let point = widget.compute_point(&native, &graphene::Point::new(x as f32, y as f32))?;
    let (dx, dy) = native.surface_transform();
    Some((f64::from(point.x()) + dx, f64::from(point.y()) + dy))
}

/// The colour GTK draws at `x, y` of `widget` (its own coordinates).
pub fn pixel(widget: &impl IsA<gtk::Widget>, x: i32, y: i32) -> Result<Rgb, String> {
    let widget = widget.as_ref();
    let (width, height) = (widget.width(), widget.height());
    if width <= 0 || height <= 0 {
        return Err("the widget has no size".to_owned());
    }
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(widget)).snapshot(
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    let node = snapshot.to_node().ok_or("the widget drew nothing")?;
    let renderer = widget
        .native()
        .and_then(|native| native.renderer())
        .ok_or("no renderer")?;
    let viewport = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture: gdk::Texture = renderer.render_texture(&node, Some(&viewport));
    let (texture_width, texture_height) = (texture.width() as usize, texture.height() as usize);
    let stride = texture_width * 4;
    let mut data = vec![0; stride * texture_height];
    texture.download(&mut data, stride);
    let column = (x.max(0) as usize).min(texture_width - 1);
    let row = (y.max(0) as usize).min(texture_height - 1);
    let at = row * stride + column * 4;
    Ok(Rgb {
        r: data[at + 2],
        g: data[at + 1],
        b: data[at],
    })
}

pub fn close(a: Rgb, b: Rgb) -> bool {
    a.r.abs_diff(b.r) <= 3 && a.g.abs_diff(b.g) <= 3 && a.b.abs_diff(b.b) <= 3
}

/// The window as a PNG, with GSK's Cairo renderer (as the self-test's `screenshot`).
pub fn screenshot(widget: &impl IsA<gtk::Widget>, path: &Path) -> Result<(), String> {
    let widget = widget.as_ref();
    let (width, height) = (widget.width(), widget.height());
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(widget)).snapshot(
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    let node = snapshot.to_node().ok_or("nothing drawn")?;
    let renderer = gtk::gsk::CairoRenderer::new();
    renderer
        .realize_for_display(&WidgetExt::display(widget))
        .map_err(|error| error.to_string())?;
    let viewport = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    renderer.unrealize();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    texture.save_to_png(path).map_err(|error| error.to_string())
}

/// WebKit's web and network processes descended from this one (the web processes run under
/// bubblewrap, so they are grandchildren), from `/proc`.
pub fn webkit_processes() -> (usize, usize) {
    let mut parents = std::collections::HashMap::new();
    let mut names = std::collections::HashMap::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
            continue;
        };
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].to_owned();
        let ppid = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|ppid| ppid.parse::<u32>().ok())
            .unwrap_or(0);
        parents.insert(pid, ppid);
        names.insert(pid, name);
    }
    let me = std::process::id();
    let descends = |mut pid: u32| {
        while let Some(&parent) = parents.get(&pid) {
            if parent == me {
                return true;
            }
            if parent <= 1 {
                return false;
            }
            pid = parent;
        }
        false
    };
    let count = |prefix: &str| {
        names
            .iter()
            .filter(|(pid, name)| name.starts_with(prefix) && descends(**pid))
            .count()
    };
    (count("WebKitWebProces"), count("WebKitNetworkPr"))
}

/// The repository's `.local/reader/`, where the examples put screenshots.
pub fn output(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../.local/reader")
        .join(name)
}
