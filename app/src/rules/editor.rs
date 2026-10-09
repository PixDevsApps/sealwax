//! The rule editor (ADR-020): a page of the Rules dialog with the rule's name, "If all / any of
//! the following", its condition rows (field, operator, value; the header's name for "A
//! header…"; the size and its unit for Size) and its action rows (what, and the folder for a
//! move or a copy), each with add and remove. Save is insensitive, with the reason under the
//! rows and in its tooltip, until the rule can be written (`Draft::to_rule`, which ends with
//! `Rule::validate`). Widgets are named for the self-test: `rule-name`, `rule-match`,
//! `rule-condition-field-N`, `-operator-N`, `-header-N`, `-value-N`, `-size-over-N`,
//! `-size-N`, `-unit-N`, `-remove-N`, `rule-action-N`, `rule-action-folder-N`,
//! `rule-action-remove-N`, `rule-add-condition`, `rule-add-action`, `rule-save`,
//! `rule-problem`.

use super::draft::{
    ActionChoice, Draft, DraftAction, DraftCondition, FieldChoice, OPERATORS, SizeUnit,
    operator_label,
};
use crate::window::panes::{classed, icon_button, named};
use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use sealwax_domain::sieve::{Match, Rule};
use std::cell::RefCell;
use std::rc::{Rc, Weak};

/// A folder a move or a copy can name: its full name and how the editor shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub name: String,
    pub label: String,
}

type Save = Rc<dyn Fn(Rule)>;

struct Inner {
    name: gtk::Entry,
    matching: gtk::DropDown,
    conditions: gtk::ListBox,
    actions: gtk::ListBox,
    /// "No conditions: …", while there are none.
    every: gtk::Label,
    problem: gtk::Label,
    save: gtk::Button,
    condition_rows: RefCell<Vec<ConditionRow>>,
    action_rows: RefCell<Vec<ActionRow>>,
    targets: Vec<Target>,
    enabled: bool,
    on_save: Save,
    weak: RefCell<Weak<Inner>>,
}

/// An open editor: its page, and what its widgets' handlers share, which whoever shows the page
/// keeps while it does.
pub struct Editor {
    pub page: adw::NavigationPage,
    inner: Rc<Inner>,
}

impl Editor {
    /// Whether Save can run, and why not.
    pub fn problem(&self) -> Option<&'static str> {
        self.inner.draft().to_rule().err().map(|problem| problem.text())
    }
}

