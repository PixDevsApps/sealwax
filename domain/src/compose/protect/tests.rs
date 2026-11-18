use super::*;
use crate::compose::mime;
use crate::model::Address;
use crate::pgp::{self, UserId, Validity, Work};

const ADA: &str = "AAAA000000000000000000000000000000000001";
const BOB: &str = "BBBB000000000000000000000000000000000002";
const BOB_TOO: &str = "BBBB000000000000000000000000000000000003";
const CARL: &str = "CCCC000000000000000000000000000000000004";

fn key(fingerprint: &str, user_id: &str, validity: Validity) -> Key {
    Key {
        fingerprint: fingerprint.to_owned(),
        validity,
        can_encrypt: true,
        can_sign: true,
        disabled: false,
        secret: false,
        user_ids: vec![UserId {
            text: user_id.to_owned(),
            validity,
        }],
    }
}

fn draft(protection: Protection) -> Draft {
    Draft {
        from: Address::new("Ada", "ada@example.com"),
        to: vec![Address::new("Bob", "Bob@Example.com")],
        cc: vec![Address::new("Carl", "carl@example.com")],
        bcc: vec![Address::new("Dora", "dora@example.com")],
        subject: "Plans".into(),
        html: "<div>The plans.</div>".into(),
        message_id: "abc@example.com".into(),
        protection,
        ..Draft::default()
    }
}

fn mine() -> Key {
    Key {
        secret: true,
        ..key(ADA, "Ada <ada@example.com>", Validity::Ultimate)
    }
}

#[test]
fn a_verified_key_is_taken_over_unverified_ones() {
    let keys = vec![
        key(BOB, "Bob <bob@example.com>", Validity::Unknown),
        key(BOB_TOO, "Bob <bob@example.com>", Validity::Full),
    ];
    assert!(matches!(
        recipient_key("bob@example.com", &keys, None),
        RecipientKey::Chosen(Candidate { ref fingerprint, verified: true, .. }) if fingerprint == BOB_TOO
    ));
}

#[test]
fn several_unverified_keys_are_picked_by_the_user() {
    let keys = vec![
        key(BOB, "Bob <bob@example.com>", Validity::Unknown),
        key(BOB_TOO, "Bob <bob@example.com>", Validity::Marginal),
    ];
    let RecipientKey::Choose(candidates) = recipient_key("bob@example.com", &keys, None) else {
        panic!("not a choice");
    };
    assert_eq!(candidates.len(), 2);
    assert!(matches!(
        recipient_key("bob@example.com", &keys, Some(&BOB_TOO.to_lowercase())),
        RecipientKey::Chosen(Candidate { ref fingerprint, .. }) if fingerprint == BOB_TOO
    ));
    assert!(
        matches!(
            recipient_key("bob@example.com", &keys, Some(CARL)),
            RecipientKey::Choose(_)
        ),
        "a pick that is no longer a candidate doesn't count"
    );
}

#[test]
fn unusable_keys_are_never_candidates() {
    let mut expired = key(BOB, "Bob <bob@example.com>", Validity::Expired);
    expired.user_ids[0].validity = Validity::Unknown;
    let mut no_encryption = key(BOB_TOO, "Bob <bob@example.com>", Validity::Full);
    no_encryption.can_encrypt = false;
    let mut disabled = key(CARL, "Bob <bob@example.com>", Validity::Full);
    disabled.disabled = true;
    let distrusted = key(ADA, "Bob <bob@example.com>", Validity::Never);
    let mut revoked_id = key(ADA, "Bob <bob@example.com>", Validity::Full);
    revoked_id.user_ids[0].validity = Validity::Revoked;
    for keys in [
        vec![expired],
        vec![no_encryption],
        vec![disabled],
        vec![distrusted],
        vec![revoked_id],
        vec![key(BOB, "Bob <bob@other.example>", Validity::Full)],
    ] {
        assert_eq!(
            recipient_key("bob@example.com", &keys, None),
            RecipientKey::Missing
        );
    }
}

#[test]
fn the_own_key_is_the_configured_one_or_the_only_one_for_from() {
    let sign = Protection {
        sign: true,
        ..Protection::default()
    };
    let other = Key {
        secret: true,
        ..key(CARL, "Ada at work <ada@example.com>", Validity::Ultimate)
    };
    assert_eq!(
        own_key("ADA@example.com", &[mine()], None, &sign).as_deref(),
        Ok(ADA)
    );
    assert_eq!(
        own_key("ada@example.com", &[mine(), other.clone()], None, &sign),
        Err(OwnKeyProblem::Several)
    );
    assert_eq!(
        own_key(
            "ada@example.com",
            &[mine(), other],
            Some(&CARL.to_lowercase()),
            &sign
        )
        .as_deref(),
        Ok(CARL)
    );
    assert_eq!(
        own_key("ada@example.com", &[mine()], Some(BOB), &sign),
        Err(OwnKeyProblem::Missing)
    );
    assert_eq!(
        own_key("eve@example.com", &[mine()], None, &sign),
        Err(OwnKeyProblem::Missing)
    );
    let stub = Key {
        secret: false,
        ..mine()
    };
    assert_eq!(
        own_key("ada@example.com", &[stub], None, &sign),
        Err(OwnKeyProblem::Missing),
        "a public key alone can't sign"
    );
    let mut sign_only = mine();
    sign_only.can_encrypt = false;
    let encrypt = Protection {
        encrypt: true,
        ..Protection::default()
    };
    assert_eq!(
        own_key("ada@example.com", &[sign_only], None, &encrypt),
        Err(OwnKeyProblem::Unusable)
    );
}

