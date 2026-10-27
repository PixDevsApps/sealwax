//! The POP3 client (ML-087) against a scripted server on 127.0.0.1, over TLS with a throwaway
//! CA: implicit TLS and STLS that never downgrades, CAPA's variants and the mechanism they pick,
//! SASL PLAIN, USER and PASS, XOAUTH2 with its error challenge, refusals with RFC 2449 and 3206
//! codes, UIDL and LIST, RETR's multi-line edge cases, sizes over the limit or far over LIST's,
//! timeouts, a listing too long in all or too slow, DELE, RSET, NOOP, TOP and QUIT. What reaches the log is `pop3_log.rs`'s. Synthetic data only.

mod pop3_script;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use pop3_script::*;
use sealwax_domain::account::{LoginFailure, Security};
use sealwax_engine::pop3::{
    self, Busy, Credentials, Expire, Listing, Mailbox, Mechanism, Options, Pop3Error,
};
use std::time::{Duration, Instant};

#[tokio::test]
async fn implicit_tls_logs_in_with_sasl_plain_and_an_initial_response() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.send("+OK POP3 ready <1896.697170952@example.test>\r\n")
            .await;
        peer.caps(&[
            "TOP",
            "UIDL",
            "EXPIRE NEVER",
            "LOGIN-DELAY 300",
            "SASL PLAIN XOAUTH2",
        ])
        .await;
        let auth = peer.expect("AUTH PLAIN ").await;
        assert_eq!(
            plain_response(&auth),
            format!("\0{USERNAME}\0{SECRET}").into_bytes()
        );
        peer.send("+OK Logged in.\r\n").await;
        peer.caps(&["TOP", "UIDL", "EXPIRE 30", "LOGIN-DELAY 900"])
            .await;
        peer.expect("QUIT").await;
        peer.send("+OK Logging out.\r\n").await;
        peer
    })
    .await;
    let session = connect(&ca, port, PASSWORD).await.unwrap();
    assert_eq!(session.mechanism(), Mechanism::Plain);
    let caps = session.capabilities();
    assert!(caps.top && caps.uidl);
    assert_eq!(
        caps.expire,
        Some(Expire::Days(30)),
        "CAPA after the login wins"
    );
    assert_eq!(caps.login_delay, Some(Duration::from_secs(900)));
    session.quit().await.unwrap();
    let received = server.await.unwrap();
    assert_eq!(received.len(), 4, "{received:?}");
}

#[tokio::test]
async fn stls_comes_first_and_nothing_else_goes_out_in_plaintext() {
    let ca = ca();
    let config = ca.server.clone();
    let (port, server) = serve(None, |mut peer| async move {
        peer.send("+OK POP3 ready\r\n").await;
        assert_eq!(peer.line().await.unwrap(), "STLS");
        peer.send("+OK Begin TLS negotiation now.\r\n").await;
        let plaintext = peer.received.clone();
        let mut peer = peer.tls(config).await;
        peer.received
            .push(format!("plaintext: {}", plaintext.len()));
        peer.caps(CAPS).await;
        peer.expect("AUTH PLAIN ").await;
        peer.send("+OK Logged in.\r\n").await;
        peer.caps(CAPS_AFTER).await;
        peer
    })
    .await;
    let session = pop3::connect_with(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
        quick(),
    )
    .await
    .unwrap();
    assert_eq!(session.mechanism(), Mechanism::Plain);
    drop(session);
    let received = server.await.unwrap();
    assert_eq!(received[0], "STLS");
    assert_eq!(received[1], "plaintext: 1", "only STLS before TLS");
    assert_eq!(received[2], "CAPA", "CAPA again over TLS");
}

#[tokio::test]
async fn a_refused_stls_never_falls_back_to_plaintext() {
    let ca = ca();
    let (port, server) = serve(None, |mut peer| async move {
        peer.send("+OK POP3 ready\r\n").await;
        peer.expect("STLS").await;
        peer.send("-ERR TLS not available here\r\n").await;
        peer
    })
    .await;
    let error = pop3::connect_with(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
        quick(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, Pop3Error::StartTls), "{error}");
    assert_eq!(error.failure(), LoginFailure::NoTls);
    let received = server.await.unwrap();
    assert_eq!(received, ["STLS"]);
}

