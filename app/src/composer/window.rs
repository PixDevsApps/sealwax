//! One composer window (ML-067, ML-071, ML-072, ML-079): the approved mock's "New message"
//! frame. The header has the title, the From chip, Attach and Discard; then the To row with
//! "Cc · Bcc" (Cc and Bcc hidden until asked for), the subject, the formatting toolbar that
//! follows the editor's state, the body, and the footer with the attachments, "Draft saved"
//! and Send. The window's own `win.` actions run the registry's Composer commands, and the
//! Composer scope's keys reach them from the fields (bubble phase, so entries keep their own
//! keys) and from the body (capture phase, before WebKit: Ctrl+B and Ctrl+I toggle once, and
//! Ctrl+Z is the editor's undo).
//!
//! The window shows at once; the draft fills it when the backend has prepared it. It is saved
//! 1.5 s after the typing stops and at least every 4 s while it goes on (PRD C5: at most 5 s
//! behind), and when the window closes; the window stays hidden until that last save is done,
//! holding the application.

use super::attachments::{self, Tray};
use super::backend::{Backend, Prepared, Refusal, Sender, id_domain};
use super::editor::{BlockState, Editor, EditorEngine, EditorEvent, InlineImage};
use super::recipients::{LABEL_WIDTH, RecipientField};
use super::{ComposeRequest, chooser_answer};
use crate::actions::{self, ActionSet, ScopedShortcuts};
use crate::application::SealwaxApplication;
use crate::palette;
use crate::theme::ListenerId;
use crate::window::panes::{classed, icon, icon_button, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use sealwax_domain::actions::{ActionId, Scope};
use sealwax_domain::compose::{Draft, DraftPart};
use sealwax_domain::keymap::Keymap;
use sealwax_domain::model::{AccountId, DraftId};
use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The mock's composer frame.
pub const DEFAULT_SIZE: (i32, i32) = (1000, 820);
/// The draft is saved this long after the last edit…
pub const IDLE_SAVE: Duration = Duration::from_millis(1500);
/// …and at most this long after the first unsaved one (PRD C5: at most 5 s behind).
pub const MAX_BEHIND: Duration = Duration::from_secs(4);

/// The formatting toolbar after the style menu: the registry's commands and their icons, a
/// separator before each group.
const FORMAT_GROUPS: [&[(ActionId, &[&str])]; 2] = [
    &[
        (ActionId::Bold, &["format-text-bold-symbolic"]),
        (ActionId::Italic, &["format-text-italic-symbolic"]),
        (ActionId::Underline, &["format-text-underline-symbolic"]),
        (
            ActionId::Strikethrough,
            &["format-text-strikethrough-symbolic"],
        ),
    ],
    &[
        (ActionId::BulletedList, &["view-list-bullet-symbolic"]),
        (ActionId::NumberedList, &["view-list-ordered-symbolic"]),
        (ActionId::InsertLink, &["insert-link-symbolic"]),
        (
            ActionId::Quote,
            &[
                "format-quote-symbolic",
                "quotation-symbolic",
                "mail-reply-sender-symbolic",
            ],
        ),
        (ActionId::Indent, &["format-indent-more-symbolic"]),
        (ActionId::Outdent, &["format-indent-less-symbolic"]),
        (
            ActionId::ClearFormatting,
            &[
                "format-text-remove-formatting-symbolic",
                "edit-clear-all-symbolic",
            ],
        ),
    ],
];

/// The paragraph styles: WebKit's FormatBlock tag and the menu's name.
pub const STYLES: [(&str, &str); 5] = [
    ("p", "Paragraph"),
    ("h1", "Heading 1"),
    ("h2", "Heading 2"),
    ("h3", "Heading 3"),
    ("pre", "Preformatted"),
];

/// Every command the window runs.
const COMMANDS: [ActionId; 20] = [
    ActionId::Send,
    ActionId::AttachFile,
    ActionId::ShowCcBcc,
    ActionId::DiscardDraft,
    ActionId::Bold,
    ActionId::Italic,
    ActionId::Underline,
    ActionId::Strikethrough,
    ActionId::BulletedList,
    ActionId::NumberedList,
    ActionId::InsertLink,
    ActionId::Quote,
    ActionId::Indent,
    ActionId::Outdent,
    ActionId::ClearFormatting,
    ActionId::ComposerUndo,
    ActionId::ComposerRedo,
    ActionId::PasteAsText,
    ActionId::Compose,
    ActionId::CommandPalette,
];

/// The commands that need the editor page loaded.
const EDITING: [ActionId; 14] = [
    ActionId::Bold,
    ActionId::Italic,
    ActionId::Underline,
    ActionId::Strikethrough,
    ActionId::BulletedList,
    ActionId::NumberedList,
    ActionId::InsertLink,
    ActionId::Quote,
    ActionId::Indent,
    ActionId::Outdent,
    ActionId::ClearFormatting,
    ActionId::ComposerUndo,
    ActionId::ComposerRedo,
    ActionId::PasteAsText,
];

/// How the window ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    Open,
    /// Closed; the draft is kept.
    Closed,
    Sent,
    Discarded,
}

/// A composer window.
#[derive(Clone)]
pub struct Composer {
    inner: Rc<Inner>,
}

struct Widgets {
    title: gtk::Label,
    from: gtk::MenuButton,
    from_label: gtk::Label,
    from_list: gtk::ListBox,
    to: Rc<RecipientField>,
    cc: Rc<RecipientField>,
    bcc: Rc<RecipientField>,
    cc_button: gtk::Button,
    subject: gtk::Entry,
    style: gtk::MenuButton,
    style_label: gtk::Label,
    format: Vec<(ActionId, gtk::Button)>,
    body: gtk::Stack,
    problem: gtk::Label,
    tray: Rc<Tray>,
    warning: gtk::Label,
    saved: gtk::Label,
    toasts: adw::ToastOverlay,
}

struct Inner {
    app: glib::WeakRef<SealwaxApplication>,
    window: adw::ApplicationWindow,
    widgets: Widgets,
    editor: Editor,
    backend: Rc<dyn Backend>,
    actions: ActionSet,
    shortcuts: Vec<ScopedShortcuts>,
    keymap: RefCell<Keymap>,
    id: Cell<Option<DraftId>>,
    account: Cell<AccountId>,
    /// The prepared draft: what the window doesn't edit (Message-ID, threading, source).
    base: RefCell<Option<Draft>>,
    senders: RefCell<Vec<Sender>>,
    sender: Cell<usize>,
    /// The right-hand side of new Content-IDs, fixed when the draft opens.
    domain: RefCell<String>,
    limit: Cell<Option<u64>>,
    /// The body is loaded: edits from here on count.
    loaded: Cell<bool>,
    dirty: Cell<bool>,
    first_dirty: Cell<Option<Instant>>,
    last_edit: Cell<Option<Instant>>,
    edits: RefCell<Vec<Instant>>,
    saving: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    ending: Cell<Ending>,
    closing: Cell<bool>,
    ran: RefCell<Vec<ActionId>>,
    theme_listener: Cell<Option<ListenerId>>,
    /// Images taken from the clipboard or a drop, by the helper's token, not yet stored.
    adopting: RefCell<HashSet<String>>,
}

