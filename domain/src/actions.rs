//! The action registry (ADR-017): every user-visible command with its GAction name, title,
//! category, scope, default keys and the places that show it. The primary menu, the command
//! palette, the keyboard shortcuts dialog, the accelerators and the scoped shortcuts are all
//! generated from it, so nothing can show a command that has no action.

/// Where an action lives, and where scoped keys work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Scope {
    /// The application: `app.` actions, with or without a window.
    App,
    /// The window: `win.` actions that need no message.
    Window,
    /// The message list: `win.` actions on the selected messages.
    List,
    /// The reader: `win.` actions on the open message.
    Reader,
    /// The composer (M6).
    Composer,
}

impl Scope {
    /// The places a widget can install scoped keys for (`install_scoped`).
    pub const SCOPED: [Self; 3] = [Self::List, Self::Reader, Self::Composer];

    /// The GAction group prefix: `app` or `win`.
    pub const fn group(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Window | Self::List | Self::Reader | Self::Composer => "win",
        }
    }

    /// Whether the scope's actions work anywhere in the window. Their keys with Ctrl or Alt,
    /// and their F-keys, are application accelerators.
    pub const fn is_global(self) -> bool {
        matches!(self, Self::App | Self::Window)
    }

    /// Whether one-key shortcuts may work here. The composer and text fields keep their
    /// letters, so only the list and the reader take them.
    pub const fn allows_one_key(self) -> bool {
        matches!(self, Self::List | Self::Reader)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::App => "the application",
            Self::Window => "the window",
            Self::List => "the message list",
            Self::Reader => "the reader",
            Self::Composer => "the composer",
        }
    }
}

/// The command groups of the palette and the keyboard shortcuts dialog, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Category {
    Navigation,
    Messages,
    Compose,
    View,
    General,
    /// The account's mail rules (ADR-020, M9).
    Rules,
}

impl Category {
    pub const ALL: [Self; 6] = [
        Self::Navigation,
        Self::Messages,
        Self::Compose,
        Self::View,
        Self::General,
        Self::Rules,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Messages => "Messages",
            Self::Compose => "Compose",
            Self::View => "View",
            Self::General => "General",
            Self::Rules => "Rules",
        }
    }
}

/// The primary menu's sections, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MenuSection {
    View,
    Settings,
    Updates,
    Quit,
}

impl MenuSection {
    pub const ALL: [Self; 4] = [Self::View, Self::Settings, Self::Updates, Self::Quit];
}

/// The shape of the GAction behind an [`ActionId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Command,
    /// An on/off setting with a boolean state, shown with a check mark.
    Toggle,
}

/// Everything the registry knows about one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionSpec {
    /// The GAction name, stable across releases: `keys.toml` and scripts refer to it.
    pub name: &'static str,
    pub title: &'static str,
    pub category: Category,
    pub scope: Scope,
    pub kind: ActionKind,
    /// Default keys as GTK accelerator strings, a two-key sequence as `"g i"`, in the order the
    /// keyboard shortcuts dialog lists them. `keymap::is_accelerator` decides which of them are
    /// application accelerators; the others are scoped shortcuts that work in `key_scopes`.
    pub keys: &'static [&'static str],
    /// The message list's own keys for the action (GtkListView's arrows): shown, never
    /// installed and never rebound.
    pub list_keys: &'static [&'static str],
    /// Where the scoped keys work.
    pub key_scopes: &'static [Scope],
    pub palette: bool,
    pub menu: Option<MenuSection>,
    /// Enabled only while messages are selected.
    pub needs_selection: bool,
}

/// Scoped keys that work in both the message list and the reader.
const MAIL: &[Scope] = &[Scope::List, Scope::Reader];
const LIST: &[Scope] = &[Scope::List];
const READER: &[Scope] = &[Scope::Reader];
const COMPOSER: &[Scope] = &[Scope::Composer];

const fn command(
    name: &'static str,
    title: &'static str,
    category: Category,
    scope: Scope,
    keys: &'static [&'static str],
) -> ActionSpec {
    ActionSpec {
        name,
        title,
        category,
        scope,
        kind: ActionKind::Command,
        keys,
        list_keys: &[],
        key_scopes: match scope {
            Scope::App | Scope::Window | Scope::List => MAIL,
            Scope::Reader => READER,
            Scope::Composer => COMPOSER,
        },
        palette: true,
        menu: None,
        needs_selection: false,
    }
}

/// A command on the selected messages; its keys work in the list and the reader.
const fn on_selection(
    name: &'static str,
    title: &'static str,
    category: Category,
    keys: &'static [&'static str],
) -> ActionSpec {
    ActionSpec {
        needs_selection: true,
        ..command(name, title, category, Scope::List, keys)
    }
}

const fn go_to(
    name: &'static str,
    title: &'static str,
    keys: &'static [&'static str],
) -> ActionSpec {
    command(name, title, Category::Navigation, Scope::Window, keys)
}

