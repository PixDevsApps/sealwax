//! The other client of the randomized suites (S1's mix, M3): resets a test account, seeds it
//! and changes it at random through the testbed client. Test data only; every message is at
//! `example.test`.

#![allow(dead_code)]

use crate::common;
use crate::snapshot::Rng;
use crate::testbed_client::{self, Client, Failure};
use sealwax_engine::imap::mutf7;

/// Decoded names. INBOX takes the most traffic; `RESET` is deleted and created again once.
pub const FOLDERS: [&str; 6] = [
    "INBOX",
    "Arkiv",
    "Papirkurv",
    "Søppelpost",
    "Prosjekter/Høst 📬 2026",
    RESET,
];
pub const RESET: &str = "Midlertidig";
pub const FLAGS: [&str; 7] = [
    "\\Seen",
    "\\Flagged",
    "\\Answered",
    "\\Draft",
    "\\Deleted",
    "$Label1",
    "Prioritert",
];

pub fn wire(name: &str) -> String {
    mutf7::encode(name)
}

pub async fn client(port: u16, user: &str) -> Client {
    Client::connect(port, &common::ca(), user)
        .await
        .unwrap_or_else(|error| panic!("test client on {port}: {error:?}"))
}

pub fn random_flags(rng: &mut Rng) -> String {
    FLAGS
        .iter()
        .filter(|_| rng.chance(25))
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

pub async fn append(
    client: &mut Client,
    name_raw: &str,
    message_id: &str,
    rng: &mut Rng,
) -> Result<(), Failure> {
    let flags = random_flags(rng);
    let message = testbed_client::message(message_id, rng);
    client.append(name_raw, &flags, &message).await
}

/// Empties every selectable mailbox of the account and makes sure the test folders exist.
pub async fn reset_account(client: &mut Client, folders: &[&str]) -> Result<(), Failure> {
    for folder in folders {
        client.create(&wire(folder)).await?;
    }
    for (name, selectable) in client.list().await? {
        if selectable {
            client.empty(&name).await?;
        }
    }
    client.select("INBOX").await?;
    Ok(())
}

pub fn pick_folder(rng: &mut Rng) -> &'static str {
    if rng.chance(40) {
        FOLDERS[0]
    } else {
        FOLDERS[1 + rng.below(FOLDERS.len() - 1)]
    }
}

pub fn other_folder(rng: &mut Rng, not: &str) -> String {
    loop {
        let target = wire(pick_folder(rng));
        if target != not {
            return target;
        }
    }
}

/// One change by the other client, S1's mix: append 20 %, flags 35 %, move 15 %, expunge 15 %,
/// copy 8 %, a flag storm over INBOX 7 %.
pub async fn mutate(
    client: &mut Client,
    rng: &mut Rng,
    seed: u64,
    n: usize,
) -> Result<&'static str, Failure> {
    let folder = wire(pick_folder(rng));
    let roll = rng.below(100);
    client.select(&folder).await?;
    let existing = client.uids().await?;
    if roll < 20 || existing.is_empty() {
        append(
            client,
            &folder,
            &format!("rand.{seed}.{n}@example.test"),
            rng,
        )
        .await?;
        return Ok("append");
    }
    let picks = |rng: &mut Rng| -> Vec<u32> {
        let n = 1 + rng.below(3.min(existing.len()));
        (0..n).map(|_| *rng.pick(&existing)).collect()
    };
    match roll {
        20..55 => {
            let uids = picks(rng);
            let sign = if rng.chance(50) { "+" } else { "-" };
            let mut flags = random_flags(rng);
            if flags.is_empty() {
                flags = (*rng.pick(&FLAGS)).to_owned();
            }
            client
                .store(&uids, &format!("{sign}FLAGS ({flags})"))
                .await?;
            Ok("flags")
        }
        55..70 => {
            let uid = *rng.pick(&existing);
            let target = other_folder(rng, &folder);
            if client.has("MOVE") {
                client
                    .run(&format!("UID MOVE {uid} {}", quote(&target)), |_| {})
                    .await?;
            } else {
                client
                    .run(&format!("UID COPY {uid} {}", quote(&target)), |_| {})
                    .await?;
                client.store(&[uid], "+FLAGS.SILENT (\\Deleted)").await?;
                client.run("EXPUNGE", |_| {}).await?;
            }
            Ok("move")
        }
        70..85 => {
            let uids = picks(rng);
            client.store(&uids, "+FLAGS.SILENT (\\Deleted)").await?;
            if client.has("UIDPLUS") {
                let set: Vec<String> = uids.iter().map(u32::to_string).collect();
                client
                    .run(&format!("UID EXPUNGE {}", set.join(",")), |_| {})
                    .await?;
            } else {
                client.run("EXPUNGE", |_| {}).await?;
            }
            Ok("expunge")
        }
        85..93 => {
            let uid = *rng.pick(&existing);
            let target = other_folder(rng, &folder);
            client
                .run(&format!("UID COPY {uid} {}", quote(&target)), |_| {})
                .await?;
            Ok("copy")
        }
        _ => {
            client.select("INBOX").await?;
            if client.uids().await?.is_empty() {
                return Ok("storm-skipped");
            }
            let sign = if rng.chance(50) { "+" } else { "-" };
            client
                .run(&format!("UID STORE 1:* {sign}FLAGS (\\Flagged)"), |_| {})
                .await?;
            Ok("flag-storm")
        }
    }
}

pub fn quote(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Deletes and creates `RESET` again, with three messages; returns the UIDVALIDITY before and
/// after.
pub async fn uidvalidity_reset(
    client: &mut Client,
    rng: &mut Rng,
    seed: u64,
) -> Result<(u32, u32), Failure> {
    let name = wire(RESET);
    let before = client.uidvalidity(&name).await?;
    client.select("INBOX").await?;
    client
        .run(&format!("DELETE {}", quote(&name)), |_| {})
        .await?;
    client
        .run(&format!("CREATE {}", quote(&name)), |_| {})
        .await?;
    for i in 0..3 {
        append(
            client,
            &name,
            &format!("rand.{seed}.reset{i}@example.test"),
            rng,
        )
        .await?;
    }
    let after = client.uidvalidity(&name).await?;
    client.select("INBOX").await?;
    Ok((before, after))
}
