//! Other processes for the self-test, from Stet's session self-test: a second `sealwax` that
//! hands its command line to this instance (`spawn`); Sealwax instances of their own to signal
//! (`spawn-bg`, `signal`, `wait-process`); D-Bus calls to this instance (`bus-open`,
//! `bus-activate`); and a private D-Bus bus whose only service directory holds Sealwax's own test
//! service file, for D-Bus activation (`test-bus-*`).

use super::{Harness, StepResult, WAIT_TIMEOUT, blocking, equal};
use crate::application::APP_ID_ENV;
use crate::config::EXIT_AFTER_ENV;
use gtk4::prelude::*;
use gtk4::{gio, glib};
use std::collections::HashMap;
use std::fs::{self, File};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

/// How long a background Sealwax may take to end once asked.
const PROCESS_TIMEOUT: Duration = Duration::from_secs(60);

/// A background Sealwax quits by itself after this long, should a test lose track of it.
const SAFETY_EXIT_MS: &str = "120000";

const APPLICATION: &str = "org.freedesktop.Application";

/// A private session bus for D-Bus activation (`test-bus-start`).
pub struct TestBus {
    daemon: Child,
    address: String,
    /// The id the activated Sealwax registers.
    app_id: String,
    /// Its configuration, data, state and cache directories.
    home: PathBuf,
    /// Holds the bus socket; removed with the bus.
    socket_dir: PathBuf,
}

impl Drop for TestBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
        let _ = fs::remove_dir_all(&self.socket_dir);
    }
}

