//! The registry's actions in GTK (ADR-017): GActions on the application and the window,
//! application accelerators, and scoped shortcuts on the list, reader and composer widgets.
//! Nothing here decides what exists; it only wires up `sealwax_domain::actions`.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, gio, glib};
use sealwax_domain::actions::{ActionId, ActionKind, Scope};
use sealwax_domain::keymap::Keymap;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The registry's actions on one action map: the application's (`app.`) or a window's
/// (`win.`). Every action starts disabled; it becomes available when the app connects a
/// handler, and an action that needs a selection is enabled only while there is one.
pub struct ActionSet {
    actions: Vec<(ActionId, gio::SimpleAction)>,
    available: RefCell<HashSet<ActionId>>,
    has_selection: Cell<bool>,
    /// Commands that don't apply to what is selected, such as Archive in the Archive folder.
    blocked: RefCell<HashSet<ActionId>>,
}

impl ActionSet {
    fn install(map: &impl IsA<gio::ActionMap>, ids: impl Iterator<Item = ActionId>) -> Self {
        let actions = ids
            .map(|id| {
                let action = match id.kind() {
                    ActionKind::Command => gio::SimpleAction::new(id.name(), None),
                    ActionKind::Toggle => {
                        gio::SimpleAction::new_stateful(id.name(), None, &false.to_variant())
                    }
                };
                action.set_enabled(false);
                map.add_action(&action);
                (id, action)
            })
            .collect();
        Self {
            actions,
            available: RefCell::new(HashSet::new()),
            has_selection: Cell::new(false),
            blocked: RefCell::new(HashSet::new()),
        }
    }

    pub fn action(&self, id: ActionId) -> Option<&gio::SimpleAction> {
        self.actions
            .iter()
            .find(|(action_id, _)| *action_id == id)
            .map(|(_, action)| action)
    }

    pub fn ids(&self) -> impl Iterator<Item = ActionId> + '_ {
        self.actions.iter().map(|(id, _)| *id)
    }

    /// Runs `handler` when the command is activated, and makes it available.
    pub fn connect(&self, id: ActionId, handler: impl Fn() + 'static) {
        let Some(action) = self.action(id) else {
            tracing::warn!(action = id.name(), "not in this action set");
            return;
        };
        action.connect_activate(move |_, _| handler());
        self.set_available(id, true);
    }

    /// Makes the toggle `id` available with `state`; `handler` gets every new state.
    pub fn connect_toggle(&self, id: ActionId, state: bool, handler: impl Fn(bool) + 'static) {
        let Some(action) = self.action(id) else {
            tracing::warn!(action = id.name(), "not in this action set");
            return;
        };
        action.set_state(&state.to_variant());
        action.connect_change_state(move |action, value| {
            if let Some(on) = value.and_then(glib::Variant::get::<bool>) {
                action.set_state(&on.to_variant());
                handler(on);
            }
        });
        self.set_available(id, true);
    }

    /// Shows a toggle's state without running its handler, such as a sidebar a breakpoint hid.
    pub fn set_toggle_state(&self, id: ActionId, on: bool) {
        if let Some(action) = self.action(id) {
            action.set_state(&on.to_variant());
        }
    }

    /// Whether the app has a handler for `id` and wants it shown: an action can be taken away
    /// for a while, such as Refresh while offline.
    pub fn set_available(&self, id: ActionId, available: bool) {
        if available {
            self.available.borrow_mut().insert(id);
        } else {
            self.available.borrow_mut().remove(&id);
        }
        self.refresh(id);
    }

    pub fn is_available(&self, id: ActionId) -> bool {
        self.available.borrow().contains(&id)
    }

    /// Whether messages are selected; the actions that need a selection follow it.
    pub fn set_has_selection(&self, has_selection: bool) {
        self.has_selection.set(has_selection);
        for id in self.ids().filter(|id| id.spec().needs_selection) {
            self.refresh(id);
        }
    }

    pub fn is_enabled(&self, id: ActionId) -> bool {
        self.action(id).is_some_and(|action| action.is_enabled())
    }

    /// Whether `id` applies to what is selected now; an available command that doesn't is
    /// disabled until it does, and the keyboard shortcuts dialog still lists it as built.
    pub fn set_applies(&self, id: ActionId, applies: bool) {
        let changed = if applies {
            self.blocked.borrow_mut().remove(&id)
        } else {
            self.blocked.borrow_mut().insert(id)
        };
        if changed {
            self.refresh(id);
        }
    }

    fn refresh(&self, id: ActionId) {
        let enabled = self.is_available(id)
            && (!id.spec().needs_selection || self.has_selection.get())
            && !self.blocked.borrow().contains(&id);
        if let Some(action) = self.action(id) {
            action.set_enabled(enabled);
        }
    }
}

