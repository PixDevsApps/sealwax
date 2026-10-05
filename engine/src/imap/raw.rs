//! The mandatory raw-command layer over async-imap (ADR-006 S1 amendment). async-imap's typed
//! calls for FETCH, STORE, SEARCH, LIST, EXPUNGE, NOOP, CAPABILITY and ID stop at the tagged
//! completion without checking it, so a refused command reads as an empty success, and they
//! drop untagged responses past a 100-entry buffer. Every command goes through here instead:
//! it is sent with `run_command`, every response is read by us with a timeout, every untagged
//! response reaches the caller, and every completion is checked for OK, NO and BAD.

use super::error::{ImapError, ServerText};
use async_imap::imap_proto::{RequestId, Response, Status};
use std::time::Duration;

pub(crate) type Session = async_imap::Session<super::Stream>;

/// How long any one read may wait for the server.
pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

/// Reads one response and hands it to `f`. async-imap's `ResponseData` can't be named outside
/// the crate, so it never leaves this function.
async fn next<R>(
    session: &mut Session,
    f: impl FnOnce(&Response<'_>) -> R,
) -> Result<R, ImapError> {
    match tokio::time::timeout(READ_TIMEOUT, session.read_response()).await {
        Err(_) => Err(ImapError::Timeout),
        Ok(Err(error)) => Err(ImapError::Io(error)),
        Ok(Ok(None)) => Err(ImapError::ConnectionLost),
        Ok(Ok(Some(response))) => Ok(f(response.parsed())),
    }
}

fn text(information: Option<&std::borrow::Cow<'_, str>>) -> ServerText {
    ServerText(information.map(|text| text.to_string()).unwrap_or_default())
}

fn completion(
    status: &Status,
    information: Option<&std::borrow::Cow<'_, str>>,
) -> Result<(), ImapError> {
    match status {
        Status::Ok => Ok(()),
        Status::No => Err(ImapError::No(text(information))),
        Status::Bad => Err(ImapError::Bad(text(information))),
        _ => Err(ImapError::Protocol("unexpected completion status")),
    }
}

enum Step {
    Done(Result<(), ImapError>),
    More,
}

/// Reads until the tagged completion of `id`, handing every other response to `on_untagged`.
pub(crate) async fn finish<F>(
    session: &mut Session,
    id: &RequestId,
    mut on_untagged: F,
) -> Result<(), ImapError>
where
    F: FnMut(&Response<'_>),
{
    loop {
        let step = next(session, |response| match response {
            Response::Done {
                tag,
                status,
                outcome,
            } if tag == id => Step::Done(completion(status, outcome.information.as_ref())),
            Response::Done { .. } => {
                Step::Done(Err(ImapError::Protocol("a completion for another command")))
            }
            Response::Data {
                status: Status::Bye,
                outcome,
            } => Step::Done(Err(ImapError::Bye(text(outcome.information.as_ref())))),
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
    let id = session.run_command(command).await?;
    finish(session, &id, on_untagged).await
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
    let mut failure = None;
    while !pending.is_empty() {
        let done = next(session, |response| match response {
            Response::Done {
                tag,
                status,
                outcome,
            } => Some(Ok((
                tag.clone(),
                completion(status, outcome.information.as_ref()),
            ))),
            Response::Data {
                status: Status::Bye,
                outcome,
            } => Some(Err(ImapError::Bye(text(outcome.information.as_ref())))),
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
    fn quotes_names() {
        assert_eq!(quote("S&APg-ppelpost").unwrap(), "\"S&APg-ppelpost\"");
        assert_eq!(quote("a\"b\\c").unwrap(), "\"a\\\"b\\\\c\"");
        assert!(quote("a\r\nb").is_err());
    }
}
