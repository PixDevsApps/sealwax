//! What the reader's and the composer's WebViews share (SECURITY.md "Message rendering"): one
//! set of hardened settings, a web context whose network session keeps nothing and downloads
//! nothing, and content filters compiled into Sealwax's cache. Each side adds its own filter
//! rules, `cid:` scheme and policy handlers. Written once, so the two can't drift apart (they
//! had: the reader set two properties the composer didn't, and made the filters' folder on the
//! GTK thread).

use gtk4::{gio, glib};
use sealwax_domain::theme::WebViewFonts;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// What WebKitGTK runs in a Flatpak to see whether its web processes can get a sandbox of their
/// own (`flatpak-spawn --sandbox`, through the Flatpak portal). When it fails, WebKit starts
/// them without one, inside the app's sandbox (SECURITY.md, layer 8).
pub(crate) const FLATPAK_SANDBOX_CHECK: [&str; 5] = [
    "flatpak-spawn",
    "--sandbox",
    "--sandbox-expose-path-ro-try=/this_path_will_not_exist",
    "--",
    "true",
];

/// How long [`FLATPAK_SANDBOX_CHECK`] may take.
const SANDBOX_CHECK_TIMEOUT: Duration = Duration::from_secs(10);

/// In a Flatpak: whether WebKit's web processes get their own sandbox, asked as WebKit asks;
/// blocking.
pub(crate) fn flatpak_web_sandbox() -> Result<(), String> {
    sandbox_check(&FLATPAK_SANDBOX_CHECK, SANDBOX_CHECK_TIMEOUT)
}

/// The Flatpak check, run once on a thread of its own: its outcome, and a channel that closes
/// when it is known.
struct SandboxCheck {
    outcome: OnceLock<Result<(), String>>,
    done: async_channel::Receiver<()>,
}

fn sandbox_check_once() -> &'static SandboxCheck {
    static CHECK: OnceLock<SandboxCheck> = OnceLock::new();
    CHECK.get_or_init(|| {
        let (finished, done) = async_channel::bounded::<()>(1);
        let spawned = std::thread::Builder::new()
            .name("sealwax-web-sandbox".into())
            .spawn(move || {
                let started = Instant::now();
                let outcome = flatpak_web_sandbox();
                match &outcome {
                    Ok(()) => tracing::debug!(
                        elapsed = ?started.elapsed(),
                        "WebKit's Flatpak sandbox is usable"
                    ),
                    Err(problem) if cfg!(debug_assertions) => tracing::warn!(
                        %problem,
                        "WebKit can't sandbox its web processes in this Flatpak; a development \
                         build shows mail anyway"
                    ),
                    Err(problem) => tracing::error!(
                        %problem,
                        "WebKit can't sandbox its web processes in this Flatpak, so mail is \
                         shown as text and the composer doesn't open. The Flatpak portal \
                         (flatpak-portal) gives the sandbox; check that it runs"
                    ),
                }
                let _ = sandbox_check_once().outcome.set(outcome);
                drop(finished);
            });
        if let Err(error) = spawned {
            tracing::error!(%error, "WebKit's Flatpak sandbox can't be checked");
        }
        SandboxCheck {
            outcome: OnceLock::new(),
            done,
        }
    })
}

/// In a Flatpak, as early as possible: starts the check [`web_views_allowed`] waits for, so it
/// runs beside the start instead of before it.
pub(crate) fn start_flatpak_check() {
    if sealwax_infrastructure::sandbox::is_flatpak() {
        sandbox_check_once();
    }
}

/// Whether mail may go into a web view: always outside a Flatpak; in one when WebKit's own
/// sandbox check passed, and in a development build whatever it said. Waits for the check
/// without blocking (SECURITY.md, layer 8).
pub(crate) async fn web_views_allowed() -> bool {
    if !sealwax_infrastructure::sandbox::is_flatpak() {
        return true;
    }
    let check = sandbox_check_once();
    let _ = check.done.recv().await;
    allowed(check.outcome.get(), cfg!(debug_assertions))
}

/// The check's outcome as an answer: unknown (the check didn't run) counts as failed.
fn allowed(outcome: Option<&Result<(), String>>, development: bool) -> bool {
    match outcome {
        Some(Ok(())) => true,
        Some(Err(_)) | None => development,
    }
}