/// The editor's page; `on_save` gets the rule when Save is pressed.
pub fn new(
    title: &str,
    draft: &Draft,
    targets: Vec<Target>,
    on_save: impl Fn(Rule) + 'static,
) -> Editor {
    let name = named(
        gtk::Entry::builder()
            .text(&draft.name)
            .placeholder_text("Unnamed rule")
            .hexpand(true)
            .build(),
        "rule-name",
    );
    name.update_property(&[gtk::accessible::Property::Label("Name")]);
    let matching = named(gtk::DropDown::from_strings(&["all", "any"]), "rule-match");
    matching.set_selected(match draft.matching {
        Match::All => 0,
        Match::Any => 1,
    });
    crate::accessibility::name_button(&matching, "All or any");
    let list = |name: &str, label: &str| {
        let list = named(
            classed(
                gtk::ListBox::builder()
                    .selection_mode(gtk::SelectionMode::None)
                    .build(),
                &["boxed-list", "sw-rule-rows"],
            ),
            name,
        );
        list.update_property(&[gtk::accessible::Property::Label(label)]);
        list
    };
    let conditions = list("rule-conditions", "Conditions");
    let actions = list("rule-actions", "Actions");
    let every = named(
        classed(
            gtk::Label::builder()
                .label("No conditions: the rule acts on every message.")
                .xalign(0.0)
                .wrap(true)
                .build(),
            &["dim-label", "sw-small"],
        ),
        "rule-every",
    );
    let problem = named(
        classed(
            gtk::Label::builder().xalign(0.0).wrap(true).build(),
            &["dim-label", "sw-rule-problem"],
        ),
        "rule-problem",
    );
    let save = named(
        classed(
            gtk::Button::builder().label("Save").build(),
            &["suggested-action"],
        ),
        "rule-save",
    );
    let add_condition = add_button("Add Condition", "rule-add-condition");
    let add_action = add_button("Add Action", "rule-add-action");

    let caption = |text: &str| {
        classed(
            gtk::Label::builder().label(text).xalign(0.0).build(),
            &["heading", "sw-rule-caption"],
        )
    };
    let name_caption = caption("Name");
    name_caption.set_mnemonic_widget(Some(&name));
    let if_line = gtk::Box::builder().spacing(6).build();
    if_line.append(&gtk::Label::new(Some("If")));
    if_line.append(&matching);
    if_line.append(&gtk::Label::new(Some("of the following:")));
    let content = classed(
        gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_top(12)
            .margin_bottom(18)
            .margin_start(18)
            .margin_end(18)
            .build(),
        &["sw-rule-editor"],
    );
    content.append(&name_caption);
    content.append(&name);
    content.append(&caption("Conditions"));
    content.append(&if_line);
    content.append(&conditions);
    content.append(&every);
    content.append(&add_condition);
    content.append(&caption("Actions"));
    content.append(&actions);
    content.append(&add_action);
    content.append(&problem);
    let clamp = adw::Clamp::builder()
        .maximum_size(720)
        .child(&content)
        .build();
    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&clamp)
        .build();
    let header = adw::HeaderBar::new();
    header.pack_end(&save);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&scrolled));
    let page = adw::NavigationPage::builder()
        .title(title)
        .tag("edit")
        .child(&view)
        .build();

    let inner = Rc::new(Inner {
        name,
        matching,
        conditions,
        actions,
        every,
        problem,
        save,
        condition_rows: RefCell::new(Vec::new()),
        action_rows: RefCell::new(Vec::new()),
        targets,
        enabled: draft.enabled,
        on_save: Rc::new(on_save),
        weak: RefCell::new(Weak::new()),
    });
    inner.weak.replace(Rc::downgrade(&inner));
    for condition in &draft.conditions {
        inner.add_condition(condition);
    }
    for action in &draft.actions {
        inner.add_action(action);
    }
    let weak = Rc::downgrade(&inner);
    inner.name.connect_changed(move |_| changed(&weak));
    let weak = Rc::downgrade(&inner);
    inner.matching.connect_selected_notify(move |_| changed(&weak));
    let weak = Rc::downgrade(&inner);
    add_condition.connect_clicked(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.add_condition(&DraftCondition::default());
            inner.validate();
        }
    });
    let weak = Rc::downgrade(&inner);
    add_action.connect_clicked(move |_| {
        if let Some(inner) = weak.upgrade() {
            inner.add_action(&DraftAction {
                choice: ActionChoice::MoveTo,
                folder: None,
            });
            inner.validate();
        }
    });
    let weak = Rc::downgrade(&inner);
    inner.save.connect_clicked(move |_| {
        if let Some(inner) = weak.upgrade()
            && let Ok(rule) = inner.draft().to_rule()
        {
            (inner.on_save)(rule);
        }
    });
    inner.validate();
    Editor { page, inner }
}

fn add_button(label: &str, name: &str) -> gtk::Button {
    let content = adw::ButtonContent::builder()
        .icon_name("list-add-symbolic")
        .label(label)
        .build();
    named(
        classed(
            gtk::Button::builder()
                .child(&content)
                .halign(gtk::Align::Start)
                .build(),
            &["flat"],
        ),
        name,
    )
}

fn changed(weak: &Weak<Inner>) {
    if let Some(inner) = weak.upgrade() {
        inner.validate();
    }
}

impl Inner {
    fn draft(&self) -> Draft {
        Draft {
            name: self.name.text().to_string(),
            enabled: self.enabled,
            matching: if self.matching.selected() == 1 {
                Match::Any
            } else {
                Match::All
            },
            conditions: self
                .condition_rows
                .borrow()
                .iter()
                .map(ConditionRow::read)
                .collect(),
            actions: self
                .action_rows
                .borrow()
                .iter()
                .map(|row| row.read(&self.targets))
                .collect(),
        }
    }

    /// Save follows whether the rule can be written; the reason shows under the rows.
    fn validate(&self) {
        let problem = self.draft().to_rule().err();
        let reason = problem.map(|problem| problem.text());
        self.save.set_sensitive(problem.is_none());
        self.save.set_tooltip_text(reason);
        self.problem.set_label(reason.unwrap_or_default());
        self.problem.set_visible(reason.is_some());
        self.every
            .set_visible(self.condition_rows.borrow().is_empty());
    }

