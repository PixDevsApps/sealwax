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
}

impl Category {
    pub const ALL: [Self; 5] = [
        Self::Navigation,
        Self::Messages,
        Self::Compose,
        Self::View,
        Self::General,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Messages => "Messages",
            Self::Compose => "Compose",
            Self::View => "View",
            Self::General => "General",
        }
    }
}

/// The primary menu's sections, in their order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MenuSection {
    View,
    Settings,
    Quit,
}

impl MenuSection {
    pub const ALL: [Self; 3] = [Self::View, Self::Settings, Self::Quit];
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
    OpenMessage,
    BackToList,
    Search,
    GoToInbox,
    GoToStarred,
    GoToSent,
    GoToDrafts,
    Archive,
    Delete,
    Junk,
    MoveTo,
    Star,
    MarkRead,
    MarkUnread,
    Select,
    Undo,
    Compose,
    Reply,
    ReplyAll,
    Forward,
    Send,
    Refresh,
    ToggleSidebar,
    CommandPalette,
    AddAccount,
    RemoveAccount,
    KeyboardShortcuts,
    Preferences,
    CustomizeShortcuts,
    Quit,
}

impl ActionId {
    pub const ALL: [Self; 32] = [
        Self::NextMessage,
        Self::PreviousMessage,
        Self::OpenMessage,
        Self::BackToList,
        Self::Search,
        Self::GoToInbox,
        Self::GoToStarred,
        Self::GoToSent,
        Self::GoToDrafts,
        Self::Archive,
        Self::Delete,
        Self::Junk,
        Self::MoveTo,
        Self::Star,
        Self::MarkRead,
        Self::MarkUnread,
        Self::Select,
        Self::Undo,
        Self::Compose,
        Self::Reply,
        Self::ReplyAll,
        Self::Forward,
        Self::Send,
        Self::Refresh,
        Self::ToggleSidebar,
        Self::CommandPalette,
        Self::AddAccount,
        Self::RemoveAccount,
        Self::KeyboardShortcuts,
        Self::Preferences,
        Self::CustomizeShortcuts,
        Self::Quit,
    ];

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
            Self::OpenMessage => ActionSpec {
                key_scopes: LIST,
                ..on_selection("open-message", "Open Message", Navigation, &["Return", "o"])
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
            // Toggles the selection of the message under the cursor, for batch actions.
            Self::Select => ActionSpec {
                key_scopes: LIST,
                ..command("select", "Select", Messages, Scope::List, &["x", "space"])
            },
            // Undoes the last mail action. Its keys are scoped, so text fields keep Ctrl+Z.
            Self::Undo => command("undo", "Undo", Messages, Scope::List, &["z", "<Control>z"]),
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
            Self::Forward => on_selection("forward", "Forward", Compose, &["f", "<Control>l"]),
            Self::Send => command(
                "send",
                "Send",
                Compose,
                Scope::Composer,
                &["<Control>Return"],
            ),
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
            Self::Quit => ActionSpec {
                menu: Some(MenuSection::Quit),
                ..command("quit", "Quit", General, Scope::App, &["<Control>q"])
            },
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
                name.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
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
                ActionId::RemoveAccount,
                ActionId::KeyboardShortcuts,
                ActionId::Preferences,
                ActionId::CustomizeShortcuts,
                ActionId::Quit,
            ]
        );
        assert!(!ActionId::CommandPalette.spec().palette);
        assert!(ActionId::AddAccount.spec().palette);
        assert_eq!(ActionId::AddAccount.detailed_name(), "win.add-account");
        assert_eq!(ActionId::ToggleSidebar.kind(), ActionKind::Toggle);
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
                ActionId::Reply,
                ActionId::ReplyAll,
                ActionId::Forward,
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
        assert_eq!(keys(ActionId::OpenMessage), ["Return", "o"]);
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
    }
}