/// Every user-visible command, in the order the palette and the dialog list them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActionId {
    NextMessage,
    PreviousMessage,
    NextInConversation,
    PreviousInConversation,
    ScrollMessageDown,
    ScrollMessageUp,
    OpenMessage,
    BackToList,
    Search,
    GoToInbox,
    GoToStarred,
    GoToSent,
    GoToDrafts,
    GoToAllInboxes,
    GoToAccount1,
    GoToAccount2,
    GoToAccount3,
    GoToAccount4,
    GoToAccount5,
    GoToAccount6,
    GoToAccount7,
    GoToAccount8,
    GoToAccount9,
    Archive,
    Delete,
    Junk,
    MoveTo,
    Star,
    MarkRead,
    MarkUnread,
    MarkFolderRead,
    NewFolder,
    RenameFolder,
    DeleteFolder,
    Select,
    Undo,
    Print,
    ShowOriginal,
    SaveMessage,
    Compose,
    Reply,
    ReplyAll,
    ReplySender,
    Forward,
    Send,
    EditDraft,
    QuickReply,
    ReplyInComposer,
    AttachFile,
    ShowCcBcc,
    DiscardDraft,
    CloseComposer,
    Bold,
    Italic,
    Underline,
    Strikethrough,
    BulletedList,
    NumberedList,
    InsertLink,
    Quote,
    Indent,
    Outdent,
    ClearFormatting,
    ComposerUndo,
    ComposerRedo,
    PasteAsText,
    ExpandAll,
    CollapseAll,
    ShowImages,
    AlwaysShowImages,
    Refresh,
    ToggleSidebar,
    CommandPalette,
    AddAccount,
    EditAccount,
    RemoveAccount,
    KeyboardShortcuts,
    Preferences,
    CustomizeShortcuts,
    RebuildSearchIndex,
    StartAtLogin,
    DefaultMailApp,
    CheckForUpdates,
    Quit,
    EditRules,
    NewRuleFromMessage,
    JunkSender,
    TrustSender,
    ApplyRules,
}

impl ActionId {
    pub const ALL: [Self; 89] = [
        Self::NextMessage,
        Self::PreviousMessage,
        Self::NextInConversation,
        Self::PreviousInConversation,
        Self::ScrollMessageDown,
        Self::ScrollMessageUp,
        Self::OpenMessage,
        Self::BackToList,
        Self::Search,
        Self::GoToInbox,
        Self::GoToStarred,
        Self::GoToSent,
        Self::GoToDrafts,
        Self::GoToAllInboxes,
        Self::GoToAccount1,
        Self::GoToAccount2,
        Self::GoToAccount3,
        Self::GoToAccount4,
        Self::GoToAccount5,
        Self::GoToAccount6,
        Self::GoToAccount7,
        Self::GoToAccount8,
        Self::GoToAccount9,
        Self::Archive,
        Self::Delete,
        Self::Junk,
        Self::MoveTo,
        Self::Star,
        Self::MarkRead,
        Self::MarkUnread,
        Self::MarkFolderRead,
        Self::NewFolder,
        Self::RenameFolder,
        Self::DeleteFolder,
        Self::Select,
        Self::Undo,
        Self::Print,
        Self::ShowOriginal,
        Self::SaveMessage,
        Self::Compose,
        Self::Reply,
        Self::ReplyAll,
        Self::ReplySender,
        Self::Forward,
        Self::Send,
        Self::EditDraft,
        Self::QuickReply,
        Self::ReplyInComposer,
        Self::AttachFile,
        Self::ShowCcBcc,
        Self::DiscardDraft,
        Self::CloseComposer,
        Self::Bold,
        Self::Italic,
        Self::Underline,
        Self::Strikethrough,
        Self::BulletedList,
        Self::NumberedList,
        Self::InsertLink,
        Self::Quote,
        Self::Indent,
        Self::Outdent,
        Self::ClearFormatting,
        Self::ComposerUndo,
        Self::ComposerRedo,
        Self::PasteAsText,
        Self::ExpandAll,
        Self::CollapseAll,
        Self::ShowImages,
        Self::AlwaysShowImages,
        Self::Refresh,
        Self::ToggleSidebar,
        Self::CommandPalette,
        Self::AddAccount,
        Self::EditAccount,
        Self::RemoveAccount,
        Self::KeyboardShortcuts,
        Self::Preferences,
        Self::CustomizeShortcuts,
        Self::RebuildSearchIndex,
        Self::StartAtLogin,
        Self::DefaultMailApp,
        Self::CheckForUpdates,
        Self::Quit,
        Self::EditRules,
        Self::NewRuleFromMessage,
        Self::JunkSender,
        Self::TrustSender,
        Self::ApplyRules,
    ];

    /// Go to Account `number` (1 to 9), the sidebar's account in that place.
    pub const fn go_to_account(number: usize) -> Option<Self> {
        Some(match number {
            1 => Self::GoToAccount1,
            2 => Self::GoToAccount2,
            3 => Self::GoToAccount3,
            4 => Self::GoToAccount4,
            5 => Self::GoToAccount5,
            6 => Self::GoToAccount6,
            7 => Self::GoToAccount7,
            8 => Self::GoToAccount8,
            9 => Self::GoToAccount9,
            _ => return None,
        })
    }