    fn add_condition(&self, condition: &DraftCondition) {
        let row = ConditionRow::new(condition);
        self.conditions.append(&row.row);
        let weak = self.weak.borrow().clone();
        for dropdown in [&row.field, &row.operator, &row.size_over, &row.unit] {
            let weak = weak.clone();
            dropdown.connect_selected_notify(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.refresh_rows();
                    inner.validate();
                }
            });
        }
        for entry in [&row.header, &row.value, &row.size] {
            let weak = weak.clone();
            entry.connect_changed(move |_| changed(&weak));
        }
        let target = row.row.clone();
        row.remove.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner
                    .condition_rows
                    .borrow_mut()
                    .retain(|row| row.row != target);
                inner.conditions.remove(&target);
                inner.refresh_rows();
                inner.validate();
            }
        });
        self.condition_rows.borrow_mut().push(row);
        self.refresh_rows();
    }

    fn add_action(&self, action: &DraftAction) {
        let row = ActionRow::new(action, &self.targets);
        self.actions.append(&row.row);
        let weak = self.weak.borrow().clone();
        for dropdown in [&row.choice, &row.folder] {
            let weak = weak.clone();
            dropdown.connect_selected_notify(move |_| {
                if let Some(inner) = weak.upgrade() {
                    inner.refresh_rows();
                    inner.validate();
                }
            });
        }
        let target = row.row.clone();
        row.remove.connect_clicked(move |_| {
            if let Some(inner) = weak.upgrade() {
                inner
                    .action_rows
                    .borrow_mut()
                    .retain(|row| row.row != target);
                inner.actions.remove(&target);
                inner.refresh_rows();
                inner.validate();
            }
        });
        self.action_rows.borrow_mut().push(row);
        self.refresh_rows();
    }

    /// Each row shows the widgets its choice needs, and is named by its place.
    fn refresh_rows(&self) {
        for (index, row) in self.condition_rows.borrow().iter().enumerate() {
            row.refresh(index);
        }
        for (index, row) in self.action_rows.borrow().iter().enumerate() {
            row.refresh(index);
        }
    }
}

/// A condition's widgets.
struct ConditionRow {
    row: gtk::ListBoxRow,
    field: gtk::DropDown,
    header: gtk::Entry,
    operator: gtk::DropDown,
    value: gtk::Entry,
    size_over: gtk::DropDown,
    size: gtk::Entry,
    unit: gtk::DropDown,
    remove: gtk::Button,
}

/// A choice named for screen readers by what it chooses ("Field"), not by its value: GTK names
/// a drop-down by the item it shows.
fn dropdown(items: &[&str], label: &str) -> gtk::DropDown {
    let dropdown = gtk::DropDown::from_strings(items);
    dropdown.set_valign(gtk::Align::Center);
    crate::accessibility::name_button(&dropdown, label);
    dropdown
}

fn entry(placeholder: &str, label: &str) -> gtk::Entry {
    let entry = gtk::Entry::builder()
        .placeholder_text(placeholder)
        .valign(gtk::Align::Center)
        .build();
    entry.update_property(&[gtk::accessible::Property::Label(label)]);
    entry
}

fn index_of<T: PartialEq>(items: &[T], wanted: &T) -> u32 {
    items
        .iter()
        .position(|item| item == wanted)
        .unwrap_or_default() as u32
}

/// A row of a rule list: its choices, and its remove button at the end; not a stop of its
/// own, so Tab goes through its controls.
fn list_row(choices: &gtk::Box, remove: &gtk::Button) -> gtk::ListBoxRow {
    choices.set_hexpand(true);
    let line = gtk::Box::builder().spacing(6).build();
    line.append(choices);
    line.append(remove);
    classed(
        gtk::ListBoxRow::builder()
            .activatable(false)
            .selectable(false)
            .focusable(false)
            .child(&line)
            .build(),
        &["sw-rule-row"],
    )
}

impl ConditionRow {
    fn new(condition: &DraftCondition) -> Self {
        let fields: Vec<&str> = FieldChoice::ALL.iter().map(|field| field.label()).collect();
        let field = dropdown(&fields, "Field");
        field.set_selected(index_of(&FieldChoice::ALL, &condition.field));
        let header = entry("Header name", "Header name");
        header.set_width_chars(10);
        header.set_text(&condition.header);
        let operators: Vec<&str> = OPERATORS.iter().map(|op| operator_label(*op)).collect();
        let operator = dropdown(&operators, "Operator");
        operator.set_selected(index_of(&OPERATORS, &condition.operator));
        let value = entry("Value", "Value");
        value.set_hexpand(true);
        value.set_text(&condition.value);
        let size_over = dropdown(&["is over", "is under"], "Over or under");
        size_over.set_selected(if condition.over { 0 } else { 1 });
        let size = entry("Size", "Size");
        size.set_width_chars(6);
        size.set_input_purpose(gtk::InputPurpose::Digits);
        size.set_text(&condition.size);
        let units: Vec<&str> = SizeUnit::ALL.iter().map(|unit| unit.label()).collect();
        let unit = dropdown(&units, "Unit");
        unit.set_selected(index_of(&SizeUnit::ALL, &condition.unit));
        let remove = icon_button(&["list-remove-symbolic"], "Remove Condition");
        remove.add_css_class("flat");
        remove.set_valign(gtk::Align::Center);
        let line = gtk::Box::builder().spacing(6).build();
        for widget in [
            field.upcast_ref::<gtk::Widget>(),
            header.upcast_ref(),
            operator.upcast_ref(),
            size_over.upcast_ref(),
            value.upcast_ref(),
            size.upcast_ref(),
            unit.upcast_ref(),
        ] {
            line.append(widget);
        }
        Self {
            row: list_row(&line, &remove),
            field,
            header,
            operator,
            value,
            size_over,
            size,
            unit,
            remove,
        }
    }