#[tokio::test]
async fn plaintext_injected_after_stls_or_a_failed_handshake_ends_the_attempt() {
    let ca = ca();
    let (port, server) = serve(None, |mut peer| async move {
        peer.send("+OK POP3 ready\r\n").await;
        peer.expect("STLS").await;
        peer.send("+OK Begin TLS\r\n+OK Logged in.\r\n").await;
        peer
    })
    .await;
    let error = pop3::connect_with(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
        quick(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::NoTls, "{error}");
    assert_no_credentials(&server.await.unwrap());

    let (port, server) = serve(None, |mut peer| async move {
        peer.send("+OK POP3 ready\r\n").await;
        peer.expect("STLS").await;
        peer.send("+OK Begin TLS\r\n").await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        peer.send("this is not a TLS handshake\r\n").await;
        peer
    })
    .await;
    let error = pop3::connect_with(
        &endpoint(port, Security::StartTls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
        quick(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.failure(), LoginFailure::NoTls, "{error}");
    assert_no_credentials(&server.await.unwrap());
}

#[tokio::test]
async fn a_certificate_from_another_ca_is_refused_before_anything_is_sent() {
    let ca = ca();
    let other = self::ca();
    let (port, server) = serve(Some(other.server.clone()), |peer| async move { peer }).await;
    let error = connect(&ca, port, PASSWORD).await.unwrap_err();
    assert_eq!(error.failure(), LoginFailure::Certificate, "{error}");
    drop(server);
}

#[tokio::test]
async fn capa_picks_the_mechanism() {
    // (CAPA before the login or `None` for `-ERR`, what the client must send first after it).
    let cases: &[(Option<&[&str]>, &str)] = &[
        (Some(&["SASL PLAIN", "USER"]), "AUTH PLAIN "),
        (Some(&["sasl login plain"]), "AUTH PLAIN "),
        (Some(&["USER", "SASL CRAM-MD5"]), "USER "),
        (Some(&["TOP", "UIDL"]), "USER "),
        (None, "USER "),
    ];
    for (caps, first) in cases {
        let ca = ca();
        let caps: Option<Vec<String>> = caps.map(|c| c.iter().map(|s| s.to_string()).collect());
        let first = first.to_string();
        let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
            peer.send("+OK ready\r\n").await;
            match &caps {
                Some(caps) => {
                    let caps: Vec<&str> = caps.iter().map(String::as_str).collect();
                    peer.caps(&caps).await;
                }
                None => {
                    peer.expect("CAPA").await;
                    peer.send("-ERR unknown command\r\n").await;
                }
            }
            if first == "USER " {
                assert_eq!(peer.line().await.unwrap(), format!("USER {USERNAME}"));
                peer.send("+OK\r\n").await;
                assert_eq!(peer.line().await.unwrap(), format!("PASS {SECRET}"));
            } else {
                peer.expect(&first).await;
            }
            peer.send("+OK in\r\n").await;
            peer.expect("CAPA").await;
            peer.send("-ERR not now\r\n").await;
            peer
        })
        .await;
        let session = connect(&ca, port, PASSWORD).await.unwrap();
        drop(session);
        server.await.unwrap();
    }
}

#[tokio::test]
async fn a_server_that_offers_only_oauth_or_unknown_mechanisms_gets_no_password() {
    for (caps, failure) in [
        ("SASL XOAUTH2 OAUTHBEARER", LoginFailure::OAuthRequired),
        ("SASL CRAM-MD5", LoginFailure::Other),
    ] {
        let ca = ca();
        let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
            peer.hello(&["TOP", "UIDL", caps]).await;
            peer
        })
        .await;
        let error = connect(&ca, port, PASSWORD).await.unwrap_err();
        assert_eq!(error.failure(), failure, "{caps}: {error}");
        let received = server.await.unwrap();
        assert_eq!(received, ["CAPA"], "{caps}");
    }
}

#[tokio::test]
async fn credentials_with_line_breaks_are_never_sent() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.hello(&["USER"]).await;
        peer
    })
    .await;
    let injected = Credentials::Password {
        username: "kari\r\nDELE 1",
        password: SECRET,
    };
    let error = connect(&ca, port, injected).await.unwrap_err();
    assert!(matches!(
        error,
        Pop3Error::Rejected(LoginFailure::WrongPassword)
    ));
    assert_eq!(server.await.unwrap(), ["CAPA"]);
}