/// Adds the application's actions (`app.quit`, `app.preferences`, ...) to `app`.
pub fn install_app_actions(app: &impl IsA<gio::ActionMap>) -> ActionSet {
    ActionSet::install(
        app,
        ActionId::ALL
            .into_iter()
            .filter(|id| id.scope() == Scope::App),
    )
}

/// Adds every other action (`win.archive`, ...) to `window`.
pub fn install_window_actions(window: &impl IsA<gio::ActionMap>) -> ActionSet {
    ActionSet::install(
        window,
        ActionId::ALL
            .into_iter()
            .filter(|id| id.scope() != Scope::App),
    )
}

/// Adds the composer's actions (`win.send`, `win.bold`, ...) to `group`, which a composer card
/// inserts as its own `win` group: inside the card they hide the window's of the same names.
pub fn install_composer_actions(group: &impl IsA<gio::ActionMap>) -> ActionSet {
    ActionSet::install(
        group,
        ActionId::ALL
            .into_iter()
            .filter(|id| id.scope() == Scope::Composer),
    )
}

/// Installs the keymap's application accelerators; actions without any get none.
pub fn apply_accels(app: &impl IsA<gtk::Application>, keymap: &Keymap) {
    for id in ActionId::ALL {
        let accels = keymap.accels(id);
        let accels: Vec<&str> = accels.iter().map(String::as_str).collect();
        app.set_accels_for_action(&id.detailed_name(), &accels);
    }
}

/// How long the second key of a sequence (`g` then `i`) is waited for.
pub const SEQUENCE_TIMEOUT: Duration = Duration::from_millis(1500);

/// What a key press does in a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// The first key of a sequence: wait for the second.
    Arm,
    Run {
        action: ActionId,
        sequence: bool,
    },
    /// A key after a sequence's first key that ends no sequence: nothing runs, so a mistyped
    /// sequence never archives or deletes.
    Swallow,
    /// Not a key of the scope.
    Pass,
}

/// A scope's keys by their canonical spelling, and the pending first key of a sequence.
#[derive(Debug, Default)]
struct Dispatch {
    singles: HashMap<String, ActionId>,
    sequences: HashMap<String, HashMap<String, ActionId>>,
    pending: Option<(String, Instant)>,
}

impl Dispatch {
    fn new(scope: Scope, keymap: &Keymap) -> Self {
        let mut dispatch = Self::default();
        for (binding, action) in keymap.scoped_in(scope) {
            let first = binding.first.canonical();
            match &binding.then {
                None => {
                    dispatch.singles.insert(first, action);
                }
                Some(second) => {
                    dispatch
                        .sequences
                        .entry(first)
                        .or_default()
                        .insert(second.canonical(), action);
                }
            }
        }
        dispatch
    }

    /// Every key that needs a shortcut: single keys, and both keys of every sequence.
    fn triggers(&self) -> Vec<String> {
        let mut keys: Vec<String> = self
            .singles
            .keys()
            .chain(self.sequences.keys())
            .chain(self.sequences.values().flat_map(HashMap::keys))
            .cloned()
            .collect();
        keys.sort();
        keys.dedup();
        keys
    }