#[test]
fn the_plan_hides_bcc_keys_and_adds_the_own_key() {
    let public = vec![
        key(BOB, "Bob <bob@example.com>", Validity::Full),
        key(CARL, "Carl <carl@example.com>", Validity::Unknown),
        key(BOB_TOO, "Dora <dora@example.com>", Validity::Full),
        mine(),
    ];
    let both = draft(Protection {
        sign: true,
        encrypt: true,
        ..Protection::default()
    });
    let plan =
        plan(&both, &public, &[mine()], None).unwrap_or_else(|problem| panic!("{problem:?}"));
    assert_eq!(plan.signer.as_deref(), Some(ADA));
    assert_eq!(plan.recipients, vec![BOB, CARL, ADA]);
    assert_eq!(plan.hidden, vec![BOB_TOO]);
    assert_eq!(plan.unverified, vec!["carl@example.com"]);

    let missing = super::plan(&both, &public[..1], &[mine()], None);
    assert_eq!(
        missing,
        Err(Problem::Missing(vec![
            "carl@example.com".into(),
            "dora@example.com".into()
        ]))
    );
    let sign_only = draft(Protection {
        sign: true,
        ..Protection::default()
    });
    let plan = super::plan(&sign_only, &[], &[mine()], None).unwrap_or_else(|_| panic!());
    assert!(plan.recipients.is_empty() && plan.signer.is_some());
    assert_eq!(
        super::plan(&sign_only, &[], &[], None),
        Err(Problem::OwnKey(OwnKeyProblem::Missing))
    );
    assert_eq!(
        super::plan(&draft(Protection::default()), &[], &[], None),
        Ok(Plan::default())
    );
}

#[test]
fn picked_keys_survive_the_store() {
    let mut protection = Protection::default();
    protection.pick("Bob@Example.com", &BOB.to_lowercase());
    protection.pick("bob@example.com", BOB_TOO);
    protection.pick("carl@example.com", CARL);
    assert_eq!(protection.picked_for("BOB@example.com"), Some(BOB_TOO));
    let stored = protection.encode_picked();
    assert_eq!(Protection::decode_picked(&stored), protection.picked);
    assert!(Protection::decode_picked("nonsense\nbob@example.com XYZ").is_empty());
    assert!(!format!("{protection:?}").contains("example"));
}

fn built() -> Vec<u8> {
    let mut draft = draft(Protection::default());
    draft.bcc.clear();
    draft.message_id = "abc@example.com".into();
    mime::build(&draft, 1_791_115_200, &[])
        .unwrap_or_else(|error| panic!("{error}"))
        .transmitted
}

#[test]
fn the_content_is_cut_from_the_outer_fields() {
    let message = built();
    let split = split(&message).unwrap_or_else(|| panic!("no header end"));
    let outer = String::from_utf8_lossy(&split.outer);
    let entity = String::from_utf8_lossy(&split.entity);
    assert!(outer.contains("Subject: Plans\r\n"), "{outer}");
    assert!(outer.contains("Message-ID: <abc@example.com>"));
    assert!(!outer.to_ascii_lowercase().contains("content-type"));
    assert!(!outer.contains("MIME-Version"));
    assert!(
        entity.starts_with("Content-Type: multipart/alternative"),
        "{entity}"
    );
    assert!(entity.contains("The plans."));
}

#[test]
fn a_signed_message_hands_the_reader_exactly_what_was_signed() {
    let split = split(&built()).unwrap_or_else(|| panic!());
    let signature = b"-----BEGIN PGP SIGNATURE-----\r\n\r\nabc\r\n-----END PGP SIGNATURE-----\r\n";
    let boundary = boundary([7; 16]);
    let raw = signed(&split, signature, "pgp-sha512", &boundary).unwrap_or_else(|| panic!());
    let Some(Work::Verify {
        signed: checked,
        signature: read,
    }) = pgp::work(&raw)
    else {
        panic!("the reader doesn't see a signature");
    };
    assert_eq!(checked, signed_bytes(&split));
    assert_eq!(read.trim_ascii_end(), signature.trim_ascii_end());
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("Subject: Plans"));
    assert!(text.contains("micalg=pgp-sha512"));
    assert_eq!(
        signed(&split, b"x", "pgp-sha512", "Content-Type"),
        None,
        "a boundary found in the content is refused"
    );
}

#[test]
fn an_encrypted_message_hands_the_reader_its_ciphertext() {
    let split = split(&built()).unwrap_or_else(|| panic!());
    let ciphertext = b"-----BEGIN PGP MESSAGE-----\r\n\r\nhQEM\r\n-----END PGP MESSAGE-----";
    let raw = encrypted(&split, ciphertext, &boundary([9; 16])).unwrap_or_else(|| panic!());
    let Some(Work::Decrypt(read)) = pgp::work(&raw) else {
        panic!("the reader doesn't see ciphertext");
    };
    assert_eq!(read.trim_ascii_end(), &ciphertext[..]);
    let text = String::from_utf8_lossy(&raw);
    assert!(text.contains("Subject: Plans"), "the Subject stays visible");
    assert!(!text.contains("The plans."), "the content doesn't");
}

#[test]
fn replies_to_encrypted_mail_start_encrypted() {
    let none = Protection::default();
    let reply = for_reply(&none, true, true);
    assert!(reply.encrypt && reply.sign);
    let reply = for_reply(&none, true, false);
    assert!(reply.encrypt && !reply.sign);
    let reply = for_reply(&none, false, true);
    assert!(
        !reply.any(),
        "a signed message alone doesn't change the defaults"
    );
    let always_sign = Protection {
        sign: true,
        ..Protection::default()
    };
    assert!(for_reply(&always_sign, false, false).sign);
    assert_eq!(public_key_name(ADA), "OpenPGP_0x0000000000000001.asc");
    assert!(is_fingerprint(ADA) && !is_fingerprint("ABCD"));
}
