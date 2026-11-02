//! Keys (ADR-017): GTK accelerator strings and two-key sequences, the US and Norwegian
//! layouts, the keymap with `keys.toml`'s changes, and the audit every keymap passes: the
//! registry's defaults in the tests, and `keys.toml` before it is applied.
//!
//! Where a key works follows from the key: a key with Ctrl or Alt, or an F-key, of an action
//! that works anywhere ([`Scope::is_global`]) is an application accelerator, which GTK runs
//! before the focused widget. Every other key is a scoped shortcut, handled by the widgets of
//! the action's `key_scopes` after the focused widget, so text fields keep their keys.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;

use toml::de::DeValue;

use crate::actions::{ActionId, Category, Scope};
use crate::config::{FileError, article, did_you_mean, entries, parse_toml};

/// A single key with its modifiers, as GTK spells accelerators: `<Control><Shift>r`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Accelerator {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub super_key: bool,
    /// The key's GTK name; letters are lowercase, as GTK's parser makes them.
    pub key: String,
}

impl Accelerator {
    /// Parses `<Control><Shift>z`-style strings. Accepts GTK's modifier spellings; rejects
    /// unknown modifiers and key names [`is_key_name`] doesn't know.
    pub fn parse(accel: &str) -> Option<Self> {
        let mut rest = accel;
        let mut parsed = Self {
            control: false,
            shift: false,
            alt: false,
            super_key: false,
            key: String::new(),
        };
        while let Some(tail) = rest.strip_prefix('<') {
            let (modifier, after) = tail.split_once('>')?;
            match modifier.to_ascii_lowercase().as_str() {
                "control" | "ctrl" | "ctl" | "primary" => parsed.control = true,
                "shift" | "shft" => parsed.shift = true,
                "alt" | "mod1" => parsed.alt = true,
                "super" => parsed.super_key = true,
                _ => return None,
            }
            rest = after;
        }
        if !is_key_name(rest) {
            return None;
        }
        parsed.key = if rest.chars().count() == 1 {
            rest.to_ascii_lowercase()
        } else {
            rest.to_owned()
        };
        Some(parsed)
    }

    /// F1–F35, which many keyboards send as media keys unless Fn is held.
    pub fn is_function_key(&self) -> bool {
        self.key
            .strip_prefix('F')
            .and_then(|number| number.parse::<u8>().ok())
            .is_some_and(|number| (1..=35).contains(&number))
    }

    /// A key without Ctrl, Alt or Super that isn't an F-key: in a text field it types or edits
    /// text, so it may only be a scoped shortcut of the list or the reader.
    pub fn is_one_key(&self) -> bool {
        !self.control && !self.alt && !self.super_key && !self.is_function_key()
    }

    /// One spelling per key, `<Control><Shift><Alt><Super>key`, which GTK also parses.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.control, "<Control>"),
            (self.shift, "<Shift>"),
            (self.alt, "<Alt>"),
            (self.super_key, "<Super>"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        out.push_str(&self.key);
        out
    }

    /// `Ctrl+Shift+R`, `Shift+I`; a one-key letter stays lowercase (`e`).
    pub fn display(&self) -> String {
        let mut out = String::new();
        for (on, name) in [
            (self.super_key, "Super+"),
            (self.control, "Ctrl+"),
            (self.alt, "Alt+"),
            (self.shift, "Shift+"),
        ] {
            if on {
                out.push_str(name);
            }
        }
        let plain = !(self.super_key || self.control || self.alt || self.shift);
        out.push_str(&key_label(&self.key, plain));
        out
    }
}

fn key_label(key: &str, plain: bool) -> String {
    let named = match key {
        "Return" => "Enter",
        "KP_Enter" => "Num Enter",
        "BackSpace" => "Backspace",
        "space" => "Space",
        "Page_Up" => "PgUp",
        "Page_Down" => "PgDn",
        "KP_Add" => "Num +",
        "KP_Subtract" => "Num -",
        "KP_Multiply" => "Num *",
        "KP_Divide" => "Num /",
        _ => "",
    };
    if !named.is_empty() {
        return named.to_owned();
    }
    if let Some(layout_key) = layout_key(key) {
        return layout_key.symbol.to_owned();
    }
    match key.strip_prefix("KP_") {
        Some(rest) => format!("Num {}", key_label(rest, plain)),
        None if key.chars().count() == 1 && !plain => key.to_ascii_uppercase(),
        None => key.to_owned(),
    }
}

/// Key names a binding may use: letters, digits, F-keys, the keys of [`LAYOUT_KEYS`] and a
/// fixed list of GDK key names. The app's tests also run every default key through GTK's own
/// parser.
pub fn is_key_name(key: &str) -> bool {
    const NAMED: &[&str] = &[
        "Return",
        "Tab",
        "Escape",
        "Delete",
        "Insert",
        "BackSpace",
        "Home",
        "End",
        "Page_Up",
        "Page_Down",
        "Up",
        "Down",
        "Left",
        "Right",
        "space",
        "Menu",
        "Print",
        "KP_Enter",
        "KP_Add",
        "KP_Subtract",
        "KP_Multiply",
        "KP_Divide",
        "KP_Up",
        "KP_Down",
        "KP_Left",
        "KP_Right",
        "KP_Page_Up",
        "KP_Page_Down",
        "KP_Home",
        "KP_End",
    ];
    let mut chars = key.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => c.is_ascii_alphanumeric(),
        _ => {
            NAMED.contains(&key)
                || layout_key(key).is_some()
                || key
                    .strip_prefix('F')
                    .and_then(|number| number.parse::<u8>().ok())
                    .is_some_and(|number| (1..=35).contains(&number))
        }
    }
}

/// A key, or a two-key sequence such as `g` then `i`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Binding {
    pub first: Accelerator,
    pub then: Option<Accelerator>,
}

/// Why a string is not a [`Binding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingError {
    NotAKey,
    TooManyKeys,
}

impl Binding {
    /// Parses one key (`"<Control>r"`) or a sequence of two separated by a space (`"g i"`).
    pub fn parse(text: &str) -> Result<Self, BindingError> {
        let mut steps = text.split_whitespace();
        let first = steps.next().ok_or(BindingError::NotAKey)?;
        let then = steps.next();
        if steps.next().is_some() {
            return Err(BindingError::TooManyKeys);
        }
        let first = Accelerator::parse(first).ok_or(BindingError::NotAKey)?;
        let then = then
            .map(|step| Accelerator::parse(step).ok_or(BindingError::NotAKey))
            .transpose()?;
        Ok(Self { first, then })
    }

    /// The key, when the binding is not a sequence.
    pub fn single(&self) -> Option<&Accelerator> {
        self.then.is_none().then_some(&self.first)
    }

    pub fn steps(&self) -> impl Iterator<Item = &Accelerator> {
        std::iter::once(&self.first).chain(&self.then)
    }

    pub fn is_sequence(&self) -> bool {
        self.then.is_some()
    }

    /// A single F-key.
    pub fn is_function_key(&self) -> bool {
        self.single().is_some_and(Accelerator::is_function_key)
    }

    /// `<Control>r`, or `g i` for a sequence: the form `keys.toml` uses.
    pub fn canonical(&self) -> String {
        self.steps()
            .map(Accelerator::canonical)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// `Ctrl+R`, or `g then i`.
    pub fn display(&self) -> String {
        self.steps()
            .map(Accelerator::display)
            .collect::<Vec<_>>()
            .join(" then ")
    }

    /// AdwShortcutLabel's syntax: `<Control>r`, or `g+i` for a sequence.
    pub fn label(&self) -> String {
        self.steps()
            .map(Accelerator::canonical)
            .collect::<Vec<_>>()
            .join("+")
    }
}

impl fmt::Display for Binding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// The keyboard layouts every key must work on: the user's and the most common one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layout {
    Us,
    Norwegian,
}

impl Layout {
    pub const ALL: [Self; 2] = [Self::Us, Self::Norwegian];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Us => "US",
            Self::Norwegian => "Norwegian",
        }
    }
}

