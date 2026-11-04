//! Starting the app as a child process and reading its event lines.

use anyhow::{Context, Result, bail};
use email_spike_s6::clock::mono_ns;
use email_spike_s6::{APP_ID, env, events, paths};
use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// True inside `tools/headless.sh` (Broadway display, private session bus).
pub fn headless() -> bool {
    std::env::var("GDK_BACKEND").is_ok_and(|backend| backend == "broadway")
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
}

#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub env: Vec<(String, String)>,
    pub unset: Vec<String>,
    pub args: Vec<String>,
    pub binary: Option<PathBuf>,
    pub stderr_log: Option<PathBuf>,
}

impl Launch {
    pub fn mode(mode: &str) -> Self {
        let mut launch = Self::default();
        launch.set(env::MODE, mode);
        launch
    }

    pub fn set(&mut self, key: &str, value: impl Into<String>) -> &mut Self {
        self.env.retain(|(name, _)| name != key);
        self.env.push((key.to_owned(), value.into()));
        self
    }
}

pub struct Running {
    pub child: Child,
    pub spawned: i64,
    receiver: mpsc::Receiver<(String, Value)>,
    pub events: Vec<(String, Value)>,
}

/// Scratch XDG folders for live runs; headless runs keep the wrapper's.
fn xdg_env(command: &mut Command) -> Result<()> {
    if headless() {
        return Ok(());
    }
    let root = paths::live_xdg();
    for (name, dir) in [
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("XDG_DATA_HOME", "data"),
        ("XDG_STATE_HOME", "state"),
    ] {
        let path = root.join(dir);
        std::fs::create_dir_all(&path)?;
        command.env(name, path);
    }
    Ok(())
}

pub fn spawn(launch: &Launch) -> Result<Running> {
    let binary = launch.binary.clone().unwrap_or_else(paths::app_binary);
    let mut command = Command::new(&binary);
    command.args(&launch.args);
    xdg_env(&mut command)?;
    for name in &launch.unset {
        command.env_remove(name);
    }
    for (name, value) in &launch.env {
        command.env(name, value);
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped());
    match &launch.stderr_log {
        Some(path) => {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            command.stderr(file);
        }
        None => {
            command.stderr(Stdio::inherit());
        }
    }
    let spawned = mono_ns();
    command.env(env::EXEC_NS, spawned.to_string());
    let mut child = command
        .spawn()
        .with_context(|| format!("start {}", binary.display()))?;
    let stdout = child.stdout.take().context("no stdout")?;
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(event) = events::parse(&line) {
                if sender.send(event).is_err() {
                    return;
                }
            } else if !line.trim().is_empty() {
                eprintln!("app: {line}");
            }
        }
    });
    Ok(Running {
        child,
        spawned,
        receiver,
        events: Vec::new(),
    })
}

impl Running {
    /// The next event named `name` (others are kept in `events`).
    pub fn wait(&mut self, name: &str, timeout: Duration) -> Option<Value> {
        self.wait_for(timeout, |event, _| event == name)
    }

    pub fn wait_for(
        &mut self,
        timeout: Duration,
        matches: impl Fn(&str, &Value) -> bool,
    ) -> Option<Value> {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.receiver.recv_timeout(left) {
                Ok((event, value)) => {
                    let hit = matches(&event, &value);
                    self.events.push((event, value.clone()));
                    if hit {
                        return Some(value);
                    }
                }
                Err(_) => return None,
            }
        }
    }

    /// An event already received, or the next one named `name`.
    pub fn seen_or_wait(&mut self, name: &str, timeout: Duration) -> Option<Value> {
        self.drain();
        if let Some(value) = self.find(name).next() {
            return Some(value.clone());
        }
        self.wait(name, timeout)
    }

    /// An event already received that matches, or the next one that does.
    pub fn seen_or_wait_for(
        &mut self,
        timeout: Duration,
        matches: impl Fn(&str, &Value) -> bool,
    ) -> Option<Value> {
        self.drain();
        if let Some((_, value)) = self
            .events
            .iter()
            .find(|(event, value)| matches(event, value))
        {
            return Some(value.clone());
        }
        self.wait_for(timeout, matches)
    }

    /// Everything that arrived so far without waiting.
    pub fn drain(&mut self) {
        while let Ok(event) = self.receiver.try_recv() {
            self.events.push(event);
        }
    }

    pub fn find(&self, name: &str) -> impl Iterator<Item = &Value> {
        self.events
            .iter()
            .filter(move |(event, _)| event == name)
            .map(|(_, value)| value)
    }

    pub fn wait_exit(&mut self, timeout: Duration) -> Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait()? {
                std::thread::sleep(Duration::from_millis(30));
                self.drain();
                return Ok(status);
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let status = self.child.wait()?;
                self.drain();
                bail!("the app did not exit in time and was killed ({status})");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Whether some process owns the spike's application id on this session bus.
pub fn app_running() -> bool {
    let Ok(connection) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return false;
    };
    connection
        .call_sync(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&gio::glib::Variant::tuple_from_iter([
                gio::prelude::ToVariant::to_variant(APP_ID),
            ])),
            None,
            gio::DBusCallFlags::NONE,
            2000,
            gio::Cancellable::NONE,
        )
        .ok()
        .and_then(|reply| reply.get::<(bool,)>())
        .is_some_and(|(owned,)| owned)
}

pub fn ensure_not_running() -> Result<()> {
    for _ in 0..100 {
        if !app_running() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("another {APP_ID} instance owns the bus name; stop it first")
}

/// Activates one of the app's actions over D-Bus (`org.gtk.Actions.Activate`).
pub fn activate_action(name: &str) -> Result<i64> {
    use gio::glib::{Variant, VariantDict, VariantTy};
    use gio::prelude::ToVariant;
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    let parameters = Variant::tuple_from_iter([
        name.to_variant(),
        Variant::array_from_iter_with_type(VariantTy::VARIANT, [] as [Variant; 0]),
        VariantDict::new(None).end(),
    ]);
    let sent = mono_ns();
    connection.call_sync(
        Some(APP_ID),
        email_spike_s6::APP_PATH,
        "org.gtk.Actions",
        "Activate",
        Some(&parameters),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        5000,
        gio::Cancellable::NONE,
    )?;
    Ok(sent)
}

/// `org.freedesktop.Application.Open`, as a launcher does for a D-Bus activatable app.
pub fn dbus_open(uri: &str) -> Result<(i64, i64)> {
    dbus_open_from(uri, mono_ns())
}

/// The same, reporting `origin` as the start time the app measures from.
pub fn dbus_open_from(uri: &str, origin: i64) -> Result<(i64, i64)> {
    use gio::glib::{Variant, VariantDict};
    use gio::prelude::ToVariant;
    let connection = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
    let platform = VariantDict::new(None);
    platform.insert_value("s6-sent-ns", &origin.to_variant());
    let parameters = Variant::tuple_from_iter([vec![uri.to_owned()].to_variant(), platform.end()]);
    connection.call_sync(
        Some(APP_ID),
        email_spike_s6::APP_PATH,
        "org.freedesktop.Application",
        "Open",
        Some(&parameters),
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        5000,
        gio::Cancellable::NONE,
    )?;
    Ok((origin, mono_ns()))
}
