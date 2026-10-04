//! The thin raw-command layer over async-imap.
//!
//! async-imap's typed helpers for FETCH, STORE, SEARCH, LIST, EXPUNGE, NOOP, CAPABILITY and ID
//! stop reading at the tagged completion without checking whether it was OK, NO or BAD, and
//! they route every other untagged response into a bounded(100) channel that drops on overflow.
//! This layer sends the command with `Session::run_command` and reads every response itself
//! with `Session::read_response`, so the caller sees each untagged response (VANISHED, EXISTS,
//! EXPUNGE, FETCH, SEARCH, LIST ...) and the completion status is always checked.

use std::ops::RangeInclusive;
use std::time::Duration;

use async_imap::imap_proto::rfc4315::UidSetMember;
use async_imap::imap_proto::{RequestId, Response, ResponseCode, Status};
use tokio::io::AsyncWriteExt;

use crate::net::Session;

pub const READ_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, thiserror::Error)]
pub enum ImapError {
    #[error("connection lost")]
    ConnectionLost,
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("server said NO: {0}")]
    No(String),
    #[error("server said BAD: {0}")]
    Bad(String),
    #[error("server said BYE: {0}")]
    Bye(String),
    #[error("timed out waiting for the server")]
    Timeout,
    #[error("protocol: {0}")]
    Protocol(String),
}

impl ImapError {
    /// Errors after which the connection is unusable and must be replaced.
    pub fn is_connection(&self) -> bool {
        matches!(
            self,
            ImapError::ConnectionLost
                | ImapError::Io(_)
                | ImapError::Bye(_)
                | ImapError::Timeout
                | ImapError::Protocol(_)
        )
    }
}

impl From<async_imap::error::Error> for ImapError {
    fn from(error: async_imap::error::Error) -> Self {
        use async_imap::error::Error;
        match error {
            Error::Io(e) => ImapError::Io(e),
            Error::Bad(text) => ImapError::Bad(text),
            Error::No(text) => ImapError::No(text),
            Error::ConnectionLost => ImapError::ConnectionLost,
            other => ImapError::Protocol(other.to_string()),
        }
    }
}

