//! A long reply without literals is read in time that grows with its size, not its square
//! (async-imap as patched in `vendor/async-imap`).

mod fake;

use fake::{Fake, Flavor};
use std::time::{Duration, Instant};

#[tokio::test]
async fn a_long_reply_without_literals_is_read_at_once() {
    let fake = Fake::start(Flavor::Rev1).await;
    let mut conn = fake.connect().await;
    let mut reply = String::from("* 1 FETCH (FLAGS (");
    while reply.len() < 2_000_000 {
        reply.push_str("\\a ");
    }
    reply.push_str("\\a))\r\n");
    fake.world().cut_with("NOOP", &reply);
    let started = Instant::now();
    let error = conn.noop().await.unwrap_err();
    let took = started.elapsed();
    assert!(error.is_connection(), "{error}");
    assert_eq!(fake.world().cuts, 1);
    // 1.1.6's copy took about 3 s for this in a release build, far more in a test build.
    assert!(took < Duration::from_secs(10), "{took:?}");
}
