#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    sealwax_domain::fuzz::config(data);
    sealwax_ui_model::fuzz::keys(data);
});
