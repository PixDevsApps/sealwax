//! Renders the window to a PNG with GSK's Cairo renderer, as Stet does. Under Broadway this is
//! the only way to see what the app draws. Popovers have surfaces of their own, so the window's
//! drawing leaves them out; they are drawn over it where their surface is.

use gtk4 as gtk;
use gtk4::prelude::*;
use gtk4::{gdk, graphene, gsk};
use std::path::Path;

pub fn capture(window: &impl IsA<gtk::Widget>, path: &Path) -> Result<(), String> {
    let window = window.as_ref();
    let (width, height) = (window.width(), window.height());
    if width <= 0 || height <= 0 {
        return Err("the window has no size yet".to_owned());
    }
    let snapshot = gtk::Snapshot::new();
    gtk::WidgetPaintable::new(Some(window)).snapshot(
        &snapshot,
        f64::from(width),
        f64::from(height),
    );
    if window.is::<gtk::Window>() {
        // A popup's place is relative to the window's surface, which includes its shadow.
        let (shadow_x, shadow_y) = window
            .native()
            .map_or((0.0, 0.0), |native| native.surface_transform());
        for popover in popovers(window) {
            let Some(popup) = popover.surface().and_downcast::<gdk::Popup>() else {
                continue;
            };
            snapshot.save();
            snapshot.translate(&graphene::Point::new(
                (f64::from(popup.position_x()) - shadow_x) as f32,
                (f64::from(popup.position_y()) - shadow_y) as f32,
            ));
            gtk::WidgetPaintable::new(Some(&popover)).snapshot(
                &snapshot,
                f64::from(popover.width()),
                f64::from(popover.height()),
            );
            snapshot.restore();
        }
    }
    let node = snapshot
        .to_node()
        .ok_or_else(|| "the window drew nothing".to_owned())?;
    let renderer = gsk::CairoRenderer::new();
    renderer
        .realize_for_display(&WidgetExt::display(window))
        .map_err(|error| error.to_string())?;
    let viewport = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
    let texture = renderer.render_texture(&node, Some(&viewport));
    renderer.unrealize();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    texture.save_to_png(path).map_err(|error| error.to_string())
}

/// The popovers on screen under `root`.
fn popovers(root: &gtk::Widget) -> Vec<gtk::Popover> {
    let mut found = Vec::new();
    super::collect(root, &mut |widget| {
        if let Some(popover) = widget.downcast_ref::<gtk::Popover>()
            && popover.is_mapped()
        {
            found.push(popover.clone());
        }
    });
    found
}
