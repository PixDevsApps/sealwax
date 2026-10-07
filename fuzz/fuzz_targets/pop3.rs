#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| sealwax_engine::fuzz::pop3(data));
