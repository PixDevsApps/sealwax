use std::hash::BuildHasher;
use std::io::Read;

const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

/// 128 random bits as 26 lowercase base32 characters, safe in Message-IDs, Content-IDs and URLs.
pub fn token() -> String {
    let mut output = String::with_capacity(26);
    let mut bits = 0u32;
    let mut pending = 0;
    for byte in random_bytes() {
        bits = (bits << 8) | u32::from(byte);
        pending += 8;
        while pending >= 5 {
            pending -= 5;
            output.push(char::from(BASE32[((bits >> pending) & 31) as usize]));
        }
        bits &= (1 << pending) - 1;
    }
    if pending > 0 {
        output.push(char::from(BASE32[((bits << (5 - pending)) & 31) as usize]));
    }
    output
}

fn random_bytes() -> [u8; 16] {
    let mut bytes = [0; 16];
    let from_kernel = std::fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut bytes))
        .is_ok();
    if !from_kernel {
        let state = std::collections::hash_map::RandomState::new();
        let now = std::time::SystemTime::now();
        bytes[..8].copy_from_slice(&state.hash_one((now, 1u8)).to_le_bytes());
        bytes[8..].copy_from_slice(&state.hash_one((now, 2u8)).to_le_bytes());
    }
    bytes
}

/// The right-hand side for our Message-IDs and Content-IDs: the sender's domain in ASCII form.
/// Never the machine's hostname.
pub fn id_domain(sender: &str) -> Result<String, String> {
    let (_, domain) = sender
        .rsplit_once('@')
        .ok_or_else(|| "the sender address has no domain".to_string())?;
    let ascii = gtk4::glib::hostname_to_ascii(domain.trim().trim_end_matches('.'))
        .ok_or_else(|| "the sender domain is not a valid host name".to_string())?
        .to_ascii_lowercase();
    let valid = !ascii.is_empty()
        && ascii.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        });
    if valid {
        Ok(ascii)
    } else {
        Err("the sender domain is not a valid host name".into())
    }
}

pub fn message_id(sender: &str) -> Result<String, String> {
    Ok(format!("{}@{}", token(), id_domain(sender)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_distinct_base32() {
        let first = token();
        let second = token();
        assert_eq!(first.len(), 26);
        assert_ne!(first, second);
        assert!(first.bytes().all(|byte| BASE32.contains(&byte)));
    }

    #[test]
    fn id_domain_uses_the_sender_domain() {
        assert_eq!(id_domain("kari@Example.COM").unwrap(), "example.com");
        let idn = id_domain("ola@blåbær.example").unwrap();
        assert!(
            idn.starts_with("xn--") && idn.ends_with(".example"),
            "{idn}"
        );
        assert!(id_domain("no-domain").is_err());
        assert!(id_domain("a@bad domain.example").is_err());
    }

    #[test]
    fn message_id_never_uses_the_hostname() {
        let id = message_id("kari@example.com").unwrap();
        assert!(id.ends_with("@example.com"));
        let host = gtk4::glib::host_name().to_ascii_lowercase();
        assert!(!id.to_ascii_lowercase().contains(host.as_str()));
    }
}