/// How a key is typed on a layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Base,
    Shift,
    AltGr,
    /// Not on the layout, or only as a dead key.
    Missing,
}

/// A key whose place differs between the layouts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayoutKey {
    pub name: &'static str,
    pub symbol: &'static str,
    pub us: Level,
    pub norwegian: Level,
}

const fn layout(
    name: &'static str,
    symbol: &'static str,
    us: Level,
    norwegian: Level,
) -> LayoutKey {
    LayoutKey {
        name,
        symbol,
        us,
        norwegian,
    }
}

/// The symbol keys on the US and Norwegian layouts, from xkeyboard-config's `us(basic)` and
/// `no(basic)` (with `latin(type2)`), read on 2026-10-04. Letters a–z, digits, F-keys and the
/// named keys are in the same place on both.
pub static LAYOUT_KEYS: [LayoutKey; 39] = {
    use Level::*;
    [
        layout("exclam", "!", Shift, Shift),
        layout("at", "@", Shift, AltGr),
        layout("numbersign", "#", Shift, Shift),
        layout("dollar", "$", Shift, AltGr),
        layout("percent", "%", Shift, Shift),
        layout("asciicircum", "^", Shift, Missing),
        layout("ampersand", "&", Shift, Shift),
        layout("asterisk", "*", Shift, Shift),
        layout("parenleft", "(", Shift, Shift),
        layout("parenright", ")", Shift, Shift),
        layout("minus", "-", Base, Base),
        layout("underscore", "_", Shift, Shift),
        layout("equal", "=", Base, Shift),
        layout("plus", "+", Shift, Base),
        layout("bracketleft", "[", Base, AltGr),
        layout("bracketright", "]", Base, AltGr),
        layout("braceleft", "{", Shift, AltGr),
        layout("braceright", "}", Shift, AltGr),
        layout("semicolon", ";", Base, Shift),
        layout("colon", ":", Shift, Shift),
        layout("apostrophe", "'", Base, Base),
        layout("quotedbl", "\"", Shift, Shift),
        layout("grave", "`", Base, Missing),
        layout("asciitilde", "~", Shift, Missing),
        layout("backslash", "\\", Base, Base),
        layout("bar", "|", Shift, Base),
        layout("comma", ",", Base, Base),
        layout("less", "<", Shift, Base),
        layout("period", ".", Base, Base),
        layout("greater", ">", Shift, Shift),
        layout("slash", "/", Base, Shift),
        layout("question", "?", Shift, Shift),
        layout("sterling", "£", Missing, AltGr),
        layout("EuroSign", "€", Missing, AltGr),
        layout("section", "§", Missing, Shift),
        layout("currency", "¤", Missing, Shift),
        layout("aring", "å", Missing, Base),
        layout("ae", "æ", Missing, Base),
        layout("oslash", "ø", Missing, Base),
    ]
};

pub fn layout_key(name: &str) -> Option<&'static LayoutKey> {
    LAYOUT_KEYS.iter().find(|key| key.name == name)
}

/// How `key` is typed on `layout`, or `None` when it is the same on every layout.
pub fn level(key: &str, layout: Layout) -> Option<Level> {
    layout_key(key).map(|key| match layout {
        Layout::Us => key.us,
        Layout::Norwegian => key.norwegian,
    })
}

/// Keys that never reach Sealwax, with what takes them: fcitx5's hotkeys (5.1.22, found in
/// its libraries by Stet), window and system switching, and Omarchy's non-SUPER binds
/// (`/usr/share/omarchy/default/hypr/bindings/`, 2026-10-04). Ctrl+Alt+F-keys switch virtual
/// terminals, and SUPER belongs to Hyprland.
pub const GRABBED: &[(&str, &str)] = &[
    ("<Control>space", "fcitx5's input method switch"),
    ("<Control><Shift>u", "fcitx5's Unicode entry"),
    ("<Control><Alt><Shift>u", "fcitx5's Unicode entry"),
    ("<Control><Alt>p", "fcitx5's embedded preedit switch"),
    ("<Control><Alt>Delete", "the system"),
    ("<Alt>Tab", "window switching"),
    ("<Alt><Shift>Tab", "window switching"),
    ("<Control><Alt>Tab", "window switching"),
    ("Print", "Omarchy's screenshot"),
    ("<Alt>Print", "Omarchy's screen recording"),
];

/// GTK's keys for moving the keyboard focus and opening menus.
pub const FOCUS_KEYS: &[&str] = &[
    "Tab",
    "<Shift>Tab",
    "<Control>Tab",
    "<Control><Shift>Tab",
    "F10",
    "<Shift>F10",
    "Menu",
];

/// Keys that text fields (GtkText, GtkTextView) and the composer (WebKit's editing, and the
/// keys ADR-014 gives it) use themselves. Application accelerators run before the focused
/// widget, so none of these may be one. Ctrl+Shift+V is held for M6's paste without
/// formatting.
pub const TEXT_KEYS: &[&str] = &[
    "<Control>a",
    "<Control><Shift>a",
    "<Control>c",
    "<Control>x",
    "<Control>v",
    "<Control><Shift>v",
    "<Control>Insert",
    "<Control>z",
    "<Control><Shift>z",
    "<Control>y",
    "<Control>BackSpace",
    "<Control>Delete",
    "<Control>Left",
    "<Control>Right",
    "<Control>Up",
    "<Control>Down",
    "<Control>Home",
    "<Control>End",
    "<Control><Shift>Left",
    "<Control><Shift>Right",
    "<Control><Shift>Up",
    "<Control><Shift>Down",
    "<Control><Shift>Home",
    "<Control><Shift>End",
    "<Control>period",
    "<Control>semicolon",
    "<Control>slash",
    "<Control>backslash",
    "<Control>b",
    "<Control>i",
    "<Control>u",
    "<Control>k",
];

fn listed(list: &[&str], key: &Accelerator) -> bool {
    list.iter()
        .any(|entry| Accelerator::parse(entry).as_ref() == Some(key))
}

/// Whether `binding` of `id` is an application accelerator rather than a scoped shortcut.
pub fn is_accelerator(id: ActionId, binding: &Binding) -> bool {
    id.scope().is_global()
        && binding
            .single()
            .is_some_and(|key| key.control || key.alt || key.is_function_key())
}

/// The registry's keys with the user's lists in place of an action's own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    overrides: HashMap<ActionId, Vec<Binding>>,
}

fn defaults(keys: &[&str]) -> Vec<Binding> {
    keys.iter()
        .filter_map(|key| Binding::parse(key).ok())
        .collect()
}

impl Keymap {
    /// The defaults with `overrides` in place of those actions' keys.
    pub fn with_overrides(overrides: impl IntoIterator<Item = (ActionId, Vec<Binding>)>) -> Self {
        Self {
            overrides: overrides.into_iter().collect(),
        }
    }

    /// The keys of `id` Sealwax installs, in the order the action lists them.
    pub fn keys(&self, id: ActionId) -> Vec<Binding> {
        match self.overrides.get(&id) {
            Some(keys) => keys.clone(),
            None => defaults(id.spec().keys),
        }
    }

    /// The message list's own keys for `id`: shown, never installed.
    pub fn list_keys(id: ActionId) -> Vec<Binding> {
        defaults(id.spec().list_keys)
    }

    /// The application accelerators of `id`, F-keys last, since menus show the first.
    pub fn accels(&self, id: ActionId) -> Vec<String> {
        let mut accels: Vec<Binding> = self
            .keys(id)
            .into_iter()
            .filter(|key| is_accelerator(id, key))
            .collect();
        accels.sort_by_key(Binding::is_function_key);
        accels.iter().map(Binding::canonical).collect()
    }

    /// The scoped shortcuts of `id`, sequences included.
    pub fn scoped(&self, id: ActionId) -> Vec<Binding> {
        self.keys(id)
            .into_iter()
            .filter(|key| !is_accelerator(id, key))
            .collect()
    }

