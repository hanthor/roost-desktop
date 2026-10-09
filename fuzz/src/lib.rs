use tuna_shell_control::{decode_frame, encode_frame};

/// Successful decodes must remain representable as a valid protocol frame.
pub fn check_frame(data: &[u8]) {
    if let Ok(message) = decode_frame(data) {
        let encoded = encode_frame(&message);
        assert!(decode_frame(&encoded).is_ok());
    }
}
