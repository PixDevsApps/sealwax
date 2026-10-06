//! Downloading message bodies (ML-044) on the raw layer. The folder is EXAMINEd (read-only) and
//! its UIDVALIDITY checked against the store's; every fetch is `BODY.PEEK`, so nothing is ever
//! marked `\Seen`. By RFC822.SIZE, a message up to [`WHOLE_LIMIT`] comes whole, several per
//! round trip; a larger one as its header and the text parts the reader shows, rebuilt in the
//! message's own MIME structure with every attachment left empty and recorded by its IMAP
//! section, which [`fetch_part`] fetches on demand. Each body goes into the blob store before
//! the store's upgrade refers to it. [`fetch_original`] fetches a message whole for Show
//! Original and Save as .eml (M5), storing nothing.

use super::connection::Connection;
use super::error::ImapError;
use super::raw;
use super::sync::view;
use crate::api::BodyError;
pub use crate::api::FetchedPart;
use async_imap::imap_proto::{
    AttributeValue, BodyContentCommon, BodyStructure, ContentEncoding, MessageSection, Response,
    SectionPath,
};
use mail_parser::MessageParser;
use sealwax_domain::bodies::WHOLE_LIMIT;
use sealwax_domain::message;
use sealwax_domain::model::{BlobKey, MessageId};
use sealwax_infrastructure::blobs::BlobStore;
use sealwax_infrastructure::store::{
    BodyState, BodyTarget, BodyUpgrade, FolderSync, PartSource, Store, StoreError,
};
use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Instant;

/// Whole messages per FETCH, at most.
const WHOLE_BATCH: usize = 50;
/// And at most this many bytes of them (by RFC822.SIZE).
const WHOLE_BATCH_BYTES: u64 = 16 * 1024 * 1024;

/// Where downloaded bodies go. `writes` keeps blob writes and their upgrades apart from the
/// disk cap's evictions, which delete blobs after their commit.
#[derive(Clone, Copy)]
pub struct Sink<'a> {
    pub store: &'a Store,
    pub blobs: &'a BlobStore,
    pub writes: &'a tokio::sync::Mutex<()>,
}

/// Why a batch of downloads stopped.
#[derive(Debug, thiserror::Error)]
pub enum BodyFetchError {
    #[error(transparent)]
    Imap(#[from] ImapError),
    #[error("the local store failed: {0}")]
    Store(#[from] StoreError),
    #[error("the blob store failed ({0})")]
    Blob(io::ErrorKind),
}

impl BodyFetchError {
    /// What the messages left unanswered are told.
    pub fn body_error(&self) -> BodyError {
        match self {
            BodyFetchError::Imap(error) if error.is_connection() => BodyError::Offline,
            BodyFetchError::Imap(_) => BodyError::Other,
            BodyFetchError::Store(_) | BodyFetchError::Blob(_) => BodyError::Store,
        }
    }
}

impl From<io::Error> for BodyFetchError {
    fn from(error: io::Error) -> Self {
        BodyFetchError::Blob(error.kind())
    }
}

/// One part of a BODYSTRUCTURE, owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Node {
    Multipart {
        boundary: Option<String>,
        parts: Vec<Node>,
    },
    Leaf {
        /// A text/plain or text/html part that isn't an attachment: what the reader shows.
        text: bool,
        octets: u32,
        base64: bool,
    },
}

fn is_attachment(common: &BodyContentCommon<'_>) -> bool {
    common
        .disposition
        .as_ref()
        .is_some_and(|disposition| disposition.ty.eq_ignore_ascii_case("attachment"))
}

impl Node {
    pub(crate) fn from_structure(body: &BodyStructure<'_>) -> Node {
        let leaf = |common: &BodyContentCommon<'_>, octets: u32, encoding: &ContentEncoding<'_>| {
            let subtype = common.ty.subtype.to_ascii_lowercase();
            Node::Leaf {
                text: common.ty.ty.eq_ignore_ascii_case("text")
                    && (subtype == "plain" || subtype == "html")
                    && !is_attachment(common),
                octets,
                base64: matches!(encoding, ContentEncoding::Base64),
            }
        };
        match body {
            BodyStructure::Multipart { common, bodies, .. } => Node::Multipart {
                boundary: common.ty.params.as_ref().and_then(|params| {
                    params
                        .iter()
                        .find(|(name, _)| name.eq_ignore_ascii_case("boundary"))
                        .map(|(_, value)| value.to_string())
                }),
                parts: bodies.iter().map(Node::from_structure).collect(),
            },
            BodyStructure::Text { common, other, .. }
            | BodyStructure::Basic { common, other, .. }
            | BodyStructure::Message { common, other, .. } => {
                leaf(common, other.octets, &other.transfer_encoding)
            }
        }
    }

