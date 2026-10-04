use crate::js;
use crate::mime::{InlineImage, Mailbox};
use crate::probe::{Paint, PaintProbe};
use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::Instant;
use webkit6::javascriptcore::Value;
use webkit6::prelude::*;
use webkit6::{
    ContextMenuAction, EditorTypingAttributes, NetworkSession, PolicyDecisionType,
    UserContentFilter, UserContentManager, WebView,
};

pub const WORLD: &str = "mail-composer";
pub const HANDLER: &str = "composer";
pub const CSP: &str = "default-src 'none'; img-src data: cid:; style-src 'unsafe-inline'; \
                       font-src data:; form-action 'none'; base-uri 'none'; frame-src 'none'";
const HELPER: &str = include_str!("helper.js");
const EDITOR_HTML: &str = include_str!("editor.html");
const FILTER_JSON: &str = r#"[
  {"trigger": {"url-filter": "^https?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^wss?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^ftps?:"}, "action": {"type": "block"}},
  {"trigger": {"url-filter": "^file:"}, "action": {"type": "block"}}
]"#;

pub fn state_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.local/s7"))
}

pub async fn compile_filter() -> Result<UserContentFilter, String> {
    let dir = state_dir().join("content-filters");
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let store = webkit6::UserContentFilterStore::new(&dir.to_string_lossy());
    store
        .save_future(
            "mail-composer-block-remote",
            &glib::Bytes::from_static(FILTER_JSON.as_bytes()),
        )
        .await
        .map_err(|error| error.to_string())
}

/// Which of ADR-011's independent layers a view gets; the composer always gets all of them.
#[derive(Clone)]
pub struct Layers {
    pub csp: bool,
    pub filter: Option<UserContentFilter>,
}

fn hardened_settings() -> webkit6::Settings {
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript_markup(false);
    settings.set_enable_javascript(true);
    settings.set_javascript_can_open_windows_automatically(false);
    settings.set_javascript_can_access_clipboard(false);
    settings.set_enable_page_cache(false);
    settings.set_enable_html5_local_storage(false);
    settings.set_enable_html5_database(false);
    settings.set_enable_webgl(false);
    settings.set_enable_webaudio(false);
    settings.set_enable_media(false);
    settings.set_enable_media_stream(false);
    settings.set_enable_mediasource(false);
    settings.set_enable_encrypted_media(false);
    settings.set_enable_webrtc(false);
    settings.set_enable_fullscreen(false);
    settings.set_enable_back_forward_navigation_gestures(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);
    settings.set_allow_top_navigation_to_data_urls(false);
    settings.set_enable_developer_extras(cfg!(debug_assertions));
    settings.set_default_font_family("sans-serif");
    settings.set_default_font_size(15);
    settings
}

/// A WebView with the reader's hardening: no markup scripts, CSP at construction, the
/// content filter, an ephemeral session, every navigation and new window refused, permission
/// requests denied, and a context menu reduced to editing items. The helper runs in `WORLD`.
pub fn hardened_view(
    layers: &Layers,
    session: &NetworkSession,
    blocked_navigations: Rc<Cell<u32>>,
) -> (WebView, UserContentManager) {
    let manager = UserContentManager::new();
    manager.register_script_message_handler(HANDLER, Some(WORLD));
    manager.add_script(&webkit6::UserScript::for_world(
        HELPER,
        webkit6::UserContentInjectedFrames::TopFrame,
        webkit6::UserScriptInjectionTime::End,
        WORLD,
        &[],
        &[],
    ));
    if let Some(filter) = &layers.filter {
        manager.add_filter(filter);
    }
    let mut builder = WebView::builder()
        .network_session(session)
        .user_content_manager(&manager)
        .settings(&hardened_settings());
    if layers.csp {
        builder = builder.default_content_security_policy(CSP);
    }
    let view = builder.build();
    view.connect_decide_policy(move |_, decision, kind| {
        if !matches!(
            kind,
            PolicyDecisionType::NavigationAction | PolicyDecisionType::NewWindowAction
        ) {
            return false;
        }
        let uri = decision
            .downcast_ref::<webkit6::NavigationPolicyDecision>()
            .and_then(|decision| decision.navigation_action())
            .and_then(|action| action.request())
            .and_then(|request| request.uri())
            .unwrap_or_default();
        if kind == PolicyDecisionType::NavigationAction && uri == "about:blank" {
            decision.use_();
        } else {
            blocked_navigations.set(blocked_navigations.get() + 1);
            decision.ignore();
        }
        true
    });
    view.connect_create(|_, _| None);
    view.connect_permission_request(|_, request| {
        request.deny();
        true
    });
    view.connect_context_menu(|_, menu, _| {
        for item in menu.items() {
            let keep = item.is_separator()
                || matches!(
                    item.stock_action(),
                    ContextMenuAction::Cut
                        | ContextMenuAction::Copy
                        | ContextMenuAction::Paste
                        | ContextMenuAction::Delete
                        | ContextMenuAction::SelectAll
                        | ContextMenuAction::SpellingGuess
                        | ContextMenuAction::NoGuessesFound
                        | ContextMenuAction::IgnoreSpelling
                        | ContextMenuAction::LearnSpelling
                        | ContextMenuAction::InputMethods
                        | ContextMenuAction::Unicode
                        | ContextMenuAction::InsertEmoji
                );
            if !keep {
                menu.remove(&item);
            }
        }
        false
    });
    (view, manager)
}