    /// Every scoped shortcut that works in `scope`, with its action, in registry order.
    pub fn scoped_in(&self, scope: Scope) -> Vec<(Binding, ActionId)> {
        ActionId::ALL
            .into_iter()
            .filter(|id| id.spec().key_scopes.contains(&scope))
            .flat_map(|id| self.scoped(id).into_iter().map(move |key| (key, id)))
            .collect()
    }

    /// Every key of `id` to show: its own, F-keys after the others, then the list's.
    pub fn shown(&self, id: ActionId) -> Vec<Binding> {
        let mut keys = self.keys(id);
        keys.sort_by_key(Binding::is_function_key);
        keys.extend(Self::list_keys(id));
        keys
    }

    /// `r, Ctrl+R`, or an empty string for an action without keys.
    pub fn display(&self, id: ActionId) -> String {
        self.shown(id)
            .iter()
            .map(Binding::display)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Every key of `id` in AdwShortcutLabel's syntax: alternatives separated by spaces.
    pub fn label(&self, id: ActionId) -> String {
        self.shown(id)
            .iter()
            .map(Binding::label)
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Whether `keys.toml` changed the keys of `id`.
    pub fn is_changed(&self, id: ActionId) -> bool {
        self.overrides.contains_key(&id)
    }

    /// How many actions `keys.toml` changed.
    pub fn changed_count(&self) -> usize {
        self.overrides.len()
    }
}

/// A rule one key breaks by itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyFault {
    Super,
    AltGr(Layout),
    Missing(Layout),
    /// Shift with a digit or a symbol, which types another symbol.
    ShiftedSymbol,
    /// A one-key shortcut where text is typed.
    OneKeyInComposer,
    Grabbed(&'static str),
    FocusKey,
    /// An application accelerator that a text field or the composer uses.
    TextKey,
}

/// What is wrong with a keymap.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Fault {
    Key(KeyFault),
    /// `other_key`, the same key or an application accelerator that runs before it, belongs
    /// to `other` where both work; `list_key` when it is the message list's own key.
    Taken {
        other: ActionId,
        other_key: Binding,
        list_key: bool,
    },
    /// One key starts the other, a sequence.
    Prefix {
        other: ActionId,
        other_key: Binding,
    },
    /// Only F-keys, which many keyboards send as media keys unless Fn is held.
    OnlyFunctionKeys,
    /// Every key needs Shift for its symbol on these layouts, like `?`.
    NoUnshifted(Vec<Layout>),
}

/// A problem with one action's keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Problem {
    pub action: ActionId,
    pub key: Option<Binding>,
    pub fault: Fault,
}

