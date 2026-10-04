//! S3 list spike: a GtkListView over a lazy, sectioned model of the 100k-message store, with
//! rows close to the DESIGN mock. A built-in script scrolls programmatically (no synthetic
//! input), jumps to the end and back, inserts new mail, and prints frame, latency and memory
//! measurements. The process exits by itself.

mod model;
mod probe;
mod row;
mod theme;
mod worker;

use anyhow::{Context, Result, bail};
use email_spike_s3::sections::{LABELS, Thresholds};
use email_spike_s3::stats::{ms, summarize};
use email_spike_s3::store::{MailboxInfo, View};
use gtk4::prelude::*;
use gtk4::{gdk, glib, pango};
use libadwaita as adw;
use model::{MessageItem, MessageList};
use probe::{Probe, memory, phase_stats};
use row::{MessageRow, RowStyle};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use worker::{ReadReply, ReadRequest, StoreEvent, WriteRequest};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Layout {
    Panes,
    Full,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Script {
    Full,
    Smoke,
    Rows,
}

struct Args {
    db: PathBuf,
    inbox_only: bool,
    layout: Layout,
    script: Script,
    fullscreen: bool,
    exit_after: Duration,
    sweep: Duration,
    round: bool,
    inserts: i64,
    rows: RowStyle,
    css: Vec<String>,
    sections: bool,
}

fn parse_args() -> Result<Args> {
    let exit_after = match std::env::var("MAIL_EXIT_AFTER_MS") {
        Ok(value) => Duration::from_millis(value.parse().context("MAIL_EXIT_AFTER_MS")?),
        Err(_) => Duration::from_secs(55),
    };
    let mut args = Args {
        db: email_spike_s3::default_db_path(),
        inbox_only: false,
        layout: Layout::Panes,
        script: Script::Full,
        fullscreen: false,
        exit_after,
        sweep: Duration::from_secs(20),
        round: true,
        inserts: 0,
        rows: RowStyle::Trim,
        css: Vec::new(),
        sections: true,
    };
    let mut raw = std::env::args().skip(1);
    while let Some(flag) = raw.next() {
        let mut value = || raw.next().with_context(|| format!("{flag} needs a value"));
        match flag.as_str() {
            "--db" => args.db = PathBuf::from(value()?),
            "--inbox" => args.inbox_only = true,
            "--full-width" => args.layout = Layout::Full,
            "--smoke" => args.script = Script::Smoke,
            "--fullscreen" => args.fullscreen = true,
            "--square" => args.round = false,
            "--sweep-seconds" => args.sweep = Duration::from_secs(value()?.parse()?),
            "--inserts" => args.inserts = value()?.parse()?,
            "--bench-rows" => args.script = Script::Rows,
            "--rows" => {
                let name = value()?;
                args.rows =
                    RowStyle::parse(&name).with_context(|| format!("unknown row style {name}"))?;
            }
            "--css" => args.css.push(value()?),
            "--no-sections" => args.sections = false,
            "--exit-after-ms" => args.exit_after = Duration::from_millis(value()?.parse()?),
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(args)
}

#[derive(Default)]
struct Results {
    open_query_ms: f64,
    open_reply_ms: f64,
    open_frame_ms: Option<f64>,
    start_to_rows_ms: Option<f64>,
    jumps_end: Vec<f64>,
    jumps_start: Vec<f64>,
    jump_timeouts: usize,
    insert_writer: Vec<f64>,
    insert_event: Vec<f64>,
    insert_frame: Vec<f64>,
    insert_timeouts: usize,
    inserted: Vec<i64>,
    page_query: Vec<f64>,
    memory: Vec<(String, [(String, f64); 8])>,
    window: (i32, i32),
    list: (i32, i32),
    row_height: i32,
    realized_rows: usize,
    renderer: String,
    backend: String,
}

struct App {
    args: Args,
    started: Instant,
    model: MessageList,
    list: gtk4::ListView,
    scrolled: gtk4::ScrolledWindow,
    window: gtk4::Window,
    probe: Rc<Probe>,
    reads: async_channel::Sender<ReadRequest>,
    writes: async_channel::Sender<(WriteRequest, Instant)>,
    results: RefCell<Results>,
    inbox: RefCell<Option<MailboxInfo>>,
    mailboxes: RefCell<Vec<MailboxInfo>>,
    view: Cell<View>,
    thresholds: Thresholds,
    main_loop: glib::MainLoop,
    insert_done: RefCell<Option<async_channel::Sender<()>>>,
}

fn main() -> Result<()> {
    let started = Instant::now();
    let args = parse_args()?;
    if !args.db.exists() {
        bail!("{} does not exist; run s3-bench first", args.db.display());
    }
    adw::init().context("libadwaita init")?;
    let palette = theme::palette();
    adw::StyleManager::default().set_color_scheme(if palette.dark {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(&theme::css(&palette));
    let display = gdk::Display::default().context("no display")?;
    let fonts = std::env::var("S3_FONTS").unwrap_or_default();
    if let Some(settings) = gtk4::Settings::default() {
        match fonts.as_str() {
            "" => {}
            "unhinted" => {
                settings.set_gtk_font_rendering(gtk4::FontRendering::Manual);
                settings.set_gtk_xft_hinting(0);
                settings.set_gtk_xft_hintstyle(Some("hintnone"));
                settings.set_gtk_hint_font_metrics(false);
            }
            "unhinted-metrics" => settings.set_gtk_hint_font_metrics(false),
            other => bail!("unknown S3_FONTS value {other}"),
        }
    }
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
    let channels = worker::start(&args.db, 2)?;
    let thresholds = Thresholds::local_now()?;
    let model = MessageList::new(channels.reads.clone());
    let probe = Probe::new();
    let main_loop = glib::MainLoop::new(None, false);
    let (window, list, scrolled) = build_window(&args, &model, &probe, thresholds);
    let app = Rc::new(App {
        args,
        started,
        model,
        list,
        scrolled,
        window,
        probe,
        reads: channels.reads.clone(),
        writes: channels.writes.clone(),
        results: RefCell::new(Results::default()),
        inbox: RefCell::new(None),
        mailboxes: RefCell::new(Vec::new()),
        view: Cell::new(View::All),
        thresholds,
        main_loop: main_loop.clone(),
        insert_done: RefCell::new(None),
    });
    app.results.borrow_mut().backend = display.type_().name().to_owned();

    glib::spawn_future_local(handle_replies(app.clone(), channels.replies.clone()));
    glib::spawn_future_local(handle_events(app.clone(), channels.events.clone()));
    let first_frame = app.probe.until(Duration::from_secs(10), |_| true);
    let start_app = app.clone();
    glib::spawn_future_local(async move {
        let _ = first_frame.await;
        start_app
            .results
            .borrow_mut()
            .memory
            .push(("window painted, list empty".to_owned(), memory()));
        let _ = start_app.reads.try_send(ReadRequest::Mailboxes);
    });

    let probe_clock = app.probe.clone();
    app.window.connect_realize(move |window| {
        if let Some(clock) = window.frame_clock() {
            probe_clock.attach(&clock);
        }
    });
    let quit = app.main_loop.clone();
    app.window.connect_close_request(move |_| {
        quit.quit();
        glib::Propagation::Proceed
    });
    if app.args.fullscreen {
        app.window.fullscreen();
    }
    app.window.present();
    app.list.grab_focus();
    let quit = app.main_loop.clone();
    glib::timeout_add_local_once(app.args.exit_after, move || {
        eprintln!("s3-list: exit timer reached");
        quit.quit();
    });
    main_loop.run();

    let inserted = std::mem::take(&mut app.results.borrow_mut().inserted);
    if !inserted.is_empty() {
        let _ = channels
            .writes
            .send_blocking((WriteRequest::Expunge(inserted.clone()), Instant::now()));
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match channels.events.try_recv() {
                Ok(StoreEvent::Expunged(count)) => {
                    println!("\nCleanup: expunged the {count} inserted messages again.");
                    break;
                }
                Ok(_) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
    channels
        .stop
        .store(true, std::sync::atomic::Ordering::Relaxed);
    report(&app);
    app.window.destroy();
    Ok(())
}

fn build_window(
    args: &Args,
    model: &MessageList,
    probe: &Rc<Probe>,
    thresholds: Thresholds,
) -> (gtk4::Window, gtk4::ListView, gtk4::ScrolledWindow) {
    let selection = gtk4::SingleSelection::builder()
        .model(model)
        .autoselect(false)
        .can_unselect(true)
        .build();
    let factory = gtk4::SignalListItemFactory::new();
    let row_probe = probe.clone();
    let round = args.round;
    let style = args.rows;
    factory.connect_setup(move |_, object| {
        let Some(item) = object.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        item.set_child(Some(&MessageRow::new(
            row_probe.clone(),
            thresholds,
            round,
            style,
        )));
    });
    factory.connect_bind(|_, object| {
        let Some(item) = object.downcast_ref::<gtk4::ListItem>() else {
            return;
        };
        if let (Some(row), Some(message)) = (
            item.child().and_downcast::<MessageRow>(),
            item.item().and_downcast::<MessageItem>(),
        ) {
            row.bind(&message, item.position());
        }
    });
    factory.connect_unbind(|_, object| {
        if let Some(row) = object
            .downcast_ref::<gtk4::ListItem>()
            .and_then(|item| item.child().and_downcast::<MessageRow>())
        {
            row.unbind();
        }
    });
    let headers = gtk4::SignalListItemFactory::new();
    headers.connect_setup(|_, object| {
        if let Some(header) = object.downcast_ref::<gtk4::ListHeader>() {
            header.set_child(Some(
                &gtk4::Label::builder()
                    .xalign(0.0)
                    .css_classes(["section-header"])
                    .build(),
            ));
        }
    });
    headers.connect_bind(|_, object| {
        let Some(header) = object.downcast_ref::<gtk4::ListHeader>() else {
            return;
        };
        if let (Some(label), Some(item)) = (
            header.child().and_downcast::<gtk4::Label>(),
            header.item().and_downcast::<MessageItem>(),
        ) {
            label.set_text(&LABELS[usize::from(item.section())].to_uppercase());
        }
    });
    let list = gtk4::ListView::builder()
        .model(&selection)
        .factory(&factory)
        .css_classes(["message-list"])
        .build();
    if args.sections {
        list.set_header_factory(Some(&headers));
    }
    for class in &args.css {
        list.add_css_class(class);
    }
    let scrolled = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();

    let root = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    root.add_css_class("chrome");
    let pane = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    pane.add_css_class("list-pane");
    pane.append(&list_head(if args.inbox_only {
        "Inbox"
    } else {
        "All mail"
    }));
    pane.append(&scrolled);
    match args.layout {
        Layout::Panes => {
            pane.set_size_request(400, -1);
            root.append(&sidebar());
            root.append(&pane);
            root.append(&reader());
        }
        Layout::Full => {
            pane.set_hexpand(true);
            root.append(&pane);
        }
    }
    let window = gtk4::Window::builder()
        .title("Mail S3 list spike")
        .default_width(1600)
        .default_height(1000)
        .child(&root)
        .build();
    (window, list, scrolled)
}

fn sidebar() -> gtk4::Box {
    let bar = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(1)
        .width_request(232)
        .css_classes(["sidebar"])
        .build();
    bar.append(
        &gtk4::Button::builder()
            .label("Compose")
            .css_classes(["compose"])
            .build(),
    );
    let folder = |icon: &str, name: &str, count: &str, current: bool| {
        let row = gtk4::Box::builder()
            .spacing(10)
            .css_classes(["folder"])
            .build();
        if current {
            row.add_css_class("current");
        }
        row.append(
            &gtk4::Image::builder()
                .icon_name(icon)
                .pixel_size(15)
                .build(),
        );
        row.append(
            &gtk4::Label::builder()
                .label(name)
                .xalign(0.0)
                .hexpand(true)
                .build(),
        );
        row.append(
            &gtk4::Label::builder()
                .label(count)
                .css_classes(["count"])
                .build(),
        );
        row
    };
    bar.append(&folder("mail-inbox-symbolic", "All Inboxes", "", false));
    bar.append(
        &gtk4::Label::builder()
            .label("PERSONAL")
            .xalign(0.0)
            .css_classes(["account"])
            .build(),
    );
    for (icon, name, current) in [
        ("mail-inbox-symbolic", "Inbox", false),
        ("mail-send-symbolic", "Sent", false),
        ("folder-documents-symbolic", "Drafts", false),
        ("folder-symbolic", "Archive", false),
        ("mail-mark-junk-symbolic", "Junk", false),
        ("user-trash-symbolic", "Trash", false),
        ("mail-unread-symbolic", "All mail", true),
    ] {
        bar.append(&folder(icon, name, "", current));
    }
    bar
}

fn list_head(title: &str) -> gtk4::Box {
    let head = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(10)
        .css_classes(["list-head"])
        .build();
    head.append(
        &gtk4::Label::builder()
            .label(title)
            .xalign(0.0)
            .css_classes(["list-title"])
            .build(),
    );
    head.append(
        &gtk4::SearchEntry::builder()
            .placeholder_text("Search — from: has:attachment is:unread")
            .build(),
    );
    let chips = gtk4::Box::builder().spacing(6).build();
    let all = gtk4::ToggleButton::builder()
        .label("All")
        .active(true)
        .css_classes(["chip"])
        .build();
    chips.append(&all);
    for name in ["Unread", "Starred", "Attachments"] {
        chips.append(
            &gtk4::ToggleButton::builder()
                .label(name)
                .css_classes(["chip"])
                .group(&all)
                .build(),
        );
    }
    head.append(&chips);
    head
}

fn reader() -> gtk4::Box {
    let pane = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .hexpand(true)
        .css_classes(["reader"])
        .build();
    let bar = gtk4::Box::builder()
        .spacing(2)
        .css_classes(["reader-bar"])
        .build();
    for icon in [
        "mail-archive-symbolic",
        "user-trash-symbolic",
        "mail-mark-junk-symbolic",
        "mail-unread-symbolic",
        "starred-symbolic",
        "view-more-symbolic",
    ] {
        bar.append(
            &gtk4::Button::builder()
                .icon_name(icon)
                .css_classes(["flat"])
                .build(),
        );
    }
    pane.append(&bar);
    let column = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(18)
        .margin_top(28)
        .margin_start(40)
        .margin_end(40)
        .halign(gtk4::Align::Center)
        .width_request(760)
        .build();
    column.append(
        &gtk4::Label::builder()
            .label("Hytteturen – endelig plan")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["reader-subject"])
            .build(),
    );
    let card = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .css_classes(["reader-card"])
        .build();
    card.append(
        &gtk4::Label::builder()
            .label("Ingrid Solberg <ingrid@fjord.example> · to me · 10:32")
            .xalign(0.0)
            .css_classes(["reader-meta"])
            .build(),
    );
    card.append(
        &gtk4::Label::builder()
            .label(
                "Hei!\n\nJeg har booket hytta for helgen. Den har plass til seks, badstue og \
                 brøytet vei helt fram. Ta med blåbærsyltetøy og kanelboller.\n\nHilsen\nIngrid",
            )
            .xalign(0.0)
            .wrap(true)
            .wrap_mode(pango::WrapMode::WordChar)
            .css_classes(["reader-body"])
            .build(),
    );
    column.append(&card);
    pane.append(&column);
    pane
}

async fn handle_replies(app: Rc<App>, replies: async_channel::Receiver<ReadReply>) {
    let mut open_sent: Option<Instant> = None;
    while let Ok(reply) = replies.recv().await {
        match reply {
            ReadReply::Mailboxes(mailboxes) => {
                let inbox = mailboxes
                    .iter()
                    .find(|m| m.role.as_deref() == Some("inbox"))
                    .cloned();
                let view = match (&inbox, app.args.inbox_only) {
                    (Some(inbox), true) => View::Mailbox(inbox.id),
                    _ => View::All,
                };
                app.view.set(view);
                *app.inbox.borrow_mut() = inbox;
                *app.mailboxes.borrow_mut() = mailboxes;
                open_sent = Some(Instant::now());
                let _ = app.reads.try_send(ReadRequest::Open {
                    view,
                    thresholds: app.thresholds,
                    limit: model::PAGE,
                });
            }
            ReadReply::Opened {
                view,
                snapshot,
                query_ms,
            } => {
                let sent = open_sent.unwrap_or_else(Instant::now);
                {
                    let mut results = app.results.borrow_mut();
                    results.open_query_ms = query_ms;
                    results.open_reply_ms = ms(sent.elapsed());
                }
                let first_frame = app.probe.until(Duration::from_secs(5), |probe| {
                    probe.bound_rows() > 0 && probe.placeholders() == 0
                });
                app.model.reset(view, app.thresholds, snapshot);
                let app = app.clone();
                glib::spawn_future_local(async move {
                    let frame = first_frame.await;
                    {
                        let mut results = app.results.borrow_mut();
                        results.open_frame_ms = frame.map(|at| ms(at - sent));
                        results.start_to_rows_ms = frame.map(|at| ms(at - app.started));
                        results.window = (app.window.width(), app.window.height());
                        results.list = (app.list.width(), app.list.height());
                        results.renderer = app
                            .window
                            .renderer()
                            .map(|renderer| renderer.type_().name().to_owned())
                            .unwrap_or_default();
                        results
                            .memory
                            .push(("first rows painted".to_owned(), memory()));
                    }
                    run_script(app).await;
                });
            }
            ReadReply::Page {
                view,
                page,
                query_ms,
            } => {
                app.results.borrow_mut().page_query.push(query_ms);
                app.model.apply_page(view, page);
            }
            ReadReply::Failed(error) => eprintln!("s3-list: read failed: {error}"),
        }
    }
}

async fn handle_events(app: Rc<App>, events: async_channel::Receiver<StoreEvent>) {
    while let Ok(event) = events.recv().await {
        match event {
            StoreEvent::Inserted {
                diff,
                writer_ms,
                sent,
            } => {
                let received = Instant::now();
                let id = diff.row.id;
                let adjustment = app.scrolled.vadjustment();
                let before = (adjustment.value(), adjustment.upper());
                let at_top = adjustment.value() < 48.0;
                let position = app.model.apply_insert(*diff);
                if position == Some(0) && at_top {
                    adjustment.set_value(0.0);
                }
                if std::env::var_os("S3_DEBUG").is_some() {
                    eprintln!(
                        "s3-list: insert id {id} at {position:?}; before value {:.1} upper {:.1}; at_top {at_top}; after value {:.1} upper {:.1}; placeholders {}; has id {}",
                        before.0,
                        before.1,
                        adjustment.value(),
                        adjustment.upper(),
                        app.probe.placeholders(),
                        app.probe.has_id(id)
                    );
                    let probe = app.probe.clone();
                    let adjustment = adjustment.clone();
                    let frames = Rc::new(Cell::new(0));
                    app.list.add_tick_callback(move |_, _| {
                        frames.set(frames.get() + 1);
                        eprintln!(
                            "s3-list:   frame {} value {:.1} upper {:.1} placeholders {} has id {} bound {}",
                            frames.get(),
                            adjustment.value(),
                            adjustment.upper(),
                            probe.placeholders(),
                            probe.has_id(id),
                            probe.bound_rows()
                        );
                        if frames.get() < 6 { glib::ControlFlow::Continue } else { glib::ControlFlow::Break }
                    });
                }
                let shown = app.probe.until(Duration::from_secs(3), move |probe| {
                    probe.has_id(id) && probe.placeholders() == 0
                });
                let app = app.clone();
                glib::spawn_future_local(async move {
                    let frame = shown.await;
                    {
                        let mut results = app.results.borrow_mut();
                        results.inserted.push(id);
                        results.insert_writer.push(writer_ms);
                        results.insert_event.push(ms(received - sent));
                        match frame {
                            Some(at) => results.insert_frame.push(ms(at - sent)),
                            None => results.insert_timeouts += 1,
                        }
                    }
                    if let Some(done) = app.insert_done.borrow().as_ref() {
                        let _ = done.try_send(());
                    }
                });
            }
            StoreEvent::Expunged(_) => {}
            StoreEvent::Failed(error) => eprintln!("s3-list: write failed: {error}"),
        }
    }
}

#[derive(Clone, Copy)]
enum Scroll {
    Speed(f64),
    /// From one fraction of the list to another in the given seconds; with `skeleton`, rows
    /// skip their text while the list moves faster than `SKELETON_SPEED`.
    Sweep {
        from: f64,
        to: f64,
        seconds: f64,
        skeleton: bool,
    },
}

const SKELETON_SPEED: f64 = 30_000.0;

async fn scroll_phase(app: &Rc<App>, phase: &'static str, scroll: Scroll, duration: Duration) {
    app.probe.set_phase(phase);
    let adjustment = app.scrolled.vadjustment();
    let start = Cell::new(None::<i64>);
    let last = Cell::new(None::<i64>);
    let list = app.list.clone();
    let tick = app.list.add_tick_callback(move |_, clock| {
        let now = clock.frame_time();
        let dt = last
            .replace(Some(now))
            .map_or(0.0, |before| (now - before) as f64 / 1e6);
        let top = (adjustment.upper() - adjustment.page_size()).max(0.0);
        let before = adjustment.value();
        match scroll {
            Scroll::Speed(px_per_s) => adjustment.set_value((before + px_per_s * dt).min(top)),
            Scroll::Sweep {
                from,
                to,
                seconds,
                skeleton,
            } => {
                let began = *start.get().get_or_insert(now);
                start.set(Some(began));
                let progress = ((now - began) as f64 / 1e6 / seconds).min(1.0);
                adjustment.set_value((from + (to - from) * progress) * top);
                if skeleton && dt > 0.0 {
                    let speed = (adjustment.value() - before).abs() / dt;
                    row::set_skeleton(speed > SKELETON_SPEED, &list);
                }
            }
        }
        glib::ControlFlow::Continue
    });
    glib::timeout_future(duration).await;
    tick.remove();
    row::set_skeleton(false, &app.list);
    app.probe.set_phase("idle");
}

async fn settle(app: &Rc<App>) {
    let _ = app
        .probe
        .until(Duration::from_secs(2), |probe| probe.placeholders() == 0)
        .await;
    glib::timeout_future(Duration::from_millis(300)).await;
}

async fn jump(app: &Rc<App>, to_end: bool) -> Option<f64> {
    settle(app).await;
    app.model.drop_cache();
    let last = app.model.len().saturating_sub(1);
    let target = if to_end { last } else { 0 };
    app.probe.set_phase(if to_end {
        "jump to end"
    } else {
        "jump to start"
    });
    let shown = app.probe.until(Duration::from_secs(3), move |probe| {
        probe.has_position(target) && probe.placeholders() == 0
    });
    let started = Instant::now();
    app.list
        .scroll_to(target, gtk4::ListScrollFlags::NONE, None);
    let frame = shown.await;
    app.probe.set_phase("idle");
    frame.map(|at| ms(at - started))
}

async fn bench_rows(app: &Rc<App>) {
    let far = app.model.len() / 2;
    for target in [far, 0] {
        let shown = app.probe.until(Duration::from_secs(3), move |probe| {
            probe.has_position(target) && probe.placeholders() == 0
        });
        app.list
            .scroll_to(target, gtk4::ListScrollFlags::NONE, None);
        let _ = shown.await;
        settle(app).await;
    }
    for round in 0..12 {
        let target = if round % 2 == 0 { far } else { 0 };
        app.probe.set_phase("warm jump");
        let shown = app.probe.until(Duration::from_secs(3), move |probe| {
            probe.has_position(target) && probe.placeholders() == 0
        });
        let started = Instant::now();
        app.list
            .scroll_to(target, gtk4::ListScrollFlags::NONE, None);
        if let Some(at) = shown.await {
            app.results.borrow_mut().jumps_end.push(ms(at - started));
        }
        app.probe.set_phase("idle");
        settle(app).await;
    }
}

async fn run_script(app: Rc<App>) {
    glib::timeout_future(Duration::from_millis(800)).await;
    if app.args.script == Script::Rows {
        bench_rows(&app).await;
        app.results
            .borrow_mut()
            .memory
            .push(("end of script".to_owned(), memory()));
        app.main_loop.quit();
        return;
    }
    let (steady, fling, jumps, inserts) = match app.args.script {
        Script::Full => (Duration::from_secs(6), Duration::from_secs(6), 5, 10),
        Script::Smoke | Script::Rows => (Duration::from_millis(1500), Duration::ZERO, 1, 2),
    };
    let inserts = if app.args.inserts > 0 {
        app.args.inserts
    } else {
        inserts
    };
    scroll_phase(&app, "steady 1500 px/s", Scroll::Speed(1_500.0), steady).await;
    if !fling.is_zero() {
        scroll_phase(&app, "fling 12000 px/s", Scroll::Speed(12_000.0), fling).await;
        jump(&app, false).await;
        settle(&app).await;
        let half = app.args.sweep / 2;
        let seconds = half.as_secs_f64();
        scroll_phase(
            &app,
            "sweep rows 0-50k",
            Scroll::Sweep {
                from: 0.0,
                to: 0.5,
                seconds,
                skeleton: false,
            },
            half,
        )
        .await;
        scroll_phase(
            &app,
            "sweep rows 50k-100k, skeleton rows",
            Scroll::Sweep {
                from: 0.5,
                to: 1.0,
                seconds,
                skeleton: true,
            },
            half,
        )
        .await;
        settle(&app).await;
    }
    for _ in 0..jumps {
        let end = jump(&app, true).await;
        let start = jump(&app, false).await;
        let mut results = app.results.borrow_mut();
        for (value, list) in [(end, 0), (start, 1)] {
            match value {
                Some(ms) if list == 0 => results.jumps_end.push(ms),
                Some(ms) => results.jumps_start.push(ms),
                None => results.jump_timeouts += 1,
            }
        }
    }
    settle(&app).await;
    let inbox = app.inbox.borrow().clone();
    if let Some(inbox) = inbox {
        app.probe.set_phase("insert");
        let (done, wait) = async_channel::bounded(1);
        *app.insert_done.borrow_mut() = Some(done);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or_default();
        for index in 0..inserts {
            let request = WriteRequest::Incoming {
                view: app.view.get(),
                mailbox: inbox.id,
                seed: 9_000 + index as u64,
                date: now + 1 + index,
                uid: inbox.next_uid + index,
            };
            let _ = app.writes.try_send((request, Instant::now()));
            let _ = futures_timeout(wait.recv(), Duration::from_secs(4)).await;
            glib::timeout_future(Duration::from_millis(250)).await;
        }
        app.probe.set_phase("idle");
    }
    glib::timeout_future(Duration::from_millis(500)).await;
    app.results
        .borrow_mut()
        .memory
        .push(("end of script".to_owned(), memory()));
    let trimmed = unsafe { libc::malloc_trim(0) };
    app.results.borrow_mut().memory.push((
        format!("after malloc_trim(0) (returned {trimmed})"),
        memory(),
    ));
    let (height, realized) = rows(&app);
    {
        let mut results = app.results.borrow_mut();
        results.row_height = height;
        results.realized_rows = realized;
    }
    app.main_loop.quit();
}

async fn futures_timeout<T>(
    future: impl std::future::Future<Output = T>,
    limit: Duration,
) -> Option<T> {
    use std::pin::pin;
    use std::task::Poll;
    let mut future = pin!(future);
    let mut timer = pin!(glib::timeout_future(limit));
    std::future::poll_fn(move |context| {
        if let Poll::Ready(value) = future.as_mut().poll(context) {
            return Poll::Ready(Some(value));
        }
        if timer.as_mut().poll(context).is_ready() {
            return Poll::Ready(None);
        }
        Poll::Pending
    })
    .await
}

/// The height of a realized row and how many row widgets the list view keeps realized.
fn rows(app: &App) -> (i32, usize) {
    let mut height = 0;
    let mut realized = 0;
    let mut child = app.list.first_child();
    while let Some(widget) = child {
        if widget.css_name() == "row" {
            realized += 1;
            if height == 0 && widget.height() > 0 {
                height = widget.height();
            }
        }
        child = widget.next_sibling();
    }
    (height, realized)
}

fn report(app: &App) {
    let results = app.results.borrow();
    let stats = app.model.stats();
    println!("\n# s3-list results\n");
    println!(
        "- GTK {}.{}.{}, libadwaita {}.{}.{}; backend {}; renderer {}",
        gtk4::major_version(),
        gtk4::minor_version(),
        gtk4::micro_version(),
        adw::major_version(),
        adw::minor_version(),
        adw::micro_version(),
        results.backend,
        if results.renderer.is_empty() {
            "unknown"
        } else {
            &results.renderer
        }
    );
    println!(
        "- rows `{}`, list CSS {:?}, sections {}, font settings: {}",
        app.args.rows.name(),
        app.args.css,
        if app.args.sections { "on" } else { "off" },
        gtk4::Settings::default().map_or("unknown".to_owned(), |settings| format!(
            "rendering {:?}, hint-font-metrics {}, xft-hinting {}, xft-hintstyle {:?}, xft-antialias {}",
            settings.gtk_font_rendering(),
            settings.is_gtk_hint_font_metrics(),
            settings.gtk_xft_hinting(),
            settings.gtk_xft_hintstyle(),
            settings.gtk_xft_antialias()
        ))
    );
    println!(
        "- window {}×{}, list view {}×{}, row {} px tall (plus 2 px gap), {} row widgets realized at the end, layout {}, view {} with {} rows, release build: {}",
        results.window.0,
        results.window.1,
        results.list.0,
        results.list.1,
        results.row_height,
        results.realized_rows,
        match app.args.layout {
            Layout::Panes => "three panes (list 400 px)",
            Layout::Full => "list at full width",
        },
        match app.view.get() {
            View::All => "All mail".to_owned(),
            View::Mailbox(_) => "INBOX".to_owned(),
        },
        app.model.len(),
        !cfg!(debug_assertions)
    );
    println!(
        "- folder open: reader query {:.2} ms; request → reply on the GTK thread {:.2} ms; \
         request → first painted frame with rows {}; process start → that frame {}",
        results.open_query_ms,
        results.open_reply_ms,
        results
            .open_frame_ms
            .map_or("not reached".to_owned(), |v| format!("{v:.1} ms")),
        results
            .start_to_rows_ms
            .map_or("not reached".to_owned(), |v| format!("{v:.1} ms"))
    );
    let pages = summarize(&results.page_query);
    println!(
        "- page queries on the reader threads: n={} p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms",
        pages.n, pages.p50, pages.p95, pages.max
    );
    println!(
        "- model: {} page requests, {} applied, {} stale, {} stashed, {} evicted, {} items created, {} pages cached at the end",
        stats.requested,
        stats.applied,
        stats.stale,
        stats.stashed,
        stats.evicted,
        stats.items_created,
        app.model.cached_pages()
    );

    println!("\n## Frames\n");
    println!(
        "Interval = difference of consecutive GdkFrameTimings presentation times (frame times \
         where no presentation time arrived); work = before-paint → end of after-paint on the \
         GTK thread.\n"
    );
    println!(
        "| phase | frames | refresh ms | interval p50 | p95 | p99 | max | > 16.7 ms | > 1.5 refresh | work p50 | p95 | p99 | max | layout p95 | paint p95 | frames with placeholders | binds/frame |"
    );
    println!(
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
    );
    let frames = app.probe.frames();
    let mut phases: Vec<&'static str> = Vec::new();
    for frame in &frames {
        if !phases.contains(&frame.phase) {
            phases.push(frame.phase);
        }
    }
    for phase in phases {
        if let Some(stats) = phase_stats(&frames, phase) {
            let interval = if stats.presented.n > 0 {
                stats.presented
            } else {
                stats.interval
            };
            println!(
                "| {phase} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {:.2} | {} | {:.1} |",
                stats.frames,
                stats.refresh_ms,
                interval.p50,
                interval.p95,
                interval.p99,
                interval.max,
                stats.over_budget,
                stats.missed_vblank,
                stats.cpu.p50,
                stats.cpu.p95,
                stats.cpu.p99,
                stats.cpu.max,
                stats.layout.p95,
                stats.paint.p95,
                stats.with_placeholders,
                stats.binds_per_frame
            );
        }
    }

    let warm: Vec<&probe::Frame> = frames
        .iter()
        .filter(|frame| frame.phase == "warm jump" && frame.binds >= 50)
        .collect();
    if !warm.is_empty() {
        let pick = |f: fn(&probe::Frame) -> f64| {
            summarize(&warm.iter().map(|frame| f(frame)).collect::<Vec<_>>())
        };
        let (binds, work, layout, paint) = (
            pick(|frame| f64::from(frame.binds)),
            pick(|frame| frame.cpu_ms),
            pick(|frame| frame.layout_ms),
            pick(|frame| frame.paint_ms),
        );
        println!(
            "\nWarm jumps (frames that bound ≥ 50 loaded rows): n={}, binds p50 {:.0}, work p50 {:.2} ms (layout {:.2}, paint {:.2}), work max {:.2} ms, layout per bound row {:.3} ms",
            work.n,
            binds.p50,
            work.p50,
            layout.p50,
            paint.p50,
            work.max,
            layout.p50 / binds.p50.max(1.0)
        );
    }
    println!("\n## Jumps and inserts\n");
    println!("| measurement | n | p50 ms | p95 ms | max ms |");
    println!("| --- | --- | --- | --- | --- |");
    let line = |name: &str, values: &[f64]| {
        let summary = summarize(values);
        println!(
            "| {name} | {} | {:.1} | {:.1} | {:.1} |",
            summary.n, summary.p50, summary.p95, summary.max
        );
    };
    line(
        "jump to end (cache dropped) → first frame with the last row and no placeholders",
        &results.jumps_end,
    );
    line("jump to start (cache dropped) → same", &results.jumps_start);
    line("insert: writer transaction + diff", &results.insert_writer);
    line(
        "insert: request → diff on the GTK thread",
        &results.insert_event,
    );
    line(
        "insert: request → first frame showing the new row",
        &results.insert_frame,
    );
    println!(
        "\nTimeouts: {} jumps, {} inserts.",
        results.jump_timeouts, results.insert_timeouts
    );

    println!("\n## Memory (MiB, /proc/self/status and smaps_rollup)\n");
    println!(
        "| point | VmRSS | VmHWM | RssAnon | RssFile | RssShmem | Pss | Pss_Anon | Pss_File |"
    );
    println!("| --- | --- | --- | --- | --- | --- | --- | --- | --- |");
    for (point, values) in &results.memory {
        println!(
            "| {point} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
            values[0].1,
            values[1].1,
            values[2].1,
            values[3].1,
            values[4].1,
            values[5].1,
            values[6].1,
            values[7].1
        );
    }
}
