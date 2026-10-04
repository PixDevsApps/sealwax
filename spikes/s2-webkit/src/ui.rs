//! The conversation pane of approach A: a GtkScrolledWindow holding GTK message headers and
//! one body per message (a WebView inside a themed or paper frame, or a text view).

use email_spike_s2::corpus::Message;
use email_spike_s2::theme::{self, BodyStyle, Palette};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, glib};
use libadwaita as adw;
use std::collections::HashMap;

/// Width of the reading column (DESIGN.md centres a ≈ 760 px text column; newsletters are
/// usually 600–640 px wide, so the column leaves room for the paper card's margins).
pub const READER_WIDTH: i32 = 860;

pub fn install_css(palette: &Palette, radius: u32) {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&theme::app_css(palette, radius));
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    adw::StyleManager::default().set_color_scheme(if palette.dark {
        adw::ColorScheme::ForceDark
    } else {
        adw::ColorScheme::ForceLight
    });
}

pub struct Pane {
    pub window: adw::ApplicationWindow,
    pub scroller: gtk::ScrolledWindow,
    pub column: gtk::Box,
}

pub fn pane(app: &adw::Application) -> Pane {
    let column = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Start)
        .width_request(READER_WIDTH)
        .margin_top(12)
        .margin_bottom(24)
        .css_classes(["conversation"])
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&column)
        .build();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Mail S2 spike")
        .default_width(1280)
        .default_height(1000)
        .css_classes(["s2"])
        .content(&scroller)
        .build();
    Pane {
        window,
        scroller,
        column,
    }
}

pub fn clear(column: &gtk::Box) {
    while let Some(child) = column.first_child() {
        column.remove(&child);
    }
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|word| word.chars().next())
        .take(2)
        .collect::<String>()
        .to_uppercase()
}

/// A message: the GTK header (avatar, sender, subject, date) and an empty body frame.
pub fn message(message: &Message, index: usize) -> (gtk::Box, gtk::Box) {
    let avatar = gtk::Label::builder()
        .label(initials(&message.sender))
        .css_classes(["avatar"])
        .valign(gtk::Align::Center)
        .build();
    let sender = gtk::Label::builder()
        .label(&message.sender)
        .xalign(0.0)
        .css_classes(["sender"])
        .build();
    let subject = gtk::Label::builder()
        .label(&message.subject)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["dim"])
        .build();
    let names = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .build();
    names.append(&sender);
    names.append(&subject);
    let date = gtk::Label::builder()
        .label(format!(
            "Oct {} · 09:{:02}",
            1 + index % 4,
            (index * 7) % 60
        ))
        .css_classes(["dim"])
        .valign(gtk::Align::Start)
        .build();
    let header = gtk::Box::builder()
        .spacing(10)
        .css_classes(["message-header"])
        .build();
    header.append(&avatar);
    header.append(&names);
    header.append(&date);
    let body = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .overflow(gtk::Overflow::Hidden)
        .build();
    let root = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .css_classes(["message"])
        .build();
    root.append(&header);
    root.append(&body);
    (root, body)
}

mod imp {
    use gtk4 as gtk;
    use gtk4::glib;
    use gtk4::prelude::*;
    use gtk4::subclass::prelude::*;

    #[derive(Default)]
    pub struct BodyBox;

