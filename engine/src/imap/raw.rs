//! The mandatory raw-command layer over async-imap (ADR-006 S1 amendment). async-imap's typed
//! calls for FETCH, STORE, SEARCH, LIST, EXPUNGE, NOOP, CAPABILITY and ID stop at the tagged
//! completion without checking it, so a refused command reads as an empty success, and they
//! drop untagged responses past a 100-entry buffer. Every command goes through here instead:
//! it is sent with `run_command`, every response is read by us with a timeout and counted
//! against the command's budget, every untagged response reaches the caller, and every
//! completion is checked for OK, NO and BAD. A tagged OK's COPYUID (RFC 4315) or READ-ONLY
//! reaches the caller too.

use super::error::{ImapError, ServerText};
use async_imap::imap_proto::rfc4315::UidSetMember;
use async_imap::imap_proto::{Outcome, RequestId, Response, ResponseCode, Status};
use std::time::Duration;

pub(crate) type Session = async_imap::Session<super::Stream>;

/// How long any one read may wait for the server.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

const MIB: u64 = 1024 * 1024;

/// How much one command may read: far more than any honest answer to a command Sealwax sends,
/// so a server that never ends its responses, each one well formed and in time, can't hold the
/// connection or fill memory with what the caller collects from them. Past either limit the
/// command ends with [`ImapError::TooLong`], and with it the connection.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Budget {
    /// Responses, the completion included.
    responses: u64,
    /// Bytes read from the server while the command ran.
    bytes: u64,
}

impl Budget {
    /// Any command but a body FETCH. The largest honest answer is a full flag scan: a folder of
    /// a million messages answers with a million responses of about 100 MB.
    pub(crate) const COMMAND: Budget = Budget {
        responses: 2_000_000,
        bytes: 256 * MIB,
    };
    /// A FETCH of bodies or an attachment, one of whose responses may be as large as async-imap
    /// reads one (512 MiB).
    pub(crate) const BODIES: Budget = Budget {
        bytes: 1024 * MIB,
        ..Budget::COMMAND
    };
}

/// The bytes the session has read from the server.
fn read(session: &mut Session) -> u64 {
    let stream: &mut super::Stream = session.as_mut();
    stream.read()
}

/// What a command has read against its budget.
struct Spent {
    budget: Budget,
    /// The session's bytes read when the command was sent.
    from: u64,
    responses: u64,
}

impl Spent {
    fn new(session: &mut Session, budget: Budget) -> Self {
        Spent {
            budget,
            from: read(session),
            responses: 0,
        }
    }

    /// Counts the response just read.
    fn count(&mut self, session: &mut Session, command: &'static str) -> Result<(), ImapError> {
        self.responses += 1;
        let bytes = read(session) - self.from;
        if self.responses > self.budget.responses || bytes > self.budget.bytes {
            return Err(ImapError::TooLong(command));
        }
        Ok(())
    }
}

/// The commands Sealwax sends, by the words that name them.
const VERBS: [&str; 28] = [
    "UID FETCH",
    "UID SEARCH",
    "UID STORE",
    "UID COPY",
    "UID MOVE",
    "UID EXPUNGE",
    "FETCH",
    "SEARCH",
    "STORE",
    "EXPUNGE",
    "EXAMINE",
    "SELECT",
    "UNSELECT",
    "CLOSE",
    "STATUS",
    "LIST",
    "CREATE",
    "DELETE",
    "RENAME",
    "SUBSCRIBE",
    "APPEND",
    "NOOP",
    "IDLE",
    "ENABLE",
    "ID",
    "GETQUOTAROOT",
    "CAPABILITY",
    "LOGOUT",
];

/// The verb of `command` for an error, without its arguments (folder names, UIDs).
pub(crate) fn verb(command: &str) -> &'static str {
    VERBS
        .into_iter()
        .find(|verb| {
            command
                .get(..verb.len())
                .is_some_and(|start| start.eq_ignore_ascii_case(verb))
                && command[verb.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == ' ')
        })
        .unwrap_or("a command")
}