impl Harness {
    /// A second `sealwax` with this instance's id, as a user in another terminal would run it:
    /// it forwards its command line here and exits with `expected`.
    pub(super) async fn spawn(&self, args: &[String], expected: i32) -> StepResult {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let cwd = self.root.clone();
        let app_id = self.app_id.clone();
        let args = args.to_vec();
        let output = blocking(move || {
            Command::new(exe)
                .args(&args)
                .current_dir(cwd)
                .env(APP_ID_ENV, app_id)
                .stdin(Stdio::null())
                .output()
        })
        .await?
        .map_err(|error| error.to_string())?;
        if output.status.code() == Some(expected) {
            return Ok(());
        }
        Err(format!(
            "sealwax exited with {}, wanted {expected}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ))
    }

    /// A Sealwax of its own in the background: its own id (`<this id>.<name>`) and folders under
    /// `$TMP/<name>/`, its output in `$TMP/<name>.out` and `.err`.
    pub(super) fn spawn_bg(&self, name: &str, args: &[String]) -> StepResult {
        let app_id = format!("{}.{name}", self.app_id);
        if !gio::Application::id_is_valid(&app_id) {
            return Err(format!("{name} can't be part of an application id"));
        }
        let home = self.root.join(name);
        let mut command = self.command(name, args)?;
        command
            .env(APP_ID_ENV, app_id)
            .env(EXIT_AFTER_ENV, SAFETY_EXIT_MS)
            .envs(xdg_environment(&home));
        let child = command.spawn().map_err(|error| error.to_string())?;
        self.processes.borrow_mut().insert(name.to_owned(), child);
        Ok(())
    }

    /// `sealwax args…` in `$TMP`, its output in `$TMP/<name>.out` and `.err`.
    fn command(&self, name: &str, args: &[String]) -> Result<Command, String> {
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let output = |extension: &str| {
            File::create(self.root.join(format!("{name}.{extension}")))
                .map_err(|error| error.to_string())
        };
        let mut command = Command::new(exe);
        command
            .args(args)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(output("out")?)
            .stderr(output("err")?);
        Ok(command)
    }

    pub(super) fn signal_process(&self, name: &str, signal: &str) -> StepResult {
        let mut processes = self.processes.borrow_mut();
        let child = processes
            .get_mut(name)
            .ok_or_else(|| format!("no process {name}"))?;
        send_signal(child.id(), signal)
    }

    /// Waits for a background process to end with `wanted`: an exit status, `killed`
    /// (SIGKILL), `signal` (any signal) or `any`.
    pub(super) async fn wait_process(&self, name: &str, wanted: &str) -> StepResult {
        let started = Instant::now();
        let status = loop {
            let status = {
                let mut processes = self.processes.borrow_mut();
                let child = processes
                    .get_mut(name)
                    .ok_or_else(|| format!("no process {name}"))?;
                child.try_wait().map_err(|error| error.to_string())?
            };
            if let Some(status) = status {
                self.processes.borrow_mut().remove(name);
                break status;
            }
            if started.elapsed() > PROCESS_TIMEOUT {
                return Err(format!("{name} is still running after {PROCESS_TIMEOUT:?}"));
            }
            glib::timeout_future(Duration::from_millis(20)).await;
        };
        println!(
            "  # {name} ended after {:.2} s: {status}",
            started.elapsed().as_secs_f64()
        );
        if status_matches(status, wanted) {
            return Ok(());
        }
        let mut message = format!("{name} ended with {status}, wanted {wanted}");
        if let Ok(text) = fs::read_to_string(self.root.join(format!("{name}.err"))) {
            for line in text.lines().rev().take(8) {
                message.push('\n');
                message.push_str(line);
            }
        }
        Err(message)
    }

    pub(super) fn running(&self, name: &str) -> Result<bool, String> {
        let mut processes = self.processes.borrow_mut();
        let child = processes
            .get_mut(name)
            .ok_or_else(|| format!("no process {name}"))?;
        Ok(child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_none())
    }

    /// `org.freedesktop.Application.Open` (with `uri`) or `Activate` on this instance, as a
    /// launcher calls it for a `DBusActivatable` app.
    pub(super) async fn bus_call(&self, uri: Option<&str>) -> StepResult {
        let connection = self
            .app
            .dbus_connection()
            .ok_or("the application has no D-Bus connection")?;
        let (method, parameters) = application_call(uri);
        connection
            .call_future(
                Some(&self.app_id),
                &object_path(&self.app_id),
                APPLICATION,
                method,
                Some(&parameters),
                None,
                gio::DBusCallFlags::NONE,
                5000,
            )
            .await
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Starts a private `dbus-daemon` whose only service directory is `$TMP/testbus/services`,
    /// holding one service file for the id `<this id>.Activated` that runs this executable with
    /// `--gapplication-service`, as the installed `.service` file does. That Sealwax gets
    /// folders of its own under `$TMP/activated`; the bus's and its output go to
    /// `$TMP/testbus/daemon.out` and `daemon.err`.
    pub(super) async fn start_test_bus(&self) -> StepResult {
        if self.bus.borrow().is_some() {
            return Err("the test bus runs already".to_owned());
        }
        let dir = self.root.join("testbus");
        let services = dir.join("services");
        let home = self.root.join("activated");
        let _ = fs::remove_dir_all(&services);
        fs::create_dir_all(&services).map_err(|error| error.to_string())?;
        // A socket path must be short (108 bytes), so it goes in a directory of its own.
        let socket_dir = std::env::temp_dir().join(format!("sealwax-bus-{}", std::process::id()));
        fs::create_dir_all(&socket_dir).map_err(|error| error.to_string())?;
        let socket = socket_dir.join("socket");
        let _ = fs::remove_file(&socket);
        let address = format!("unix:path={}", socket.display());
        let app_id = format!("{}.Activated", self.app_id);
        let exe = std::env::current_exe().map_err(|error| error.to_string())?;
        let environment: Vec<String> = [
            ("DBUS_SESSION_BUS_ADDRESS".to_owned(), address.clone()),
            (APP_ID_ENV.to_owned(), app_id.clone()),
            (EXIT_AFTER_ENV.to_owned(), SAFETY_EXIT_MS.to_owned()),
        ]
        .into_iter()
        .chain(
            xdg_environment(&home)
                .into_iter()
                .map(|(name, value)| (name.to_owned(), value)),
        )
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
        write(
            &services.join(format!("{app_id}.service")),
            &format!(
                "[D-BUS Service]\nName={app_id}\nExec=/usr/bin/env {} {} --gapplication-service\n",
                environment.join(" "),
                exe.display(),
            ),
        )?;
        let config = dir.join("bus.conf");
        write(
            &config,
            &format!(
                "<!DOCTYPE busconfig PUBLIC \"-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN\"\n \
                 \"http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd\">\n\
                 <!-- Sealwax's activation self-test: its one service directory holds only \
                 Sealwax's own test service file. -->\n\
                 <busconfig>\n  <type>session</type>\n  <keep_umask/>\n  <listen>{address}</listen>\n  \
                 <auth>EXTERNAL</auth>\n  <servicedir>{}</servicedir>\n  \
                 <policy context=\"default\">\n    <allow send_destination=\"*\" eavesdrop=\"true\"/>\n    \
                 <allow eavesdrop=\"true\"/>\n    <allow own=\"*\"/>\n  </policy>\n</busconfig>\n",
                services.display()
            ),
        )?;
        let out = File::create(dir.join("daemon.out")).map_err(|error| error.to_string())?;
        let err = File::create(dir.join("daemon.err")).map_err(|error| error.to_string())?;
        let daemon = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .arg("--nofork")
            .arg("--nopidfile")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err)
            .spawn()
            .map_err(|error| format!("dbus-daemon: {error}"))?;
        let bus = TestBus {
            daemon,
            address,
            app_id,
            home,
            socket_dir,
        };
        let started = Instant::now();
        while !socket.exists() {
            if started.elapsed() > WAIT_TIMEOUT {
                return Err("the test bus did not start".to_owned());
            }
            glib::timeout_future(Duration::from_millis(10)).await;
        }
        self.bus.replace(Some(bus));
        Ok(())
    }

    /// `sealwax args…` on the test bus with the activated id, as a terminal there would run it.
    pub(super) fn test_bus_spawn(&self, name: &str, args: &[String]) -> StepResult {
        let mut command = self.command(name, args)?;
        {
            let bus = self.bus.borrow();
            let bus = bus.as_ref().ok_or("run test-bus-start first")?;
            command
                .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
                .env(APP_ID_ENV, &bus.app_id)
                .envs(xdg_environment(&bus.home.join(name)));
        }
        let child = command.spawn().map_err(|error| error.to_string())?;
        self.processes.borrow_mut().insert(name.to_owned(), child);
        Ok(())
    }

    fn test_bus(&self) -> Result<(String, String), String> {
        let bus = self.bus.borrow();
        let bus = bus.as_ref().ok_or("run test-bus-start first")?;
        Ok((bus.address.clone(), bus.app_id.clone()))
    }

    /// `Open` (with `uri`) or `Activate` on the activated id, over the test bus; the bus starts
    /// the service first when it isn't running.
    pub(super) async fn test_bus_call(&self, uri: Option<&str>) -> StepResult {
        let (address, app_id) = self.test_bus()?;
        let uri = uri.map(str::to_owned);
        blocking(move || -> StepResult {
            let (method, parameters) = application_call(uri.as_deref());
            connect(&address)?
                .call_sync(
                    Some(&app_id),
                    &object_path(&app_id),
                    APPLICATION,
                    method,
                    Some(&parameters),
                    None,
                    gio::DBusCallFlags::NONE,
                    15000,
                    gio::Cancellable::NONE,
                )
                .map(|_| ())
                .map_err(|error| error.to_string())
        })
        .await?
    }

    /// Sends `signal` to the Sealwax that owns the activated id on the test bus.
    pub(super) async fn test_bus_signal(&self, signal: &str) -> StepResult {
        let (address, app_id) = self.test_bus()?;
        let pid = blocking(move || owner_pid(&address, &app_id))
            .await?
            .ok_or("no Sealwax owns the activated id")?;
        send_signal(pid, signal)
    }

    /// Whether the activated id has an owner on the test bus.
    pub(super) async fn test_bus_owned(&self) -> Result<bool, String> {
        let (address, app_id) = self.test_bus()?;
        blocking(move || connect(&address).map(|connection| has_owner(&connection, &app_id)))
            .await?
    }

    /// Stops the test bus; a Sealwax it started that still runs is stopped first, and fails the
    /// step.
    pub(super) async fn stop_test_bus(&self) -> StepResult {
        let Some(bus) = self.bus.take() else {
            return Ok(());
        };
        let (address, app_id) = (bus.address.clone(), bus.app_id.clone());
        let leftover = blocking(move || owner_pid(&address, &app_id))
            .await
            .ok()
            .flatten();
        if let Some(pid) = leftover {
            println!("  # stopping the activated Sealwax (process {pid}) that still runs");
            let _ = send_signal(pid, "KILL");
        }
        drop(bus);
        match leftover {
            Some(pid) => Err(format!(
                "the activated Sealwax (process {pid}) was still running"
            )),
            None => Ok(()),
        }
    }

    /// Stops what the script left running: background processes and the test bus.
    pub(super) async fn stop_processes(&self) {
        let mut processes = std::mem::take(&mut *self.processes.borrow_mut());
        for (name, child) in processes.iter_mut() {
            if let Ok(None) = child.try_wait() {
                println!("  # stopping {name}, which still runs");
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        let _ = self.stop_test_bus().await;
    }

    pub(super) async fn check_bus_owner(&self, wanted: bool) -> StepResult {
        equal(
            "activated Sealwax on the test bus",
            self.test_bus_owned().await?,
            wanted,
        )
    }
}

/// The method and arguments of an `org.freedesktop.Application` call: `Open` for a URI,
/// `Activate` otherwise; no platform data.
fn application_call(uri: Option<&str>) -> (&'static str, glib::Variant) {
    let platform: HashMap<String, glib::Variant> = HashMap::new();
    match uri {
        Some(uri) => ("Open", (vec![uri.to_owned()], platform).to_variant()),
        None => ("Activate", (platform,).to_variant()),
    }
}

/// The object path GApplication exports `app_id` at.
fn object_path(app_id: &str) -> String {
    format!("/{}", app_id.replace('.', "/").replace('-', "_"))
}

/// Configuration, data, state and cache folders of their own under `home`, so another Sealwax
/// never touches the user's.
fn xdg_environment(home: &Path) -> Vec<(&'static str, String)> {
    let dir = |name: &str| home.join(name).to_string_lossy().into_owned();
    vec![
        ("XDG_CONFIG_HOME", dir("config")),
        ("XDG_DATA_HOME", dir("data")),
        ("XDG_STATE_HOME", dir("state")),
        ("XDG_CACHE_HOME", dir("cache")),
    ]
}

fn write(path: &Path, text: &str) -> StepResult {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    fs::write(path, text).map_err(|error| error.to_string())
}

fn connect(address: &str) -> Result<gio::DBusConnection, String> {
    gio::DBusConnection::for_address_sync(
        address,
        gio::DBusConnectionFlags::AUTHENTICATION_CLIENT
            | gio::DBusConnectionFlags::MESSAGE_BUS_CONNECTION,
        None,
        gio::Cancellable::NONE,
    )
    .map_err(|error| error.to_string())
}

fn has_owner(connection: &gio::DBusConnection, name: &str) -> bool {
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&(name,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.child_value(0).get::<bool>())
        .unwrap_or(false)
}

fn owner_pid(address: &str, name: &str) -> Option<u32> {
    connect(address)
        .ok()?
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            Some(&(name,).to_variant()),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .ok()?
        .child_value(0)
        .get::<u32>()
}

fn send_signal(pid: u32, signal: &str) -> StepResult {
    signal_number(signal)?;
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("kill -{signal} {pid} failed: {status}"))
    }
}

pub(super) fn signal_number(name: &str) -> Result<i32, String> {
    match name {
        "TERM" => Ok(15),
        "HUP" => Ok(1),
        "INT" => Ok(2),
        "KILL" => Ok(9),
        other => Err(format!("no signal {other}; use TERM, HUP, INT or KILL")),
    }
}

fn status_matches(status: ExitStatus, wanted: &str) -> bool {
    match wanted {
        "any" => true,
        "killed" => status.signal() == Some(9),
        "signal" => status.signal().is_some(),
        code => code
            .parse::<i32>()
            .is_ok_and(|code| status.code() == Some(code)),
    }
}