impl KeyFault {
    fn describe(self) -> String {
        match self {
            Self::Super => "uses Super, which belongs to Hyprland".to_owned(),
            Self::AltGr(layout) => format!(
                "needs AltGr on the {} layout; pick a key both the US and the Norwegian layout \
                 type without it",
                layout.label()
            ),
            Self::Missing(layout) => format!("is not on the {} layout", layout.label()),
            Self::ShiftedSymbol => "adds Shift to a digit or a symbol, which types another \
                                    symbol; write the symbol itself, like question for ?"
                .to_owned(),
            Self::OneKeyInComposer => "has no Ctrl or Alt; the composer keeps its letters, so its \
                                       keys need Ctrl or Alt (F-keys are the exception)"
                .to_owned(),
            Self::Grabbed(by) => format!("never reaches Sealwax: {by} takes it"),
            Self::FocusKey => "moves the keyboard focus or opens a menu".to_owned(),
            Self::TextKey => "is a text field's or the composer's own key, and a key with Ctrl \
                              of a command that works anywhere would take it from them"
                .to_owned(),
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let action = self.action.name();
        let key = self
            .key
            .as_ref()
            .map(Binding::canonical)
            .unwrap_or_default();
        match &self.fault {
            Fault::Key(fault) => write!(f, "{action}: “{key}” {}", fault.describe()),
            Fault::Taken {
                other, list_key, ..
            } => {
                let whose = if *list_key {
                    "the list's own key for "
                } else {
                    ""
                };
                write!(f, "{action}: “{key}” is also {whose}{}", other.name())
            }
            Fault::Prefix { other, other_key } => write!(
                f,
                "{action}: “{key}” and {}'s “{other_key}” start the same way",
                other.name()
            ),
            Fault::OnlyFunctionKeys => write!(f, "{action} has only F-keys"),
            Fault::NoUnshifted(layouts) => write!(
                f,
                "{action} has no key without Shift on the {}",
                layouts_label(layouts)
            ),
        }
    }
}

/// The rules a key of `id` must follow on its own.
pub fn key_fault(id: ActionId, binding: &Binding) -> Option<KeyFault> {
    let composer = id.spec().key_scopes.contains(&Scope::Composer);
    for step in binding.steps() {
        if step.super_key {
            return Some(KeyFault::Super);
        }
        if let Some(key) = layout_key(&step.key) {
            for (layout, level) in [(Layout::Us, key.us), (Layout::Norwegian, key.norwegian)] {
                match level {
                    Level::AltGr => return Some(KeyFault::AltGr(layout)),
                    Level::Missing => return Some(KeyFault::Missing(layout)),
                    Level::Base | Level::Shift => {}
                }
            }
            if step.shift {
                return Some(KeyFault::ShiftedSymbol);
            }
        }
        if step.shift && step.key.chars().all(|c| c.is_ascii_digit()) {
            return Some(KeyFault::ShiftedSymbol);
        }
        if let Some((_, by)) = GRABBED
            .iter()
            .find(|(grabbed, _)| Accelerator::parse(grabbed).as_ref() == Some(step))
        {
            return Some(KeyFault::Grabbed(by));
        }
        if step.control && step.alt && step.is_function_key() {
            return Some(KeyFault::Grabbed("virtual terminal switching"));
        }
        if listed(FOCUS_KEYS, step) {
            return Some(KeyFault::FocusKey);
        }
        if composer && step.is_one_key() {
            return Some(KeyFault::OneKeyInComposer);
        }
    }
    if is_accelerator(id, binding) && listed(TEXT_KEYS, &binding.first) {
        return Some(KeyFault::TextKey);
    }
    None
}

fn needs_symbol_shift(binding: &Binding, layout: Layout) -> bool {
    binding
        .steps()
        .any(|step| level(&step.key, layout) == Some(Level::Shift))
}

fn unshifted(binding: &Binding, layout: Layout) -> bool {
    binding.steps().all(|step| {
        !step.shift && level(&step.key, layout).is_none_or(|level| level == Level::Base)
    })
}

/// The rules an action's keys must follow together.
fn action_faults(id: ActionId, keys: &[Binding]) -> Vec<Fault> {
    let mut all = keys.to_vec();
    all.extend(Keymap::list_keys(id));
    let mut faults = Vec::new();
    if all.iter().any(Binding::is_function_key) && all.iter().all(Binding::is_function_key) {
        faults.push(Fault::OnlyFunctionKeys);
    }
    let shifted: Vec<Layout> = Layout::ALL
        .into_iter()
        .filter(|layout| {
            all.iter().any(|key| needs_symbol_shift(key, *layout))
                && !all.iter().any(|key| unshifted(key, *layout))
        })
        .collect();
    if !shifted.is_empty() {
        faults.push(Fault::NoUnshifted(shifted));
    }
    faults
}

/// `US layout`, `US and Norwegian layouts`.
fn layouts_label(layouts: &[Layout]) -> String {
    let names: Vec<&str> = layouts.iter().map(|layout| layout.label()).collect();
    match names.as_slice() {
        [one] => format!("{one} layout"),
        _ => format!("{} layouts", names.join(" and ")),
    }
}

/// A key where it works: its action, and whether it is the message list's own.
struct Placed {
    action: ActionId,
    key: Binding,
    list_key: bool,
}

/// Everything wrong with `keymap`. A conflict is reported once, on the key that comes later in
/// the registry, and never on the message list's own keys.
pub fn audit(keymap: &Keymap) -> Vec<Problem> {
    let mut problems: Vec<Problem> = Vec::new();
    let mut report = |problem: Problem| {
        if !problems.contains(&problem) {
            problems.push(problem);
        }
    };
    for id in ActionId::ALL {
        let keys = keymap.keys(id);
        for key in &keys {
            if let Some(fault) = key_fault(id, key) {
                report(Problem {
                    action: id,
                    key: Some(key.clone()),
                    fault: Fault::Key(fault),
                });
            }
        }
        for fault in action_faults(id, &keys) {
            report(Problem {
                action: id,
                key: None,
                fault,
            });
        }
    }

    let mut global: HashMap<Accelerator, (ActionId, Binding)> = HashMap::new();
    for id in ActionId::ALL {
        for key in keymap.keys(id) {
            if !is_accelerator(id, &key) {
                continue;
            }
            match global.get(&key.first) {
                Some((other, other_key)) if *other != id => report(Problem {
                    action: id,
                    key: Some(key.clone()),
                    fault: Fault::Taken {
                        other: *other,
                        other_key: other_key.clone(),
                        list_key: false,
                    },
                }),
                Some(_) => {}
                None => {
                    global.insert(key.first.clone(), (id, key));
                }
            }
        }
    }

    for scope in Scope::SCOPED {
        let mut placed: Vec<Placed> = Vec::new();
        if scope == Scope::List {
            for id in ActionId::ALL {
                for key in Keymap::list_keys(id) {
                    placed.push(Placed {
                        action: id,
                        key,
                        list_key: true,
                    });
                }
            }
        }
        for (key, action) in keymap.scoped_in(scope) {
            placed.push(Placed {
                action,
                key,
                list_key: false,
            });
        }
        let mut seen: HashMap<Binding, usize> = HashMap::new();
        for (index, entry) in placed.iter().enumerate() {
            if let Some(&first) = seen.get(&entry.key) {
                let earlier = &placed[first];
                if earlier.action != entry.action {
                    report(Problem {
                        action: entry.action,
                        key: Some(entry.key.clone()),
                        fault: Fault::Taken {
                            other: earlier.action,
                            other_key: earlier.key.clone(),
                            list_key: earlier.list_key,
                        },
                    });
                }
                continue;
            }
            seen.insert(entry.key.clone(), index);
            if entry.list_key {
                continue;
            }
            if let Some((owner, owner_key)) = global.get(&entry.key.first) {
                let fault = if entry.key.is_sequence() {
                    Fault::Prefix {
                        other: *owner,
                        other_key: owner_key.clone(),
                    }
                } else {
                    Fault::Taken {
                        other: *owner,
                        other_key: owner_key.clone(),
                        list_key: false,
                    }
                };
                report(Problem {
                    action: entry.action,
                    key: Some(entry.key.clone()),
                    fault,
                });
            }
        }
        for sequence in placed.iter().filter(|entry| entry.key.is_sequence()) {
            for single in placed
                .iter()
                .filter(|entry| entry.key.single() == Some(&sequence.key.first))
            {
                let (problem, other) = if single.list_key || sequence.action > single.action {
                    (sequence, single)
                } else {
                    (single, sequence)
                };
                report(Problem {
                    action: problem.action,
                    key: Some(problem.key.clone()),
                    fault: Fault::Prefix {
                        other: other.action,
                        other_key: other.key.clone(),
                    },
                });
            }
        }
    }
    problems
}

/// A key as `keys.toml` writes it, and where.
struct WrittenKey {
    key: Binding,
    written: String,
    span: Range<usize>,
}

/// One line of `keys.toml`.
struct Line {
    action: ActionId,
    span: Range<usize>,
    keys: Vec<WrittenKey>,
}

const KEY_EXAMPLES: &str = "keys look like e, <Shift>i, <Control>r, F5 or g i";

impl Keymap {
    /// Reads `keys.toml`. Every mistake is listed, in the order of the file; a file with
    /// mistakes is not used at all.
    pub fn parse(text: &str) -> Result<Self, Vec<FileError>> {
        let root = parse_toml(text).map_err(|error| vec![error])?;
        let mut errors: Vec<FileError> = Vec::new();
        let mut lines: Vec<Line> = Vec::new();
        for (key, value) in entries(root.get_ref()) {
            let name = key.get_ref().as_ref();
            let Some(action) = ActionId::from_name(name) else {
                let hint = match ActionId::from_detailed_name(name) {
                    Some(id) => format!("; write it without its group, as {}", id.name()),
                    None => did_you_mean(name, ActionId::ALL.map(ActionId::name)),
                };
                errors.push(FileError::spanned(
                    text,
                    key.span(),
                    format!("unknown action {name}{hint}"),
                ));
                continue;
            };
            let Some(array) = value.get_ref().as_array() else {
                errors.push(FileError::spanned(
                    text,
                    value.span(),
                    format!(
                        "{name} takes a list of keys in brackets, like {name} = [\"<Control>k\"]"
                    ),
                ));
                continue;
            };
            let mut keys: Vec<WrittenKey> = Vec::new();
            for item in array.iter() {
                let span = item.span();
                let error = |message: String| FileError::spanned(text, span.clone(), message);
                let DeValue::String(written) = item.get_ref() else {
                    errors.push(error(format!(
                        "a key is a string in quotes, like \"e\" or \"<Control>e\", not {}",
                        article(item.get_ref().type_str())
                    )));
                    continue;
                };
                let binding = match Binding::parse(written) {
                    Ok(binding) => binding,
                    Err(BindingError::NotAKey) => {
                        errors.push(error(format!("“{written}” is not a key; {KEY_EXAMPLES}")));
                        continue;
                    }
                    Err(BindingError::TooManyKeys) => {
                        errors.push(error(format!(
                            "“{written}” has more than two keys; a sequence is two, like g i"
                        )));
                        continue;
                    }
                };
                if let Some(fault) = key_fault(action, &binding) {
                    errors.push(error(format!("“{written}” {}", fault.describe())));
                    continue;
                }
                if keys.iter().any(|other| other.key == binding) {
                    errors.push(error(format!("“{written}” is listed twice for {name}")));
                    continue;
                }
                keys.push(WrittenKey {
                    key: binding,
                    written: written.to_string(),
                    span,
                });
            }
            lines.push(Line {
                action,
                span: key.span(),
                keys,
            });
        }

        let keymap = Self::with_overrides(lines.iter().map(|line| {
            (
                line.action,
                line.keys.iter().map(|key| key.key.clone()).collect(),
            )
        }));
        for problem in audit(&keymap) {
            let error = locate(text, &lines, &problem);
            if !errors
                .iter()
                .any(|other| (other.line, other.column) == (error.line, error.column))
            {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(keymap)
        } else {
            errors.sort_by_key(|error| (error.line, error.column));
            Err(errors)
        }
    }
}

/// Where a problem of a parsed `keys.toml` is, and what to say about it. Conflicts are reported
/// on the key the file gives, the later one when the file gives both.
fn locate(text: &str, lines: &[Line], problem: &Problem) -> FileError {
    let line_of = |action: ActionId| lines.iter().find(|line| line.action == action);
    let written = |action: ActionId, key: &Binding| {
        line_of(action).and_then(|line| line.keys.iter().find(|written| written.key == *key))
    };
    let at = |span: &Range<usize>, message: String| FileError::spanned(text, span.clone(), message);
    let fallback = |message: String| FileError::at(text, 0, message);
    let action = problem.action;
    match (&problem.fault, &problem.key) {
        (Fault::Key(fault), Some(key)) => match written(action, key) {
            Some(mine) => at(
                &mine.span,
                format!("“{}” {}", mine.written, fault.describe()),
            ),
            None => fallback(problem.to_string()),
        },
        (
            Fault::Taken {
                other,
                other_key,
                list_key,
            },
            Some(key),
        ) => match (written(action, key), written(*other, other_key)) {
            (Some(mine), Some(theirs)) => {
                let (later, earlier, owner) = if mine.span.start > theirs.span.start {
                    (mine, theirs, *other)
                } else {
                    (theirs, mine, action)
                };
                let line = FileError::at(text, earlier.span.start, "").line;
                at(
                    &later.span,
                    format!(
                        "“{}” is also given to {} on line {line}",
                        later.written,
                        owner.name()
                    ),
                )
            }
            (Some(mine), None) if *list_key => at(
                &mine.span,
                format!(
                    "“{}” is the message list's own key for {}; pick another key",
                    mine.written,
                    other.title()
                ),
            ),
            (Some(mine), None) => at(&mine.span, taken(&mine.written, *other)),
            (None, Some(theirs)) => at(&theirs.span, taken(&theirs.written, action)),
            (None, None) => fallback(problem.to_string()),
        },
        (Fault::Prefix { other, other_key }, Some(key)) => {
            let (mine, owner, owner_key) = match written(action, key) {
                Some(mine) => (Some(mine), *other, other_key),
                None => (written(*other, other_key), action, key),
            };
            match mine {
                Some(mine) if mine.key.is_sequence() => at(
                    &mine.span,
                    format!(
                        "“{}” starts with {}, which is {}'s key ({}); a key can't also start a \
                         sequence",
                        mine.written,
                        mine.key.first.canonical(),
                        owner.title(),
                        owner.name()
                    ),
                ),
                Some(mine) => at(
                    &mine.span,
                    format!(
                        "“{}” starts {}'s sequence {} ({}); pick another key",
                        mine.written,
                        owner.title(),
                        owner_key.canonical(),
                        owner.name()
                    ),
                ),
                None => fallback(problem.to_string()),
            }
        }
        (Fault::OnlyFunctionKeys, _) => match line_of(action) {
            Some(line) => at(
                &line.span,
                format!(
                    "{} would only have F-keys, which many keyboards send as media keys unless \
                     Fn is held; add a key without one",
                    action.name()
                ),
            ),
            None => fallback(problem.to_string()),
        },
        (Fault::NoUnshifted(layouts), _) => match line_of(action) {
            Some(line) => at(
                &line.span,
                format!(
                    "every key of {} needs Shift for its symbol on the {}; add one that doesn't, \
                     such as a letter or a key with Ctrl",
                    action.name(),
                    layouts_label(layouts)
                ),
            ),
            None => fallback(problem.to_string()),
        },
        (_, None) => fallback(problem.to_string()),
    }
}

fn taken(written: &str, owner: ActionId) -> String {
    format!(
        "“{written}” is already {}'s key ({}); unbind it first with {} = []",
        owner.title(),
        owner.name(),
        owner.name()
    )
}

/// Where the keys of `id` work, for people: `list and reader`, `anywhere`.
fn places(id: ActionId, keymap: &Keymap) -> String {
    let scoped = match id.spec().key_scopes {
        [Scope::List, Scope::Reader] => "list and reader",
        [Scope::List] => "list",
        [Scope::Reader] => "reader",
        _ => "composer",
    };
    let mut groups: Vec<(String, &str)> = Vec::new();
    let mut add = |keys: Vec<Binding>, place: &'static str| {
        if !keys.is_empty() {
            let keys: Vec<String> = keys.iter().map(Binding::display).collect();
            groups.push((keys.join(", "), place));
        }
    };
    let (mut accels, scoped_keys): (Vec<Binding>, Vec<Binding>) = keymap
        .keys(id)
        .into_iter()
        .partition(|key| is_accelerator(id, key));
    accels.sort_by_key(Binding::is_function_key);
    add(scoped_keys, scoped);
    add(accels, "anywhere");
    add(Keymap::list_keys(id), "the list's own");
    match groups.as_slice() {
        [] => String::new(),
        [(_, place)] => (*place).to_owned(),
        _ => groups
            .iter()
            .map(|(keys, place)| format!("{keys}: {place}"))
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// The `keys.toml` that Customize Shortcuts creates: every action with its default keys,
/// commented out, grouped by category.
pub fn default_keys_file() -> String {
    let mut out = String::from(
        "# Sealwax keyboard shortcuts (keys.toml): your changes to Sealwax's default keys.\n\
         #\n\
         # Uncomment a line and change it; Sealwax applies the file when you save it. A line\n\
         # names an action and lists its keys, which replace the default ones; [] leaves the\n\
         # action without a key. A key is a GTK accelerator, or two keys pressed one after the\n\
         # other, separated by a space:\n\
         #\n\
         #   archive = [\"e\", \"<Control>e\"]\n\
         #   go-to-inbox = [\"g i\"]\n\
         #   quit = []\n\
         #\n\
         # Modifiers: <Control>, <Shift>, <Alt>. Keys: a-z, 0-9, F1-F35, Return, Escape,\n\
         # Delete, space, Up, Page_Down, ..., and symbols by name: numbersign (#), slash (/),\n\
         # exclam (!), question (?), comma (,).\n\
         #\n\
         # Where keys work:\n\
         # - Keys of the message commands work while the message list or the reader has the\n\
         #   keyboard focus, after the focused widget; only these may be single letters.\n\
         # - The other commands' keys with Ctrl or Alt, and their F-keys, work anywhere in the\n\
         #   window, before the focused widget, so they can't be a text field's own keys\n\
         #   (Ctrl+Z, Ctrl+C, ...); their other keys work in the message list and the reader.\n\
         # - The composer's keys need Ctrl or Alt: it keeps its letters.\n\
         # A key belongs to one action where it works; to give it to another, first give the\n\
         # action that has it an empty list. No key may use Super or need AltGr on the US or the\n\
         # Norwegian layout, an action on an F-key needs a key without one (many keyboards send\n\
         # media keys unless Fn is held), and an action on a shifted symbol such as ? needs a\n\
         # key without Shift.\n\
         # The message list's own keys (Up, Down) can't be changed.\n",
    );
    for category in Category::ALL {
        out.push_str(&format!("\n# {}\n", category.label()));
        for id in ActionId::ALL
            .into_iter()
            .filter(|id| id.category() == category)
        {
            let keys: Vec<String> = id
                .spec()
                .keys
                .iter()
                .map(|key| format!("\"{key}\""))
                .collect();
            let setting = format!("{} = [{}]", id.name(), keys.join(", "));
            let list: Vec<String> = Keymap::list_keys(id).iter().map(Binding::display).collect();
            let note = if list.is_empty() {
                id.title().to_owned()
            } else {
                format!("{}; the list's own {} stays", id.title(), list.join(", "))
            };
            out.push_str(&format!("# {setting:<48} # {note}\n"));
        }
    }
    out
}

/// Where the generated keymap table starts and ends in the docs; the table between them is
/// [`markdown_table`]'s, which a test keeps in step with the registry.
pub const TABLE_START: &str = "<!-- keymap table: generated from the registry -->";
pub const TABLE_END: &str = "<!-- end of the keymap table -->";

/// The default keymap as a Markdown table, in registry order.
pub fn markdown_table() -> String {
    let keymap = Keymap::default();
    let mut out = String::from(
        "| Category | Command | Keys | Keys work | `keys.toml` name |\n| --- | --- | --- | --- | --- |\n",
    );
    for id in ActionId::ALL {
        let keys = keymap.display(id);
        let keys = if keys.is_empty() {
            "—".to_owned()
        } else {
            keys.replace('|', "\\|")
        };
        out.push_str(&format!(
            "| {} | {} | {keys} | {} | `{}` |\n",
            id.category().label(),
            id.title(),
            places(id, &keymap),
            id.name()
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn binding(text: &str) -> Binding {
        Binding::parse(text).unwrap_or_else(|_| panic!("{text}"))
    }

    fn parse(text: &str) -> Result<Keymap, Vec<String>> {
        Keymap::parse(text).map_err(|errors| errors.iter().map(ToString::to_string).collect())
    }

    fn keys(keymap: &Keymap, id: ActionId) -> Vec<String> {
        keymap.keys(id).iter().map(Binding::canonical).collect()
    }

    #[test]
    fn the_default_keymap_passes_the_audit() {
        let problems = audit(&Keymap::default());
        assert!(
            problems.is_empty(),
            "{}",
            problems
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n")
        );
    }

    #[test]
    fn every_registry_key_parses_and_is_written_canonically() {
        for id in ActionId::ALL {
            for key in id.spec().keys.iter().chain(id.spec().list_keys) {
                let parsed = Binding::parse(key).unwrap_or_else(|_| panic!("{id:?}: {key}"));
                assert_eq!(parsed.canonical(), *key, "{id:?}");
            }
        }
    }

    #[test]
    fn nothing_uses_super() {
        for id in ActionId::ALL {
            for key in Keymap::default().keys(id) {
                assert!(key.steps().all(|step| !step.super_key), "{id:?}: {key}");
            }
        }
        let keymap = Keymap::with_overrides([(ActionId::Archive, vec![binding("<Super>e")])]);
        assert!(
            audit(&keymap)
                .iter()
                .any(|problem| problem.fault == Fault::Key(KeyFault::Super))
        );
    }

    #[test]
    fn the_layout_tables_match_xkeyboard_config() {
        // The Norwegian layout needs AltGr for these.
        for name in [
            "at",
            "sterling",
            "dollar",
            "braceleft",
            "bracketleft",
            "bracketright",
            "braceright",
        ] {
            assert_eq!(level(name, Layout::Norwegian), Some(Level::AltGr), "{name}");
        }
        // `\` and `|` are unshifted on no(basic): `\` left of Backspace, `|` left of 1.
        assert_eq!(level("backslash", Layout::Norwegian), Some(Level::Base));
        assert_eq!(level("bar", Layout::Norwegian), Some(Level::Base));
        // `#`, `/`, `!` and `?`: Shift+3, Shift+7, Shift+1 and Shift++ on Norwegian.
        for name in ["numbersign", "slash", "exclam", "question"] {
            assert_eq!(level(name, Layout::Norwegian), Some(Level::Shift), "{name}");
        }
        assert_eq!(level("slash", Layout::Us), Some(Level::Base));
        assert_eq!(level("plus", Layout::Norwegian), Some(Level::Base));
        assert_eq!(level("e", Layout::Us), None);
        let names: HashSet<_> = LAYOUT_KEYS.iter().map(|key| key.name).collect();
        assert_eq!(names.len(), LAYOUT_KEYS.len());
    }

    #[test]
    fn no_default_key_needs_altgr() {
        for id in ActionId::ALL {
            for key in Keymap::default().shown(id) {
                for step in key.steps() {
                    for layout in Layout::ALL {
                        assert!(
                            !matches!(
                                level(&step.key, layout),
                                Some(Level::AltGr | Level::Missing)
                            ),
                            "{id:?}: {key} on {layout:?}"
                        );
                    }
                }
            }
        }
        for written in [
            "at",
            "<Control>bracketleft",
            "dollar",
            "braceright",
            "sterling",
        ] {
            let keymap = Keymap::with_overrides([(ActionId::Archive, vec![binding(written)])]);
            assert!(
                audit(&keymap).iter().any(|problem| matches!(
                    problem.fault,
                    Fault::Key(KeyFault::AltGr(_) | KeyFault::Missing(_))
                )),
                "{written}"
            );
        }
    }

    #[test]
    fn shifted_symbols_have_the_unshifted_alternatives_design_lists() {
        let has = |id: ActionId, key: &str| Keymap::default().shown(id).contains(&binding(key));
        assert!(has(ActionId::Delete, "numbersign") && has(ActionId::Delete, "Delete"));
        assert!(has(ActionId::Search, "slash") && has(ActionId::Search, "<Control>f"));
        assert!(has(ActionId::Junk, "exclam") && has(ActionId::Junk, "<Control>j"));
        assert!(has(ActionId::KeyboardShortcuts, "question"));
        assert!(has(ActionId::KeyboardShortcuts, "<Control>question"));
        // Ctrl+? needs Shift too, on both layouts; `h` is the unshifted one.
        assert!(has(ActionId::KeyboardShortcuts, "h"));
        for id in ActionId::ALL {
            for layout in Layout::ALL {
                let shown = Keymap::default().shown(id);
                if shown.iter().any(|key| needs_symbol_shift(key, layout)) {
                    assert!(
                        shown.iter().any(|key| unshifted(key, layout)),
                        "{id:?} on {layout:?}"
                    );
                }
            }
        }
        let keymap =
            Keymap::with_overrides([(ActionId::KeyboardShortcuts, vec![binding("question")])]);
        assert_eq!(
            audit(&keymap),
            [Problem {
                action: ActionId::KeyboardShortcuts,
                key: None,
                fault: Fault::NoUnshifted(vec![Layout::Us, Layout::Norwegian]),
            }]
        );
        let keymap = Keymap::with_overrides([(ActionId::Search, vec![binding("slash")])]);
        assert_eq!(
            audit(&keymap)[0].fault,
            Fault::NoUnshifted(vec![Layout::Norwegian])
        );
    }

    #[test]
    fn every_function_key_action_has_another_key() {
        for id in ActionId::ALL {
            let shown = Keymap::default().shown(id);
            if shown.iter().any(Binding::is_function_key) {
                assert!(!shown.iter().all(Binding::is_function_key), "{id:?}");
                assert!(
                    !shown[0].is_function_key(),
                    "{id:?}: the first key shown is an F-key"
                );
                assert!(
                    !Keymap::default().accels(id)[0].starts_with('F'),
                    "{id:?}: menus show the first accelerator"
                );
            }
        }
        let keymap = Keymap::with_overrides([(ActionId::Refresh, vec![binding("F5")])]);
        assert_eq!(
            audit(&keymap),
            [Problem {
                action: ActionId::Refresh,
                key: None,
                fault: Fault::OnlyFunctionKeys,
            }]
        );
        let keymap = Keymap::with_overrides([(
            ActionId::Refresh,
            vec![binding("F5"), binding("<Control>F5")],
        )]);
        assert_eq!(audit(&keymap).len(), 1);
    }

    #[test]
    fn one_key_shortcuts_are_scoped_to_the_list_and_reader() {
        let keymap = Keymap::default();
        for id in ActionId::ALL {
            for key in keymap.keys(id) {
                if key.steps().any(Accelerator::is_one_key) {
                    assert!(!is_accelerator(id, &key), "{id:?}: {key} would be global");
                    assert!(
                        id.spec()
                            .key_scopes
                            .iter()
                            .all(|scope| scope.allows_one_key()),
                        "{id:?}: {key} would work outside the list and the reader"
                    );
                }
            }
            for accel in keymap.accels(id) {
                let accel = Accelerator::parse(&accel).unwrap();
                assert!(!accel.is_one_key(), "{id:?}");
                assert!(id.scope().is_global(), "{id:?}");
            }
        }
        let keymap = Keymap::with_overrides([(ActionId::Send, vec![binding("s")])]);
        assert_eq!(
            audit(&keymap)[0].fault,
            Fault::Key(KeyFault::OneKeyInComposer)
        );
        // A window command's single letters work only in the list and the reader.
        assert_eq!(Keymap::default().accels(ActionId::Compose), ["<Control>n"]);
        assert_eq!(Keymap::default().scoped(ActionId::Compose), [binding("c")]);
        // Undo's Ctrl+Z is scoped: text fields keep theirs.
        assert!(Keymap::default().accels(ActionId::Undo).is_empty());
    }

    #[test]
    fn accelerators_leave_text_fields_their_keys() {
        for id in ActionId::ALL {
            for accel in Keymap::default().accels(id) {
                let accel = Accelerator::parse(&accel).unwrap();
                assert!(!listed(TEXT_KEYS, &accel), "{id:?}: {}", accel.canonical());
            }
        }
        let keymap = Keymap::with_overrides([(ActionId::Compose, vec![binding("<Control>z")])]);
        assert_eq!(audit(&keymap)[0].fault, Fault::Key(KeyFault::TextKey));
        // The same key is fine as a scoped key.
        let keymap = Keymap::with_overrides([
            (ActionId::Undo, vec![binding("z")]),
            (ActionId::Archive, vec![binding("<Control>z")]),
        ]);
        assert!(audit(&keymap).is_empty(), "{:?}", audit(&keymap));
    }

    #[test]
    fn keys_avoid_known_grabs_and_focus_keys() {
        for (written, fault) in [
            (
                "<Control>space",
                KeyFault::Grabbed("fcitx5's input method switch"),
            ),
            (
                "<Control><Shift>u",
                KeyFault::Grabbed("fcitx5's Unicode entry"),
            ),
            (
                "<Control><Alt>p",
                KeyFault::Grabbed("fcitx5's embedded preedit switch"),
            ),
            (
                "<Control><Alt>F2",
                KeyFault::Grabbed("virtual terminal switching"),
            ),
            ("Print", KeyFault::Grabbed("Omarchy's screenshot")),
            ("Tab", KeyFault::FocusKey),
            ("F10", KeyFault::FocusKey),
            ("<Shift>1", KeyFault::ShiftedSymbol),
            ("<Shift>slash", KeyFault::ShiftedSymbol),
        ] {
            assert_eq!(
                key_fault(ActionId::Refresh, &binding(written)),
                Some(fault),
                "{written}"
            );
        }
        assert_eq!(key_fault(ActionId::Refresh, &binding("<Shift>F5")), None);
        assert_eq!(key_fault(ActionId::Archive, &binding("<Shift>a")), None);
    }

    #[test]
    fn no_key_conflicts_where_it_works() {
        let keymap = Keymap::default();
        let mut global = HashSet::new();
        for id in ActionId::ALL {
            for accel in keymap.accels(id) {
                assert!(global.insert(accel.clone()), "{accel} twice");
            }
        }
        for scope in Scope::SCOPED {
            let mut seen = HashSet::new();
            for (key, id) in keymap.scoped_in(scope) {
                assert!(seen.insert(key.clone()), "{key} twice in {scope:?}");
                assert!(!global.contains(&key.first.canonical()), "{id:?}: {key}");
            }
        }
        let conflict =
            |overrides: Vec<(ActionId, Vec<Binding>)>| audit(&Keymap::with_overrides(overrides));
        assert_eq!(
            conflict(vec![(ActionId::Star, vec![binding("e")])]),
            [Problem {
                action: ActionId::Star,
                key: Some(binding("e")),
                fault: Fault::Taken {
                    other: ActionId::Archive,
                    other_key: binding("e"),
                    list_key: false,
                },
            }]
        );
        // A scoped key never sees a key an accelerator takes first.
        assert_eq!(
            conflict(vec![(ActionId::Archive, vec![binding("<Control>n")])])[0].fault,
            Fault::Taken {
                other: ActionId::Compose,
                other_key: binding("<Control>n"),
                list_key: false,
            }
        );
        // The list's own keys can't be given away.
        assert_eq!(
            conflict(vec![(ActionId::Archive, vec![binding("Down")])])[0].fault,
            Fault::Taken {
                other: ActionId::NextMessage,
                other_key: binding("Down"),
                list_key: true,
            }
        );
        // The list and the reader are separate places.
        assert!(conflict(vec![(ActionId::Select, vec![binding("u")])]).is_empty());
        assert!(!conflict(vec![(ActionId::Archive, vec![binding("u")])]).is_empty());
        assert!(conflict(vec![(ActionId::Send, vec![binding("<Control>r")])]).is_empty());
    }

    #[test]
    fn the_go_to_sequences_resolve() {
        let keymap = Keymap::default();
        for scope in [Scope::List, Scope::Reader] {
            let scoped = keymap.scoped_in(scope);
            let resolve = |second: &str| {
                scoped
                    .iter()
                    .filter(|(key, _)| key.canonical() == format!("g {second}"))
                    .map(|(_, id)| *id)
                    .collect::<Vec<_>>()
            };
            assert_eq!(resolve("i"), [ActionId::GoToInbox]);
            assert_eq!(resolve("s"), [ActionId::GoToStarred]);
            assert_eq!(resolve("t"), [ActionId::GoToSent]);
            assert_eq!(resolve("d"), [ActionId::GoToDrafts]);
            assert_eq!(resolve("a"), [ActionId::GoToAllInboxes]);
            assert_eq!(resolve("1"), [ActionId::GoToAccount1]);
            assert_eq!(resolve("9"), [ActionId::GoToAccount9]);
            assert!(
                scoped
                    .iter()
                    .all(|(key, _)| key.single().is_none_or(|key| key.key != "g"))
            );
        }
        assert_eq!(binding("g i").display(), "g then i");
        assert_eq!(binding("g i").label(), "g+i");
        let problems = audit(&Keymap::with_overrides([(
            ActionId::Star,
            vec![binding("g")],
        )]));
        assert_eq!(problems.len(), 14, "{problems:?}");
        assert!(
            problems
                .iter()
                .all(|problem| matches!(problem.fault, Fault::Prefix { .. }))
        );
        assert_eq!(Binding::parse("g i x"), Err(BindingError::TooManyKeys));
    }

    #[test]
    fn accelerators_parse_compare_and_display() {
        let canonical = |key: &str| Accelerator::parse(key).unwrap().canonical();
        assert_eq!(canonical("<Ctrl>D"), "<Control>d");
        assert_eq!(canonical("<Shift><Primary>x"), "<Control><Shift>x");
        assert_eq!(canonical("F3"), "F3");
        for bad in [
            "",
            "<Control>",
            "<Ctrl",
            "<Hyper>a",
            "<Control>ab",
            "<Control>F0",
            "ø",
        ] {
            assert_eq!(Accelerator::parse(bad), None, "{bad}");
        }
        let display = |key: &str| binding(key).display();
        assert_eq!(display("<Control><Shift>r"), "Ctrl+Shift+R");
        assert_eq!(display("<Shift>i"), "Shift+I");
        assert_eq!(display("e"), "e");
        assert_eq!(display("numbersign"), "#");
        assert_eq!(display("<Control>question"), "Ctrl+?");
        assert_eq!(display("<Control>comma"), "Ctrl+,");
        assert_eq!(display("Return"), "Enter");
        assert_eq!(display("<Control>Return"), "Ctrl+Enter");
        assert_eq!(display("space"), "Space");
        assert_eq!(display("Escape"), "Escape");
        assert_eq!(display("<Control>KP_Add"), "Ctrl+Num +");
        assert_eq!(Keymap::default().display(ActionId::Reply), "r, Ctrl+R");
        assert_eq!(Keymap::default().display(ActionId::NextMessage), "j, Down");
        assert_eq!(
            Keymap::default().display(ActionId::CommandPalette),
            "Ctrl+Shift+P, F1"
        );
        assert_eq!(
            Keymap::default().label(ActionId::Delete),
            "numbersign Delete"
        );
        assert_eq!(Keymap::default().label(ActionId::GoToInbox), "g+i");
        assert_eq!(
            Keymap::default().accels(ActionId::Refresh),
            ["<Control><Alt>r", "F5"]
        );
    }

    #[test]
    fn the_defaults_are_the_registry() {
        let keymap = parse("").unwrap();
        assert_eq!(keymap, Keymap::default());
        assert_eq!(keys(&keymap, ActionId::Archive), ["e", "y"]);
        assert_eq!(keymap.accels(ActionId::Search), ["<Control>f"]);
        assert_eq!(keymap.scoped(ActionId::Search), [binding("slash")]);
        assert!(keymap.accels(ActionId::Reply).is_empty());
        assert_eq!(keymap.changed_count(), 0);
    }

    #[test]
    fn overrides_replace_and_unbind() {
        let keymap = parse(
            "archive = [\"<Ctrl>E\", \"y\"]\nquit = []\ncompose = [\"c\"]\n\
             go-to-inbox = [\"g  I\"]\n",
        )
        .unwrap();
        assert_eq!(keys(&keymap, ActionId::Archive), ["<Control>e", "y"]);
        assert!(keymap.keys(ActionId::Quit).is_empty());
        assert!(keymap.accels(ActionId::Quit).is_empty());
        assert!(keymap.accels(ActionId::Compose).is_empty());
        assert_eq!(keys(&keymap, ActionId::GoToInbox), ["g i"]);
        assert!(keymap.is_changed(ActionId::Quit));
        assert!(!keymap.is_changed(ActionId::Star));
        assert_eq!(keymap.changed_count(), 4);
        // The list's own keys stay.
        let keymap = parse("next-message = [\"w\"]\n").unwrap();
        assert_eq!(keymap.display(ActionId::NextMessage), "w, Down");
    }

    #[test]
    fn a_freed_key_can_be_reused() {
        let keymap = parse("archive = [\"y\"]\nstar = [\"e\", \"s\"]\n").unwrap();
        assert_eq!(keys(&keymap, ActionId::Star), ["e", "s"]);
        let keymap = parse("compose = [\"c\"]\narchive = [\"e\", \"<Control>n\"]\n").unwrap();
        assert_eq!(keys(&keymap, ActionId::Archive), ["e", "<Control>n"]);
    }

    #[test]
    fn conflicts_are_reported_where_they_are() {
        assert_eq!(
            parse("star = [\"e\"]\n").unwrap_err(),
            [
                "line 1, column 9: “e” is already Archive's key (archive); unbind it first with archive = []"
            ]
        );
        // Archive comes before Star in the registry; the file's line is still the one named.
        assert_eq!(
            parse("archive = [\"s\"]\n").unwrap_err(),
            [
                "line 1, column 12: “s” is already Star or Unstar's key (star); unbind it first with star = []"
            ]
        );
        assert_eq!(
            parse("star = [\"w\"]\narchive = [\"w\"]\n").unwrap_err(),
            ["line 2, column 12: “w” is also given to star on line 1"]
        );
        assert_eq!(
            parse("refresh = [\"<Control>n\", \"F5\"]\n").unwrap_err(),
            [
                "line 1, column 12: “<Control>n” is already Compose's key (compose); unbind it \
                 first with compose = []"
            ]
        );
        assert_eq!(
            parse("archive = [\"Up\"]\n").unwrap_err(),
            [
                "line 1, column 12: “Up” is the message list's own key for Previous Message; pick another key"
            ]
        );
        assert_eq!(
            parse("star = [\"g\"]\n").unwrap_err(),
            [
                "line 1, column 9: “g” starts Go to Inbox's sequence g i (go-to-inbox); pick another key"
            ]
        );
        assert_eq!(
            parse("go-to-sent = [\"e t\"]\n").unwrap_err(),
            [
                "line 1, column 15: “e t” starts with e, which is Archive's key (archive); a key \
                 can't also start a sequence"
            ]
        );
    }

    #[test]
    fn mistakes_are_reported_with_positions() {
        assert_eq!(
            parse("archve = []\n").unwrap_err(),
            ["line 1, column 1: unknown action archve; did you mean archive?"]
        );
        assert_eq!(
            parse("\"win.archive\" = []\n").unwrap_err(),
            [
                "line 1, column 1: unknown action win.archive; write it without its group, as archive"
            ]
        );
        assert_eq!(
            parse("archive = \"e\"\n").unwrap_err(),
            [
                "line 1, column 11: archive takes a list of keys in brackets, like archive = [\"<Control>k\"]"
            ]
        );
        assert_eq!(
            parse("archive = [\"e\", 3]\n").unwrap_err(),
            [
                "line 1, column 17: a key is a string in quotes, like \"e\" or \"<Control>e\", not an integer"
            ]
        );
        assert_eq!(
            parse("archive = [\"<Ctrl>ee\"]\n").unwrap_err(),
            [
                "line 1, column 12: “<Ctrl>ee” is not a key; keys look like e, <Shift>i, <Control>r, F5 or g i"
            ]
        );
        assert_eq!(
            parse("go-to-inbox = [\"g i i\"]\n").unwrap_err(),
            ["line 1, column 16: “g i i” has more than two keys; a sequence is two, like g i"]
        );
        assert_eq!(
            parse("archive = [\"e\", \"E\"]\n").unwrap_err(),
            ["line 1, column 17: “E” is listed twice for archive"]
        );
        assert_eq!(
            parse("archive = [\"at\"]\n").unwrap_err(),
            [
                "line 1, column 12: “at” needs AltGr on the Norwegian layout; pick a key both the \
                 US and the Norwegian layout type without it"
            ]
        );
        assert_eq!(
            parse("archive = [\"oslash\"]\n").unwrap_err(),
            ["line 1, column 12: “oslash” is not on the US layout"]
        );
        assert_eq!(
            parse("archive = [\"<Super>e\"]\n").unwrap_err(),
            ["line 1, column 12: “<Super>e” uses Super, which belongs to Hyprland"]
        );
        assert_eq!(
            parse("send = [\"s\"]\n").unwrap_err(),
            [
                "line 1, column 9: “s” has no Ctrl or Alt; the composer keeps its letters, so its \
                 keys need Ctrl or Alt (F-keys are the exception)"
            ]
        );
        assert_eq!(
            parse("compose = [\"<Control>c\"]\n").unwrap_err(),
            [
                "line 1, column 12: “<Control>c” is a text field's or the composer's own key, and \
                 a key with Ctrl of a command that works anywhere would take it from them"
            ]
        );
        assert_eq!(
            parse("refresh = [\"F5\"]\n").unwrap_err(),
            [
                "line 1, column 1: refresh would only have F-keys, which many keyboards send as \
                 media keys unless Fn is held; add a key without one"
            ]
        );
        assert_eq!(
            parse("shortcuts = [\"question\", \"<Control>question\"]\n").unwrap_err(),
            [
                "line 1, column 1: every key of shortcuts needs Shift for its symbol on the US and \
                 Norwegian layouts; add one that doesn't, such as a letter or a key with Ctrl"
            ]
        );
        let syntax = parse("archive = [\n").unwrap_err();
        assert!(syntax[0].starts_with("line 1,") || syntax[0].starts_with("line 2,"));
    }

    #[test]
    fn errors_come_in_file_order() {
        let errors = parse("zzz = []\narchive = \"x\"\nyyy = []\n").unwrap_err();
        assert_eq!(errors.len(), 3);
        assert!(errors[0].starts_with("line 1,"));
        assert!(errors[1].starts_with("line 2,"));
        assert!(errors[2].starts_with("line 3,"));
    }

    #[test]
    fn the_default_file_changes_nothing_and_every_line_is_valid() {
        let file = default_keys_file();
        assert_eq!(parse(&file), Ok(Keymap::default()));
        let uncommented: String = file
            .lines()
            .filter_map(|line| line.strip_prefix("# "))
            .filter(|line| !line.starts_with(' ') && line.contains(" = ["))
            .map(|line| format!("{line}\n"))
            .collect();
        let keymap = parse(&uncommented).unwrap();
        for id in ActionId::ALL {
            assert_eq!(keymap.keys(id), Keymap::default().keys(id), "{id:?}");
            assert!(
                keymap.is_changed(id),
                "{id:?} is missing from the default file"
            );
        }
        assert!(file.contains("\n# Navigation\n"));
        assert!(file.contains("the list's own Down stays"));
    }

    #[test]
    fn the_report_lists_the_registry_keymap() {
        // The milestone reports are working docs kept out of the repository (2026-10-09): the
        // table is checked where the report is; USER_GUIDE.md's is checked everywhere.
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../docs/m1/actions.md");
        let Ok(doc) = std::fs::read_to_string(path) else {
            return;
        };
        let doc = doc.as_str();
        let start = doc.find(TABLE_START).expect("the table's start marker") + TABLE_START.len();
        let end = doc[start..]
            .find(TABLE_END)
            .expect("the table's end marker")
            + start;
        let table = markdown_table();
        assert_eq!(
            doc[start..end].trim(),
            table.trim(),
            "docs/m1/actions.md's keymap table differs from the registry; replace it with \
             markdown_table()"
        );
        assert!(table.contains(
            "| Compose | Compose | c, Ctrl+N | c: list and reader; Ctrl+N: anywhere | `compose` |"
        ));
        assert!(table.contains("| Messages | Archive | e, y | list and reader | `archive` |"));
    }
}
