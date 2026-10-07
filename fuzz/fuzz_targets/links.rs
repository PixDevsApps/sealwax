#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| sealwax_domain::fuzz::links(data));