#[tokio::test]
async fn xoauth2_goes_after_a_continuation_and_an_error_challenge_is_answered() {
    let token = "t".repeat(1200);
    let ca = ca();
    let expected = format!("user={USERNAME}\x01auth=Bearer {token}\x01\x01");
    let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
        peer.hello(&["UIDL", "SASL PLAIN XOAUTH2"]).await;
        assert_eq!(peer.line().await.unwrap(), "AUTH XOAUTH2");
        peer.send("+ \r\n").await;
        let response = STANDARD.decode(peer.line().await.unwrap()).unwrap();
        assert_eq!(response, expected.as_bytes());
        peer.send("+ eyJzdGF0dXMiOiI0MDEiLCJzY2hlbWVzIjoiYmVhcmVyIn0=\r\n")
            .await;
        assert_eq!(peer.line().await.unwrap(), "");
        peer.send("-ERR [AUTH] Invalid credentials\r\n").await;
        peer
    })
    .await;
    let oauth = Credentials::OAuth2 {
        username: USERNAME,
        token: &token,
    };
    let error = connect(&ca, port, oauth).await.unwrap_err();
    assert!(
        matches!(error, Pop3Error::Rejected(LoginFailure::WrongPassword)),
        "{error}"
    );
    server.await.unwrap();

    let ca = self::ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.hello(&["UIDL", "SASL XOAUTH2"]).await;
        let auth = peer.expect("AUTH XOAUTH2 ").await;
        let response = STANDARD
            .decode(auth.trim_start_matches("AUTH XOAUTH2 "))
            .unwrap();
        assert_eq!(response, b"user=kari.nordmann\x01auth=Bearer short\x01\x01");
        peer.send("+OK User successfully authenticated.\r\n").await;
        peer.caps(&["UIDL"]).await;
        peer
    })
    .await;
    let short = Credentials::OAuth2 {
        username: USERNAME,
        token: "short",
    };
    let session = connect(&ca, port, short).await.unwrap();
    assert_eq!(session.mechanism(), Mechanism::XOAuth2);
    drop(session);
    server.await.unwrap();

    let ca = self::ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.hello(&["USER", "SASL PLAIN"]).await;
        peer
    })
    .await;
    let error = connect(&ca, port, short).await.unwrap_err();
    assert!(matches!(error, Pop3Error::NoMechanism), "{error}");
    assert_eq!(server.await.unwrap(), ["CAPA"]);
}

#[tokio::test]
async fn refusals_are_classified_and_their_text_is_dropped() {
    let cases = [
        (
            "-ERR [AUTH] Authentication failed for kari.nordmann@example.test",
            "Rejected(WrongPassword)",
        ),
        (
            "-ERR [IN-USE] Mailbox is locked by another session",
            "Busy(InUse)",
        ),
        (
            "-ERR [LOGIN-DELAY] Logged in too recently",
            "Busy(LoginDelay(Some(900s)))",
        ),
        ("-ERR [SYS/TEMP] Try again later", "Busy(Temporary)"),
        (
            "-ERR [SYS/PERM] Your account is not enabled for POP access.",
            "Rejected(ImapDisabled)",
        ),
        (
            "-ERR [AUTH] Application-specific password required",
            "Rejected(AppPasswordRequired)",
        ),
        ("-ERR Unable to lock maildrop", "Busy(InUse)"),
        (
            "-ERR Logon failure: unknown user name or bad password.",
            "Rejected(WrongPassword)",
        ),
    ];
    for (refusal, expected) in cases {
        let ca = ca();
        let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
            peer.hello(&["USER", "RESP-CODES", "AUTH-RESP-CODE", "LOGIN-DELAY 900"])
                .await;
            peer.expect("USER ").await;
            peer.send("+OK\r\n").await;
            peer.expect("PASS ").await;
            peer.send(&format!("{refusal}\r\n")).await;
            peer
        })
        .await;
        let error = connect(&ca, port, PASSWORD).await.unwrap_err();
        assert_eq!(format!("{error:?}"), expected, "{refusal}");
        let shown = format!("{error} {error:?}");
        assert!(
            !shown.contains("kari") && !shown.contains("session"),
            "{shown}"
        );
        server.await.unwrap();
    }
}