impl Composer {
    /// Builds the window for `request`, shows it at once and fills it when the draft is ready.
    pub fn open(
        app: &SealwaxApplication,
        backend: Rc<dyn Backend>,
        request: ComposeRequest,
    ) -> Composer {
        let state = app.state();
        let theme_controller = state.theme();
        let theme = theme_controller
            .as_ref()
            .map(|controller| controller.theme())
            .unwrap_or_else(|| {
                Rc::new(sealwax_domain::theme::Theme::new(
                    &sealwax_domain::theme::ThemeInputs::builtin(
                        sealwax_domain::theme::Mode::from_dark(
                            adw::StyleManager::default().is_dark(),
                        ),
                    ),
                    &sealwax_domain::theme::ThemeOptions::default(),
                ))
            });
        let keymap = state
            .preferences()
            .map(|prefs| prefs.keymap())
            .unwrap_or_default();
        let engine = EditorEngine::shared(&state.config().dirs.cache);
        engine.set_blobs(backend.blobs());
        let editor = Editor::new(&engine, &theme);
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("New message")
            .default_width(DEFAULT_SIZE.0)
            .default_height(DEFAULT_SIZE.1)
            .build();
        window.add_css_class("sw-composer");
        let widgets = build(&window, &editor, &keymap, state.config().tiling);
        let actions = actions::install_window_actions(&window);
        let window_keys = ScopedShortcuts::new(Scope::Composer, &keymap);
        window_keys.attach(&window, gtk::PropagationPhase::Bubble);
        let body_keys = ScopedShortcuts::new(Scope::Composer, &keymap);
        body_keys.attach(editor.view(), gtk::PropagationPhase::Capture);
        let inner = Rc::new(Inner {
            app: app.downgrade(),
            window,
            widgets,
            editor,
            backend,
            actions,
            shortcuts: vec![window_keys, body_keys],
            keymap: RefCell::new(keymap),
            id: Cell::new(None),
            account: Cell::new(AccountId::default()),
            base: RefCell::new(None),
            senders: RefCell::new(Vec::new()),
            sender: Cell::new(0),
            domain: RefCell::new(String::new()),
            limit: Cell::new(None),
            loaded: Cell::new(false),
            dirty: Cell::new(false),
            first_dirty: Cell::new(None),
            last_edit: Cell::new(None),
            edits: RefCell::new(Vec::new()),
            saving: Cell::new(false),
            timer: RefCell::new(None),
            ending: Cell::new(Ending::Open),
            closing: Cell::new(false),
            ran: RefCell::new(Vec::new()),
            theme_listener: Cell::new(None),
            adopting: RefCell::new(HashSet::new()),
        });
        if let Some(controller) = theme_controller {
            let weak = Rc::downgrade(&inner);
            let id = controller.connect_changed(move |theme| {
                if let Some(inner) = weak.upgrade() {
                    inner.editor.set_theme(theme);
                }
            });
            inner.theme_listener.set(Some(id));
        }
        inner.connect();
        inner.window.present();
        let starting = inner.clone();
        glib::spawn_future_local(async move { starting.start(request).await });
        Composer { inner }
    }

    pub fn window(&self) -> &adw::ApplicationWindow {
        &self.inner.window
    }

    pub fn editor(&self) -> &Editor {
        &self.inner.editor
    }

    pub fn id(&self) -> Option<DraftId> {
        self.inner.id.get()
    }

    pub fn ending(&self) -> Ending {
        self.inner.ending.get()
    }

    pub fn is_loaded(&self) -> bool {
        self.inner.loaded.get()
    }

    pub fn tray(&self) -> &Rc<Tray> {
        &self.inner.widgets.tray
    }

    /// The recipient row `to`, `cc` or `bcc`.
    pub fn field(&self, name: &str) -> Option<&Rc<RecipientField>> {
        let widgets = &self.inner.widgets;
        match name {
            "to" => Some(&widgets.to),
            "cc" => Some(&widgets.cc),
            "bcc" => Some(&widgets.bcc),
            _ => None,
        }
    }

    pub fn subject(&self) -> &gtk::Entry {
        &self.inner.widgets.subject
    }

    pub fn sender(&self) -> Option<Sender> {
        self.inner
            .senders
            .borrow()
            .get(self.inner.sender.get())
            .cloned()
    }

    pub fn select_sender(&self, index: usize) {
        self.inner.choose_sender(index);
    }

    pub fn warning(&self) -> Option<String> {
        let warning = &self.inner.widgets.warning;
        warning.is_visible().then(|| warning.label().to_string())
    }

    pub fn saved_label(&self) -> Option<String> {
        let saved = &self.inner.widgets.saved;
        saved.is_visible().then(|| saved.label().to_string())
    }

    /// When each edit (of the body or a field) happened.
    pub fn edits(&self) -> Vec<Instant> {
        self.inner.edits.borrow().clone()
    }

    /// The commands this window ran, oldest first.
    pub fn ran(&self) -> Vec<ActionId> {
        self.inner.ran.borrow().clone()
    }

    pub fn actions(&self) -> &ActionSet {
        &self.inner.actions
    }

    /// The toolbar's button for `id`.
    pub fn format_button(&self, id: ActionId) -> Option<gtk::Button> {
        self.inner
            .widgets
            .format
            .iter()
            .find(|(action, _)| *action == id)
            .map(|(_, button)| button.clone())
    }

    pub fn style_label(&self) -> String {
        self.inner.widgets.style_label.label().to_string()
    }

    /// A named widget of the window.
    pub fn widget(&self, name: &str) -> Option<gtk::Widget> {
        let mut found = None;
        find(self.inner.window.upcast_ref(), name, &mut found);
        found
    }

    pub fn toasts(&self) -> &adw::ToastOverlay {
        &self.inner.widgets.toasts
    }

    /// The draft as it would be saved now.
    pub async fn draft(&self) -> Option<Draft> {
        self.inner.collect().await
    }

    /// Takes `paths` as files dropped on the window: over the body, images go inline.
    pub fn drop_files(&self, paths: Vec<PathBuf>, over_body: bool) {
        self.inner.drop_files(paths, over_body);
    }

    /// Takes the keys of `keymap` (keys.toml changed).
    pub fn update_keys(&self, keymap: &Keymap) {
        for shortcuts in &self.inner.shortcuts {
            shortcuts.update(keymap);
        }
        self.inner.keymap.replace(keymap.clone());
    }