    fn choice(&self) -> FieldChoice {
        FieldChoice::ALL
            .get(self.field.selected() as usize)
            .copied()
            .unwrap_or(FieldChoice::From)
    }

    fn read(&self) -> DraftCondition {
        DraftCondition {
            field: self.choice(),
            header: self.header.text().to_string(),
            operator: OPERATORS
                .get(self.operator.selected() as usize)
                .copied()
                .unwrap_or(OPERATORS[0]),
            value: self.value.text().to_string(),
            over: self.size_over.selected() == 0,
            size: self.size.text().to_string(),
            unit: SizeUnit::ALL
                .get(self.unit.selected() as usize)
                .copied()
                .unwrap_or(SizeUnit::Megabytes),
        }
    }

    fn refresh(&self, index: usize) {
        let choice = self.choice();
        let size = choice == FieldChoice::Size;
        self.header.set_visible(choice == FieldChoice::Header);
        self.operator.set_visible(!size);
        self.value.set_visible(!size);
        self.size_over.set_visible(size);
        self.size.set_visible(size);
        self.unit.set_visible(size);
        for (widget, part) in [
            (self.field.upcast_ref::<gtk::Widget>(), "field"),
            (self.header.upcast_ref(), "header"),
            (self.operator.upcast_ref(), "operator"),
            (self.value.upcast_ref(), "value"),
            (self.size_over.upcast_ref(), "size-over"),
            (self.size.upcast_ref(), "size"),
            (self.unit.upcast_ref(), "unit"),
            (self.remove.upcast_ref(), "remove"),
        ] {
            widget.set_widget_name(&format!("rule-condition-{part}-{index}"));
        }
    }
}

/// An action's widgets.
struct ActionRow {
    row: gtk::ListBoxRow,
    choice: gtk::DropDown,
    folder: gtk::DropDown,
    remove: gtk::Button,
}

/// The folder menu's first item, while none is chosen.
pub const NO_FOLDER: &str = "Choose a folder";

impl ActionRow {
    fn new(action: &DraftAction, targets: &[Target]) -> Self {
        let choices: Vec<&str> = ActionChoice::ALL.iter().map(|c| c.label()).collect();
        let choice = dropdown(&choices, "Action");
        choice.set_selected(index_of(&ActionChoice::ALL, &action.choice));
        let mut folders = vec![NO_FOLDER];
        folders.extend(targets.iter().map(|target| target.label.as_str()));
        let folder = dropdown(&folders, "Folder");
        folder.set_hexpand(true);
        let chosen = action
            .folder
            .as_ref()
            .and_then(|name| targets.iter().position(|target| &target.name == name))
            .map_or(0, |index| index as u32 + 1);
        folder.set_selected(chosen);
        let remove = icon_button(&["list-remove-symbolic"], "Remove Action");
        remove.add_css_class("flat");
        remove.set_valign(gtk::Align::Center);
        let line = gtk::Box::builder().spacing(6).build();
        line.append(&choice);
        line.append(&folder);
        Self {
            row: list_row(&line, &remove),
            choice,
            folder,
            remove,
        }
    }

    fn kind(&self) -> ActionChoice {
        ActionChoice::ALL
            .get(self.choice.selected() as usize)
            .copied()
            .unwrap_or(ActionChoice::MoveTo)
    }

    fn read(&self, targets: &[Target]) -> DraftAction {
        let choice = self.kind();
        let folder = match self.folder.selected() {
            0 => None,
            index => targets
                .get(index as usize - 1)
                .map(|target| target.name.clone()),
        };
        DraftAction {
            choice,
            folder: folder.filter(|_| choice.takes_folder()),
        }
    }

    fn refresh(&self, index: usize) {
        self.folder.set_visible(self.kind().takes_folder());
        self.choice.set_widget_name(&format!("rule-action-{index}"));
        self.folder
            .set_widget_name(&format!("rule-action-folder-{index}"));
        self.remove
            .set_widget_name(&format!("rule-action-remove-{index}"));
    }
}
