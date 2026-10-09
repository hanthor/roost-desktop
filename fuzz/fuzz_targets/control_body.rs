#![no_main]
use libfuzzer_sys::fuzz_target;
use tuna_shell_control::MAX_FRAME_BYTES;

// Supply the length prefix so mutations reach postcard and field validation.
fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_FRAME_BYTES + 1 {
        return;
    }
    let mut frame = Vec::with_capacity(data.len() + 4);
    frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
    frame.extend_from_slice(data);
    tuna_control_fuzz::check_frame(&frame);
});