    fn press(&mut self, key: &str, now: Instant) -> Outcome {
        if let Some((first, at)) = self.pending.take()
            && now.duration_since(at) <= SEQUENCE_TIMEOUT
        {
            if let Some(action) = self
                .sequences
                .get(&first)
                .and_then(|seconds| seconds.get(key))
            {
                return Outcome::Run {
                    action: *action,
                    sequence: true,
                };
            }
            if !self.sequences.contains_key(key) {
                return Outcome::Swallow;
            }
        }
        if self.sequences.contains_key(key) {
            self.pending = Some((key.to_owned(), now));
            return Outcome::Arm;
        }
        match self.singles.get(key) {
            Some(action) => Outcome::Run {
                action: *action,
                sequence: false,
            },
            None => Outcome::Pass,
        }
    }

    fn pending(&self, now: Instant) -> Option<&str> {
        self.pending
            .as_ref()
            .filter(|(_, at)| now.duration_since(*at) <= SEQUENCE_TIMEOUT)
            .map(|(key, _)| key.as_str())
    }
}

/// The scoped shortcuts of one scope (ADR-017): one-key shortcuts, two-key sequences and the
/// scope's keys with modifiers. Every widget they are attached to shares one list of
/// shortcuts, which [`ScopedShortcuts::update`] refills when `keys.toml` changes.
pub struct ScopedShortcuts {
    scope: Scope,
    model: gio::ListStore,
    dispatch: Rc<RefCell<Dispatch>>,
}

impl ScopedShortcuts {
    pub fn new(scope: Scope, keymap: &Keymap) -> Self {
        let shortcuts = Self {
            scope,
            model: gio::ListStore::new::<gtk::Shortcut>(),
            dispatch: Rc::new(RefCell::new(Dispatch::default())),
        };
        shortcuts.update(keymap);
        shortcuts
    }

    pub fn scope(&self) -> Scope {
        self.scope
    }

    /// The shortcuts every attached controller uses.
    pub fn model(&self) -> &gio::ListStore {
        &self.model
    }

    /// The first key of a sequence waiting for its second, such as `g`.
    pub fn pending(&self) -> Option<String> {
        self.dispatch
            .borrow()
            .pending(Instant::now())
            .map(str::to_owned)
    }

    /// Takes the keys from `keymap`, for every attached widget.
    pub fn update(&self, keymap: &Keymap) {
        let dispatch = Dispatch::new(self.scope, keymap);
        let triggers = dispatch.triggers();
        self.dispatch.replace(dispatch);
        let shortcuts: Vec<gtk::Shortcut> = triggers
            .into_iter()
            .filter_map(|key| {
                let Some(trigger) = gtk::ShortcutTrigger::parse_string(&key) else {
                    tracing::warn!(key, "GTK cannot parse a scoped key");
                    return None;
                };
                let dispatch = self.dispatch.clone();
                let action = gtk::CallbackAction::new(move |widget, _| {
                    let outcome = dispatch.borrow_mut().press(&key, Instant::now());
                    match outcome {
                        Outcome::Arm | Outcome::Swallow => glib::Propagation::Stop,
                        Outcome::Pass => glib::Propagation::Proceed,
                        Outcome::Run { action, sequence } => {
                            let named = gtk::NamedAction::new(&action.detailed_name());
                            let ran =
                                named.activate(gtk::ShortcutActionFlags::empty(), widget, None);
                            if ran || sequence {
                                glib::Propagation::Stop
                            } else {
                                glib::Propagation::Proceed
                            }
                        }
                    }
                });
                Some(gtk::Shortcut::new(Some(trigger), Some(action)))
            })
            .collect();
        self.model.splice(0, self.model.n_items(), &shortcuts);
    }