#[tokio::test]
async fn a_refused_or_garbled_greeting_ends_the_attempt() {
    for (greeting, expected) in [
        (
            "-ERR [SYS/TEMP] Too many connections\r\n",
            "Busy(Temporary)",
        ),
        ("-ERR Go away\r\n", "Err"),
        ("* OK IMAP4rev1 ready\r\n", "Protocol(\"unexpected reply\")"),
    ] {
        let ca = ca();
        let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
            peer.send(greeting).await;
            peer
        })
        .await;
        let error = connect(&ca, port, PASSWORD).await.unwrap_err();
        assert_eq!(format!("{error:?}"), expected);
        assert!(server.await.unwrap().is_empty());
    }
}

#[tokio::test]
async fn a_silent_server_times_out_at_connect() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |peer| async move {
        tokio::time::sleep(Duration::from_secs(2)).await;
        peer
    })
    .await;
    let started = Instant::now();
    let options = Options {
        connect_timeout: Duration::from_millis(300),
        ..quick()
    };
    let error = pop3::connect_with(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
        options,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, Pop3Error::Timeout), "{error}");
    assert_eq!(error.failure(), LoginFailure::Timeout);
    assert!(started.elapsed() < Duration::from_secs(2));
    server.await.unwrap();
}

#[tokio::test]
async fn uidl_and_list_make_the_listing_and_retr_uses_its_sizes() {
    let ca = ca();
    let message = b"Subject: one\r\n\r\nbody\r\n".to_vec();
    let sent = message.clone();
    let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
        peer.logged_in().await;
        peer.expect("UIDL").await;
        peer.send("+OK\r\n1 000000016ac391ba\r\n2 XyZ-~!\r\n3 16573596181\r\n.\r\n")
            .await;
        peer.expect("LIST").await;
        peer.send("+OK 3 messages (120 octets)\r\n1 22\r\n2 40 extra\r\n3 58\r\n.\r\n")
            .await;
        assert_eq!(peer.line().await.unwrap(), "RETR 1");
        peer.send_bytes(&wire_form(&sent)).await;
        assert_eq!(peer.line().await.unwrap(), "LIST 9");
        peer.send("-ERR no such message\r\n").await;
        peer.expect("NOOP").await;
        peer.send("+OK\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    let listing = session.listing().await.unwrap();
    let expected = [
        (1, "000000016ac391ba", 22),
        (2, "XyZ-~!", 40),
        (3, "16573596181", 58),
    ]
    .map(|(number, uidl, size)| Listing {
        number,
        uidl: uidl.to_owned(),
        size,
    });
    assert_eq!(listing, expected);
    assert!(!format!("{listing:?}").contains("16ac391ba"));
    assert_eq!(session.retrieve(1).await.unwrap(), message);
    assert!(matches!(session.retrieve(9).await, Err(Pop3Error::Err)));
    session.noop().await.unwrap();
    drop(session);
    let received = server.await.unwrap();
    assert!(!received.iter().any(|line| line.starts_with("LIST 1")));
}

#[tokio::test]
async fn retrieve_without_a_listing_asks_list_for_the_size() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        assert_eq!(peer.line().await.unwrap(), "LIST 2");
        peer.send("+OK 2 9\r\n").await;
        assert_eq!(peer.line().await.unwrap(), "RETR 2");
        peer.send("+OK\r\nmessage\r\n.\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    assert_eq!(session.retrieve(2).await.unwrap(), b"message\r\n");
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn a_server_without_uidl_is_refused() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        peer.expect("UIDL").await;
        peer.send("-ERR unknown command\r\n").await;
        peer.expect("UIDL").await;
        peer.send("-ERR [SYS/TEMP] index busy\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    assert!(matches!(session.listing().await, Err(Pop3Error::NoUidl)));
    assert!(matches!(
        session.listing().await,
        Err(Pop3Error::Busy(Busy::Temporary))
    ));
    drop(session);
    server.await.unwrap();

    // The login check asks UIDL itself when CAPA doesn't list it.
    let ca = self::ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.hello(&["USER", "SASL PLAIN"]).await;
        peer.expect("AUTH PLAIN ").await;
        peer.send("+OK\r\n").await;
        peer.caps(&["TOP"]).await;
        peer.expect("UIDL").await;
        peer.send("-ERR no\r\n").await;
        peer
    })
    .await;
    let failure = pop3::check_login(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap_err();
    assert_eq!(failure, LoginFailure::NoUidl);
    let received = server.await.unwrap();
    assert!(!received.iter().any(|line| line == "QUIT"));

    let ca = self::ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.hello(&["SASL PLAIN"]).await;
        peer.expect("AUTH PLAIN ").await;
        peer.send("+OK\r\n").await;
        peer.expect("CAPA").await;
        peer.send("-ERR\r\n").await;
        peer.expect("UIDL").await;
        peer.send("+OK\r\n1 a\r\n.\r\n").await;
        peer.expect("QUIT").await;
        peer.send("+OK\r\n").await;
        peer
    })
    .await;
    pop3::check_login(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
    )
    .await
    .unwrap();
    let received = server.await.unwrap();
    assert!(
        !received
            .iter()
            .any(|line| line.starts_with("RETR") || line.starts_with("DELE"))
    );
}