/// Reads one response to `command` (its verb), counts it against the command's budget and hands
/// it to `f`. async-imap's `ResponseData` can't be named outside the crate, so it never leaves
/// this function.
async fn next<R>(
    session: &mut Session,
    command: &'static str,
    spent: &mut Spent,
    f: impl FnOnce(&Response<'_>) -> R,
) -> Result<R, ImapError> {
    let response = match tokio::time::timeout(READ_TIMEOUT, session.read_response()).await {
        Err(_) => return Err(ImapError::Timeout),
        Ok(Err(error)) => return Err(ImapError::reading(error, command)),
        Ok(Ok(None)) => return Err(ImapError::ConnectionLost),
        Ok(Ok(Some(response))) => response,
    };
    spent.count(session, command)?;
    Ok(f(response.parsed()))
}

/// The server's text with its response code in front (`[TRYCREATE] …`): imap-proto parses some
/// codes out of the text and leaves the others (RFC 5530's) in it, so classifying a refusal sees
/// every code the same way ([`ServerText::code`]).
fn text(outcome: &Outcome<'_>) -> ServerText {
    let code = match outcome.code {
        Some(ResponseCode::TryCreate) => "[TRYCREATE] ",
        Some(ResponseCode::ReadOnly) => "[READ-ONLY] ",
        Some(ResponseCode::Alert) => "[ALERT] ",
        Some(ResponseCode::Parse) => "[PARSE] ",
        _ => "",
    };
    let information = outcome.information.as_deref().unwrap_or_default();
    ServerText(format!("{code}{information}"))
}

fn completion(status: &Status, outcome: &Outcome<'_>) -> Result<(), ImapError> {
    match status {
        Status::Ok => Ok(()),
        Status::No => Err(ImapError::No(text(outcome))),
        Status::Bad => Err(ImapError::Bad(text(outcome))),
        _ => Err(ImapError::Protocol("unexpected completion status")),
    }
}

/// COPYUID (RFC 4315): the target's UIDVALIDITY and each copied message's source and target UID,
/// pairwise in the server's order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CopyUid {
    pub uidvalidity: u32,
    pub pairs: Vec<(u32, u32)>,
}

/// UIDs a COPYUID may name: far more than any command Sealwax sends, so a hostile set can't make
/// it allocate gigabytes.
const COPYUID_LIMIT: u64 = 100_000;

fn expand(set: &[UidSetMember]) -> Option<Vec<u32>> {
    let size: u64 = set
        .iter()
        .map(|member| match member {
            UidSetMember::Uid(_) => 1,
            UidSetMember::UidRange(range) => u64::from(range.end() - range.start()) + 1,
        })
        .sum();
    if size > COPYUID_LIMIT {
        return None;
    }
    let mut uids = Vec::new();
    for member in set {
        match member {
            UidSetMember::Uid(uid) => uids.push(*uid),
            UidSetMember::UidRange(range) => uids.extend(range.clone()),
        }
    }
    Some(uids)
}

/// The COPYUID in a response code; `None` when there is none or its two sets don't pair up.
pub(crate) fn copy_uid(code: Option<&ResponseCode<'_>>) -> Option<CopyUid> {
    let Some(ResponseCode::CopyUid(uidvalidity, source, target)) = code else {
        return None;
    };
    let (source, target) = (expand(source)?, expand(target)?);
    (source.len() == target.len()).then(|| CopyUid {
        uidvalidity: *uidvalidity,
        pairs: source.into_iter().zip(target).collect(),
    })
}

/// What a tagged OK's response code says, where Sealwax reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Code {
    CopyUid(CopyUid),
    ReadOnly,
    /// APPENDUID (RFC 4315): the target's UIDVALIDITY and the appended message's UID.
    AppendUid {
        uidvalidity: u32,
        uid: u32,
    },
}