    /// Saves the draft now and lets the window go: for Quit, while every window is about to
    /// be destroyed.
    pub async fn save_and_release(&self) {
        let inner = &self.inner;
        if inner.ending.get() != Ending::Open {
            return;
        }
        inner.cancel_timer();
        inner.save().await;
        if let Some(id) = inner.id.get() {
            inner.backend.close(id);
        }
        inner.ending.set(Ending::Closed);
    }
}

fn find(widget: &gtk::Widget, name: &str, found: &mut Option<gtk::Widget>) {
    if found.is_some() {
        return;
    }
    if widget.widget_name() == name {
        *found = Some(widget.clone());
        return;
    }
    if let Some(popover) = widget
        .downcast_ref::<gtk::MenuButton>()
        .and_then(|button| button.popover())
    {
        find(popover.upcast_ref(), name, found);
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        find(&current, name, found);
        child = current.next_sibling();
    }
}

fn separator() -> gtk::Separator {
    classed(
        gtk::Separator::new(gtk::Orientation::Vertical),
        &["sw-composer-separator"],
    )
}

fn key_text(keymap: &Keymap, id: ActionId) -> String {
    keymap
        .shown(id)
        .first()
        .map(|binding| binding.display())
        .unwrap_or_default()
}

fn build(
    window: &adw::ApplicationWindow,
    editor: &Editor,
    keymap: &Keymap,
    tiling: bool,
) -> Widgets {
    // The header: the title, the From chip, Attach and Discard.
    let title = named(
        classed(
            gtk::Label::builder().label("New message").build(),
            &["sw-composer-title"],
        ),
        "composer-title",
    );
    let dot = classed(gtk::Label::new(Some("·")), &["sw-composer-dot"]);
    let from_caption = classed(gtk::Label::new(Some("From")), &["sw-composer-from-caption"]);
    let from_label = classed(
        gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(48)
            .build(),
        &["sw-composer-from-label"],
    );
    let from_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    from_box.append(&from_caption);
    from_box.append(&from_label);
    from_box.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
    let from_list = classed(
        gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build(),
        &["sw-composer-from-list"],
    );
    let from = named(
        classed(
            gtk::MenuButton::builder()
                .child(&from_box)
                .popover(
                    &gtk::Popover::builder()
                        .child(&from_list)
                        .has_arrow(false)
                        .build(),
                )
                .tooltip_text("Send from")
                .valign(gtk::Align::Center)
                .sensitive(false)
                .build(),
            &["sw-composer-from"],
        ),
        "composer-from",
    );
    from.update_property(&[gtk::accessible::Property::Label("Send from")]);
    let attach = named(
        icon_button(
            &["mail-attachment-symbolic", "paperclip-symbolic"],
            ActionId::AttachFile.title(),
        ),
        "composer-attach",
    );
    attach.set_action_name(Some(&ActionId::AttachFile.detailed_name()));
    let discard = named(
        icon_button(&["user-trash-symbolic"], ActionId::DiscardDraft.title()),
        "composer-discard",
    );
    discard.set_action_name(Some(&ActionId::DiscardDraft.detailed_name()));
    let header = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build(),
        &["sw-composer-header"],
    );
    header.append(&title);
    header.append(&dot);
    header.append(&from);
    header.append(&gtk::Box::builder().hexpand(true).build());
    header.append(&attach);
    header.append(&discard);
    if !tiling {
        header.append(&gtk::WindowControls::new(gtk::PackType::End));
    }
    let handle = gtk::WindowHandle::builder().child(&header).build();

    // The fields.
    let to = RecipientField::new("To", "composer-to");
    let cc = RecipientField::new("Cc", "composer-cc");
    let bcc = RecipientField::new("Bcc", "composer-bcc");
    let cc_button = named(
        classed(
            gtk::Button::builder()
                .label("Cc · Bcc")
                .tooltip_text(ActionId::ShowCcBcc.title())
                .valign(gtk::Align::Center)
                .build(),
            &["sw-composer-ccbcc"],
        ),
        "composer-ccbcc",
    );
    cc_button.set_action_name(Some(&ActionId::ShowCcBcc.detailed_name()));
    to.append_end(&cc_button);
    cc.widget().set_visible(false);
    bcc.widget().set_visible(false);
    let subject = named(
        classed(
            gtk::Entry::builder().hexpand(true).has_frame(false).build(),
            &["sw-composer-subject"],
        ),
        "composer-subject",
    );
    subject.update_property(&[gtk::accessible::Property::Label("Subject")]);
    let subject_row = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build(),
        &["sw-composer-row"],
    );
    subject_row.append(&classed(
        gtk::Label::builder()
            .label("Subject")
            .xalign(0.0)
            .width_request(LABEL_WIDTH)
            .build(),
        &["sw-composer-field-label"],
    ));
    subject_row.append(&subject);
    let fields = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .build(),
        &["sw-composer-fields"],
    );
    fields.append(to.widget());
    fields.append(cc.widget());
    fields.append(bcc.widget());
    fields.append(&subject_row);

    // The toolbar.
    let style_label = gtk::Label::new(Some("Paragraph"));
    let style_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    style_box.append(&style_label);
    style_box.append(&gtk::Image::from_icon_name("pan-down-symbolic"));
    let styles = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    let style_popover = classed(
        gtk::Popover::builder()
            .child(&styles)
            .has_arrow(false)
            .build(),
        &["menu", "sw-composer-styles"],
    );
    for (tag, name) in STYLES {
        let button = named(
            classed(
                gtk::Button::builder().label(name).build(),
                &["flat", "sw-composer-style"],
            ),
            &format!("composer-style-{tag}"),
        );
        if let Some(label) = button.child().and_downcast::<gtk::Label>() {
            label.set_xalign(0.0);
        }
        button.set_action_name(Some("composer.style"));
        button.set_action_target_value(Some(&tag.to_variant()));
        styles.append(&button);
    }
    let style = named(
        classed(
            gtk::MenuButton::builder()
                .child(&style_box)
                .popover(&style_popover)
                .tooltip_text("Paragraph style")
                .build(),
            &["sw-composer-style-button"],
        ),
        "composer-style",
    );
    style.update_property(&[gtk::accessible::Property::Label("Paragraph style")]);
    let toolbar = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build(),
        &["sw-composer-toolbar"],
    );
    toolbar.append(&style);
    let mut format = Vec::new();
    for group in FORMAT_GROUPS {
        toolbar.append(&separator());
        for (id, icons) in group {
            let button = named(
                classed(
                    gtk::Button::builder()
                        .child(&icon(icons))
                        .tooltip_text(crate::reader::tooltip(keymap, *id, id.title(), None))
                        .focus_on_click(false)
                        .build(),
                    &["sw-icon-button", "sw-composer-format"],
                ),
                &format!("composer-{}", id.name()),
            );
            button.update_property(&[gtk::accessible::Property::Label(id.title())]);
            button.set_action_name(Some(&id.detailed_name()));
            toolbar.append(&button);
            format.push((*id, button));
        }
    }
    toolbar.append(&gtk::Box::builder().hexpand(true).build());
    let undo_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(6)
        .build();
    undo_box.append(&gtk::Label::new(Some("Undo")));
    undo_box.append(&classed(
        gtk::Label::new(Some(&key_text(keymap, ActionId::ComposerUndo))),
        &["sw-composer-key-hint"],
    ));
    let undo = named(
        classed(
            gtk::Button::builder()
                .child(&undo_box)
                .tooltip_text(crate::reader::tooltip(
                    keymap,
                    ActionId::ComposerUndo,
                    ActionId::ComposerUndo.title(),
                    None,
                ))
                .focus_on_click(false)
                .build(),
            &["flat", "sw-composer-undo"],
        ),
        "composer-undo",
    );
    undo.set_action_name(Some(&ActionId::ComposerUndo.detailed_name()));
    toolbar.append(&undo);
    format.push((ActionId::ComposerUndo, undo));

    // The body: blank while the draft is prepared, the editor, or why it can't open.
    let problem = classed(
        gtk::Label::builder()
            .wrap(true)
            .justify(gtk::Justification::Center)
            .valign(gtk::Align::Center)
            .build(),
        &["sw-composer-problem"],
    );
    let body = gtk::Stack::builder().vexpand(true).hexpand(true).build();
    body.add_named(
        &gtk::Box::new(gtk::Orientation::Vertical, 0),
        Some("loading"),
    );
    body.add_named(editor.view(), Some("editor"));
    body.add_named(&problem, Some("problem"));
    body.set_visible_child_name("loading");

    // The footer.
    let tray = Tray::new();
    let warning = named(
        classed(
            gtk::Label::builder()
                .wrap(true)
                .xalign(0.0)
                .visible(false)
                .max_width_chars(48)
                .build(),
            &["sw-composer-warning"],
        ),
        "composer-warning",
    );
    let saved = named(
        classed(
            gtk::Label::builder().visible(false).build(),
            &["sw-composer-saved"],
        ),
        "composer-saved",
    );
    let send_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(8)
        .build();
    send_box.append(&classed(
        gtk::Label::new(Some("Send")),
        &["sw-composer-send-label"],
    ));
    send_box.append(&classed(
        gtk::Label::new(Some(&key_text(keymap, ActionId::Send))),
        &["sw-composer-send-key"],
    ));
    let send = named(
        classed(
            gtk::Button::builder()
                .child(&send_box)
                .tooltip_text(crate::reader::tooltip(
                    keymap,
                    ActionId::Send,
                    ActionId::Send.title(),
                    None,
                ))
                .valign(gtk::Align::Center)
                .build(),
            &["sw-composer-send"],
        ),
        "composer-send",
    );
    send.update_property(&[gtk::accessible::Property::Label("Send")]);
    send.set_action_name(Some(&ActionId::Send.detailed_name()));
    let footer = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .build(),
        &["sw-composer-footer"],
    );
    footer.append(tray.widget());
    footer.append(&warning);
    footer.append(&saved);
    footer.append(&send);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    content.append(&handle);
    content.append(&fields);
    content.append(&toolbar);
    content.append(&body);
    content.append(&footer);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&content));
    window.set_content(Some(&toasts));
    Widgets {
        title,
        from,
        from_label,
        from_list,
        to,
        cc,
        bcc,
        cc_button,
        subject,
        style,
        style_label,
        format,
        body,
        problem,
        tray,
        warning,
        saved,
        toasts,
    }
}