pub fn ephemeral_session() -> NetworkSession {
    let session = NetworkSession::new_ephemeral();
    session.connect_download_started(|_, download| download.cancel());
    session
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Bold,
    Italic,
    Underline,
    Strikethrough,
    BulletedList,
    NumberedList,
    Link,
    Quote,
    Indent,
    Outdent,
    ClearFormatting,
    Undo,
    Redo,
}

impl Action {
    pub const TOOLBAR: [Action; 13] = [
        Action::Bold,
        Action::Italic,
        Action::Underline,
        Action::Strikethrough,
        Action::BulletedList,
        Action::NumberedList,
        Action::Link,
        Action::Quote,
        Action::Indent,
        Action::Outdent,
        Action::ClearFormatting,
        Action::Undo,
        Action::Redo,
    ];

    fn icon(self) -> Option<&'static str> {
        Some(match self {
            Action::Bold => "format-text-bold-symbolic",
            Action::Italic => "format-text-italic-symbolic",
            Action::Underline => "format-text-underline-symbolic",
            Action::Strikethrough => "format-text-strikethrough-symbolic",
            Action::BulletedList => "view-list-bullet-symbolic",
            Action::NumberedList => "view-list-ordered-symbolic",
            Action::Link => "insert-link-symbolic",
            Action::Quote => return None,
            Action::Indent => "format-indent-more-symbolic",
            Action::Outdent => "format-indent-less-symbolic",
            Action::ClearFormatting => "edit-clear-all-symbolic",
            Action::Undo => "edit-undo-symbolic",
            Action::Redo => "edit-redo-symbolic",
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Action::Bold => "Bold",
            Action::Italic => "Italic",
            Action::Underline => "Underline",
            Action::Strikethrough => "Strikethrough",
            Action::BulletedList => "Bulleted list",
            Action::NumberedList => "Numbered list",
            Action::Link => "Link",
            Action::Quote => "Quote",
            Action::Indent => "Indent",
            Action::Outdent => "Outdent",
            Action::ClearFormatting => "Clear formatting",
            Action::Undo => "Undo",
            Action::Redo => "Redo",
        }
    }