fn sandbox_check(command: &[&str], timeout: Duration) -> Result<(), String> {
    let (program, args) = command.split_first().ok_or("no command")?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{program} didn't start: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(status)) => return Err(format!("{program} --sandbox failed ({status})")),
            Ok(None) if started.elapsed() < timeout => {
                std::thread::sleep(Duration::from_millis(5));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} --sandbox didn't finish"));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// Runtime features that make requests without any script and that the old settings no
/// longer control in WebKitGTK 2.52 (S2): DNS prefetch, a clicked link's preconnect,
/// `<link rel=prefetch|preload>` and speculation rules. Each is turned off where this WebKit
/// still has it.
pub(crate) const NETWORK_FEATURES: [&str; 5] = [
    "LinkDNSPrefetch",
    "LinkPreconnect",
    "LinkPrefetch",
    "LinkPreload",
    "SpeculationRulesPrefetch",
];

/// The [`NETWORK_FEATURES`] WebKit dropped, with the first WebKitGTK (major, minor) without
/// each. 2.54 has no switch for `<link rel=dns-prefetch>` any more (310198@main): it always
/// runs there, and only the reader's pings filter stops it.
#[cfg(test)]
pub(crate) const RETIRED_FEATURES: [(&str, (u32, u32)); 1] = [("LinkDNSPrefetch", (2, 54))];

/// The settings of every view that shows or edits mail. Script markup is off, so no message
/// script runs; JavaScript itself stays on only for Sealwax's isolated-world helpers. Nothing
/// is stored, played, accelerated or opened, and the [`NETWORK_FEATURES`] are off.
#[allow(
    deprecated,
    reason = "the hardening turns off features WebKitGTK deprecated but still has"
)]
pub(crate) fn hardened_settings(fonts: &WebViewFonts) -> webkit6::Settings {
    let settings = webkit6::Settings::new();
    settings.set_enable_javascript(true);
    settings.set_enable_javascript_markup(false);
    settings.set_auto_load_images(true);
    settings.set_load_icons_ignoring_image_load_setting(false);
    settings.set_hardware_acceleration_policy(webkit6::HardwareAccelerationPolicy::Never);
    settings.set_enable_page_cache(false);
    settings.set_enable_html5_local_storage(false);
    settings.set_enable_html5_database(false);
    settings.set_enable_webgl(false);
    settings.set_enable_webaudio(false);
    settings.set_enable_media(false);
    settings.set_enable_media_stream(false);
    settings.set_enable_mediasource(false);
    settings.set_enable_encrypted_media(false);
    settings.set_enable_media_capabilities(false);
    settings.set_enable_webrtc(false);
    settings.set_enable_developer_extras(cfg!(debug_assertions));
    settings.set_enable_write_console_messages_to_stdout(false);
    settings.set_enable_fullscreen(false);
    settings.set_enable_back_forward_navigation_gestures(false);
    settings.set_enable_site_specific_quirks(false);
    settings.set_javascript_can_open_windows_automatically(false);
    settings.set_javascript_can_access_clipboard(false);
    settings.set_allow_file_access_from_file_urls(false);
    settings.set_allow_universal_access_from_file_urls(false);
    settings.set_allow_top_navigation_to_data_urls(false);
    settings.set_allow_modal_dialogs(false);
    settings.set_print_backgrounds(true);
    // Does nothing since WebKitGTK 2.52 (S2); the feature switches below and the pings filter
    // do the work. It stays off in case a future WebKit honours it again. Hyperlink auditing's
    // setter is left out: it does nothing either and warns at every view.
    settings.set_enable_dns_prefetching(false);
    if let Some(features) = webkit6::Settings::all_features() {
        for index in 0..features.length() {
            if let Some(feature) = features.get(index)
                && feature
                    .identifier()
                    .is_some_and(|id| NETWORK_FEATURES.contains(&id.as_str()))
            {
                settings.set_feature_enabled(&feature, false);
            }
        }
    }
    apply_fonts(&settings, fonts);
    settings
}

/// WebKit's default fonts, set explicitly because it would otherwise pick its own.
pub(crate) fn apply_fonts(settings: &webkit6::Settings, fonts: &WebViewFonts) {
    settings.set_default_font_family(fonts.default_family);
    settings.set_sans_serif_font_family(&fonts.sans_serif);
    settings.set_monospace_font_family(&fonts.monospace);
    settings.set_default_font_size(fonts.default_size);
    settings.set_default_monospace_font_size(fonts.monospace_size);
}

/// A web context with an ephemeral network session: no disk cache beyond a document viewer's,
/// TLS errors fail the load, no cookie is taken, and every download is cancelled (`refused`
/// runs for each).
pub(crate) fn ephemeral_context(
    refused: impl Fn() + 'static,
) -> (webkit6::WebContext, webkit6::NetworkSession) {
    let context = webkit6::WebContext::new();
    context.set_cache_model(webkit6::CacheModel::DocumentViewer);
    let session = webkit6::NetworkSession::new_ephemeral();
    session.set_tls_errors_policy(webkit6::TLSErrorsPolicy::Fail);
    if let Some(cookies) = session.cookie_manager() {
        cookies.set_accept_policy(webkit6::CookieAcceptPolicy::Never);
    }
    session.connect_download_started(move |_, download| {
        download.cancel();
        refused();
    });
    (context, session)
}