impl Inner {
    fn connect(self: &Rc<Self>) {
        for id in COMMANDS {
            let weak = Rc::downgrade(self);
            self.actions.connect(id, move || {
                if let Some(inner) = weak.upgrade() {
                    inner.ran.borrow_mut().push(id);
                    inner.run(id);
                }
            });
        }
        for id in EDITING {
            self.actions.set_applies(id, false);
        }
        self.actions.set_applies(ActionId::Send, false);

        let styles = gio::SimpleActionGroup::new();
        let style = gio::SimpleAction::new("style", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(self);
        style.connect_activate(move |_, value| {
            if let (Some(inner), Some(tag)) = (
                weak.upgrade(),
                value.and_then(|value| value.get::<String>()),
            ) {
                inner.widgets.style.popdown();
                inner.set_style(&tag);
            }
        });
        styles.add_action(&style);
        self.window.insert_action_group("composer", Some(&styles));

        let weak = Rc::downgrade(self);
        self.editor.set_handler(move |event| {
            if let Some(inner) = weak.upgrade() {
                inner.editor_event(event);
            }
        });
        for field in [&self.widgets.to, &self.widgets.cc, &self.widgets.bcc] {
            let weak = Rc::downgrade(self);
            field.connect_changed(move || {
                if let Some(inner) = weak.upgrade() {
                    inner.changed();
                }
            });
            let backend = self.backend.clone();
            let weak = Rc::downgrade(self);
            field.set_query(move |query| {
                let account = weak
                    .upgrade()
                    .map(|inner| inner.account.get())
                    .unwrap_or_default();
                backend.recipients(account, query)
            });
        }
        let weak = Rc::downgrade(self);
        self.widgets.subject.connect_changed(move |subject| {
            if let Some(inner) = weak.upgrade() {
                let text = subject.text();
                inner.window.set_title(Some(
                    if text.trim().is_empty() {
                        inner.widgets.title.label().to_string()
                    } else {
                        text.to_string()
                    }
                    .as_str(),
                ));
                inner.changed();
            }
        });
        let weak = Rc::downgrade(self);
        self.widgets.tray.connect_changed(move || {
            if let Some(inner) = weak.upgrade() {
                inner.update_size();
                inner.changed();
            }
        });
        let weak = Rc::downgrade(self);
        self.widgets.from_list.connect_row_activated(move |_, row| {
            if let (Some(inner), Ok(index)) = (weak.upgrade(), usize::try_from(row.index())) {
                if let Some(popover) = inner.widgets.from.popover() {
                    popover.popdown();
                }
                inner.choose_sender(index);
            }
        });

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        drop.connect_drop(move |_, value, x, y| {
            let Some(inner) = weak.upgrade() else {
                return false;
            };
            let Ok(files) = value.get::<gdk::FileList>() else {
                return false;
            };
            let paths: Vec<PathBuf> = files.files().iter().filter_map(gio::File::path).collect();
            if paths.is_empty() {
                return false;
            }
            let over_body = inner
                .editor
                .view()
                .compute_bounds(&inner.window)
                .is_some_and(|bounds| {
                    bounds.contains_point(&gtk::graphene::Point::new(x as f32, y as f32))
                });
            inner.drop_files(paths, over_body);
            true
        });
        self.window.add_controller(drop);

        let weak = Rc::downgrade(self);
        self.window
            .connect_close_request(move |_| match weak.upgrade() {
                Some(inner) => inner.close_requested(),
                None => glib::Propagation::Proceed,
            });
        let weak = Rc::downgrade(self);
        self.window.connect_destroy(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner.destroyed();
            }
        });
    }

    async fn start(self: Rc<Self>, request: ComposeRequest) {
        let prepared = self.backend.prepare(request).await;
        let filter = self.editor.engine().filter().await;
        let prepared = match (prepared, filter) {
            (Err(problem), _) => {
                self.show_problem(&problem);
                return;
            }
            (Ok(_), None) => {
                self.show_problem(
                    "The composer can't open safely: its content filter didn't compile.",
                );
                return;
            }
            (Ok(prepared), Some(filter)) => {
                self.editor.engine().set_blobs(self.backend.blobs());
                self.editor.load(&filter);
                prepared
            }
        };
        self.fill(prepared);
        let account = self.account.get();
        let limit = self.backend.submission_limit(account).await;
        self.limit.set(limit);
        self.update_size();
    }

    fn show_problem(&self, problem: &str) {
        self.widgets.problem.set_label(problem);
        self.widgets.body.set_visible_child_name("problem");
    }

    fn fill(self: &Rc<Self>, prepared: Prepared) {
        let Prepared {
            id,
            draft,
            senders,
            title,
        } = prepared;
        self.id.set(Some(id));
        self.account.set(draft.account);
        self.widgets.title.set_label(&title);
        if draft.subject.trim().is_empty() {
            self.window.set_title(Some(&title));
        }
        *self.domain.borrow_mut() = id_domain(&draft.from.email);
        let index = senders
            .iter()
            .position(|sender| sender.address.email.eq_ignore_ascii_case(&draft.from.email))
            .unwrap_or(0);
        while let Some(row) = self.widgets.from_list.row_at_index(0) {
            self.widgets.from_list.remove(&row);
        }
        for (position, sender) in senders.iter().enumerate() {
            let label = gtk::Label::builder()
                .label(sender_text(sender))
                .xalign(0.0)
                .build();
            let row = gtk::ListBoxRow::builder().child(&label).build();
            row.set_widget_name(&format!("composer-from-{position}"));
            self.widgets.from_list.append(&row);
        }
        self.widgets.from.set_sensitive(!senders.is_empty());
        self.senders.replace(senders);
        self.sender.set(index);
        self.show_sender();
        self.widgets.to.set_addresses(&draft.to);
        self.widgets.cc.set_addresses(&draft.cc);
        self.widgets.bcc.set_addresses(&draft.bcc);
        if !draft.cc.is_empty() || !draft.bcc.is_empty() {
            self.show_cc_bcc(false);
        }
        self.widgets.subject.set_text(&draft.subject);
        let (inline, attached): (Vec<DraftPart>, Vec<DraftPart>) = draft
            .parts
            .iter()
            .cloned()
            .partition(|part| part.content_id.is_some());
        for part in inline {
            if let Some(content_id) = part.content_id {
                self.editor.add_part(
                    content_id,
                    InlineImage {
                        mime_type: part.mime_type,
                        blob: part.blob,
                        size: part.size,
                        name: part.name,
                    },
                );
            }
        }
        self.widgets.tray.set_parts(&attached);
        self.base.replace(Some(draft));
        self.actions.set_applies(ActionId::Send, true);
    }

    fn show_sender(&self) {
        let senders = self.senders.borrow();
        if let Some(sender) = senders.get(self.sender.get()) {
            self.widgets.from_label.set_label(&sender_text(sender));
        }
    }

    fn choose_sender(self: &Rc<Self>, index: usize) {
        if index < self.senders.borrow().len() && index != self.sender.get() {
            self.sender.set(index);
            self.show_sender();
            self.changed();
        }
    }

    fn editor_event(self: &Rc<Self>, event: EditorEvent) {
        match event {
            EditorEvent::Ready => {
                let inner = self.clone();
                glib::spawn_future_local(async move { inner.ready().await });
            }
            EditorEvent::Input { .. } => self.changed(),
            EditorEvent::State(state) => self.show_state(&state),
            EditorEvent::History { undo, redo } => {
                if self.loaded.get() {
                    self.actions.set_applies(ActionId::ComposerUndo, undo);
                    self.actions.set_applies(ActionId::ComposerRedo, redo);
                }
            }
            EditorEvent::Image {
                token,
                mime_type,
                data,
                name,
            } => self.store_image(token, mime_type, data, name),
            EditorEvent::File {
                name,
                mime_type,
                data,
            } => {
                let Some(blobs) = self.backend.blobs() else {
                    self.toast("Attachments can't be added until the mail store is open");
                    return;
                };
                let weak = Rc::downgrade(self);
                self.widgets.tray.add(
                    &name.clone(),
                    attachments::add_base64(blobs, name, mime_type, data),
                    move |name, _| {
                        if let Some(inner) = weak.upgrade() {
                            inner.toast(&format!("Couldn't attach {name}"));
                        }
                    },
                );
            }
            EditorEvent::Transfer { kind, mode, stats } => {
                tracing::debug!(
                    kind,
                    mode,
                    handlers = stats.as_ref().map_or(0, |stats| stats.handlers),
                    remote = stats.as_ref().map_or(0, |stats| stats.remote),
                    "composer: a paste or drop was cleaned"
                );
            }
            EditorEvent::Problem(problem) => tracing::info!("composer: {problem}"),
            EditorEvent::Crashed => {
                self.loaded.set(false);
                self.toast("The editor stopped; the draft as last saved is kept");
            }
        }
    }

    async fn ready(self: Rc<Self>) {
        let html = self
            .base
            .borrow()
            .as_ref()
            .map(|draft| draft.html.clone())
            .unwrap_or_default();
        self.editor.set_content(&html).await;
        self.widgets.body.set_visible_child_name("editor");
        self.loaded.set(true);
        for id in EDITING {
            self.actions.set_applies(id, true);
        }
        let (undo, redo) = self.editor.history();
        self.actions.set_applies(ActionId::ComposerUndo, undo);
        self.actions.set_applies(ActionId::ComposerRedo, redo);
        if self.widgets.to.is_empty() {
            self.widgets.to.entry().grab_focus();
        } else {
            let _ = self.editor.eval("sealwaxComposer.caretAtStart()").await;
            self.editor.view().grab_focus();
        }
    }

    fn show_state(&self, state: &BlockState) {
        for (id, button) in &self.widgets.format {
            let pressed = match id {
                ActionId::Bold => state.bold,
                ActionId::Italic => state.italic,
                ActionId::Underline => state.underline,
                ActionId::Strikethrough => state.strikethrough,
                ActionId::BulletedList => state.bulleted,
                ActionId::NumberedList => state.numbered,
                ActionId::InsertLink => !state.link.is_empty(),
                ActionId::Quote => state.quote,
                ActionId::Indent => state.indent,
                _ => continue,
            };
            if pressed {
                button.add_css_class("sw-pressed");
            } else {
                button.remove_css_class("sw-pressed");
            }
        }
        let name = STYLES
            .iter()
            .find(|(tag, _)| *tag == state.block)
            .map_or("Paragraph", |(_, name)| name);
        self.widgets.style_label.set_label(name);
    }

    fn run(self: &Rc<Self>, id: ActionId) {
        let editor = &self.editor;
        match id {
            ActionId::Send => {
                let inner = self.clone();
                glib::spawn_future_local(async move { inner.send().await });
                return;
            }
            ActionId::AttachFile => {
                let inner = self.clone();
                glib::spawn_future_local(async move { inner.attach().await });
                return;
            }
            ActionId::ShowCcBcc => {
                self.show_cc_bcc(true);
                return;
            }
            ActionId::DiscardDraft => {
                let inner = self.clone();
                glib::spawn_future_local(async move { inner.discard().await });
                return;
            }
            ActionId::Compose => {
                if let Some(app) = self.app.upgrade() {
                    super::open(
                        &app,
                        ComposeRequest::New {
                            account: Some(self.account.get()),
                        },
                    );
                }
                return;
            }
            ActionId::CommandPalette => {
                palette::present(&self.window, &self.keymap.borrow());
                return;
            }
            ActionId::InsertLink => {
                self.link_dialog();
                return;
            }
            ActionId::PasteAsText => {
                self.paste_text();
                return;
            }
            ActionId::Bold => editor.command("Bold"),
            ActionId::Italic => editor.command("Italic"),
            ActionId::Underline => editor.command("Underline"),
            ActionId::Strikethrough => editor.command("Strikethrough"),
            ActionId::BulletedList => editor.command("InsertUnorderedList"),
            ActionId::NumberedList => editor.command("InsertOrderedList"),
            ActionId::Quote if editor.block().quote => editor.command("Outdent"),
            ActionId::Quote => editor.command_with("FormatBlock", "blockquote"),
            ActionId::Indent => editor.command("Indent"),
            ActionId::Outdent => editor.command("Outdent"),
            ActionId::ClearFormatting => editor.command("RemoveFormat"),
            ActionId::ComposerUndo => editor.command(webkit6::EDITING_COMMAND_UNDO),
            ActionId::ComposerRedo => editor.command(webkit6::EDITING_COMMAND_REDO),
            _ => return,
        }
        editor.view().grab_focus();
    }

    fn set_style(&self, tag: &str) {
        if STYLES.iter().any(|(known, _)| *known == tag) {
            self.editor.command_with("FormatBlock", tag);
            self.editor.view().grab_focus();
        }
    }

    fn show_cc_bcc(&self, focus: bool) {
        self.widgets.cc.widget().set_visible(true);
        self.widgets.bcc.widget().set_visible(true);
        self.widgets.cc_button.set_visible(false);
        if focus {
            self.widgets.cc.entry().grab_focus();
        }
    }

    fn toast(&self, text: &str) {
        self.widgets.toasts.add_toast(adw::Toast::new(text));
    }

    // Autosave.

    /// The body or a field changed: save at most `MAX_BEHIND` after the first unsaved change,
    /// and `IDLE_SAVE` after the last.
    fn changed(self: &Rc<Self>) {
        if !self.loaded.get() || self.ending.get() != Ending::Open {
            return;
        }
        let now = Instant::now();
        self.edits.borrow_mut().push(now);
        self.last_edit.set(Some(now));
        if self.first_dirty.get().is_none() {
            self.first_dirty.set(Some(now));
        }
        self.dirty.set(true);
        self.schedule();
    }

    fn schedule(self: &Rc<Self>) {
        if self.saving.get() || !self.dirty.get() {
            return;
        }
        let (Some(first), Some(last)) = (self.first_dirty.get(), self.last_edit.get()) else {
            return;
        };
        let due = (last + IDLE_SAVE).min(first + MAX_BEHIND);
        let wait = due.saturating_duration_since(Instant::now());
        self.cancel_timer();
        let weak = Rc::downgrade(self);
        let source = glib::timeout_add_local_once(wait, move || {
            if let Some(inner) = weak.upgrade() {
                inner.timer.replace(None);
                glib::spawn_future_local(async move {
                    inner.save().await;
                });
            }
        });
        self.timer.replace(Some(source));
    }

    fn cancel_timer(&self) {
        if let Some(source) = self.timer.take() {
            source.remove();
        }
    }

    /// Saves the draft as it is now; returns whether the backend kept it.
    async fn save(self: &Rc<Self>) -> bool {
        let Some(id) = self.id.get() else {
            return false;
        };
        if self.saving.replace(true) {
            return false;
        }
        self.dirty.set(false);
        self.first_dirty.set(None);
        let saved = match self.collect().await {
            Some(draft) => self.backend.save(id, draft).await,
            None => Err("nothing to save".to_owned()),
        };
        self.saving.set(false);
        match &saved {
            Ok(()) => {
                let time = glib::DateTime::now_local()
                    .and_then(|now| now.format("%H:%M"))
                    .map(|time| time.to_string())
                    .unwrap_or_default();
                self.widgets.saved.set_label(&format!("Draft saved {time}"));
                self.widgets.saved.set_visible(true);
            }
            Err(problem) => {
                tracing::warn!("composer: the draft could not be saved: {problem}");
                self.widgets.saved.set_label("Draft not saved");
                self.widgets.saved.set_visible(true);
            }
        }
        if self.dirty.get() && self.ending.get() == Ending::Open {
            self.schedule();
        }
        saved.is_ok()
    }

    /// The draft as the window holds it now.
    async fn collect(&self) -> Option<Draft> {
        let base = self.base.borrow().clone()?;
        let domain = self.domain.borrow().clone();
        let (html, cids) = match self.editor.serialize(&domain).await {
            Some(serialized) => (serialized.html, serialized.cids),
            None => {
                let cids = base
                    .parts
                    .iter()
                    .filter_map(|part| part.content_id.clone())
                    .collect();
                (base.html.clone(), cids)
            }
        };
        let mut parts = self.widgets.tray.parts();
        let images = self.editor.parts();
        let mut seen = HashSet::new();
        for cid in cids {
            if !seen.insert(cid.clone()) {
                continue;
            }
            if let Some(image) = images.get(&cid) {
                let name = if image.name.is_empty() {
                    let extension = image
                        .mime_type
                        .rsplit('/')
                        .next()
                        .unwrap_or("png")
                        .replace("jpeg", "jpg");
                    format!("image-{}.{extension}", seen.len())
                } else {
                    image.name.clone()
                };
                parts.push(DraftPart {
                    name: sealwax_domain::attachment::safe_file_name(Some(&name), &image.mime_type),
                    mime_type: image.mime_type.clone(),
                    size: image.size,
                    blob: image.blob,
                    content_id: Some(cid),
                });
            }
        }
        let from = self
            .senders
            .borrow()
            .get(self.sender.get())
            .map(|sender| sender.address.clone())
            .unwrap_or_else(|| base.from.clone());
        Some(Draft {
            from,
            to: self.widgets.to.addresses(),
            cc: self.widgets.cc.addresses(),
            bcc: self.widgets.bcc.addresses(),
            subject: self.widgets.subject.text().to_string(),
            html,
            parts,
            ..base
        })
    }

    // Images and attachments.

    fn store_image(self: &Rc<Self>, token: String, mime_type: String, data: String, name: String) {
        let Some(blobs) = self.backend.blobs() else {
            self.toast("Images can't be added until the mail store is open");
            return;
        };
        if !self.adopting.borrow_mut().insert(token.clone()) {
            return;
        }
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let stored = gio::spawn_blocking(move || {
                let bytes = glib::base64_decode(&data);
                let size = bytes.len() as u64;
                blobs.put(&bytes).map(|blob| (blob, size))
            })
            .await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            inner.adopting.borrow_mut().remove(&token);
            match stored {
                Ok(Ok((blob, size))) => {
                    let content_id = format!("{token}@{}", inner.domain.borrow());
                    inner.editor.add_part(
                        content_id.clone(),
                        InlineImage {
                            mime_type,
                            blob,
                            size,
                            name,
                        },
                    );
                    inner.editor.adopt(&token, &content_id);
                    inner.update_size();
                    inner.changed();
                }
                _ => inner.toast("Couldn't add the image"),
            }
        });
    }

    async fn attach(self: Rc<Self>) {
        let paths = match chooser_answer() {
            Some(paths) => paths,
            None => {
                let dialog = gtk::FileDialog::builder()
                    .title("Attach Files")
                    .modal(true)
                    .build();
                let Ok(files) = dialog.open_multiple_future(Some(&self.window)).await else {
                    return;
                };
                (0..files.n_items())
                    .filter_map(|index| files.item(index).and_downcast::<gio::File>())
                    .filter_map(|file| file.path())
                    .collect()
            }
        };
        self.attach_paths(paths);
    }

    fn attach_paths(self: &Rc<Self>, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        let Some(blobs) = self.backend.blobs() else {
            self.toast("Attachments can't be added until the mail store is open");
            return;
        };
        for path in paths {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let weak = Rc::downgrade(self);
            self.widgets.tray.add(
                &name,
                attachments::add_path(blobs.clone(), path),
                move |name, _| {
                    if let Some(inner) = weak.upgrade() {
                        inner.toast(&format!("Couldn't attach {name}"));
                    }
                },
            );
        }
    }

    /// Files dropped on the window: images over the body go inline, everything else is
    /// attached.
    fn drop_files(self: &Rc<Self>, paths: Vec<PathBuf>, over_body: bool) {
        let images = over_body
            && self.loaded.get()
            && paths.iter().all(|path| {
                let name = path.to_string_lossy();
                let (content_type, _) = gio::content_type_guess(Some(name.as_ref()), None);
                gio::content_type_get_mime_type(&content_type).is_some_and(|mime| {
                    matches!(
                        mime.as_str(),
                        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
                    )
                })
            });
        if !images {
            self.attach_paths(paths);
            return;
        }
        let Some(blobs) = self.backend.blobs() else {
            self.toast("Images can't be added until the mail store is open");
            return;
        };
        for path in paths {
            let weak = Rc::downgrade(self);
            let adding = attachments::add_path(blobs.clone(), path);
            glib::spawn_future_local(async move {
                let added = adding.await;
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                match added {
                    Ok(part) => {
                        let token = glib::uuid_string_random().replace('-', "");
                        let content_id = format!("{token}@{}", inner.domain.borrow());
                        inner.editor.add_part(
                            content_id.clone(),
                            InlineImage {
                                mime_type: part.mime_type,
                                blob: part.blob,
                                size: part.size,
                                name: part.name.clone(),
                            },
                        );
                        inner.editor.eval_detached(&format!(
                            "sealwaxComposer.insertImage({}, {})",
                            super::js::literal(&content_id),
                            super::js::literal(&part.name)
                        ));
                        inner.update_size();
                    }
                    Err(_) => inner.toast("Couldn't add the image"),
                }
            });
        }
    }

    /// The encoded size against the account's limit: a warning in the footer when over.
    fn update_size(&self) {
        let size = self.size();
        let over = self.limit.get().filter(|limit| size > *limit);
        match over {
            Some(limit) => {
                self.widgets.warning.set_label(&format!(
                    "Too large to send: {} with attachments; this account's server takes at \
                     most {}",
                    glib::format_size(size),
                    glib::format_size(limit)
                ));
                self.widgets.warning.set_visible(true);
            }
            None => self.widgets.warning.set_visible(false),
        }
    }

    fn size(&self) -> u64 {
        let images = self.editor.parts();
        let parts = self
            .widgets
            .tray
            .parts()
            .into_iter()
            .map(|part| part.size)
            .chain(images.values().map(|image| image.size));
        attachments::message_size(parts, 0)
    }

    // Send, discard and close.

    async fn send(self: Rc<Self>) {
        let Some(id) = self.id.get() else {
            return;
        };
        if self.ending.get() != Ending::Open {
            return;
        }
        let fields = [&self.widgets.to, &self.widgets.cc, &self.widgets.bcc];
        for field in fields {
            field.commit();
        }
        if fields.iter().any(|field| field.invalid() > 0) {
            self.toast("Fix or remove the address marked in red first");
            return;
        }
        if fields.iter().all(|field| field.addresses().is_empty()) {
            self.toast("Add a recipient first");
            self.widgets.to.entry().grab_focus();
            return;
        }
        if self.widgets.tray.busy() > 0 {
            self.toast("Wait until the attachments are added");
            return;
        }
        let Some(draft) = self.collect().await else {
            return;
        };
        let size =
            attachments::message_size(draft.parts.iter().map(|part| part.size), draft.html.len());
        if let Some(limit) = self.limit.get().filter(|limit| size > *limit) {
            self.update_size();
            self.toast(&format!(
                "Not sent: {} is more than this account's server takes ({})",
                glib::format_size(size),
                glib::format_size(limit)
            ));
            return;
        }
        self.cancel_timer();
        match self.backend.send(id, draft).await {
            Ok(()) => {
                self.ending.set(Ending::Sent);
                self.window.destroy();
            }
            Err(Refusal(reason)) => self.toast(&reason),
        }
    }

    async fn discard(self: Rc<Self>) {
        let Some(id) = self.id.get() else {
            self.window.destroy();
            return;
        };
        let empty = self.editor.is_empty().await
            && [&self.widgets.to, &self.widgets.cc, &self.widgets.bcc]
                .iter()
                .all(|field| field.is_empty())
            && self.widgets.subject.text().trim().is_empty()
            && self.widgets.tray.parts().is_empty();
        if !empty {
            let dialog = adw::AlertDialog::builder()
                .heading("Discard this draft?")
                .body("The message and its attachments are deleted.")
                .default_response("cancel")
                .close_response("cancel")
                .build();
            dialog.set_widget_name("composer-discard-dialog");
            dialog.add_responses(&[("cancel", "Cancel"), ("discard", "Discard")]);
            dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
            let answer = dialog.choose_future(Some(&self.window)).await;
            if answer != "discard" {
                return;
            }
        }
        self.cancel_timer();
        if let Err(problem) = self.backend.discard(id).await {
            tracing::warn!("composer: the draft could not be discarded: {problem}");
            self.toast("The draft couldn't be discarded");
            return;
        }
        self.ending.set(Ending::Discarded);
        self.window.destroy();
    }

    fn link_dialog(self: &Rc<Self>) {
        let current = self.editor.block().link;
        let entry = gtk::Entry::builder()
            .text(&current)
            .placeholder_text("https://example.org/")
            .activates_default(true)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label("Link address")]);
        let dialog = adw::AlertDialog::builder()
            .heading(if current.is_empty() {
                "Insert Link"
            } else {
                "Edit Link"
            })
            .body("A web address (http or https) or an email address (mailto:).")
            .extra_child(&entry)
            .default_response("apply")
            .close_response("cancel")
            .build();
        dialog.set_widget_name("composer-link-dialog");
        dialog.add_response("cancel", "Cancel");
        if !current.is_empty() {
            dialog.add_response("remove", "Remove Link");
        }
        dialog.add_response("apply", "Apply");
        dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
        let valid = |text: &str| link_target(text).is_some();
        dialog.set_response_enabled("apply", valid(&current));
        let target = dialog.downgrade();
        entry.connect_changed(move |entry| {
            if let Some(dialog) = target.upgrade() {
                dialog.set_response_enabled("apply", valid(&entry.text()));
            }
        });
        let weak = Rc::downgrade(self);
        dialog.connect_response(None, move |_, response| {
            let Some(inner) = weak.upgrade() else {
                return;
            };
            let editor = inner.editor.clone();
            let text = entry.text().to_string();
            let response = response.to_owned();
            glib::spawn_future_local(async move {
                match response.as_str() {
                    "apply" => {
                        if let Some(url) = link_target(&text) {
                            if !editor.block().link.is_empty() {
                                editor.select_link().await;
                            }
                            let _ = editor
                                .eval(&format!(
                                    "sealwaxComposer.link({})",
                                    super::js::literal(&url)
                                ))
                                .await;
                        }
                    }
                    "remove" => {
                        let selected = editor.select_link().await;
                        if selected {
                            editor.command("Unlink");
                        }
                    }
                    _ => {}
                }
                editor.view().grab_focus();
            });
        });
        dialog.present(Some(&self.window));
    }

    fn paste_text(self: &Rc<Self>) {
        let focused = gtk::prelude::GtkWindowExt::focus(&self.window);
        if let Some(text) = focused.and_then(|widget| widget.downcast::<gtk::Text>().ok()) {
            text.emit_paste_clipboard();
            return;
        }
        let clipboard = self.window.clipboard();
        let editor = self.editor.clone();
        glib::spawn_future_local(async move {
            if let Ok(Some(text)) = clipboard.read_text_future().await {
                editor.insert_text(&text).await;
            }
        });
    }

    fn close_requested(self: &Rc<Self>) -> glib::Propagation {
        if self.ending.get() != Ending::Open {
            return glib::Propagation::Proceed;
        }
        if self.closing.replace(true) {
            return glib::Propagation::Stop;
        }
        self.cancel_timer();
        self.window.set_visible(false);
        let hold = self.app.upgrade().map(|app| app.hold());
        let inner = self.clone();
        glib::spawn_future_local(async move {
            if inner.base.borrow().is_some() {
                inner.save().await;
            }
            if let Some(id) = inner.id.get() {
                inner.backend.close(id);
            }
            inner.ending.set(Ending::Closed);
            inner.window.destroy();
            drop(hold);
        });
        glib::Propagation::Stop
    }

    fn destroyed(&self) {
        self.cancel_timer();
        if self.ending.get() == Ending::Open {
            self.ending.set(Ending::Closed);
        }
        if let (Some(id), Some(app)) = (self.theme_listener.take(), self.app.upgrade())
            && let Some(controller) = app.state().theme()
        {
            controller.disconnect(id);
        }
        self.editor.release();
        super::forget(&self.window);
    }
}