    /// Whether every multipart names its boundary, so the message can be rebuilt.
    fn rebuildable(&self) -> bool {
        match self {
            Node::Multipart { boundary, parts } => {
                boundary.is_some() && parts.iter().all(Node::rebuildable)
            }
            Node::Leaf { .. } => true,
        }
    }

    /// Each leaf's IMAP section, depth first; a message that isn't multipart has only `TEXT`.
    fn leaves(&self) -> Vec<(String, &Node)> {
        fn walk<'n>(node: &'n Node, prefix: &str, out: &mut Vec<(String, &'n Node)>) {
            if let Node::Multipart { parts, .. } = node {
                for (index, part) in parts.iter().enumerate() {
                    let section = format!("{prefix}{}", index + 1);
                    match part {
                        Node::Multipart { .. } => walk(part, &format!("{section}."), out),
                        Node::Leaf { .. } => out.push((section, part)),
                    }
                }
            }
        }
        let mut out = Vec::new();
        match self {
            Node::Multipart { .. } => walk(self, "", &mut out),
            Node::Leaf { .. } => out.push(("TEXT".to_owned(), self)),
        }
        out
    }

    /// The sections of every part, multiparts included, depth first.
    fn sections(&self) -> Vec<String> {
        fn walk(node: &Node, prefix: &str, out: &mut Vec<String>) {
            if let Node::Multipart { parts, .. } = node {
                for (index, part) in parts.iter().enumerate() {
                    let section = format!("{prefix}{}", index + 1);
                    out.push(section.clone());
                    walk(part, &format!("{section}."), out);
                }
            }
        }
        let mut out = Vec::new();
        walk(self, "", &mut out);
        out
    }
}

/// How a large message is fetched: the FETCH items, or the whole message when its structure
/// can't be rebuilt or is all text anyway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Plan {
    Whole,
    Parts(Vec<String>),
}

pub(crate) fn plan(root: &Node) -> Plan {
    match root {
        Node::Leaf { text: true, .. } => Plan::Whole,
        _ if !root.rebuildable() => Plan::Whole,
        Node::Leaf { .. } => Plan::Parts(vec!["BODY.PEEK[HEADER]".to_owned()]),
        Node::Multipart { .. } => {
            let mut items = vec!["BODY.PEEK[HEADER]".to_owned()];
            for section in root.sections() {
                items.push(format!("BODY.PEEK[{section}.MIME]"));
            }
            for (section, leaf) in root.leaves() {
                if matches!(leaf, Node::Leaf { text: true, .. }) {
                    items.push(format!("BODY.PEEK[{section}]"));
                }
            }
            Plan::Parts(items)
        }
    }
}

/// Header bytes end with their blank line.
fn with_blank_line(mut header: Vec<u8>) -> Vec<u8> {
    if !header.ends_with(b"\r\n\r\n") {
        header.extend_from_slice(if header.ends_with(b"\r\n") {
            b"\r\n"
        } else {
            b"\r\n\r\n"
        });
    }
    header
}