    #[glib::object_subclass]
    impl ObjectSubclass for BodyBox {
        const NAME: &'static str = "S2BodyBox";
        type Type = super::BodyBox;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for BodyBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for BodyBox {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let height = self
                .obj()
                .first_child()
                .map_or(0, |child| child.size_request().1.max(0));
            match orientation {
                gtk::Orientation::Vertical => (height, height, -1, -1),
                _ => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.obj().first_child() {
                child.allocate(width, height, baseline, None);
            }
        }
    }
}

glib::wrapper! {
    /// Holds one body view at exactly its requested height and the column's width. A WebView
    /// reports WebKit's contents size as its natural height, which never drops below its
    /// current viewport, so a plain box would keep a body taller than its content.
    pub struct BodyBox(ObjectSubclass<imp::BodyBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BodyBox {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let body: Self = glib::Object::new();
        child.set_parent(&body);
        body
    }
}

pub fn set_style(frame: &gtk::Box, style: BodyStyle) {
    let (add, remove) = match style {
        BodyStyle::Paper => ("paper", "themed-body"),
        BodyStyle::Themed => ("themed-body", "paper"),
    };
    frame.remove_css_class(remove);
    frame.add_css_class(add);
}

pub fn background(palette: &Palette, style: BodyStyle) -> gdk::RGBA {
    let [red, green, blue] = match style {
        BodyStyle::Paper => theme::PAPER.floats(),
        BodyStyle::Themed => palette.background.floats(),
    };
    gdk::RGBA::new(red, green, blue, 1.0)
}

pub fn plain_text(message: &Message) -> gtk::TextView {
    let view = gtk::TextView::builder()
        .editable(false)
        .cursor_visible(false)
        .wrap_mode(gtk::WrapMode::WordChar)
        .left_margin(18)
        .right_margin(18)
        .top_margin(14)
        .bottom_margin(14)
        .css_classes(["plain"])
        .build();
    view.buffer().set_text(&message.body);
    view
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Uniformity {
    pub width: i32,
    pub height: i32,
    /// Distinct colours among the sampled pixels, capped at 4096.
    pub distinct: usize,
    /// Share of sampled pixels that have the most common colour.
    pub dominant: f64,
}

impl Uniformity {
    pub fn blank(&self) -> bool {
        self.distinct <= 1
    }
}

impl std::fmt::Display for Uniformity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}x{} distinct={} dominant={:.3}",
            self.width, self.height, self.distinct, self.dominant
        )
    }
}

pub fn uniformity(texture: &gdk::Texture) -> Uniformity {
    let (width, height) = (
        texture.width().max(0) as usize,
        texture.height().max(0) as usize,
    );
    let stride = width * 4;
    let mut data = vec![0u8; stride * height];
    texture.download(&mut data, stride);
    let mut counts: HashMap<u32, usize> = HashMap::new();
    let mut sampled = 0usize;
    for y in (0..height).step_by(2) {
        for x in (0..width).step_by(2) {
            let at = y * stride + x * 4;
            let pixel = u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
            sampled += 1;
            if counts.len() < 4096 || counts.contains_key(&pixel) {
                *counts.entry(pixel).or_default() += 1;
            }
        }
    }
    let dominant = counts.values().copied().max().unwrap_or(0);
    Uniformity {
        width: width as i32,
        height: height as i32,
        distinct: counts.len(),
        dominant: if sampled == 0 {
            1.0
        } else {
            dominant as f64 / sampled as f64
        },
    }
}

/// What GTK would draw for `widget`: its render node through the window's renderer. For a
/// WebView this is the buffer the web process handed over, so a blank frame shows up here.
pub fn render_widget(widget: &impl IsA<gtk::Widget>) -> Result<gdk::Texture, String> {
    let widget = widget.as_ref();
    let paintable = gtk::WidgetPaintable::new(Some(widget));
    let snapshot = gtk::Snapshot::new();
    paintable.snapshot(
        &snapshot,
        f64::from(widget.width()),
        f64::from(widget.height()),
    );
    let node = snapshot.to_node().ok_or("no render node")?;
    let renderer = widget
        .native()
        .and_then(|native| native.renderer())
        .ok_or("no renderer")?;
    Ok(renderer.render_texture(&node, None))
}

pub fn renderer_name(widget: &impl IsA<gtk::Widget>) -> String {
    widget
        .as_ref()
        .native()
        .and_then(|native| native.renderer())
        .map_or_else(
            || "none".into(),
            |renderer| renderer.type_().name().to_owned(),
        )
}

pub fn display_name() -> String {
    gdk::Display::default().map_or_else(
        || "none".into(),
        |display| display.type_().name().to_owned(),
    )
}

pub fn fc_match(pattern: &str) -> String {
    std::process::Command::new("fc-match")
        .args(["-f", "%{family[0]}", pattern])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .filter(|family| !family.trim().is_empty())
        .unwrap_or_else(|| pattern.to_owned())
}

pub fn hyprland_rounding() -> Option<u32> {
    std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE")?;
    let output = std::process::Command::new("hyprctl")
        .args(["-j", "getoption", "decoration:rounding"])
        .output()
        .ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let at = text.find("\"int\":")? + 6;
    text[at..]
        .trim_start()
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

pub fn wait_for_size(widget: &impl IsA<gtk::Widget>) -> impl std::future::Future<Output = ()> {
    let widget = widget.as_ref().clone();
    async move {
        for _ in 0..200 {
            if widget.width() > 0 && widget.is_mapped() {
                return;
            }
            glib::timeout_future(std::time::Duration::from_millis(10)).await;
        }
    }
}
