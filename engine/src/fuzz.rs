//! Entry points for the engine's fuzz targets in `fuzz/` (ML-093, docs/m9/security.md): the
//! protocol parsers Sealwax wrote itself. The tests run them over the checked-in corpus, so a
//! crash once found stays fixed. Built only for tests and with the `fuzz` feature.

pub use crate::imap::fuzz::imap;
pub use crate::pop3::fuzz::pop3;

#[cfg(test)]
mod tests {
    use std::path::Path;

    fn corpus(name: &str, target: fn(&[u8])) {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../fuzz/corpus")
            .join(name);
        let mut files = 0;
        for entry in std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
            let path = entry.expect("a corpus entry").path();
            let data = std::fs::read(&path).expect("a corpus file");
            let result = std::panic::catch_unwind(|| target(&data));
            assert!(result.is_ok(), "{name}: {} fails", path.display());
            files += 1;
        }
        assert!(files > 0, "{name}: an empty corpus");
    }

    #[test]
    fn pop3_corpus() {
        corpus("pop3", super::pop3);
    }

    #[test]
    fn imap_corpus() {
        corpus("imap", super::imap);
    }
}