#[tokio::test]
async fn listings_that_disagree_are_refused_and_shared_uidls_left_out() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        peer.expect("UIDL").await;
        peer.send("+OK\r\n1 a\r\n2 b\r\n.\r\n").await;
        peer.expect("LIST").await;
        peer.send("+OK\r\n1 10\r\n.\r\n").await;
        peer.expect("UIDL").await;
        peer.send("+OK\r\n1 a\r\n2 dup\r\n3 c\r\n4 dup\r\n.\r\n")
            .await;
        peer.expect("LIST").await;
        peer.send("+OK\r\n1 10\r\n2 20\r\n3 30\r\n4 40\r\n.\r\n")
            .await;
        peer.expect("UIDL").await;
        peer.send("+OK\r\n1 has space\r\n.\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    assert!(matches!(
        session.listing().await,
        Err(Pop3Error::Protocol("UIDL and LIST disagree"))
    ));
    let listing = session.listing().await.unwrap();
    let kept: Vec<(u32, &str)> = listing
        .iter()
        .map(|entry| (entry.number, entry.uidl.as_str()))
        .collect();
    assert_eq!(kept, [(1, "a"), (3, "c")]);
    assert!(matches!(
        session.listing().await,
        Err(Pop3Error::Protocol(_))
    ));
    assert!(
        matches!(session.noop().await, Err(Pop3Error::ConnectionLost)),
        "a protocol error ends the session"
    );
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn retr_keeps_every_byte_and_line_end_and_undoes_dot_stuffing() {
    let message: &[u8] = b"Subject: =?utf-8?q?bl=C3=A5b=C3=A6r?=\r\nContent-Type: text/plain; charset=latin1\r\n\r\n\
        .\r\n\
        ..two dots\r\n\
        .leading dot\r\n\
        bare\nline feeds\n\
        latin \xe6\xf8\xe5 and utf-8 \xc3\xa6\xc3\xb8\xc3\xa5\r\n\
        nul \x00 and controls \x01\x7f\r\n\
        \r\n\
        last line\r\n";
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
        peer.logged_in().await;
        answer_listing(&mut peer, &[message.len() as u64]).await;
        peer.expect("RETR 1").await;
        peer.send_bytes(&wire_form(message)).await;
        // A dot followed by a bare LF isn't the end (RFC 1939, 3): the dot is dropped.
        peer.expect("RETR 1").await;
        peer.send("+OK\r\n.\nstill the message\r\n.\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.listing().await.unwrap();
    assert_eq!(session.retrieve(1).await.unwrap(), message);
    assert_eq!(
        session.retrieve(1).await.unwrap(),
        b"\nstill the message\r\n"
    );
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn a_message_cut_mid_retr_is_lost_not_truncated() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        answer_listing(&mut peer, &[5000]).await;
        peer.expect("RETR 1").await;
        peer.send("+OK\r\nSubject: cut\r\n\r\nthe first half of the bo")
            .await;
        // Closed here, mid-line, as a dropped connection.
        peer.hang_up()
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.listing().await.unwrap();
    let error = session.retrieve(1).await.unwrap_err();
    assert!(matches!(error, Pop3Error::ConnectionLost), "{error}");
    assert!(error.is_connection());
    assert!(matches!(
        session.quit().await,
        Err(Pop3Error::ConnectionLost)
    ));
    let received = server.await.unwrap();
    assert!(!received.iter().any(|line| line == "QUIT"));
}

#[tokio::test]
async fn a_server_that_goes_silent_mid_retr_times_out() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        answer_listing(&mut peer, &[5000]).await;
        peer.expect("RETR 1").await;
        peer.send("+OK\r\nSubject: slow\r\n\r\npart").await;
        tokio::time::sleep(Duration::from_secs(2)).await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.listing().await.unwrap();
    let started = Instant::now();
    assert!(matches!(session.retrieve(1).await, Err(Pop3Error::Timeout)));
    assert!(started.elapsed() < Duration::from_millis(1500));
    assert!(matches!(
        session.noop().await,
        Err(Pop3Error::ConnectionLost)
    ));
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn messages_over_the_limit_are_not_downloaded() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        answer_listing(&mut peer, &[2000, 100]).await;
        // Message 1 is never asked for; message 2 turns out larger than LIST said.
        peer.expect("TOP 1 0").await;
        peer.send("+OK\r\nSubject: big\r\n\r\n.\r\n").await;
        peer.expect("RETR 2").await;
        peer.send("+OK\r\n").await;
        for _ in 0..30 {
            peer.send("0123456789012345678901234567890123456789\r\n")
                .await;
        }
        peer.send(".\r\n").await;
        peer
    })
    .await;
    let options = Options {
        max_message: 1000,
        ..quick()
    };
    let mut session = pop3::connect_with(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
        options,
    )
    .await
    .unwrap();
    session.listing().await.unwrap();
    assert!(matches!(
        session.retrieve(1).await,
        Err(Pop3Error::TooLarge(2000))
    ));
    assert_eq!(
        session.header(1).await.unwrap(),
        b"Subject: big\r\n\r\n",
        "the session goes on after a size from LIST"
    );
    let error = session.retrieve(2).await.unwrap_err();
    assert!(
        matches!(error, Pop3Error::TooLarge(n) if n > 1000),
        "{error}"
    );
    assert!(matches!(
        session.noop().await,
        Err(Pop3Error::ConnectionLost)
    ));
    drop(session);
    let received = server.await.unwrap();
    assert!(!received.iter().any(|line| line == "RETR 1"));
}

