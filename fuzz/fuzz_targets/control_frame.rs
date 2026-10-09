#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    tuna_control_fuzz::check_frame(data);
});
