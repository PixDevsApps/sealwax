//! The mandatory raw-command layer over async-imap (ADR-006 S1 amendment). async-imap's typed
//! calls for FETCH, STORE, SEARCH, LIST, EXPUNGE, NOOP, CAPABILITY and ID stop at the tagged
//! completion without checking it, so a refused command reads as an empty success, and they
//! drop untagged responses past a 100-entry buffer. Every command goes through here instead:
//! it is sent with `run_command`, every response is read by us with a timeout, every untagged
//! response reaches the caller, and every completion is checked for OK, NO and BAD. A tagged
//! OK's COPYUID (RFC 4315) or READ-ONLY reaches the caller too.

use super::error::{ImapError, ServerText};
use async_imap::imap_proto::rfc4315::UidSetMember;
use async_imap::imap_proto::{Outcome, RequestId, Response, ResponseCode, Status};
use std::time::Duration;

pub(crate) type Session = async_imap::Session<super::Stream>;

/// How long any one read may wait for the server.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

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

/// Reads one response to `command` (its verb) and hands it to `f`. async-imap's `ResponseData`
/// can't be named outside the crate, so it never leaves this function.
async fn next<R>(
    session: &mut Session,
    command: &'static str,
    f: impl FnOnce(&Response<'_>) -> R,
) -> Result<R, ImapError> {
    match tokio::time::timeout(READ_TIMEOUT, session.read_response()).await {
        Err(_) => Err(ImapError::Timeout),
        Ok(Err(error)) => Err(ImapError::reading(error, command)),
        Ok(Ok(None)) => Err(ImapError::ConnectionLost),
        Ok(Ok(Some(response))) => Ok(f(response.parsed())),
    }
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
pub(crate) async fn finish<F>(
    session: &mut Session,
    id: &RequestId,
    command: &'static str,
    mut on_untagged: F,
) -> Result<Option<Code>, ImapError>
where
    F: FnMut(&Response<'_>),
{
    loop {
        let step = next(session, command, |response| match response {
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

/// Runs one command and reads its responses.
pub(crate) async fn run<F>(
    session: &mut Session,
    command: &str,
    on_untagged: F,
) -> Result<(), ImapError>
where
    F: FnMut(&Response<'_>),
{
    run_coded(session, command, on_untagged).await.map(drop)
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
    let id = session.run_command(command).await?;
    finish(session, &id, verb(command), on_untagged).await
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
    loop {
        let ahead = next(session, verb, |response| match response {
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
    finish(session, &id, verb, on_untagged).await
}

/// Sends several commands without waiting and reads until every one has completed. Replies may
/// come in any order (Stalwart runs pipelined commands concurrently); the first failure is
/// returned once all completions are read.
pub(crate) async fn run_pipelined<F>(
    session: &mut Session,
    commands: &[String],
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
    let mut failure = None;
    while !pending.is_empty() {
        let done = next(session, verb, |response| match response {
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
}