    pub const fn spec(self) -> ActionSpec {
        use Category::*;
        match self {
            // Down and Up are the message list's own keys; j and k also work in the reader.
            Self::NextMessage => ActionSpec {
                list_keys: &["Down"],
                ..command(
                    "next-message",
                    "Next Message",
                    Navigation,
                    Scope::List,
                    &["j"],
                )
            },
            Self::PreviousMessage => ActionSpec {
                list_keys: &["Up"],
                ..command(
                    "previous-message",
                    "Previous Message",
                    Navigation,
                    Scope::List,
                    &["k"],
                )
            },
            // Within the conversation the reader shows (Gmail's `n` and `p`; accepted at the M5
            // gate, 2026-10-06): the next or previous message expands and the column scrolls to it.
            Self::NextInConversation => command(
                "next-in-conversation",
                "Next Message in Conversation",
                Navigation,
                Scope::List,
                &["n"],
            ),
            Self::PreviousInConversation => command(
                "previous-in-conversation",
                "Previous Message in Conversation",
                Navigation,
                Scope::List,
                &["p"],
            ),
            // Half a page of the message the reader shows, from the list and the reader (issue #10,
            // accepted 2026-10-19): j and k still move between messages.
            Self::ScrollMessageDown => command(
                "scroll-message-down",
                "Scroll Message Down",
                Navigation,
                Scope::List,
                &["d"],
            ),
            Self::ScrollMessageUp => command(
                "scroll-message-up",
                "Scroll Message Up",
                Navigation,
                Scope::List,
                &["<Shift>d"],
            ),
            // `l` (issue #10, accepted 2026-10-19) moves into the message as a file manager does.
            Self::OpenMessage => ActionSpec {
                key_scopes: LIST,
                ..on_selection(
                    "open-message",
                    "Open Message",
                    Navigation,
                    &["Return", "o", "l"],
                )
            },
            Self::BackToList => command(
                "back-to-list",
                "Back to List",
                Navigation,
                Scope::Reader,
                &["Escape", "u"],
            ),
            // `/` needs Shift on the Norwegian layout, so Ctrl+F is its unshifted alternative.
            Self::Search => command(
                "search",
                "Search",
                Navigation,
                Scope::Window,
                &["slash", "<Control>f"],
            ),
            Self::GoToInbox => go_to("go-to-inbox", "Go to Inbox", &["g i"]),
            Self::GoToStarred => go_to("go-to-starred", "Go to Starred", &["g s"]),
            Self::GoToSent => go_to("go-to-sent", "Go to Sent", &["g t"]),
            Self::GoToDrafts => go_to("go-to-drafts", "Go to Drafts", &["g d"]),
            // Issue #12 (accepted 2026-10-19): All Inboxes, and the Inbox of the sidebar's first to
            // ninth account.
            Self::GoToAllInboxes => go_to("go-to-all-inboxes", "Go to All Inboxes", &["g a"]),
            Self::GoToAccount1 => go_to("go-to-account-1", "Go to Account 1", &["g 1"]),
            Self::GoToAccount2 => go_to("go-to-account-2", "Go to Account 2", &["g 2"]),
            Self::GoToAccount3 => go_to("go-to-account-3", "Go to Account 3", &["g 3"]),
            Self::GoToAccount4 => go_to("go-to-account-4", "Go to Account 4", &["g 4"]),
            Self::GoToAccount5 => go_to("go-to-account-5", "Go to Account 5", &["g 5"]),
            Self::GoToAccount6 => go_to("go-to-account-6", "Go to Account 6", &["g 6"]),
            Self::GoToAccount7 => go_to("go-to-account-7", "Go to Account 7", &["g 7"]),
            Self::GoToAccount8 => go_to("go-to-account-8", "Go to Account 8", &["g 8"]),
            Self::GoToAccount9 => go_to("go-to-account-9", "Go to Account 9", &["g 9"]),
            Self::Archive => on_selection("archive", "Archive", Messages, &["e", "y"]),
            Self::Delete => on_selection("delete", "Delete", Messages, &["numbersign", "Delete"]),
            Self::Junk => on_selection("junk", "Move to Junk", Messages, &["exclam", "<Control>j"]),
            Self::MoveTo => {
                on_selection("move-to", "Move to…", Messages, &["v", "<Control><Shift>v"])
            }
            Self::Star => on_selection("star", "Star or Unstar", Messages, &["s"]),
            Self::MarkRead => on_selection("mark-read", "Mark as Read", Messages, &["<Shift>i"]),
            Self::MarkUnread => {
                on_selection("mark-unread", "Mark as Unread", Messages, &["<Shift>u"])
            }
            // Marks every message of the folder the list shows as read, on the server too (ML-052).
            // No default key until the user picks one; the palette and the sidebar reach it.
            Self::MarkFolderRead => command(
                "mark-folder-read",
                "Mark Folder as Read",
                Messages,
                Scope::Window,
                &[],
            ),
            // Folder management (ADR-020, M9): on the folder right-clicked in the sidebar, else on
            // the folder shown; New Folder from an account's heading goes at the account's top.
            // No default keys until the user picks them (ADR-017).
            Self::NewFolder => command("new-folder", "New Folder…", Messages, Scope::Window, &[]),
            Self::RenameFolder => command(
                "rename-folder",
                "Rename Folder…",
                Messages,
                Scope::Window,
                &[],
            ),
            Self::DeleteFolder => command(
                "delete-folder",
                "Delete Folder…",
                Messages,
                Scope::Window,
                &[],
            ),
            // Toggles the selection of the message under the cursor, for batch actions.
            Self::Select => ActionSpec {
                key_scopes: LIST,
                ..command("select", "Select", Messages, Scope::List, &["x", "space"])
            },
            // Undoes the last mail action. Its keys are scoped, so text fields keep Ctrl+Z.
            Self::Undo => command("undo", "Undo", Messages, Scope::List, &["z", "<Control>z"]),
            // The message the reader shows (ML-063). Ctrl+P is the approved mock's; Ctrl+U and
            // Ctrl+S were accepted at the M5 gate (2026-10-06).
            Self::Print => on_selection("print", "Print…", Messages, &["<Control>p"]),
            Self::ShowOriginal => {
                on_selection("show-original", "Show Original", Messages, &["<Control>u"])
            }
            Self::SaveMessage => {
                on_selection("save-message", "Save as .eml…", Messages, &["<Control>s"])
            }
            Self::Compose => command(
                "compose",
                "Compose",
                Compose,
                Scope::Window,
                &["c", "<Control>n"],
            ),
            Self::Reply => on_selection("reply", "Reply", Compose, &["r", "<Control>r"]),
            Self::ReplyAll => on_selection(
                "reply-all",
                "Reply All",
                Compose,
                &["a", "<Control><Shift>r"],
            ),
            Self::ReplySender => on_selection("reply-sender", "Reply to Sender", Compose, &[]),
            Self::Forward => on_selection("forward", "Forward", Compose, &["f", "<Control>l"]),
            Self::Send => command(
                "send",
                "Send",
                Compose,
                Scope::Composer,
                &["<Control>Return"],
            ),
            // Opens the selected message of a Drafts folder in the composer (ML-074). No default
            // key until the user picks one.
            Self::EditDraft => on_selection("edit-draft", "Edit Draft", Compose, &[]),
            // The quick reply under the conversation (ML-068): Quick Reply puts the focus in it;
            // Open Reply in Composer takes what it holds into the full composer, with the
            // approved mock's Ctrl+Shift+E.
            Self::QuickReply => command("quick-reply", "Quick Reply", Compose, Scope::Reader, &[]),
            Self::ReplyInComposer => command(
                "reply-in-composer",
                "Open Reply in Composer",
                Compose,
                Scope::Reader,
                &["<Control><Shift>e"],
            ),
            // The composer window's own commands (ML-067, ML-072). Ctrl+B, Ctrl+I, Ctrl+U,
            // Ctrl+K and Ctrl+Z are the approved mock's; WebKitGTK binds only Ctrl+B, Ctrl+I and
            // Ctrl+A in editable content, so the composer binds the rest (ADR-014); redo's two
            // keys are ADR-014's, and paste as text's Ctrl+Shift+V is a proposal for the gate.
            Self::AttachFile => {
                command("attach-file", "Attach File…", Compose, Scope::Composer, &[])
            }
            Self::ShowCcBcc => command(
                "show-cc-bcc",
                "Add Cc and Bcc",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::DiscardDraft => command(
                "discard-draft",
                "Discard Draft",
                Compose,
                Scope::Composer,
                &[],
            ),
            // Saves the draft and closes the card, as its Close button does: Ctrl+W, the user's
            // pick after a user asked for a key (ADR-017 amendment, 2026-10-19).
            Self::CloseComposer => command(
                "close-composer",
                "Close Composer",
                Compose,
                Scope::Composer,
                &["<Control>w"],
            ),
            Self::Bold => command("bold", "Bold", Compose, Scope::Composer, &["<Control>b"]),
            Self::Italic => command(
                "italic",
                "Italic",
                Compose,
                Scope::Composer,
                &["<Control>i"],
            ),
            Self::Underline => command(
                "underline",
                "Underline",
                Compose,
                Scope::Composer,
                &["<Control>u"],
            ),
            Self::Strikethrough => command(
                "strikethrough",
                "Strikethrough",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::BulletedList => command(
                "bulleted-list",
                "Bulleted List",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::NumberedList => command(
                "numbered-list",
                "Numbered List",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::InsertLink => command(
                "insert-link",
                "Link…",
                Compose,
                Scope::Composer,
                &["<Control>k"],
            ),
            Self::Quote => command("quote", "Quote", Compose, Scope::Composer, &[]),
            Self::Indent => command("indent", "Indent", Compose, Scope::Composer, &[]),
            Self::Outdent => command("outdent", "Outdent", Compose, Scope::Composer, &[]),
            Self::ClearFormatting => command(
                "clear-formatting",
                "Clear Formatting",
                Compose,
                Scope::Composer,
                &[],
            ),
            Self::ComposerUndo => command(
                "composer-undo",
                "Undo Typing",
                Compose,
                Scope::Composer,
                &["<Control>z"],
            ),
            Self::ComposerRedo => command(
                "composer-redo",
                "Redo Typing",
                Compose,
                Scope::Composer,
                &["<Control><Shift>z", "<Control>y"],
            ),
            Self::PasteAsText => command(
                "paste-as-text",
                "Paste as Plain Text",
                Compose,
                Scope::Composer,
                &["<Control><Shift>v"],
            ),
            // The conversation the reader shows (ML-057) and its remote content (ML-058). No
            // default keys until the user picks them; the reader's More menu and the palette
            // reach them.
            Self::ExpandAll => command(
                "expand-all",
                "Expand All Messages",
                View,
                Scope::Reader,
                &[],
            ),
            Self::CollapseAll => command(
                "collapse-all",
                "Collapse All Messages",
                View,
                Scope::Reader,
                &[],
            ),
            Self::ShowImages => command("show-images", "Show Images", View, Scope::Reader, &[]),
            // On while the shown message's sender is on the list of senders whose images load.
            Self::AlwaysShowImages => ActionSpec {
                kind: ActionKind::Toggle,
                ..command(
                    "always-show-images",
                    "Always Show Images from This Sender",
                    View,
                    Scope::Reader,
                    &[],
                )
            },
            // Every F-key action has a key without one: the F-row sends media keys without Fn.
            Self::Refresh => ActionSpec {
                menu: Some(MenuSection::View),
                ..command(
                    "refresh",
                    "Refresh",
                    View,
                    Scope::Window,
                    &["<Control><Alt>r", "F5"],
                )
            },
            Self::ToggleSidebar => ActionSpec {
                kind: ActionKind::Toggle,
                menu: Some(MenuSection::View),
                ..command(
                    "toggle-sidebar",
                    "Toggle Sidebar",
                    View,
                    Scope::Window,
                    &["<Control><Shift>s", "F9"],
                )
            },
            Self::CommandPalette => ActionSpec {
                palette: false,
                menu: Some(MenuSection::View),
                ..command(
                    "command-palette",
                    "Command Palette",
                    View,
                    Scope::Window,
                    &["<Control><Shift>p", "F1"],
                )
            },
            // The setup assistant (ML-033), over the window. No default key: the palette, the
            // main menu and the welcome page reach it, and `keys.toml` can bind one.
            Self::AddAccount => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command("add-account", "Add Account", General, Scope::Window, &[])
            },
            // An account's name and colour in Sealwax (the user's UI tweak, 2026-10-08): asks
            // which account, then the engine writes `accounts.toml`. Available while the file
            // lists an account; no default key (ADR-017: keys wait for the user).
            Self::EditAccount => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command("edit-account", "Edit Account", General, Scope::Window, &[])
            },
            // ML-038: asks which account and whether, then the engine deletes it. Available
            // while `accounts.toml` lists an account.
            Self::RemoveAccount => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command(
                    "remove-account",
                    "Remove Account",
                    General,
                    Scope::Window,
                    &[],
                )
            },
            // `app.shortcuts` with Ctrl+? is libadwaita's convention. `?` needs Shift on both
            // layouts, so `h` is the unshifted alternative.
            Self::KeyboardShortcuts => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command(
                    "shortcuts",
                    "Keyboard Shortcuts",
                    General,
                    Scope::App,
                    &["question", "h", "<Control>question"],
                )
            },
            Self::Preferences => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command(
                    "preferences",
                    "Preferences (config.toml)",
                    General,
                    Scope::App,
                    &["<Control>comma"],
                )
            },
            Self::CustomizeShortcuts => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command(
                    "customize-shortcuts",
                    "Customize Shortcuts (keys.toml)",
                    General,
                    Scope::App,
                    &[],
                )
            },
            // ML-081: indexes every stored message again in the background; search keeps
            // working on the old index until the new one is done.
            Self::RebuildSearchIndex => command(
                "rebuild-search-index",
                "Rebuild Search Index",
                General,
                Scope::App,
                &[],
            ),
            // ML-084, opt-in autostart (PRD D6): on while Sealwax's autostart entry exists. Asks
            // before it writes the entry, the only file Sealwax puts outside its own folders.
            Self::StartAtLogin => ActionSpec {
                kind: ActionKind::Toggle,
                menu: Some(MenuSection::Settings),
                ..command(
                    "start-at-login",
                    "Start Sealwax at Login",
                    General,
                    Scope::App,
                    &[],
                )
            },
            // Asks before it makes Sealwax the desktop's handler for mailto: links (PRD C8): the
            // only change Sealwax makes outside its own folders, and only on the user's word.
            Self::DefaultMailApp => command(
                "default-mail-app",
                "Make Sealwax the Default Mail App…",
                General,
                Scope::App,
                &[],
            ),
            // Asks GitHub for the latest release, only when the user runs it (ADR-021 amendment,
            // 2026-10-20); a menu section of its own, between the settings and Quit. No default
            // key (ADR-017).
            Self::CheckForUpdates => ActionSpec {
                menu: Some(MenuSection::Updates),
                ..command(
                    "check-for-updates",
                    "Check for Updates…",
                    General,
                    Scope::App,
                    &[],
                )
            },
            Self::Quit => ActionSpec {
                menu: Some(MenuSection::Quit),
                ..command("quit", "Quit", General, Scope::App, &["<Control>q"])
            },
            // The account's rules (ADR-020, M9): the Rules dialog for the account the list shows,
            // the message commands on the selected messages' sender, and Apply Rules on the
            // folder shown. No default keys until the user picks them (ADR-017).
            Self::EditRules => ActionSpec {
                menu: Some(MenuSection::Settings),
                ..command("edit-rules", "Rules…", Rules, Scope::Window, &[])
            },
            Self::NewRuleFromMessage => on_selection(
                "new-rule-from-message",
                "Create Rule from Message…",
                Rules,
                &[],
            ),
            Self::JunkSender => on_selection(
                "junk-sender",
                "Always Junk Messages from Sender",
                Rules,
                &[],
            ),
            Self::TrustSender => on_selection(
                "trust-sender",
                "Never Junk Messages from Sender",
                Rules,
                &[],
            ),
            Self::ApplyRules => command(
                "apply-rules",
                "Apply Rules to This Folder",
                Rules,
                Scope::Window,
                &[],
            ),
        }
    }

    pub const fn name(self) -> &'static str {
        self.spec().name
    }

    pub const fn title(self) -> &'static str {
        self.spec().title
    }

    pub const fn category(self) -> Category {
        self.spec().category
    }

    pub const fn scope(self) -> Scope {
        self.spec().scope
    }

    pub const fn kind(self) -> ActionKind {
        self.spec().kind
    }

    /// The name GTK activates: `app.quit`, `win.archive`.
    pub fn detailed_name(self) -> String {
        format!("{}.{}", self.scope().group(), self.name())
    }

    /// The action named `name` (`archive`), as `keys.toml` writes it.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|id| id.name() == name)
    }

    /// The action behind a detailed name (`win.archive`).
    pub fn from_detailed_name(detailed: &str) -> Option<Self> {
        let (group, name) = detailed.split_once('.')?;
        Self::from_name(name).filter(|id| id.scope().group() == group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn all_lists_every_action_once_in_declaration_order() {
        let names: HashSet<_> = ActionId::ALL.iter().map(|id| id.name()).collect();
        assert_eq!(names.len(), ActionId::ALL.len());
        let mut sorted = ActionId::ALL;
        sorted.sort();
        assert_eq!(sorted, ActionId::ALL);
        for id in ActionId::ALL {
            assert_eq!(ActionId::from_name(id.name()), Some(id));
            assert_eq!(ActionId::from_detailed_name(&id.detailed_name()), Some(id));
        }
        assert_eq!(ActionId::from_name("no-such-action"), None);
        assert_eq!(ActionId::from_detailed_name("app.archive"), None);
        assert_eq!(ActionId::from_detailed_name("archive"), None);
    }

    #[test]
    fn names_are_kebab_case_gaction_names() {
        for id in ActionId::ALL {
            let name = id.name();
            assert!(!name.starts_with('-') && !name.ends_with('-'), "{name}");
            assert!(!name.contains("--"), "{name}");
            assert!(name.starts_with(|c: char| c.is_ascii_lowercase()), "{name}");
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "{name}"
            );
        }
    }

    #[test]
    fn titles_are_present_and_unique() {
        let titles: HashSet<_> = ActionId::ALL.iter().map(|id| id.title()).collect();
        assert_eq!(titles.len(), ActionId::ALL.len());
        for id in ActionId::ALL {
            assert!(!id.title().trim().is_empty(), "{id:?}");
        }
    }

    #[test]
    fn categories_follow_declaration_order() {
        let mut last = Category::ALL[0];
        for id in ActionId::ALL {
            assert!(
                id.category() >= last,
                "{id:?} is out of its category's place"
            );
            last = id.category();
        }
        for category in Category::ALL {
            assert!(
                ActionId::ALL.iter().any(|id| id.category() == category),
                "{category:?} is empty"
            );
        }
    }

    #[test]
    fn scopes_and_groups() {
        for id in ActionId::ALL {
            let spec = id.spec();
            let group = if spec.scope == Scope::App {
                "app"
            } else {
                "win"
            };
            assert_eq!(spec.scope.group(), group, "{id:?}");
            assert!(!spec.key_scopes.is_empty(), "{id:?}");
            for scope in spec.key_scopes {
                assert!(
                    Scope::SCOPED.contains(scope),
                    "{id:?} has keys in {scope:?}"
                );
            }
            match spec.scope {
                Scope::App | Scope::Window => assert_eq!(spec.key_scopes, MAIL, "{id:?}"),
                Scope::List => assert!(spec.key_scopes == MAIL || spec.key_scopes == LIST),
                Scope::Reader => assert_eq!(spec.key_scopes, READER, "{id:?}"),
                Scope::Composer => assert_eq!(spec.key_scopes, COMPOSER, "{id:?}"),
            }
            if !spec.list_keys.is_empty() {
                assert!(spec.key_scopes.contains(&Scope::List), "{id:?}");
            }
        }
        assert_eq!(ActionId::Quit.detailed_name(), "app.quit");
        assert_eq!(ActionId::KeyboardShortcuts.detailed_name(), "app.shortcuts");
        assert_eq!(ActionId::Archive.detailed_name(), "win.archive");
        assert_eq!(ActionId::Send.detailed_name(), "win.send");
        assert!(Scope::List.allows_one_key() && Scope::Reader.allows_one_key());
        assert!(!Scope::Composer.allows_one_key() && !Scope::Window.allows_one_key());
    }

    #[test]
    fn every_action_is_reachable() {
        for id in ActionId::ALL {
            let spec = id.spec();
            assert!(spec.palette || spec.menu.is_some(), "{id:?} is unreachable");
        }
        let menu: Vec<ActionId> = ActionId::ALL
            .into_iter()
            .filter(|id| id.spec().menu.is_some())
            .collect();
        assert_eq!(
            menu,
            [
                ActionId::Refresh,
                ActionId::ToggleSidebar,
                ActionId::CommandPalette,
                ActionId::AddAccount,
                ActionId::EditAccount,
                ActionId::RemoveAccount,
                ActionId::KeyboardShortcuts,
                ActionId::Preferences,
                ActionId::CustomizeShortcuts,
                ActionId::StartAtLogin,
                ActionId::CheckForUpdates,
                ActionId::Quit,
                ActionId::EditRules,
            ]
        );
        assert!(!ActionId::CommandPalette.spec().palette);
        assert!(ActionId::AddAccount.spec().palette);
        assert_eq!(ActionId::AddAccount.detailed_name(), "win.add-account");
        assert_eq!(ActionId::ToggleSidebar.kind(), ActionKind::Toggle);
        assert_eq!(ActionId::StartAtLogin.kind(), ActionKind::Toggle);
        assert_eq!(ActionId::StartAtLogin.detailed_name(), "app.start-at-login");
    }

    #[test]
    fn only_message_commands_need_a_selection() {
        let needs: Vec<ActionId> = ActionId::ALL
            .into_iter()
            .filter(|id| id.spec().needs_selection)
            .collect();
        assert_eq!(
            needs,
            [
                ActionId::OpenMessage,
                ActionId::Archive,
                ActionId::Delete,
                ActionId::Junk,
                ActionId::MoveTo,
                ActionId::Star,
                ActionId::MarkRead,
                ActionId::MarkUnread,
                ActionId::Print,
                ActionId::ShowOriginal,
                ActionId::SaveMessage,
                ActionId::Reply,
                ActionId::ReplyAll,
                ActionId::ReplySender,
                ActionId::Forward,
                ActionId::EditDraft,
                ActionId::NewRuleFromMessage,
                ActionId::JunkSender,
                ActionId::TrustSender,
            ]
        );
        for id in needs {
            assert_eq!(id.scope(), Scope::List, "{id:?}");
        }
    }

    /// DESIGN.md's keymap table (2026-10-04), plus the keys this registry adds to it.
    #[test]
    fn the_design_keymap() {
        let keys = |id: ActionId| id.spec().keys.to_vec();
        assert_eq!(keys(ActionId::NextMessage), ["j"]);
        assert_eq!(ActionId::NextMessage.spec().list_keys, ["Down"]);
        assert_eq!(keys(ActionId::PreviousMessage), ["k"]);
        assert_eq!(ActionId::PreviousMessage.spec().list_keys, ["Up"]);
        assert_eq!(keys(ActionId::OpenMessage), ["Return", "o", "l"]);
        assert_eq!(keys(ActionId::BackToList), ["Escape", "u"]);
        assert_eq!(keys(ActionId::Compose), ["c", "<Control>n"]);
        assert_eq!(keys(ActionId::Reply), ["r", "<Control>r"]);
        assert_eq!(keys(ActionId::ReplyAll), ["a", "<Control><Shift>r"]);
        assert_eq!(keys(ActionId::Forward), ["f", "<Control>l"]);
        assert_eq!(keys(ActionId::Archive), ["e", "y"]);
        assert_eq!(keys(ActionId::Delete), ["numbersign", "Delete"]);
        assert_eq!(keys(ActionId::Junk), ["exclam", "<Control>j"]);
        assert_eq!(keys(ActionId::MoveTo), ["v", "<Control><Shift>v"]);
        assert_eq!(keys(ActionId::Star), ["s"]);
        assert_eq!(keys(ActionId::MarkRead), ["<Shift>i"]);
        assert_eq!(keys(ActionId::MarkUnread), ["<Shift>u"]);
        assert_eq!(keys(ActionId::Select), ["x", "space"]);
        assert_eq!(keys(ActionId::Search), ["slash", "<Control>f"]);
        assert_eq!(keys(ActionId::GoToInbox), ["g i"]);
        assert_eq!(keys(ActionId::GoToStarred), ["g s"]);
        assert_eq!(keys(ActionId::GoToSent), ["g t"]);
        assert_eq!(keys(ActionId::GoToDrafts), ["g d"]);
        // Issues #10 and #12, accepted 2026-10-19.
        assert_eq!(keys(ActionId::ScrollMessageDown), ["d"]);
        assert_eq!(keys(ActionId::ScrollMessageUp), ["<Shift>d"]);
        assert_eq!(keys(ActionId::GoToAllInboxes), ["g a"]);
        for number in 1..=9 {
            let id = ActionId::go_to_account(number).unwrap();
            assert_eq!(keys(id), [format!("g {number}")], "{id:?}");
            assert_eq!(id.name(), format!("go-to-account-{number}"));
        }
        assert_eq!(ActionId::go_to_account(0), None);
        assert_eq!(ActionId::go_to_account(10), None);
        assert_eq!(keys(ActionId::Send), ["<Control>Return"]);
        assert_eq!(keys(ActionId::Undo), ["z", "<Control>z"]);
        assert_eq!(keys(ActionId::CommandPalette), ["<Control><Shift>p", "F1"]);
        // DESIGN lists `?` and Ctrl+?; both need Shift, so `h` is added (proposed for DESIGN).
        assert_eq!(
            keys(ActionId::KeyboardShortcuts),
            ["question", "h", "<Control>question"]
        );
        // Not in DESIGN's table (proposed).
        assert_eq!(keys(ActionId::Preferences), ["<Control>comma"]);
        assert_eq!(keys(ActionId::Quit), ["<Control>q"]);
        assert_eq!(keys(ActionId::Refresh), ["<Control><Alt>r", "F5"]);
        assert_eq!(keys(ActionId::ToggleSidebar), ["<Control><Shift>s", "F9"]);
        assert!(keys(ActionId::CustomizeShortcuts).is_empty());
        // No default key until the user picks one (ADR-017: keys wait for the user).
        assert!(keys(ActionId::AddAccount).is_empty());
        assert!(keys(ActionId::EditAccount).is_empty());
        assert!(keys(ActionId::MarkFolderRead).is_empty());
        for id in [
            ActionId::NewFolder,
            ActionId::RenameFolder,
            ActionId::DeleteFolder,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
            assert_eq!(id.scope(), Scope::Window, "{id:?}");
            assert!(id.spec().palette && id.spec().menu.is_none(), "{id:?}");
        }
        assert_eq!(keys(ActionId::Print), ["<Control>p"]);
        assert_eq!(keys(ActionId::ShowOriginal), ["<Control>u"]);
        assert_eq!(keys(ActionId::SaveMessage), ["<Control>s"]);
        assert_eq!(keys(ActionId::NextInConversation), ["n"]);
        assert_eq!(keys(ActionId::PreviousInConversation), ["p"]);
        for id in [
            ActionId::ExpandAll,
            ActionId::CollapseAll,
            ActionId::ShowImages,
            ActionId::AlwaysShowImages,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
        }
        assert_eq!(ActionId::AlwaysShowImages.kind(), ActionKind::Toggle);
        // M6: the mock's composer and quick reply keys, ADR-014's redo keys, and paste as
        // text's proposed Ctrl+Shift+V.
        assert_eq!(keys(ActionId::Bold), ["<Control>b"]);
        assert_eq!(keys(ActionId::Italic), ["<Control>i"]);
        assert_eq!(keys(ActionId::Underline), ["<Control>u"]);
        assert_eq!(keys(ActionId::InsertLink), ["<Control>k"]);
        assert_eq!(keys(ActionId::ComposerUndo), ["<Control>z"]);
        assert_eq!(
            keys(ActionId::ComposerRedo),
            ["<Control><Shift>z", "<Control>y"]
        );
        assert_eq!(keys(ActionId::PasteAsText), ["<Control><Shift>v"]);
        assert_eq!(keys(ActionId::ReplyInComposer), ["<Control><Shift>e"]);
        assert_eq!(keys(ActionId::CloseComposer), ["<Control>w"]);
        for id in [
            ActionId::EditDraft,
            ActionId::QuickReply,
            ActionId::AttachFile,
            ActionId::ShowCcBcc,
            ActionId::DiscardDraft,
            ActionId::Strikethrough,
            ActionId::BulletedList,
            ActionId::NumberedList,
            ActionId::Quote,
            ActionId::Indent,
            ActionId::Outdent,
            ActionId::ClearFormatting,
            ActionId::DefaultMailApp,
            ActionId::RebuildSearchIndex,
            ActionId::StartAtLogin,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
        }
        // M9's rules (ADR-020): no default keys until the user picks them.
        for id in [
            ActionId::EditRules,
            ActionId::NewRuleFromMessage,
            ActionId::JunkSender,
            ActionId::TrustSender,
            ActionId::ApplyRules,
        ] {
            assert!(keys(id).is_empty(), "{id:?}");
            assert_eq!(id.category(), Category::Rules, "{id:?}");
            assert!(id.spec().palette, "{id:?}");
        }
        assert_eq!(ActionId::EditRules.detailed_name(), "win.edit-rules");
        assert_eq!(ActionId::ApplyRules.detailed_name(), "win.apply-rules");
    }
}
