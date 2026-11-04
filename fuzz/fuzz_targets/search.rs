#![no_main]

libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    sealwax_domain::fuzz::search(data);
    sealwax_ui_model::fuzz::search_hints(data);
});