fn sender_text(sender: &Sender) -> String {
    if sender.label.is_empty() || sender.label == sender.address.email {
        sender.address.email.clone()
    } else {
        format!("{} · {}", sender.label, sender.address.email)
    }
}

/// The link a user typed, if it is one Sealwax writes: http(s) or mailto. A bare address
/// becomes `mailto:`, a bare host `https://`.
pub fn link_target(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    let lower = text.to_ascii_lowercase();
    if lower.starts_with("https://") || lower.starts_with("http://") {
        return (text.len() > lower.find("://")? + 3).then(|| text.to_owned());
    }
    if let Some(address) = lower.strip_prefix("mailto:") {
        return super::recipients::valid_address(address.split('?').next()?)
            .then(|| text.to_owned());
    }
    if lower.contains(':') {
        return None;
    }
    if super::recipients::valid_address(text) {
        return Some(format!("mailto:{text}"));
    }
    let host = text.split('/').next()?;
    (host.contains('.') && !host.starts_with('.') && !host.ends_with('.'))
        .then(|| format!("https://{text}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_are_web_or_mail_addresses_only() {
        assert_eq!(
            link_target("https://example.org/a"),
            Some("https://example.org/a".into())
        );
        assert_eq!(
            link_target("HTTP://example.org"),
            Some("HTTP://example.org".into())
        );
        assert_eq!(
            link_target("mailto:ola@example.org"),
            Some("mailto:ola@example.org".into())
        );
        assert_eq!(
            link_target("ola@example.org"),
            Some("mailto:ola@example.org".into())
        );
        assert_eq!(
            link_target("example.org/page"),
            Some("https://example.org/page".into())
        );
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "ftp://example.org",
            "https://",
            "not a link",
            "",
            "mailto:nobody",
        ] {
            assert_eq!(link_target(bad), None, "{bad}");
        }
    }

    #[test]
    fn saves_stay_within_five_seconds() {
        assert!(MAX_BEHIND + Duration::from_millis(500) <= Duration::from_secs(5));
        assert!(IDLE_SAVE < MAX_BEHIND);
    }
}
