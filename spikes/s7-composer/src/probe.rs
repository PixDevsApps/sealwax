//! A one-child container that notices every time its child is re-snapshotted. GTK only
//! re-runs a widget's snapshot when it or a descendant queued a draw, so with the WebView as
//! the only child a probe snapshot means "the WebView drew new content in this frame", and the
//! frame clock's `after-paint` that follows is the end of that frame.

use gtk4::glib;
use gtk4::prelude::*;
use gtk4::subclass::prelude::*;
use std::time::Instant;

/// The right-hand strip where WebKit's overlay scrollbar fades in and out; left out of the hash.
const SCROLLBAR_STRIP: usize = 20;

#[derive(Clone, Copy, Debug)]
pub struct Paint {
    pub snapshot_at: Instant,
    pub painted_at: Instant,
    /// Hash of the pixels the WebView drew in this frame (without the scrollbar strip), only
    /// while capturing.
    pub hash: Option<u64>,
}

mod imp {
    use super::*;
    use std::cell::{Cell, OnceCell, RefCell};

    #[derive(Default)]
    pub struct PaintProbe {
        pub snapshot_in_frame: Cell<Option<Instant>>,
        pub snapshots: Cell<u64>,
        pub sender: OnceCell<async_channel::Sender<Paint>>,
        pub frame_handler: RefCell<Option<(gtk4::gdk::FrameClock, glib::SignalHandlerId)>>,
        pub capture: Cell<bool>,
        pub renderer: RefCell<Option<gtk4::gsk::Renderer>>,
        pub frame_hash: Cell<Option<u64>>,
        pub last_hash: Cell<Option<u64>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PaintProbe {
        const NAME: &'static str = "S7PaintProbe";
        type Type = super::PaintProbe;
        type ParentType = gtk4::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_layout_manager_type::<gtk4::BinLayout>();
        }
    }

    impl ObjectImpl for PaintProbe {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for PaintProbe {
        fn realize(&self) {
            self.parent_realize();
            if let Some(clock) = self.obj().frame_clock() {
                let probe = self.obj().downgrade();
                let handler = clock.connect_after_paint(move |_| {
                    if let Some(probe) = probe.upgrade() {
                        probe.imp().after_paint();
                    }
                });
                self.frame_handler.replace(Some((clock, handler)));
            }
        }

        fn unrealize(&self) {
            if let Some((clock, handler)) = self.frame_handler.take() {
                clock.disconnect(handler);
            }
            self.parent_unrealize();
        }

        fn snapshot(&self, snapshot: &gtk4::Snapshot) {
            self.snapshots.set(self.snapshots.get() + 1);
            if self.snapshot_in_frame.get().is_none() {
                self.snapshot_in_frame.set(Some(Instant::now()));
            }
            if !self.capture.get() {
                self.parent_snapshot(snapshot);
                return;
            }
            let inner = gtk4::Snapshot::new();
            self.parent_snapshot(&inner);
            if let Some(node) = inner.to_node() {
                let hash = self.hash(&node);
                self.frame_hash.set(Some(hash));
                self.last_hash.set(Some(hash));
                snapshot.append_node(&node);
            }
        }
    }

    impl PaintProbe {
        fn after_paint(&self) {
            let hash = self.frame_hash.take();
            if let Some(snapshot_at) = self.snapshot_in_frame.take()
                && let Some(sender) = self.sender.get()
            {
                let _ = sender.try_send(Paint {
                    snapshot_at,
                    painted_at: Instant::now(),
                    hash,
                });
            }
        }

        /// Renders the child's node with a Cairo renderer and hashes the pixels.
        fn hash(&self, node: &gtk4::gsk::RenderNode) -> u64 {
            use gtk4::gsk::prelude::*;
            use std::hash::{Hash, Hasher};
            let mut renderer = self.renderer.borrow_mut();
            if renderer.is_none() {
                let cairo = gtk4::gsk::CairoRenderer::new();
                if cairo.realize_for_display(&self.obj().display()).is_ok() {
                    *renderer = Some(cairo.upcast());
                }
            }
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            if let Some(renderer) = renderer.as_ref() {
                let texture = renderer.render_texture(node, None);
                if let Some(dir) = std::env::var_os("MAIL_S7_DUMP_FRAMES") {
                    let count = self.snapshots.get();
                    let _ = texture.save_to_png(
                        std::path::Path::new(&dir).join(format!("frame-{count:05}.png")),
                    );
                }
                let stride = texture.width() as usize * 4;
                let mut pixels = vec![0; stride * texture.height() as usize];
                texture.download(&mut pixels, stride);
                let kept = stride.saturating_sub(SCROLLBAR_STRIP * 4);
                for row in pixels.chunks(stride.max(1)) {
                    row[..kept.min(row.len())].hash(&mut hasher);
                }
            }
            hasher.finish()
        }
    }
}

glib::wrapper! {
    pub struct PaintProbe(ObjectSubclass<imp::PaintProbe>)
        @extends gtk4::Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl PaintProbe {
    pub fn new(child: &impl IsA<gtk4::Widget>) -> (Self, async_channel::Receiver<Paint>) {
        let probe: Self = glib::Object::new();
        child.set_parent(&probe);
        let (sender, receiver) = async_channel::unbounded();
        let _ = probe.imp().sender.set(sender);
        (probe, receiver)
    }

    pub fn snapshots(&self) -> u64 {
        self.imp().snapshots.get()
    }

    /// While capturing, every frame's WebView pixels are rendered and hashed (slow; only for
    /// checking which frame first shows an edit, never while timing).
    pub fn set_capture(&self, capture: bool) {
        self.imp().capture.set(capture);
        if !capture && let Some(renderer) = self.imp().renderer.take() {
            use gtk4::gsk::prelude::*;
            renderer.unrealize();
        }
    }

    pub fn last_hash(&self) -> Option<u64> {
        self.imp().last_hash.get()
    }
}