fn code(outcome: &Outcome<'_>) -> Option<Code> {
    match &outcome.code {
        Some(ResponseCode::ReadOnly) => Some(Code::ReadOnly),
        Some(ResponseCode::AppendUid(uidvalidity, set)) => match set.as_slice() {
            [UidSetMember::Uid(uid)] => Some(Code::AppendUid {
                uidvalidity: *uidvalidity,
                uid: *uid,
            }),
            _ => None,
        },
        _ => copy_uid(outcome.code.as_ref()).map(Code::CopyUid),
    }
}

enum Step {
    Done(Result<Option<Code>, ImapError>),
    More,
}

/// Reads until the tagged completion of `id`, handing every other response to `on_untagged`, and
/// returns what the tagged OK's response code says.
async fn finish<F>(
    session: &mut Session,
    id: &RequestId,
    command: &'static str,
    spent: &mut Spent,
    mut on_untagged: F,
) -> Result<Option<Code>, ImapError>
where
    F: FnMut(&Response<'_>),
{
    loop {
        let step = next(session, command, spent, |response| match response {
            Response::Done {
                tag,
                status,
                outcome,
            } if tag == id => Step::Done(completion(status, outcome).map(|()| code(outcome))),
            Response::Done { .. } => {
                Step::Done(Err(ImapError::Protocol("a completion for another command")))
            }
            Response::Data {
                status: Status::Bye,
                outcome,
            } => Step::Done(Err(ImapError::Bye(text(outcome)))),
            other => {
                on_untagged(other);
                Step::More
            }
        })
        .await?;
        if let Step::Done(result) = step {
            return result;
        }
    }
}

/// Runs one command and reads its responses, within [`Budget::COMMAND`].
pub(crate) async fn run<F>(
    session: &mut Session,
    command: &str,
    on_untagged: F,
) -> Result<(), ImapError>
where
    F: FnMut(&Response<'_>),
{
    run_within(session, command, Budget::COMMAND, on_untagged).await
}

/// [`run`] within `budget`.
pub(crate) async fn run_within<F>(
    session: &mut Session,
    command: &str,
    budget: Budget,
    on_untagged: F,
) -> Result<(), ImapError>
where
    F: FnMut(&Response<'_>),
{
    coded(session, command, budget, on_untagged).await.map(drop)
}

/// [`run`], returning what the tagged OK's response code says (COPYUID, READ-ONLY).
pub(crate) async fn run_coded<F>(
    session: &mut Session,
    command: &str,
    on_untagged: F,
) -> Result<Option<Code>, ImapError>
where
    F: FnMut(&Response<'_>),
{
    coded(session, command, Budget::COMMAND, on_untagged).await
}

async fn coded<F>(
    session: &mut Session,
    command: &str,
    budget: Budget,
    on_untagged: F,
) -> Result<Option<Code>, ImapError>
where
    F: FnMut(&Response<'_>),
{
    let id = session.run_command(command).await?;
    let mut spent = Spent::new(session, budget);
    finish(session, &id, verb(command), &mut spent, on_untagged).await
}

/// What the server said before a literal's data.
enum Ahead {
    Go,
    Done(Result<Option<Code>, ImapError>),
    More,
}

/// The slowest a literal may go out before its write times out: 16 KiB a second, and never
/// less than [`READ_TIMEOUT`].
fn write_timeout(bytes: usize) -> Duration {
    READ_TIMEOUT.max(Duration::from_secs((bytes / 16_384) as u64))
}

/// Runs `command`, which ends in a synchronizing literal (`{n}`), with `literal` as its data:
/// waits for the continuation, sends the bytes and the line's end, and reads the completion. A
/// server that refuses before the continuation (too large, over quota) answers here, and
/// nothing of the literal is sent.
pub(crate) async fn run_literal<F>(
    session: &mut Session,
    command: &str,
    literal: &[u8],
    mut on_untagged: F,
) -> Result<Option<Code>, ImapError>
where
    F: FnMut(&Response<'_>),
{
    use tokio::io::AsyncWriteExt;
    let id = session.run_command(command).await?;
    let verb = verb(command);
    let mut spent = Spent::new(session, Budget::COMMAND);
    loop {
        let ahead = next(session, verb, &mut spent, |response| match response {
            Response::Continue { .. } => Ahead::Go,
            Response::Done {
                tag,
                status,
                outcome,
            } if *tag == id => Ahead::Done(match completion(status, outcome) {
                Ok(()) => Err(ImapError::Protocol("a literal completed before its data")),
                Err(error) => Err(error),
            }),
            Response::Done { .. } => {
                Ahead::Done(Err(ImapError::Protocol("a completion for another command")))
            }
            Response::Data {
                status: Status::Bye,
                outcome,
            } => Ahead::Done(Err(ImapError::Bye(text(outcome)))),
            other => {
                on_untagged(other);
                Ahead::More
            }
        })
        .await?;
        match ahead {
            Ahead::Go => break,
            Ahead::Done(result) => return result,
            Ahead::More => {}
        }
    }
    let stream: &mut super::Stream = session.as_mut();
    let sent = async {
        stream.write_all(literal).await?;
        stream.write_all(b"\r\n").await?;
        stream.flush().await
    };
    tokio::time::timeout(write_timeout(literal.len()), sent)
        .await
        .map_err(|_| ImapError::Timeout)??;
    finish(session, &id, verb, &mut spent, on_untagged).await
}

/// Sends several commands without waiting and reads until every one has completed, all of them
/// within one [`Budget::COMMAND`]. Replies may come in any order (Stalwart runs pipelined
/// commands concurrently); the first failure is returned once all completions are read.
pub(crate) async fn run_pipelined<F>(
    session: &mut Session,
    commands: &[String],
    on_untagged: F,
) -> Result<(), ImapError>
where
    F: FnMut(&Response<'_>),
{
    pipelined(session, commands, Budget::COMMAND, on_untagged).await
}

async fn pipelined<F>(
    session: &mut Session,
    commands: &[String],
    budget: Budget,
    mut on_untagged: F,
) -> Result<(), ImapError>
where
    F: FnMut(&Response<'_>),
{
    let mut pending = Vec::with_capacity(commands.len());
    for command in commands {
        pending.push(session.run_command(command).await?);
    }
    let verb = commands
        .first()
        .map_or("a command", |command| verb(command));
    let mut spent = Spent::new(session, budget);
    let mut failure = None;
    while !pending.is_empty() {
        let done = next(session, verb, &mut spent, |response| match response {
            Response::Done {
                tag,
                status,
                outcome,
            } => Some(Ok((tag.clone(), completion(status, outcome)))),
            Response::Data {
                status: Status::Bye,
                outcome,
            } => Some(Err(ImapError::Bye(text(outcome)))),
            other => {
                on_untagged(other);
                None
            }
        })
        .await?;
        match done {
            None => {}
            Some(Err(bye)) => return Err(bye),
            Some(Ok((tag, result))) => {
                if !pending.contains(&tag) {
                    return Err(ImapError::Protocol("a completion for another command"));
                }
                pending.retain(|id| *id != tag);
                if let Err(error) = result {
                    failure.get_or_insert(error);
                }
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// An IMAP quoted string. Mailbox names come from the server's LIST, so quotes and backslashes
/// are escaped; CR and LF can't be quoted and make the name unusable.
pub(crate) fn quote(text: &str) -> Result<String, ImapError> {
    if text.contains(['\r', '\n', '\0']) {
        return Err(ImapError::Protocol("a name with a line break"));
    }
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    Ok(out)
}

/// Compresses UIDs into an IMAP sequence set (`1:3,7,9:10`).
pub(crate) fn uid_set(uids: &[u32]) -> String {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut out = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        if !out.is_empty() {
            out.push(',');
        }
        if start == end {
            out.push_str(&start.to_string());
        } else {
            out.push_str(&format!("{start}:{end}"));
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::imap::{Connection, Credentials, connect};
    use crate::net::testing::{Peer, bind, test_ca};
    use async_imap::imap_proto::{AttributeValue, MailboxDatum};
    use sealwax_domain::account::{Endpoint, Security};

    #[test]
    fn compresses_uid_sets() {
        assert_eq!(uid_set(&[9, 1, 2, 3, 7, 10, 3]), "1:3,7,9:10");
        assert_eq!(uid_set(&[5]), "5");
        assert_eq!(uid_set(&[]), "");
    }

    #[test]
    fn errors_name_the_verb_only() {
        assert_eq!(verb("UID FETCH 1:* (UID FLAGS)"), "UID FETCH");
        assert_eq!(verb("EXAMINE \"Søppelpost\" (CONDSTORE)"), "EXAMINE");
        assert_eq!(verb("uid search UID 5:*"), "UID SEARCH");
        assert_eq!(verb("LIST \"\" \"*\" RETURN (STATUS (MESSAGES))"), "LIST");
        assert_eq!(verb("IDLE"), "IDLE");
        assert_eq!(verb("IDENTIFY x"), "a command");
        assert_eq!(verb("XLIST x"), "a command");
    }

    #[test]
    fn quotes_names() {
        assert_eq!(quote("S&APg-ppelpost").unwrap(), "\"S&APg-ppelpost\"");
        assert_eq!(quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
        assert!(quote("a\r\nb").is_err());
    }

    fn outcome(line: &[u8]) -> Outcome<'_> {
        match Response::parse(line).unwrap().1 {
            Response::Done { outcome, .. } | Response::Data { outcome, .. } => outcome,
            other => panic!("not a status response: {other:?}"),
        }
    }

    #[test]
    fn copyuid_pairs_sources_with_targets() {
        let tagged = outcome(b"A1 OK [COPYUID 38505 304,319:320 3956:3958] Done\r\n");
        assert_eq!(
            code(&tagged),
            Some(Code::CopyUid(CopyUid {
                uidvalidity: 38505,
                pairs: vec![(304, 3956), (319, 3957), (320, 3958)],
            }))
        );
        let untagged = outcome(b"* OK [COPYUID 7 1:2 10:11] Moved\r\n");
        assert_eq!(copy_uid(untagged.code.as_ref()).unwrap().pairs.len(), 2);
        let uneven = outcome(b"A2 OK [COPYUID 7 1:3 10:11] Done\r\n");
        assert_eq!(code(&uneven), None);
        let huge = outcome(b"A3 OK [COPYUID 7 1:4000000000 1:4000000000] Done\r\n");
        assert_eq!(code(&huge), None);
        let read_only = outcome(b"A4 OK [READ-ONLY] Examined\r\n");
        assert_eq!(code(&read_only), Some(Code::ReadOnly));
    }

    #[test]
    fn appenduid_names_the_new_message() {
        let tagged = outcome(b"A1 OK [APPENDUID 38505 3955] APPEND completed\r\n");
        assert_eq!(
            code(&tagged),
            Some(Code::AppendUid {
                uidvalidity: 38505,
                uid: 3955
            })
        );
        let several = outcome(b"A2 OK [APPENDUID 38505 3955:3956] APPEND completed\r\n");
        assert_eq!(code(&several), None);
    }

    #[test]
    fn refusals_keep_their_response_code_in_front() {
        for (line, expected) in [
            (
                &b"A1 NO [TRYCREATE] Mailbox doesn't exist\r\n"[..],
                "TRYCREATE",
            ),
            (b"A1 NO [NONEXISTENT] Unknown Mailbox\r\n", "NONEXISTENT"),
            (b"A1 NO [OVERQUOTA] Quota exceeded\r\n", "OVERQUOTA"),
            (b"A1 NO [READ-ONLY] Mailbox is read-only\r\n", "READ-ONLY"),
            (b"A1 NO [CANNOT] Same mailbox\r\n", "CANNOT"),
        ] {
            assert_eq!(text(&outcome(line)).code(), Some(expected));
        }
        assert_eq!(text(&outcome(b"A1 NO Something failed\r\n")).code(), None);
    }

    const SMALL: Budget = Budget {
        responses: 1_000,
        bytes: MIB,
    };

    /// Far longer than a command held to [`SMALL`] takes, so a flood the budget misses fails its
    /// test instead of holding it.
    const ENDS: Duration = Duration::from_secs(30);

    /// A client logged in to a scripted server, and the server's side, whose script goes on
    /// from there.
    async fn logged_in() -> (Connection, Peer) {
        let ca = test_ca();
        let config = ca.server.clone();
        let (listener, port) = bind().await;
        let server = tokio::spawn(async move {
            let mut peer = Peer::accept_tls(&listener, config).await;
            peer.send("* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN] ready\r\n")
                .await;
            let tag = peer.expect("AUTHENTICATE PLAIN").await;
            peer.send("+ \r\n").await;
            peer.line().await;
            peer.send(&format!("{tag} OK authenticated\r\n")).await;
            let tag = peer.expect("CAPABILITY").await;
            peer.send("* CAPABILITY IMAP4rev1\r\n").await;
            peer.send(&format!("{tag} OK done\r\n")).await;
            peer
        });
        let conn = connect(
            &Endpoint {
                host: "127.0.0.1".into(),
                port,
                security: Security::Tls,
                certificate: None,
            },
            Credentials::Password {
                username: "user",
                password: "secret-password",
            },
            ca.trust.client_config().unwrap(),
        )
        .await
        .unwrap();
        (conn, server.await.unwrap())
    }

    fn flags(from: u32, to: u32) -> String {
        (from..=to)
            .map(|n| format!("* {n} FETCH (UID {n} FLAGS (\\Seen))\r\n"))
            .collect()
    }

    #[tokio::test]
    async fn responses_without_end_stop_at_the_budget() {
        let (mut conn, mut peer) = logged_in().await;
        let server = tokio::spawn(async move {
            peer.expect("NOOP").await;
            peer.repeat("* 1 EXISTS\r\n").await;
        });
        let mut seen = 0;
        let run = run_within(&mut conn.session, "NOOP", SMALL, |_| seen += 1);
        let error = tokio::time::timeout(ENDS, run).await.unwrap().unwrap_err();
        assert!(matches!(error, ImapError::TooLong("NOOP")), "{error:?}");
        assert!(error.is_connection());
        assert_eq!(seen, 1_000);
        drop(conn);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn long_responses_without_end_stop_at_the_budget_in_bytes() {
        let (mut conn, mut peer) = logged_in().await;
        let line = format!("* SEARCH{}\r\n", " 4294967295".repeat(1_000));
        let server = tokio::spawn(async move {
            peer.expect("UID SEARCH").await;
            peer.repeat(&line).await;
        });
        let mut uids = 0;
        let run = run_within(&mut conn.session, "UID SEARCH ALL", SMALL, |response| {
            if let Response::MailboxData(MailboxDatum::Search(found)) = response {
                uids += found.len() as u64;
            }
        });
        let error = tokio::time::timeout(ENDS, run).await.unwrap().unwrap_err();
        assert!(
            matches!(error, ImapError::TooLong("UID SEARCH")),
            "{error:?}"
        );
        assert!(uids > 0 && uids * 11 <= MIB, "{uids}");
        drop(conn);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn an_answer_within_the_budget_is_read_whole_and_one_past_it_is_not() {
        let (mut conn, mut peer) = logged_in().await;
        let server = tokio::spawn(async move {
            let tag = peer.expect("UID FETCH").await;
            peer.send(&format!("{}{tag} OK done\r\n", flags(1, 999)))
                .await;
            let tag = peer.expect("UID FETCH").await;
            peer.send(&format!("{}{tag} OK done\r\n", flags(1, 1_000)))
                .await;
            peer.drain().await
        });
        let command = "UID FETCH 1:* (UID FLAGS)";
        let mut seen = 0;
        run_within(&mut conn.session, command, SMALL, |_| seen += 1)
            .await
            .unwrap();
        assert_eq!(seen, 999);
        let error = run_within(&mut conn.session, command, SMALL, |_| {})
            .await
            .unwrap_err();
        assert!(
            matches!(error, ImapError::TooLong("UID FETCH")),
            "{error:?}"
        );
        drop(conn);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn pipelined_commands_share_one_budget() {
        let (mut conn, mut peer) = logged_in().await;
        let server = tokio::spawn(async move {
            let first = peer.expect("UID FETCH 1:600").await;
            let second = peer.expect("UID FETCH 601:1200").await;
            peer.send(&format!("{}{first} OK done\r\n", flags(1, 600)))
                .await;
            peer.send(&format!("{}{second} OK done\r\n", flags(601, 1_200)))
                .await;
            peer.drain().await
        });
        let commands = [
            "UID FETCH 1:600 (UID FLAGS)".to_owned(),
            "UID FETCH 601:1200 (UID FLAGS)".to_owned(),
        ];
        let error = pipelined(&mut conn.session, &commands, SMALL, |_| {})
            .await
            .unwrap_err();
        assert!(
            matches!(error, ImapError::TooLong("UID FETCH")),
            "{error:?}"
        );
        drop(conn);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_literal_that_arrives_in_pieces_comes_whole() {
        let (mut conn, mut peer) = logged_in().await;
        let data: String = (0..1 << 20)
            .map(|n| char::from(b'a' + (n % 26) as u8))
            .collect();
        let expected = data.clone();
        let server = tokio::spawn(async move {
            let tag = peer.expect("UID FETCH").await;
            peer.send(&format!("* 1 FETCH (UID 1 BODY[] {{{}}}\r\n", data.len()))
                .await;
            for piece in data.as_bytes().chunks(1_000) {
                peer.send(std::str::from_utf8(piece).unwrap()).await;
            }
            peer.send(&format!(")\r\n{tag} OK done\r\n")).await;
            peer.drain().await
        });
        let mut body = None;
        let command = "UID FETCH 1 (BODY.PEEK[])";
        run_within(&mut conn.session, command, Budget::BODIES, |response| {
            if let Response::Fetch(_, attributes) = response {
                for attribute in attributes {
                    if let AttributeValue::BodySection { data, .. } = attribute {
                        body = data.as_deref().map(<[u8]>::to_vec);
                    }
                }
            }
        })
        .await
        .unwrap();
        assert!(body.as_deref() == Some(expected.as_bytes()));
        drop(conn);
        server.await.unwrap();
    }

    /// async-imap #151's stream-returning commands ended at a tagged `NO` or `BAD`, or at a lost
    /// connection, as if the answer were whole. Sealwax reads commands here instead: a refusal
    /// after data, with or without text, is the command's error, and a connection lost before the
    /// completion is a connection error.
    #[tokio::test]
    async fn an_answer_is_whole_only_at_a_tagged_ok() {
        for ending in ["NO [SERVERBUG] Failed", "BAD", ""] {
            let (mut conn, mut peer) = logged_in().await;
            let server = tokio::spawn(async move {
                let tag = peer.expect("UID FETCH").await;
                peer.send("* 1 FETCH (UID 1 FLAGS (\\Seen))\r\n").await;
                if ending.is_empty() {
                    return None;
                }
                peer.send(&format!("{tag} {ending}\r\n")).await;
                Some(peer)
            });
            let mut seen = 0;
            let error = run(&mut conn.session, "UID FETCH 1:* (FLAGS)", |_| seen += 1)
                .await
                .unwrap_err();
            assert_eq!(seen, 1, "{ending:?}");
            match ending {
                "" => assert!(error.is_connection(), "{error:?}"),
                "BAD" => assert!(matches!(error, ImapError::Bad(_)), "{error:?}"),
                _ => assert!(
                    matches!(&error, ImapError::No(text) if text.code() == Some("SERVERBUG")),
                    "{error:?}"
                ),
            }
            drop(conn);
            server.await.unwrap();
        }
    }

    #[test]
    fn the_budgets_hold_the_largest_honest_answers() {
        let flag_scan = 1_000_000 * (flags(999_999, 999_999).len() as u64 + 40);
        assert!(Budget::COMMAND.bytes > 2 * flag_scan);
        const { assert!(Budget::COMMAND.responses >= 2 * 1_000_000) };
        const { assert!(Budget::BODIES.bytes > 512 * MIB + MIB) };
    }
}