/// The parts of a tagged OK the sync needs.
#[derive(Clone, Debug, Default)]
pub struct Completion {
    pub append_uid: Option<(u32, Vec<RangeInclusive<u32>>)>,
    pub copy_uid: Option<CopyUid>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyUid {
    pub uidvalidity: u32,
    pub source: Vec<RangeInclusive<u32>>,
    pub destination: Vec<RangeInclusive<u32>>,
}

pub fn uid_ranges(set: &[UidSetMember]) -> Vec<RangeInclusive<u32>> {
    set.iter()
        .map(|m| match m {
            UidSetMember::UidRange(r) => r.clone(),
            UidSetMember::Uid(u) => *u..=*u,
        })
        .collect()
}

pub fn copy_uid(code: Option<&ResponseCode<'_>>) -> Option<CopyUid> {
    match code {
        Some(ResponseCode::CopyUid(uidvalidity, source, destination)) => Some(CopyUid {
            uidvalidity: *uidvalidity,
            source: uid_ranges(source),
            destination: uid_ranges(destination),
        }),
        _ => None,
    }
}

/// Reads one response and hands it to `f` (async-imap's `ResponseData` can't be named outside
/// the crate, so it never leaves this function).
async fn next<R>(
    session: &mut Session,
    timeout: Duration,
    f: impl FnOnce(&Response<'_>) -> R,
) -> Result<R, ImapError> {
    match tokio::time::timeout(timeout, session.read_response()).await {
        Err(_) => Err(ImapError::Timeout),
        Ok(Err(e)) => Err(ImapError::Io(e)),
        Ok(Ok(None)) => Err(ImapError::ConnectionLost),
        Ok(Ok(Some(response))) => Ok(f(response.parsed())),
    }
}

enum Step {
    Done(Result<Completion, ImapError>),
    More,
}

/// Reads until the tagged completion of `id`, handing every other response to `on_untagged`.
pub async fn finish<F>(
    session: &mut Session,
    id: &RequestId,
    mut on_untagged: F,
) -> Result<Completion, ImapError>
where
    F: FnMut(&Response<'_>),
{
    loop {
        let step = next(session, READ_TIMEOUT, |response| match response {
            Response::Done {
                tag,
                status,
                outcome,
            } if tag == id => {
                let text = outcome
                    .information
                    .as_deref()
                    .unwrap_or_default()
                    .to_owned();
                Step::Done(match status {
                    Status::Ok => Ok(Completion {
                        append_uid: match &outcome.code {
                            Some(ResponseCode::AppendUid(v, uids)) => Some((*v, uid_ranges(uids))),
                            _ => None,
                        },
                        copy_uid: copy_uid(outcome.code.as_ref()),
                    }),
                    Status::No => Err(ImapError::No(text)),
                    Status::Bad => Err(ImapError::Bad(text)),
                    other => Err(ImapError::Protocol(format!("tagged {other:?}"))),
                })
            }
            Response::Done { .. } => Step::Done(Err(ImapError::Protocol(
                "completion for another tag".into(),
            ))),
            Response::Data {
                status: Status::Bye,
                outcome,
            } => Step::Done(Err(ImapError::Bye(
                outcome
                    .information
                    .as_deref()
                    .unwrap_or_default()
                    .to_owned(),
            ))),
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

/// Pipelines several commands (one write each, no waiting) and reads until every one has
/// completed; the first failure is returned after all completions are read.
pub async fn run_pipelined<F>(
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
        let step = next(session, READ_TIMEOUT, |response| match response {
            Response::Done {
                tag,
                status,
                outcome,
            } => {
                let text = outcome
                    .information
                    .as_deref()
                    .unwrap_or_default()
                    .to_owned();
                let result = match status {
                    Status::Ok => Ok(()),
                    Status::No => Err(ImapError::No(text)),
                    Status::Bad => Err(ImapError::Bad(text)),
                    other => Err(ImapError::Protocol(format!("tagged {other:?}"))),
                };
                Some((tag.clone(), result))
            }
            Response::Data {
                status: Status::Bye,
                outcome,
            } => Some((
                RequestId(String::new()),
                Err(ImapError::Bye(
                    outcome
                        .information
                        .as_deref()
                        .unwrap_or_default()
                        .to_owned(),
                )),
            )),
            other => {
                on_untagged(other);
                None
            }
        })
        .await?;
        if let Some((tag, result)) = step {
            if let Err(ImapError::Bye(text)) = result {
                return Err(ImapError::Bye(text));
            }
            pending.retain(|id| *id != tag);
            if let Err(error) = result {
                failure.get_or_insert(error);
            }
        }
    }
    failure.map_or(Ok(()), Err)
}

/// Runs one command and reads its responses.
pub async fn run<F>(
    session: &mut Session,
    command: &str,
    on_untagged: F,
) -> Result<Completion, ImapError>
where
    F: FnMut(&Response<'_>),
{
    let id = session.run_command(command).await?;
    finish(session, &id, on_untagged).await
}

/// APPEND with the literal written by us, so the APPENDUID response code is kept. Uses a
/// non-synchronizing literal when the server offers LITERAL+, else waits for the continuation.
pub async fn append(
    session: &mut Session,
    mailbox: &str,
    flags: &str,
    message: &[u8],
    literal_plus: bool,
    on_untagged: impl FnMut(&Response<'_>),
) -> Result<Completion, ImapError> {
    let plus = if literal_plus { "+" } else { "" };
    let id = session
        .run_command(format!(
            "APPEND {} ({flags}) {{{}{plus}}}",
            quote(mailbox),
            message.len()
        ))
        .await?;
    if !literal_plus {
        next(session, READ_TIMEOUT, |response| match response {
            Response::Continue(_) => Ok(()),
            Response::Done {
                status: Status::No,
                outcome,
                ..
            } => Err(ImapError::No(
                outcome
                    .information
                    .as_deref()
                    .unwrap_or_default()
                    .to_owned(),
            )),
            Response::Done {
                status: Status::Bad,
                outcome,
                ..
            } => Err(ImapError::Bad(
                outcome
                    .information
                    .as_deref()
                    .unwrap_or_default()
                    .to_owned(),
            )),
            _ => Err(ImapError::Protocol("expected a continuation".into())),
        })
        .await??;
    }
    let stream = session.as_mut();
    stream.write_all(message).await?;
    stream.write_all(b"\r\n").await?;
    stream.flush().await?;
    finish(session, &id, on_untagged).await
}

/// An IMAP quoted string. Mailbox names reach us as server-supplied wire names, so quotes and
/// backslashes are escaped; CR and LF can't be quoted and are rejected by the caller's data.
pub fn quote(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for ch in text.chars() {
        if ch == '"' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

/// Compresses sorted UIDs into an IMAP sequence set (`1:3,7,9:10`).
pub fn uid_set(uids: &[u32]) -> String {
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

pub fn expand(ranges: &[RangeInclusive<u32>]) -> Vec<u32> {
    ranges.iter().flat_map(|r| r.clone()).collect()
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
        assert_eq!(quote("S&APg-ppelpost"), "\"S&APg-ppelpost\"");
        assert_eq!(quote("a\"b\\c"), "\"a\\\"b\\\\c\"");
    }
}
