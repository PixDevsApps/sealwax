mod config;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

fn main() -> glib::ExitCode {
    let started = Instant::now();
    if let Err(error) = sealwax_infrastructure::logging::init() {
        eprintln!("Could not initialize logging: {error}");
        return glib::ExitCode::FAILURE;
    }
    let exit_after = match config::exit_after_from_env() {
        Ok(exit_after) => exit_after,
        Err(message) => {
            tracing::error!("{message}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = adw::Application::builder()
        .application_id(config::APP_ID)
        .build();
    app.connect_activate(move |app| activate(app, started, exit_after));
    app.run_with_args::<&str>(&[])
}

fn activate(app: &adw::Application, started: Instant, exit_after: Option<Duration>) {
    if let Some(window) = app.active_window() {
        window.present();
        return;
    }
    let placeholder = adw::StatusPage::builder()
        .icon_name("mail-unread-symbolic")
        .title("Sealwax")
        .description("M0 scaffold")
        .build();
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Sealwax")
        .default_width(1200)
        .default_height(800)
        .content(&placeholder)
        .build();
    log_first_frame(&placeholder, started);
    window.present();
    if let Some(after) = exit_after {
        glib::timeout_add_local_once(
            after,
            glib::clone!(
                #[weak]
                app,
                move || app.quit()
            ),
        );
    }
}

/// Logs `first-frame <ms since process start>` after the first paint that follows `widget`
/// mapping. The CI smoke run greps for it.
fn log_first_frame(widget: &impl IsA<gtk4::Widget>, started: Instant) {
    let on_map = Rc::new(RefCell::new(None));
    let handler = widget.connect_map(glib::clone!(
        #[strong]
        on_map,
        move |widget| {
            if let Some(handler) = on_map.take() {
                widget.disconnect(handler);
            }
            let Some(clock) = widget.frame_clock() else {
                tracing::warn!("mapped widget has no frame clock; first-frame not measured");
                return;
            };
            let on_paint = Rc::new(RefCell::new(None));
            let handler = clock.connect_after_paint(glib::clone!(
                #[strong]
                on_paint,
                move |clock| {
                    if let Some(handler) = on_paint.take() {
                        clock.disconnect(handler);
                    }
                    let ms = started.elapsed().as_secs_f64() * 1000.0;
                    tracing::info!("first-frame {ms:.1}");
                }
            ));
            on_paint.replace(Some(handler));
        }
    ));
    on_map.replace(Some(handler));
}
