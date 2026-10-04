//! Two containers the reader needs. [`BodyBox`] holds a WebView at exactly its requested
//! height (S2: a WebView's natural height is WebKit's contents size, which never drops below
//! its viewport). [`Stage`] shows one page and lays the next one out unseen at the same width,
//! so a message appears once its body has its height, with no jump and no blank frame.
//! [`hand_over_focus`] keeps GTK from moving the focus out of a page that has left the window.

use gtk4 as gtk;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;

mod body_box {
    use super::*;

    #[derive(Default)]
    pub struct BodyBox;

    #[glib::object_subclass]
    impl ObjectSubclass for BodyBox {
        const NAME: &'static str = "SealwaxReaderBodyBox";
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
    /// One child at exactly its height request and the full width.
    pub struct BodyBox(ObjectSubclass<body_box::BodyBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl BodyBox {
    pub fn new(child: &impl IsA<gtk::Widget>) -> Self {
        let body: Self = glib::Object::new();
        body.set_child(Some(child));
        body
    }

    pub fn set_child(&self, child: Option<&impl IsA<gtk::Widget>>) {
        while let Some(old) = self.first_child() {
            old.unparent();
        }
        if let Some(child) = child {
            if child.parent().is_some() {
                child.unparent();
            }
            child.set_parent(self);
        }
        self.queue_resize();
    }
}

mod stage {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    pub struct Stage {
        pub shown: RefCell<Option<gtk::Widget>>,
        pub staged: RefCell<Option<gtk::Widget>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Stage {
        const NAME: &'static str = "SealwaxReaderStage";
        type Type = super::Stage;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for Stage {
        fn dispose(&self) {
            for child in [self.shown.take(), self.staged.take()]
                .into_iter()
                .flatten()
            {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Stage {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.shown
                .borrow()
                .as_ref()
                .map_or(gtk::SizeRequestMode::ConstantSize, |child| {
                    child.request_mode()
                })
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            match self.shown.borrow().as_ref() {
                Some(child) => child.measure(orientation, for_size),
                None => (0, 0, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            if let Some(child) = self.shown.borrow().as_ref() {
                child.allocate(width, height, baseline, None);
            }
            if let Some(child) = self.staged.borrow().as_ref() {
                let (_, natural, _, _) = child.measure(gtk::Orientation::Vertical, width);
                child.allocate(width, natural.max(1), -1, None);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(child) = self.shown.borrow().as_ref() {
                self.obj().snapshot_child(child, snapshot);
            }
        }
    }
}

glib::wrapper! {
    /// The page on screen, and at most one page being laid out at the same width without being
    /// drawn or reachable by pointer or keyboard.
    pub struct Stage(ObjectSubclass<stage::Stage>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for Stage {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Stage {
    pub fn shown(&self) -> Option<gtk::Widget> {
        self.imp().shown.borrow().clone()
    }

    pub fn staged(&self) -> Option<gtk::Widget> {
        self.imp().staged.borrow().clone()
    }

    /// Lays `page` out unseen, replacing any page already waiting.
    pub fn stage(&self, page: &impl IsA<gtk::Widget>) {
        let page = page.as_ref();
        page.set_can_target(false);
        page.set_can_focus(false);
        if let Some(old) = self.imp().staged.replace(Some(page.clone())) {
            old.unparent();
        }
        page.set_parent(self);
        self.queue_resize();
    }

    /// Drops the waiting page, if any.
    pub fn unstage(&self) {
        if let Some(old) = self.imp().staged.take() {
            old.unparent();
            self.queue_resize();
        }
    }

    /// Shows the waiting page in place of the shown one; returns the page it replaced. If the
    /// focus was in the replaced page, `focus` takes it.
    pub fn promote(&self, focus: Option<&gtk::Widget>) -> Option<gtk::Widget> {
        let page = self.imp().staged.take()?;
        page.set_can_target(true);
        page.set_can_focus(true);
        let old = self.imp().shown.replace(Some(page));
        if let Some(old) = &old {
            hand_over_focus(old, focus);
            old.unparent();
        }
        self.queue_resize();
        old
    }

    /// Shows `page` at once; returns the page it replaced. If the focus was in the replaced
    /// page, `focus` takes it. A waiting page stays waiting.
    pub fn show(
        &self,
        page: Option<&impl IsA<gtk::Widget>>,
        focus: Option<&gtk::Widget>,
    ) -> Option<gtk::Widget> {
        let page = page.map(|page| page.as_ref().clone());
        if let Some(page) = &page {
            page.set_parent(self);
        }
        let old = self.imp().shown.replace(page);
        if let Some(old) = &old {
            hand_over_focus(old, focus);
            old.unparent();
        }
        self.queue_resize();
        old
    }
}

/// Before `old` leaves the window: if the focus is in it, `target` takes it, or nothing does.
/// Left to GTK 4.22, the focus moves later, from a widget no longer in the window, through the
/// whole window's focus chain (a Gtk-CRITICAL on the way), to the first selectable label.
pub fn hand_over_focus(old: &impl IsA<gtk::Widget>, target: Option<&gtk::Widget>) {
    let old = old.as_ref();
    let Some(root) = old.root() else {
        return;
    };
    let inside = root
        .focus()
        .is_some_and(|focus| &focus == old || focus.is_ancestor(old));
    if inside && !target.is_some_and(|target| target.grab_focus()) {
        root.set_focus(None::<&gtk::Widget>);
    }
}