    fn toggles(self) -> bool {
        matches!(
            self,
            Action::Bold
                | Action::Italic
                | Action::Underline
                | Action::Strikethrough
                | Action::BulletedList
                | Action::NumberedList
                | Action::Link
                | Action::Quote
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockState {
    pub bulleted: bool,
    pub numbered: bool,
    pub quote: bool,
    pub indent: bool,
    pub link: String,
    pub dom_bold: bool,
    pub dom_italic: bool,
    pub dom_underline: bool,
    pub dom_strikethrough: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CleanStats {
    pub dropped: Vec<(String, i32)>,
    pub handlers: i32,
    pub styles: i32,
    pub attributes: i32,
    pub remote: i32,
    pub images: i32,
    pub links: i32,
    pub comments: i32,
    pub unwrapped: i32,
}

impl CleanStats {
    pub fn from_value(value: &Value) -> Self {
        let dropped = js::get(value, "dropped")
            .map(|map| {
                js::keys(&map)
                    .into_iter()
                    .map(|key| {
                        let count = js::number(&map, &key) as i32;
                        (key, count)
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            dropped,
            handlers: js::number(value, "handlers") as i32,
            styles: js::number(value, "styles") as i32,
            attributes: js::number(value, "attributes") as i32,
            remote: js::number(value, "remote") as i32,
            images: js::number(value, "images") as i32,
            links: js::number(value, "links") as i32,
            comments: js::number(value, "comments") as i32,
            unwrapped: js::number(value, "unwrapped") as i32,
        }
    }

    pub fn summary(&self) -> String {
        let dropped: Vec<String> = self
            .dropped
            .iter()
            .map(|(tag, count)| format!("{tag}×{count}"))
            .collect();
        format!(
            "dropped [{}], {} event handlers, {} style attributes, {} other attributes, \
             {} remote references, {} non-data images, {} unsafe links, {} comments, {} unwrapped",
            dropped.join(" "),
            self.handlers,
            self.styles,
            self.attributes,
            self.remote,
            self.images,
            self.links,
            self.comments,
            self.unwrapped
        )
    }
}

#[derive(Clone, Debug)]
pub struct Transfer {
    pub kind: String,
    pub mode: String,
    pub types: Vec<String>,
    pub files: i32,
    pub images: i32,
    pub attachments: i32,
    pub stats: Option<CleanStats>,
}

#[derive(Clone, Debug)]
pub enum Event {
    Ready {
        editable: bool,
    },
    State(BlockState),
    Transfer(Transfer),
    Attach {
        name: String,
        mime: String,
    },
    Adopted {
        mime: String,
        size: f64,
    },
    Input {
        input_type: String,
        data: String,
        composing: bool,
    },
    Error(String),
}

fn parse_event(value: &Value) -> Option<Event> {
    Some(match js::string(value, "type").as_str() {
        "ready" => Event::Ready {
            editable: js::boolean(value, "editable"),
        },
        "state" => Event::State(BlockState {
            bulleted: js::boolean(value, "bulleted"),
            numbered: js::boolean(value, "numbered"),
            quote: js::boolean(value, "quote"),
            indent: js::boolean(value, "indent"),
            link: js::string(value, "link"),
            dom_bold: js::boolean(value, "bold"),
            dom_italic: js::boolean(value, "italic"),
            dom_underline: js::boolean(value, "underline"),
            dom_strikethrough: js::boolean(value, "strikethrough"),
        }),
        kind @ ("paste" | "drop") => Event::Transfer(Transfer {
            kind: kind.to_string(),
            mode: js::string(value, "mode"),
            types: js::array(value, "types")
                .iter()
                .map(|kind| kind.to_str().to_string())
                .collect(),
            files: js::number(value, "files") as i32,
            images: js::number(value, "images") as i32,
            attachments: js::number(value, "attachments") as i32,
            stats: js::get(value, "stats").map(|stats| CleanStats::from_value(&stats)),
        }),
        "attach" => Event::Attach {
            name: js::string(value, "name"),
            mime: js::string(value, "mime"),
        },
        "input" => Event::Input {
            input_type: js::string(value, "inputType"),
            data: js::string(value, "data"),
            composing: js::boolean(value, "composing"),
        },
        "adopted" => Event::Adopted {
            mime: js::string(value, "mime"),
            size: js::number(value, "size"),
        },
        "error" => Event::Error(js::string(value, "message")),
        _ => return None,
    })
}

pub struct Fields {
    pub grid: gtk4::Grid,
    pub to: gtk4::Entry,
    pub cc: gtk4::Entry,
    pub bcc: gtk4::Entry,
    pub subject: gtk4::Entry,
    pub from: Mailbox,
}

impl Fields {
    fn new() -> Self {
        let grid = gtk4::Grid::builder()
            .row_spacing(4)
            .column_spacing(8)
            .margin_start(10)
            .margin_end(10)
            .margin_top(8)
            .margin_bottom(6)
            .build();
        let from = crate::fixtures::sender();
        let (to, cc, bcc) = crate::fixtures::recipients();
        let format = |list: &[Mailbox]| {
            list.iter()
                .map(|mailbox| match &mailbox.name {
                    Some(name) => format!("{name} <{}>", mailbox.address),
                    None => mailbox.address.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let entry = |text: &str| gtk4::Entry::builder().text(text).hexpand(true).build();
        let fields = Self {
            to: entry(&format(&to)),
            cc: entry(&format(&cc)),
            bcc: entry(&format(&bcc)),
            subject: entry(crate::fixtures::NORWEGIAN_SUBJECT),
            grid,
            from,
        };
        let from_label = gtk4::Label::builder()
            .label(format(std::slice::from_ref(&fields.from)))
            .xalign(0.0)
            .build();
        let rows: [(&str, gtk4::Widget); 5] = [
            ("From", from_label.upcast()),
            ("To", fields.to.clone().upcast()),
            ("Cc", fields.cc.clone().upcast()),
            ("Bcc", fields.bcc.clone().upcast()),
            ("Subject", fields.subject.clone().upcast()),
        ];
        for (row, (label, widget)) in rows.into_iter().enumerate() {
            let label = gtk4::Label::builder().label(label).xalign(1.0).build();
            label.add_css_class("dim-label");
            fields.grid.attach(&label, 0, row as i32, 1, 1);
            fields.grid.attach(&widget, 1, row as i32, 1, 1);
        }
        fields
    }

    pub fn mailboxes(entry: &gtk4::Entry) -> Vec<Mailbox> {
        entry
            .text()
            .split(',')
            .filter_map(|item| {
                let item = item.trim();
                if item.is_empty() {
                    return None;
                }
                Some(match item.rsplit_once('<') {
                    Some((name, address)) => Mailbox {
                        name: Some(name.trim().trim_matches('"').to_string())
                            .filter(|name| !name.is_empty()),
                        address: address.trim_end_matches('>').trim().to_string(),
                    },
                    None => Mailbox {
                        name: None,
                        address: item.to_string(),
                    },
                })
            })
            .collect()
    }
}

pub struct Composer {
    pub root: gtk4::Box,
    pub view: WebView,
    pub probe: PaintProbe,
    pub paints: async_channel::Receiver<Paint>,
    pub events: async_channel::Receiver<Event>,
    pub fields: Fields,
    pub block: RefCell<BlockState>,
    pub typing: Cell<u32>,
    pub typing_changed_at: Cell<Option<Instant>>,
    pub editor_changed_at: Cell<Option<Instant>>,
    pub blocked_navigations: Rc<Cell<u32>>,
    toggles: HashMap<Action, gtk4::ToggleButton>,
    buttons: HashMap<Action, gtk4::Button>,
    link_popover: gtk4::Popover,
    link_entry: gtk4::Entry,
}

impl Composer {
    /// `accelerated: false` sets WebKit's hardware-acceleration-policy to NEVER for this view.
    pub fn new(
        filter: &UserContentFilter,
        session: &NetworkSession,
        accelerated: bool,
    ) -> Rc<Self> {
        let blocked_navigations = Rc::new(Cell::new(0));
        let layers = Layers {
            csp: true,
            filter: Some(filter.clone()),
        };
        let (view, manager) = hardened_view(&layers, session, blocked_navigations.clone());
        if !accelerated && let Some(settings) = WebViewExt::settings(&view) {
            settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
        }
        view.set_editable(true);
        view.set_hexpand(true);
        view.set_vexpand(true);
        let (probe, paints) = PaintProbe::new(&view);
        probe.set_hexpand(true);
        probe.set_vexpand(true);

        let toolbar = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(2)
            .margin_start(8)
            .margin_end(8)
            .margin_bottom(6)
            .build();
        let mut toggles = HashMap::new();
        let mut buttons = HashMap::new();
        for action in Action::TOOLBAR {
            let button: gtk4::Button = if action.toggles() {
                let toggle = gtk4::ToggleButton::new();
                toggles.insert(action, toggle.clone());
                toggle.upcast()
            } else {
                gtk4::Button::new()
            };
            match action.icon() {
                Some(icon) => button.set_icon_name(icon),
                None => button.set_label(action.label()),
            }
            button.set_tooltip_text(Some(action.label()));
            button.set_focus_on_click(false);
            button.add_css_class("flat");
            if matches!(action, Action::Link | Action::Indent | Action::Undo) {
                toolbar.append(&gtk4::Separator::new(gtk4::Orientation::Vertical));
            }
            toolbar.append(&button);
            buttons.insert(action, button);
        }
        let link_entry = gtk4::Entry::builder()
            .placeholder_text("https://example.org/")
            .width_chars(32)
            .build();
        let link_popover = gtk4::Popover::builder().child(&link_entry).build();
        link_popover.set_parent(&buttons[&Action::Link]);

        let fields = Fields::new();
        let root = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        root.append(&fields.grid);
        root.append(&toolbar);
        root.append(&gtk4::Separator::new(gtk4::Orientation::Horizontal));
        root.append(&probe);

        let (sender, events) = async_channel::unbounded();
        let composer = Rc::new(Self {
            root,
            view,
            probe,
            paints,
            events,
            fields,
            block: RefCell::new(BlockState::default()),
            typing: Cell::new(0),
            typing_changed_at: Cell::new(None),
            editor_changed_at: Cell::new(None),
            blocked_navigations,
            toggles,
            buttons,
            link_popover,
            link_entry,
        });
        composer.connect(&manager, sender);
        composer.update_history(false, false);
        composer.view.load_html(EDITOR_HTML, None);
        composer
    }

    fn connect(
        self: &Rc<Self>,
        manager: &UserContentManager,
        sender: async_channel::Sender<Event>,
    ) {
        let weak = Rc::downgrade(self);
        manager.connect_script_message_received(Some(HANDLER), move |_, value| {
            let Some(event) = parse_event(value) else {
                return;
            };
            if let (Event::State(state), Some(composer)) = (&event, weak.upgrade()) {
                composer.block_changed(state.clone());
            }
            let _ = sender.try_send(event);
        });
        if let Some(editor) = self.view.editor_state() {
            let weak = Rc::downgrade(self);
            editor.connect_typing_attributes_notify(move |editor| {
                if let Some(composer) = weak.upgrade() {
                    composer.typing_changed(editor.typing_attributes());
                }
            });
            let weak = Rc::downgrade(self);
            editor.connect_changed(move |editor| {
                if let Some(composer) = weak.upgrade() {
                    composer.editor_changed_at.set(Some(Instant::now()));
                    composer.update_history(editor.is_undo_available(), editor.is_redo_available());
                }
            });
        }
        for (action, button) in &self.buttons {
            let weak: Weak<Self> = Rc::downgrade(self);
            let action = *action;
            button.connect_clicked(move |_| {
                if let Some(composer) = weak.upgrade() {
                    composer.run(action);
                }
            });
        }
        let weak = Rc::downgrade(self);
        self.link_entry.connect_activate(move |entry| {
            if let Some(composer) = weak.upgrade() {
                let url = entry.text().trim().to_string();
                if url.starts_with("https://")
                    || url.starts_with("http://")
                    || url.starts_with("mailto:")
                {
                    composer.create_link(&url);
                }
                composer.link_popover.popdown();
                composer.view.grab_focus();
            }
        });
    }

    fn typing_changed(&self, attributes: u32) {
        self.typing.set(attributes);
        self.typing_changed_at.set(Some(Instant::now()));
        let flags = EditorTypingAttributes::from_bits_truncate(attributes);
        for (action, flag) in [
            (Action::Bold, EditorTypingAttributes::BOLD),
            (Action::Italic, EditorTypingAttributes::ITALIC),
            (Action::Underline, EditorTypingAttributes::UNDERLINE),
            (Action::Strikethrough, EditorTypingAttributes::STRIKETHROUGH),
        ] {
            self.toggles[&action].set_active(flags.contains(flag));
        }
    }

    fn block_changed(&self, state: BlockState) {
        self.toggles[&Action::BulletedList].set_active(state.bulleted);
        self.toggles[&Action::NumberedList].set_active(state.numbered);
        self.toggles[&Action::Quote].set_active(state.quote);
        self.toggles[&Action::Link].set_active(!state.link.is_empty());
        self.block.replace(state);
    }

    fn update_history(&self, undo: bool, redo: bool) {
        self.buttons[&Action::Undo].set_sensitive(undo);
        self.buttons[&Action::Redo].set_sensitive(redo);
    }

    pub fn toggle_active(&self, action: Action) -> bool {
        self.toggles
            .get(&action)
            .is_some_and(|toggle| toggle.is_active())
    }

    pub fn button_sensitive(&self, action: Action) -> bool {
        self.buttons[&action].is_sensitive()
    }

    pub fn typing_has(&self, flag: EditorTypingAttributes) -> bool {
        EditorTypingAttributes::from_bits_truncate(self.typing.get()).contains(flag)
    }

    /// The toolbar's commands, all WebKit editing commands.
    pub fn run(&self, action: Action) {
        let view = &self.view;
        let block = self.block.borrow().clone();
        match action {
            Action::Bold => view.execute_editing_command("Bold"),
            Action::Italic => view.execute_editing_command("Italic"),
            Action::Underline => view.execute_editing_command("Underline"),
            Action::Strikethrough => view.execute_editing_command("Strikethrough"),
            Action::BulletedList => view.execute_editing_command("InsertUnorderedList"),
            Action::NumberedList => view.execute_editing_command("InsertOrderedList"),
            Action::Link if !block.link.is_empty() => {
                let view = view.clone();
                glib::MainContext::default().spawn_local(async move {
                    let selected = view
                        .evaluate_javascript_future("mailComposer.selectLink()", Some(WORLD), None)
                        .await
                        .is_ok_and(|value| value.to_boolean());
                    if selected {
                        view.execute_editing_command("Unlink");
                    }
                });
            }
            Action::Link => {
                self.link_entry.set_text("");
                self.link_popover.popup();
                return;
            }
            Action::Quote if block.quote => view.execute_editing_command("Outdent"),
            Action::Quote => {
                view.execute_editing_command_with_argument("FormatBlock", "blockquote")
            }
            Action::Indent => view.execute_editing_command("Indent"),
            Action::Outdent => view.execute_editing_command("Outdent"),
            Action::ClearFormatting => view.execute_editing_command("RemoveFormat"),
            Action::Undo => view.execute_editing_command(webkit6::EDITING_COMMAND_UNDO),
            Action::Redo => view.execute_editing_command(webkit6::EDITING_COMMAND_REDO),
        }
        view.grab_focus();
    }

    pub fn create_link(&self, url: &str) {
        self.view
            .execute_editing_command_with_argument("CreateLink", url);
    }

    pub async fn eval(&self, script: &str) -> Result<Value, String> {
        self.view
            .evaluate_javascript_future(script, Some(WORLD), None)
            .await
            .map_err(|error| error.to_string())
    }

    pub fn eval_detached(&self, script: &str) {
        self.view
            .evaluate_javascript(script, Some(WORLD), None, None::<&gio::Cancellable>, |_| {});
    }

    pub async fn html(&self) -> String {
        self.eval("mailComposer.html()")
            .await
            .map(|value| value.to_str().to_string())
            .unwrap_or_default()
    }

    /// The outgoing HTML fragment with `cid:` references, and the inline images.
    pub async fn serialize(&self, domain: &str) -> Result<(String, Vec<InlineImage>), String> {
        let value = self
            .eval(&format!("mailComposer.serialize({})", js::literal(domain)))
            .await?;
        let mut images = Vec::new();
        for (index, image) in js::array(&value, "images").iter().enumerate() {
            let mime_type = js::string(image, "mime");
            let data =
                mail_parser::decoders::base64::base64_decode(js::string(image, "data").as_bytes())
                    .ok_or("an inline image is not valid base64")?;
            let name = js::string(image, "name");
            let extension = mime_type
                .rsplit('/')
                .next()
                .unwrap_or("bin")
                .replace("jpeg", "jpg");
            images.push(InlineImage {
                content_id: js::string(image, "cid"),
                filename: Some(if name.is_empty() {
                    format!("image-{}.{extension}", index + 1)
                } else {
                    name
                }),
                mime_type,
                data,
            });
        }
        Ok((js::string(&value, "html"), images))
    }
}

impl Drop for Composer {
    fn drop(&mut self) {
        self.link_popover.unparent();
    }
}