/// Compiles `json` as the content filter `id` in `directory`, made private first off the GTK
/// thread.
pub(crate) async fn compile_filter(
    directory: &Path,
    id: &str,
    json: &'static str,
) -> Result<webkit6::UserContentFilter, String> {
    let dir = directory.to_owned();
    let made = gio::spawn_blocking(move || sealwax_infrastructure::xdg::ensure_private_dir(&dir))
        .await
        .map_err(|_| "the filter folder could not be made".to_owned())?;
    made.map_err(|error| error.to_string())?;
    let store = webkit6::UserContentFilterStore::new(&directory.to_string_lossy());
    store
        .save_future(id, &glib::Bytes::from_static(json.as_bytes()))
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gtk4::prelude::*;
    use sealwax_domain::theme::{Mode, Theme, ThemeInputs, ThemeOptions};

    fn fonts() -> WebViewFonts {
        Theme::new(&ThemeInputs::builtin(Mode::Dark), &ThemeOptions::default()).webview_fonts()
    }

    /// WebKit takes the first thread that uses it as its main thread and aborts when another
    /// does, and libtest runs each test on a thread of its own: so the checks that touch WebKit
    /// run in this one test, one after the other.
    #[test]
    fn webkit_checks() {
        the_hardening_reads_back_from_webkit();
        every_network_feature_is_known_to_this_webkit();
    }

    /// What the reader and the composer rely on, read back from WebKit: a setter WebKit stopped
    /// honouring, or a line lost in an edit, shows here.
    fn the_hardening_reads_back_from_webkit() {
        let settings = hardened_settings(&fonts());
        let on = |name: &str| settings.property::<bool>(name);
        for name in ["enable-javascript", "auto-load-images", "print-backgrounds"] {
            assert!(on(name), "{name}");
        }
        for name in [
            "enable-javascript-markup",
            "load-icons-ignoring-image-load-setting",
            "enable-page-cache",
            "enable-html5-local-storage",
            "enable-html5-database",
            "enable-webgl",
            "enable-webaudio",
            "enable-media",
            "enable-media-stream",
            "enable-mediasource",
            "enable-encrypted-media",
            "enable-media-capabilities",
            "enable-webrtc",
            "enable-write-console-messages-to-stdout",
            "enable-fullscreen",
            "enable-back-forward-navigation-gestures",
            "enable-site-specific-quirks",
            "javascript-can-open-windows-automatically",
            "javascript-can-access-clipboard",
            "allow-file-access-from-file-urls",
            "allow-universal-access-from-file-urls",
            "allow-top-navigation-to-data-urls",
            "allow-modal-dialogs",
        ] {
            assert!(!on(name), "{name}");
        }
        assert_eq!(on("enable-developer-extras"), cfg!(debug_assertions));
        assert_eq!(
            settings.hardware_acceleration_policy(),
            webkit6::HardwareAccelerationPolicy::Never
        );
        let features = webkit6::Settings::all_features().expect("WebKit lists its features");
        for index in 0..features.length() {
            let feature = features.get(index).expect("a listed feature");
            if feature
                .identifier()
                .is_some_and(|id| NETWORK_FEATURES.contains(&id.as_str()))
            {
                assert!(
                    !settings.is_feature_enabled(&feature),
                    "{:?}",
                    feature.identifier()
                );
            }
        }
    }

    /// A feature WebKit renamed would silently stay on. One it dropped is known up to the
    /// version that dropped it and not after, so its return shows here too.
    fn every_network_feature_is_known_to_this_webkit() {
        let features = webkit6::Settings::all_features().expect("WebKit lists its features");
        let known: Vec<String> = (0..features.length())
            .filter_map(|index| features.get(index))
            .filter_map(|feature| feature.identifier().map(String::from))
            .collect();
        let version = (
            webkit6::functions::major_version(),
            webkit6::functions::minor_version(),
        );
        for name in NETWORK_FEATURES {
            let expected = RETIRED_FEATURES
                .iter()
                .find(|(retired, _)| *retired == name)
                .is_none_or(|(_, dropped)| version < *dropped);
            assert_eq!(
                known.iter().any(|id| id == name),
                expected,
                "{name} in WebKitGTK {}.{}",
                version.0,
                version.1
            );
        }
        assert!(
            RETIRED_FEATURES
                .iter()
                .all(|(name, _)| NETWORK_FEATURES.contains(name)),
            "a retired feature is still turned off where WebKit has it"
        );
    }

    #[test]
    fn the_flatpak_sandbox_check_reads_the_exit_status() {
        assert_eq!(sandbox_check(&["true"], Duration::from_secs(5)), Ok(()));
        assert!(sandbox_check(&["false"], Duration::from_secs(5)).is_err());
        assert!(sandbox_check(&["/nonexistent/flatpak-spawn"], Duration::from_secs(5)).is_err());
        let started = Instant::now();
        assert!(sandbox_check(&["sleep", "5"], Duration::from_millis(100)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(FLATPAK_SANDBOX_CHECK[..2], ["flatpak-spawn", "--sandbox"]);
    }

    #[test]
    fn mail_goes_into_a_web_view_only_after_a_passed_check() {
        let failed = Err("flatpak-spawn --sandbox failed".to_owned());
        assert!(allowed(Some(&Ok(())), false));
        assert!(!allowed(Some(&failed), false));
        assert!(!allowed(None, false));
        assert!(allowed(Some(&failed), true), "a development build goes on");
    }

    #[test]
    fn outside_a_flatpak_web_views_are_allowed_at_once() {
        if !sealwax_infrastructure::sandbox::is_flatpak() {
            start_flatpak_check();
            assert!(glib::MainContext::default().block_on(web_views_allowed()));
        }
    }
}
