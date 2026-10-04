//! The "other client" of background mode: a direct connection to the testbed (not through the
//! relay) that empties INBOX and appends synthetic messages.

use anyhow::Result;
use email_spike_s1::net::Connection;
use email_spike_s1::rng::Rng;
use email_spike_s1::testbed::{self, Server};
use email_spike_s1::{ops, raw, synth};
use email_spike_s6::clock::mono_ns;

pub struct Appender {
    connection: Connection,
    rng: Rng,
}

impl Appender {
    pub async fn connect(server: Server, user: &str) -> Result<Self> {
        let mut account = testbed::account(server, user)?;
        account.connect.enable_qresync = false;
        account.connect.send_id = false;
        Ok(Self {
            connection: account.connect().await?,
            rng: Rng::new(6),
        })
    }

    /// Deletes everything in INBOX.
    pub async fn empty_inbox(&mut self) -> Result<u32> {
        let view = ops::select(&mut self.connection, "INBOX").await?;
        if view.exists > 0 {
            raw::run(
                &mut self.connection.session,
                "UID STORE 1:* +FLAGS.SILENT (\\Deleted)",
                |_| {},
            )
            .await?;
            raw::run(&mut self.connection.session, "EXPUNGE", |_| {}).await?;
        }
        Ok(view.exists)
    }

    /// APPENDs one synthetic message; returns the monotonic time of the tagged OK.
    pub async fn append(&mut self, message_id: &str) -> Result<i64> {
        let message = synth::message(message_id, &mut self.rng);
        let literal_plus = self.connection.caps.has("LITERAL+");
        raw::append(
            &mut self.connection.session,
            "INBOX",
            "",
            &message,
            literal_plus,
            |_| {},
        )
        .await?;
        Ok(mono_ns())
    }

    pub async fn count(&mut self) -> Result<u32> {
        Ok(ops::select(&mut self.connection, "INBOX").await?.exists)
    }

    pub async fn logout(self) {
        self.connection.logout().await;
    }
}
