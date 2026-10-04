use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Protocol {
    Imap,
    Pop3,
    Smtp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Security {
    Tls,
    StartTls,
    Plain,
}

impl Security {
    pub fn is_tls(self) -> bool {
        self != Security::Plain
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Username {
    Address,
    LocalPart,
    Template(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    Password,
    PasswordEncrypted,
    OAuth2,
    Other(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Server {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub security: Security,
    pub username: Username,
    pub auth: Vec<Auth>,
}

impl Server {
    pub fn new(protocol: Protocol, host: &str, port: u16, security: Security) -> Self {
        Self {
            protocol,
            host: host.to_ascii_lowercase(),
            port,
            security,
            username: Username::Address,
            auth: Vec::new(),
        }
    }

    pub fn with_username(mut self, username: Username) -> Self {
        self.username = username;
        self
    }

    pub fn with_auth(mut self, auth: &[Auth]) -> Self {
        self.auth = auth.to_vec();
        self
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Source {
    Preset,
    Autoconfig,
    WellKnown,
    Srv,
    Ispdb,
    MxPreset,
    MxIspdb,
    Probe,
}

impl Source {
    pub const ALL: [Source; 8] = [
        Source::Preset,
        Source::Autoconfig,
        Source::WellKnown,
        Source::Srv,
        Source::Ispdb,
        Source::MxPreset,
        Source::MxIspdb,
        Source::Probe,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Source::Preset => "preset",
            Source::Autoconfig => "autoconfig",
            Source::WellKnown => "well-known",
            Source::Srv => "srv",
            Source::Ispdb => "ispdb",
            Source::MxPreset => "mx-preset",
            Source::MxIspdb => "mx-ispdb",
            Source::Probe => "probe",
        }
    }
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    BridgeOnly,
    NoImap,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Hints {
    pub provider: Option<String>,
    pub oauth: Option<String>,
    pub app_password: bool,
    pub enable_imap: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Finding {
    Servers {
        servers: Vec<Server>,
        hints: Hints,
    },
    Unsupported {
        provider: &'static str,
        why: Unsupported,
    },
    NoMail(&'static str),
    Nothing,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub source: Source,
    pub elapsed_ms: f64,
    pub result: Result<Finding, String>,
    pub detail: String,
}

impl Report {
    pub fn finding(&self) -> Option<&Finding> {
        self.result.as_ref().ok()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub server: Server,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Outcome {
    Settings {
        imap: Option<Box<Choice>>,
        smtp: Option<Box<Choice>>,
        hints: Hints,
    },
    Unsupported {
        provider: &'static str,
        why: Unsupported,
        source: Source,
    },
    NoMail {
        why: &'static str,
        source: Source,
    },
}

impl Outcome {
    pub fn is_usable(&self) -> bool {
        match self {
            Outcome::Settings { imap, smtp, .. } => imap.is_some() && smtp.is_some(),
            Outcome::Unsupported { .. } | Outcome::NoMail { .. } => true,
        }
    }

    pub fn winners(&self) -> String {
        match self {
            Outcome::Settings { imap, smtp, .. } => {
                let names: Vec<&str> = [imap, smtp]
                    .iter()
                    .map(|choice| choice.as_ref().map_or("-", |c| c.source.name()))
                    .collect();
                if names[0] == names[1] {
                    names[0].to_string()
                } else {
                    format!("{}+{}", names[0], names[1])
                }
            }
            Outcome::Unsupported { source, .. } | Outcome::NoMail { source, .. } => {
                source.name().to_string()
            }
        }
    }
}

impl fmt::Display for Security {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Security::Tls => "TLS",
            Security::StartTls => "STARTTLS",
            Security::Plain => "plain",
        })
    }
}

impl fmt::Display for Username {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Username::Address => f.write_str("address"),
            Username::LocalPart => f.write_str("local-part"),
            Username::Template(t) => write!(f, "other({t})"),
        }
    }
}

impl fmt::Display for Server {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{} {} user={}",
            self.host, self.port, self.security, self.username
        )
    }
}