/// The message rebuilt from its header and fetched sections (`1.2.MIME`, `1.2`): every part
/// keeps its own MIME header, the text parts their content, the rest is left empty.
pub(crate) fn rebuild(root: &Node, sections: &HashMap<String, Vec<u8>>) -> Vec<u8> {
    fn multipart(
        out: &mut Vec<u8>,
        boundary: &str,
        parts: &[Node],
        prefix: &str,
        sections: &HashMap<String, Vec<u8>>,
    ) {
        for (index, part) in parts.iter().enumerate() {
            let section = format!("{prefix}{}", index + 1);
            out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
            let header = sections
                .get(&format!("{section}.MIME"))
                .cloned()
                .unwrap_or_default();
            out.extend_from_slice(&with_blank_line(header));
            match part {
                Node::Multipart {
                    boundary: Some(inner),
                    parts,
                } => multipart(out, inner, parts, &format!("{section}."), sections),
                Node::Leaf { text: true, .. } => {
                    out.extend_from_slice(sections.get(&section).map_or(&[][..], Vec::as_slice));
                }
                _ => {}
            }
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    }
    let mut out = with_blank_line(sections.get("HEADER").cloned().unwrap_or_default());
    if let Node::Multipart {
        boundary: Some(boundary),
        parts,
    } = root
    {
        multipart(&mut out, boundary, parts, "", sections);
    }
    out
}

/// The IMAP section of each part of a raw message by its mail-parser index: `1`, `2.1`…, or
/// `TEXT` for the body of a message that isn't multipart.
pub(crate) fn part_sections(raw: &[u8]) -> HashMap<u32, String> {
    let mut out = HashMap::new();
    let Some(parsed) = MessageParser::default().parse(raw) else {
        return out;
    };
    let Some(root) = parsed.parts.first() else {
        return out;
    };
    match root.sub_parts() {
        None => {
            out.insert(0, "TEXT".to_owned());
        }
        Some(children) => {
            let mut stack: Vec<(u32, String)> = children
                .iter()
                .enumerate()
                .map(|(index, id)| (*id, (index + 1).to_string()))
                .collect();
            while let Some((id, section)) = stack.pop() {
                if let Some(children) = parsed.parts.get(id as usize).and_then(|p| p.sub_parts()) {
                    for (index, child) in children.iter().enumerate() {
                        stack.push((*child, format!("{section}.{}", index + 1)));
                    }
                }
                out.insert(id, section);
            }
        }
    }
    out
}

/// The store's upgrade for a raw (or rebuilt) message. A rebuilt one's attachments take their
/// size from the structure: base64 as decoded from 76-character lines, the rest as sent.
pub(crate) fn upgrade(
    target: &BodyTarget,
    key: BlobKey,
    raw: &[u8],
    structure: Option<&Node>,
    used: i64,
) -> BodyUpgrade {
    let parsed = message::parse(raw);
    let sections = part_sections(raw);
    let leaves: HashMap<String, (u32, bool)> = structure
        .map(|root| {
            root.leaves()
                .into_iter()
                .filter_map(|(section, node)| match node {
                    Node::Leaf { octets, base64, .. } => Some((section, (*octets, *base64))),
                    Node::Multipart { .. } => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut attachments = parsed.attachments.clone();
    for attachment in &mut attachments {
        if let Some((octets, base64)) = sections
            .get(&attachment.part)
            .and_then(|section| leaves.get(section))
        {
            attachment.size = if *base64 {
                u64::from(*octets) * 57 / 78
            } else {
                u64::from(*octets)
            };
        }
    }
    let has_attachments = attachments
        .iter()
        .any(|attachment| !attachment.is_embedded());
    BodyUpgrade {
        message: target.message,
        uid: target.uid,
        blob: key,
        blob_size: raw.len() as u64,
        body_state: if structure.is_some() {
            BodyState::Text
        } else {
            BodyState::Full
        },
        body_kind: parsed.body_kind,
        has_attachments,
        snippet: parsed.snippet,
        text_parts: parsed.text_parts,
        html_parts: parsed.html_parts,
        sections: attachments
            .iter()
            .filter_map(|attachment| {
                sections
                    .get(&attachment.part)
                    .map(|section| (attachment.part, section.clone()))
            })
            .collect(),
        attachments,
        search_text: parsed.body_text,
        used,
    }
}

/// EXAMINEs `name_raw` (the sync's EXAMINE) and returns its UIDVALIDITY.
async fn examine(conn: &mut Connection, name_raw: &str) -> Result<Option<u32>, ImapError> {
    Ok(Some(view::examine(conn, name_raw).await?.uidvalidity))
}

/// A section path as this module names it: `HEADER`, `TEXT`, `2.1`, `2.1.MIME`.
fn section_name(section: &Option<SectionPath>) -> Option<String> {
    let suffix = |text: &Option<MessageSection>| match text {
        None => "",
        Some(MessageSection::Mime) => ".MIME",
        Some(MessageSection::Header) => ".HEADER",
        Some(MessageSection::Text) => ".TEXT",
    };
    match section.as_ref()? {
        SectionPath::Full(MessageSection::Header) => Some("HEADER".to_owned()),
        SectionPath::Full(MessageSection::Text) => Some("TEXT".to_owned()),
        SectionPath::Full(MessageSection::Mime) => None,
        SectionPath::Part(path, text) => Some(format!(
            "{}{}",
            path.iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join("."),
            suffix(text)
        )),
    }
}

/// What one FETCH response carried for a UID we asked for.
#[derive(Default)]
struct Fetched {
    whole: Option<Vec<u8>>,
    sections: HashMap<String, Vec<u8>>,
    structure: Option<Node>,
}

/// Runs a UID FETCH and collects, per wanted UID, the bodies and structures it returned.
/// FETCH responses for other UIDs are unsolicited hints and are dropped. A NO still hands over
/// what came back; the UIDs missing from it are the caller's to judge.
async fn uid_fetch(
    conn: &mut Connection,
    uids: &[u32],
    items: &str,
) -> Result<HashMap<u32, Fetched>, ImapError> {
    let wanted: HashSet<u32> = uids.iter().copied().collect();
    let mut out: HashMap<u32, Fetched> = HashMap::new();
    let command = format!("UID FETCH {} ({items})", raw::uid_set(uids));
    let result = raw::run(&mut conn.session, &command, |response| {
        let Response::Fetch(_, attributes) = response else {
            return;
        };
        let Some(uid) = attributes.iter().find_map(|attribute| match attribute {
            AttributeValue::Uid(uid) => Some(*uid),
            _ => None,
        }) else {
            return;
        };
        if !wanted.contains(&uid) {
            return;
        }
        let entry = out.entry(uid).or_default();
        for attribute in attributes {
            match attribute {
                AttributeValue::BodyStructure(body) => {
                    entry.structure = Some(Node::from_structure(body));
                }
                AttributeValue::BodySection {
                    section: None,
                    data,
                    ..
                } => entry.whole = Some(data.as_deref().unwrap_or_default().to_vec()),
                AttributeValue::BodySection { section, data, .. } => {
                    if let Some(name) = section_name(section) {
                        entry
                            .sections
                            .insert(name, data.as_deref().unwrap_or_default().to_vec());
                    }
                }
                _ => {}
            }
        }
    })
    .await;
    match result {
        Ok(()) | Err(ImapError::No(_)) => Ok(out),
        Err(error) => Err(error),
    }
}

/// Stores one downloaded body: the blob first, then the upgrade; a message gone meanwhile
/// leaves nothing behind.
async fn keep(
    sink: Sink<'_>,
    target: &BodyTarget,
    raw: Vec<u8>,
    structure: Option<Node>,
    used: i64,
) -> Result<Result<(), BodyError>, BodyFetchError> {
    let _writes = sink.writes.lock().await;
    let blobs = sink.blobs.clone();
    let for_worker = target.clone();
    let built = tokio::task::spawn_blocking(move || -> io::Result<BodyUpgrade> {
        let key = blobs.put(&raw)?;
        Ok(upgrade(&for_worker, key, &raw, structure.as_ref(), used))
    })
    .await
    .map_err(|_| BodyFetchError::Blob(io::ErrorKind::Other))??;
    let upgraded = sink.store.upgrade_body(built).recv().await?;
    if !upgraded.orphaned_blobs.is_empty() {
        let blobs = sink.blobs.clone();
        let orphaned = upgraded.orphaned_blobs.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for key in &orphaned {
                if let Err(error) = blobs.remove(key) {
                    tracing::warn!(%error, "a raw message no row refers to couldn't be deleted");
                }
            }
        })
        .await;
    }
    Ok(if upgraded.found {
        Ok(())
    } else {
        Err(BodyError::Gone)
    })
}

/// Downloads the bodies of `targets`, all in `folder`, and stores them, calling `done` for each
/// message as soon as it is answered: stored, or `Gone` when the server no longer has it (its
/// UID, or the folder's UIDVALIDITY changed). An error ends the batch; `done` was not called
/// for the messages left.
pub async fn fetch_bodies(
    conn: &mut Connection,
    sink: Sink<'_>,
    folder: &FolderSync,
    targets: &[BodyTarget],
    used: i64,
    mut done: impl FnMut(MessageId, Result<(), BodyError>),
) -> Result<(), BodyFetchError> {
    if targets.is_empty() {
        return Ok(());
    }
    let started = Instant::now();
    let uidvalidity = examine(conn, &folder.name_raw).await?;
    if uidvalidity.is_none() || uidvalidity != folder.uidvalidity {
        tracing::info!(%folder.folder, "the folder's UIDVALIDITY changed; its bodies are gone");
        for target in targets {
            done(target.message, Err(BodyError::Gone));
        }
        return Ok(());
    }
    let (whole, large): (Vec<&BodyTarget>, Vec<&BodyTarget>) = targets
        .iter()
        .partition(|target| target.size <= WHOLE_LIMIT);
    let mut chunks: Vec<Vec<&BodyTarget>> = Vec::new();
    let mut bytes = 0;
    for target in whole {
        match chunks.last_mut() {
            Some(chunk)
                if chunk.len() < WHOLE_BATCH && bytes + target.size <= WHOLE_BATCH_BYTES =>
            {
                bytes += target.size;
                chunk.push(target);
            }
            _ => {
                bytes = target.size;
                chunks.push(vec![target]);
            }
        }
    }
    let mut stored = 0;
    for chunk in chunks {
        let uids: Vec<u32> = chunk.iter().map(|target| target.uid).collect();
        let mut fetched = uid_fetch(conn, &uids, "UID BODY.PEEK[]").await?;
        for target in chunk {
            let answer = match fetched.remove(&target.uid).and_then(|found| found.whole) {
                Some(raw) => keep(sink, target, raw, None, used).await?,
                None => Err(BodyError::Gone),
            };
            stored += u32::from(answer.is_ok());
            done(target.message, answer);
        }
    }
    if !large.is_empty() {
        let uids: Vec<u32> = large.iter().map(|target| target.uid).collect();
        let mut structures = uid_fetch(conn, &uids, "UID BODYSTRUCTURE").await?;
        for target in large {
            let Some(root) = structures
                .remove(&target.uid)
                .and_then(|found| found.structure)
            else {
                done(target.message, Err(BodyError::Gone));
                continue;
            };
            let answer = match plan(&root) {
                Plan::Whole => {
                    let mut fetched = uid_fetch(conn, &[target.uid], "UID BODY.PEEK[]").await?;
                    match fetched.remove(&target.uid).and_then(|found| found.whole) {
                        Some(raw) => keep(sink, target, raw, None, used).await?,
                        None => Err(BodyError::Gone),
                    }
                }
                Plan::Parts(items) => {
                    let items = format!("UID {}", items.join(" "));
                    let mut fetched = uid_fetch(conn, &[target.uid], &items).await?;
                    match fetched.remove(&target.uid) {
                        Some(found) if found.sections.contains_key("HEADER") => {
                            let raw = rebuild(&root, &found.sections);
                            keep(sink, target, raw, Some(root), used).await?
                        }
                        _ => Err(BodyError::Gone),
                    }
                }
            };
            stored += u32::from(answer.is_ok());
            done(target.message, answer);
        }
    }
    tracing::debug!(
        %folder.folder,
        asked = targets.len(),
        stored,
        ms = started.elapsed().as_secs_f64() * 1000.0,
        "bodies fetched"
    );
    Ok(())
}

/// Fetches one attachment of a message stored as its text parts, by the section the store
/// recorded (see [`Store::part_source`]), and decodes it; nothing is stored. `Gone` when the
/// server no longer has the message.
pub async fn fetch_part(
    conn: &mut Connection,
    folder: &FolderSync,
    source: &PartSource,
) -> Result<Result<FetchedPart, BodyError>, ImapError> {
    let uidvalidity = examine(conn, &folder.name_raw).await?;
    if uidvalidity.is_none() || uidvalidity != folder.uidvalidity {
        return Ok(Err(BodyError::Gone));
    }
    let header = if source.section == "TEXT" {
        "HEADER".to_owned()
    } else {
        format!("{}.MIME", source.section)
    };
    let items = format!("UID BODY.PEEK[{header}] BODY.PEEK[{}]", source.section);
    let mut fetched = uid_fetch(conn, &[source.uid], &items).await?;
    let Some(mut found) = fetched.remove(&source.uid) else {
        return Ok(Err(BodyError::Gone));
    };
    let (Some(header), Some(body)) = (
        found.sections.remove(&header),
        found.sections.remove(&source.section),
    ) else {
        return Ok(Err(BodyError::Gone));
    };
    let mut raw = with_blank_line(header);
    raw.extend_from_slice(&body);
    let data = MessageParser::default()
        .parse(raw.as_slice())
        .and_then(|parsed| parsed.parts.first().map(|part| part.contents().to_vec()))
        .unwrap_or(body);
    Ok(Ok(FetchedPart {
        mime_type: source.mime_type.clone(),
        data,
    }))
}

/// Fetches the message at `uid` of `folder` whole, the bytes as the server has them, after the
/// UIDVALIDITY check; nothing is stored and nothing is marked `\Seen`. `Gone` when the server
/// no longer has the message.
pub async fn fetch_original(
    conn: &mut Connection,
    folder: &FolderSync,
    uid: u32,
) -> Result<Result<Vec<u8>, BodyError>, ImapError> {
    let uidvalidity = examine(conn, &folder.name_raw).await?;
    if uidvalidity.is_none() || uidvalidity != folder.uidvalidity {
        return Ok(Err(BodyError::Gone));
    }
    let mut fetched = uid_fetch(conn, &[uid], "UID BODY.PEEK[]").await?;
    Ok(fetched
        .remove(&uid)
        .and_then(|found| found.whole)
        .ok_or(BodyError::Gone))
}

#[cfg(test)]
mod tests;