    /// Handles the scope's keys on `widget`. In the bubble phase the focused widget sees every
    /// key first, so an entry inside `widget` keeps its letters; in the capture phase the keys
    /// win over `widget`'s own bindings (GtkListView's Enter and Space), which suits a widget
    /// with no text input inside.
    pub fn attach(
        &self,
        widget: &impl IsA<gtk::Widget>,
        phase: gtk::PropagationPhase,
    ) -> gtk::ShortcutController {
        let controller = gtk::ShortcutController::for_model(&self.model);
        controller.set_propagation_phase(phase);
        controller.set_name(Some(&format!("sealwax-{}-keys", scope_name(self.scope))));
        widget.add_controller(controller.clone());

        // A key that is none of the scope's ends a pending sequence, so `g`, Down, `i` doesn't
        // go to the Inbox.
        let cancel = gtk::EventControllerKey::new();
        cancel.set_propagation_phase(phase);
        let dispatch = self.dispatch.clone();
        let model = self.model.clone();
        cancel.connect_key_pressed(move |cancel, _, _, _| {
            if dispatch.borrow().pending.is_none() {
                return glib::Propagation::Proceed;
            }
            let Some(event) = cancel.current_event() else {
                return glib::Propagation::Proceed;
            };
            if event
                .downcast_ref::<gdk::KeyEvent>()
                .is_some_and(gdk::KeyEvent::is_modifier)
            {
                return glib::Propagation::Proceed;
            }
            let ours = (0..model.n_items())
                .filter_map(|index| model.item(index).and_downcast::<gtk::Shortcut>())
                .filter_map(|shortcut| shortcut.trigger())
                .any(|trigger| trigger.trigger(&event, false) == gdk::KeyMatch::Exact);
            if !ours {
                dispatch.borrow_mut().pending = None;
            }
            glib::Propagation::Proceed
        });
        widget.add_controller(cancel);
        controller
    }
}

fn scope_name(scope: Scope) -> &'static str {
    match scope {
        Scope::App => "app",
        Scope::Window => "window",
        Scope::List => "list",
        Scope::Reader => "reader",
        Scope::Composer => "composer",
    }
}