#[tokio::test]
async fn a_huge_listing() {
    const COUNT: u64 = 100_000;
    let sizes: Vec<u64> = (0..COUNT).map(|i| 1000 + i % 5000).collect();
    let (uidl, list) = listing_replies(&sizes);
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
        peer.logged_in().await;
        peer.expect("UIDL").await;
        peer.send(&uidl).await;
        peer.expect("LIST").await;
        peer.send(&list).await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    let started = Instant::now();
    let listing = session.listing().await.unwrap();
    let elapsed = started.elapsed();
    assert_eq!(listing.len(), COUNT as usize);
    assert_eq!(listing[99_999].uidl, "uid-100000");
    assert_eq!(listing[99_999].size, sizes[99_999]);
    println!(
        "listing of {COUNT}: {:.1} ms ({} build, localhost)",
        elapsed.as_secs_f64() * 1000.0,
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn retr_may_send_up_to_twice_list_s_size_and_no_more() {
    let line = format!("{}\r\n", "x".repeat(99));
    let within = line.repeat(1_970).into_bytes();
    let beyond = line.repeat(700).into_bytes();
    let sent = within.clone();
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
        peer.logged_in().await;
        answer_listing(&mut peer, &[100_000, 100]).await;
        peer.expect("RETR 1").await;
        peer.send_bytes(&wire_form(&sent)).await;
        peer.expect("RETR 2").await;
        peer.send_bytes(&wire_form(&beyond)).await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.listing().await.unwrap();
    assert_eq!(
        session.retrieve(1).await.unwrap(),
        within,
        "1.97 times LIST's size, as from a server that counts a line end as one octet"
    );
    let error = session.retrieve(2).await.unwrap_err();
    let limit = 2 * 100 + 64 * 1024;
    assert!(
        matches!(error, Pop3Error::TooLarge(n) if n > limit && n < 70_700),
        "{error}"
    );
    assert!(matches!(
        session.noop().await,
        Err(Pop3Error::ConnectionLost)
    ));
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn a_listing_too_long_in_all_is_refused() {
    let sizes = vec![1000; 500];
    let (uidl, _) = listing_replies(&sizes);
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
        peer.logged_in().await;
        peer.expect("UIDL").await;
        peer.send(&uidl).await;
        peer
    })
    .await;
    let options = Options {
        max_listing: 2_000,
        ..quick()
    };
    let mut session = pop3::connect_with(
        &endpoint(port, Security::Tls),
        PASSWORD,
        ca.trust.client_config().unwrap(),
        options,
    )
    .await
    .unwrap();
    let error = session.listing().await.unwrap_err();
    assert!(
        matches!(error, Pop3Error::Protocol("reply too long")),
        "{error}"
    );
    assert!(matches!(
        session.noop().await,
        Err(Pop3Error::ConnectionLost)
    ));
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn a_listing_that_trickles_ends_at_its_deadline() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        peer.expect("UIDL").await;
        peer.send("+OK\r\n").await;
        for n in 1..=30 {
            if !peer.try_send(&format!("{n} uid-{n}\r\n")).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    let started = Instant::now();
    let error = session.listing().await.unwrap_err();
    assert!(matches!(error, Pop3Error::Timeout), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "a line every 200 ms, each within the 500 ms wait, for 6 s in all"
    );
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn dele_rset_noop_and_quit() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        peer.expect("DELE 1").await;
        peer.send("+OK message 1 deleted\r\n").await;
        peer.expect("DELE 1").await;
        peer.send("-ERR message 1 already deleted\r\n").await;
        peer.expect("RSET").await;
        peer.send("+OK\r\n").await;
        peer.expect("DELE 2").await;
        peer.send("-ERR [SYS/TEMP] can't now\r\n").await;
        peer.expect("NOOP").await;
        peer.send("+OK\r\n").await;
        peer.expect("QUIT").await;
        peer.send("+OK 1 message deleted\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.delete(1).await.unwrap();
    assert!(matches!(session.delete(1).await, Err(Pop3Error::Err)));
    session.reset().await.unwrap();
    assert!(matches!(
        session.delete(2).await,
        Err(Pop3Error::Busy(Busy::Temporary))
    ));
    session.noop().await.unwrap();
    session.quit().await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn quit_that_fails_says_so() {
    // -ERR: some marked messages weren't deleted (RFC 1939, 6).
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        peer.expect("DELE 1").await;
        peer.send("+OK\r\n").await;
        peer.expect("QUIT").await;
        peer.send("-ERR some deleted messages not removed\r\n")
            .await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.delete(1).await.unwrap();
    assert!(matches!(session.quit().await, Err(Pop3Error::Err)));
    server.await.unwrap();

    // No answer: the deletions may or may not have happened.
    for close in [true, false] {
        let ca = self::ca();
        let (port, server) = serve(Some(ca.server.clone()), move |mut peer| async move {
            peer.logged_in().await;
            peer.expect("DELE 1").await;
            peer.send("+OK\r\n").await;
            peer.expect("QUIT").await;
            if close {
                return peer.hang_up();
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
            peer
        })
        .await;
        let mut session = connect(&ca, port, PASSWORD).await.unwrap();
        session.delete(1).await.unwrap();
        let error = session.quit().await.unwrap_err();
        if close {
            assert!(matches!(error, Pop3Error::ConnectionLost), "{error}");
        } else {
            assert!(matches!(error, Pop3Error::Timeout), "{error}");
        }
        server.await.unwrap();
    }
}

#[tokio::test]
async fn a_busy_retr_leaves_the_session_usable() {
    let ca = ca();
    let (port, server) = serve(Some(ca.server.clone()), |mut peer| async move {
        peer.logged_in().await;
        answer_listing(&mut peer, &[10]).await;
        peer.expect("RETR 1").await;
        peer.send("-ERR [SYS/TEMP] message can't be read now\r\n")
            .await;
        peer.expect("RETR 1").await;
        peer.send("+OK\r\n0123456\r\n.\r\n").await;
        peer
    })
    .await;
    let mut session = connect(&ca, port, PASSWORD).await.unwrap();
    session.listing().await.unwrap();
    assert!(matches!(
        session.retrieve(1).await,
        Err(Pop3Error::Busy(Busy::Temporary))
    ));
    assert_eq!(session.retrieve(1).await.unwrap(), b"0123456\r\n");
    drop(session);
    server.await.unwrap();
}

#[tokio::test]
async fn a_mailbox_without_top_answers_err_for_a_header() {
    struct Memory;
    impl Mailbox for Memory {
        async fn listing(&mut self) -> Result<Vec<Listing>, Pop3Error> {
            Ok(Vec::new())
        }
        async fn retrieve(&mut self, _: u32) -> Result<Vec<u8>, Pop3Error> {
            Err(Pop3Error::Err)
        }
        async fn delete(&mut self, _: u32) -> Result<(), Pop3Error> {
            Ok(())
        }
        async fn quit(self) -> Result<(), Pop3Error> {
            Ok(())
        }
    }
    assert!(matches!(Memory.header(1).await, Err(Pop3Error::Err)));
}