/// Attaches the scoped shortcuts of `scope` to `widget` in the bubble phase, so text fields
/// inside it keep their keys. Keep the result: it attaches the same shortcuts to more widgets,
/// and [`ScopedShortcuts::update`] applies `keys.toml` changes.
pub fn install_scoped(
    widget: &impl IsA<gtk::Widget>,
    scope: Scope,
    keymap: &Keymap,
) -> ScopedShortcuts {
    let shortcuts = ScopedShortcuts::new(scope, keymap);
    shortcuts.attach(widget, gtk::PropagationPhase::Bubble);
    shortcuts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GTK's own accelerator parser, called through the C API: it needs no display, unlike the
    /// Rust wrapper, which asserts that GTK is initialised.
    fn gtk_parses(accel: &str) -> Option<(u32, u32)> {
        let text = std::ffi::CString::new(accel).ok()?;
        let (mut key, mut mods) = (0, 0);
        // SAFETY: a NUL-terminated string and two out-parameters; no display is used.
        let parsed =
            unsafe { gtk4::ffi::gtk_accelerator_parse(text.as_ptr(), &mut key, &mut mods) };
        (parsed != 0 && key != 0).then_some((key, mods))
    }

    #[test]
    fn every_registry_key_parses_as_a_gtk_accelerator() {
        let keymap = Keymap::default();
        for id in ActionId::ALL {
            for binding in keymap.shown(id) {
                for step in binding.steps() {
                    assert!(
                        gtk_parses(&step.canonical()).is_some(),
                        "{id:?}: GTK rejects {}",
                        step.canonical()
                    );
                }
            }
        }
        assert!(gtk_parses("<Control>notakey").is_none());
    }

    #[test]
    fn gtk_sees_no_accelerator_twice() {
        let keymap = Keymap::default();
        let mut seen = HashMap::new();
        for id in ActionId::ALL {
            for accel in keymap.accels(id) {
                let parsed = gtk_parses(&accel).unwrap();
                if let Some(other) = seen.insert(parsed, id) {
                    panic!("{accel} is bound to both {other:?} and {id:?}");
                }
            }
        }
        for scope in Scope::SCOPED {
            let mut seen = HashMap::new();
            for (binding, id) in keymap.scoped_in(scope) {
                let steps: Vec<_> = binding
                    .steps()
                    .map(|step| gtk_parses(&step.canonical()).unwrap())
                    .collect();
                if let Some(other) = seen.insert(steps, id) {
                    panic!("{binding} is bound to both {other:?} and {id:?} in {scope:?}");
                }
            }
        }
    }

    #[test]
    fn single_keys_run_their_action() {
        let mut dispatch = Dispatch::new(Scope::List, &Keymap::default());
        let now = Instant::now();
        assert_eq!(
            dispatch.press("e", now),
            Outcome::Run {
                action: ActionId::Archive,
                sequence: false
            }
        );
        assert_eq!(
            dispatch.press("<Control>r", now),
            Outcome::Run {
                action: ActionId::Reply,
                sequence: false
            }
        );
        assert_eq!(
            dispatch.press("space", now),
            Outcome::Run {
                action: ActionId::Select,
                sequence: false
            }
        );
        assert_eq!(dispatch.press("i", now), Outcome::Pass);
        let mut reader = Dispatch::new(Scope::Reader, &Keymap::default());
        assert_eq!(reader.press("space", now), Outcome::Pass);
        assert_eq!(
            reader.press("u", now),
            Outcome::Run {
                action: ActionId::BackToList,
                sequence: false
            }
        );
    }

    #[test]
    fn sequences_resolve_within_the_timeout() {
        let mut dispatch = Dispatch::new(Scope::List, &Keymap::default());
        let now = Instant::now();
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(dispatch.pending(now), Some("g"));
        assert_eq!(
            dispatch.press("s", now + Duration::from_millis(400)),
            Outcome::Run {
                action: ActionId::GoToStarred,
                sequence: true
            }
        );
        assert_eq!(dispatch.pending(now), None);
        // Without the first key, s stars.
        assert_eq!(
            dispatch.press("s", now),
            Outcome::Run {
                action: ActionId::Star,
                sequence: false
            }
        );
        // Too late: the second key counts on its own.
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(
            dispatch.press("s", now + SEQUENCE_TIMEOUT + Duration::from_millis(1)),
            Outcome::Run {
                action: ActionId::Star,
                sequence: false
            }
        );
    }

    #[test]
    fn a_mistyped_sequence_runs_nothing() {
        let mut dispatch = Dispatch::new(Scope::List, &Keymap::default());
        let now = Instant::now();
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(dispatch.press("e", now), Outcome::Swallow);
        assert_eq!(
            dispatch.press("e", now),
            Outcome::Run {
                action: ActionId::Archive,
                sequence: false
            }
        );
        // `g g i` starts over.
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(dispatch.press("g", now), Outcome::Arm);
        assert_eq!(
            dispatch.press("i", now),
            Outcome::Run {
                action: ActionId::GoToInbox,
                sequence: true
            }
        );
    }

    #[test]
    fn every_scoped_key_has_a_trigger() {
        let dispatch = Dispatch::new(Scope::Reader, &Keymap::default());
        let triggers = dispatch.triggers();
        for key in [
            "g",
            "i",
            "s",
            "t",
            "d",
            "j",
            "u",
            "Escape",
            "<Control>r",
            "question",
        ] {
            assert!(triggers.contains(&key.to_owned()), "{key}");
        }
        assert!(!triggers.contains(&"space".to_owned()));
        assert!(
            !triggers.contains(&"<Control>n".to_owned()),
            "an accelerator"
        );
        assert!(triggers.contains(&"<Control><Shift>e".to_owned()));
        let composer = Dispatch::new(Scope::Composer, &Keymap::default());
        assert_eq!(
            composer.triggers(),
            [
                "<Control><Shift>v",
                "<Control><Shift>z",
                "<Control>Return",
                "<Control>b",
                "<Control>i",
                "<Control>k",
                "<Control>u",
                "<Control>y",
                "<Control>z",
            ]
        );
    }
}
