// Tests for shell-control-schema: protocol versioning, serialization, and frame encoding/decoding

#[cfg(test)]
mod protocol_version_tests {
    use shell_control_schema::{ProtocolVersion, CURRENT_VERSION};

    #[test]
    fn version_is_compatible_with_same_version() {
        let version = ProtocolVersion {
            major: 1,
            minor: 2,
        };
        assert!(version.is_compatible_with(&version));
    }

    #[test]
    fn version_is_compatible_with_higher_minor() {
        let older = ProtocolVersion {
            major: 1,
            minor: 0,
        };
        let newer = ProtocolVersion {
            major: 1,
            minor: 5,
        };
        assert!(older.is_compatible_with(&newer));
    }

    #[test]
    fn version_is_not_compatible_with_different_major() {
        let v1 = ProtocolVersion {
            major: 1,
            minor: 0,
        };
        let v2 = ProtocolVersion {
            major: 2,
            minor: 0,
        };
        assert!(!v1.is_compatible_with(&v2));
    }

    #[test]
    fn version_is_not_compatible_with_lower_minor() {
        let older = ProtocolVersion {
            major: 1,
            minor: 5,
        };
        let newer = ProtocolVersion {
            major: 1,
            minor: 0,
        };
        assert!(!older.is_compatible_with(&newer));
    }

    #[test]
    fn current_version_is_defined() {
        // Verify that CURRENT_VERSION is accessible and well-formed
        let major = CURRENT_VERSION.major;
        let minor = CURRENT_VERSION.minor;
        assert!(major > 0 || minor > 0); // At least one should be non-zero
    }

    #[test]
    fn version_equality_works() {
        let v1 = ProtocolVersion { major: 1, minor: 2 };
        let v2 = ProtocolVersion { major: 1, minor: 2 };
        let v3 = ProtocolVersion { major: 1, minor: 3 };

        assert_eq!(v1, v2);
        assert_ne!(v1, v3);
    }

    #[test]
    fn version_copy_trait_works() {
        let v1 = ProtocolVersion {
            major: 1,
            minor: 0,
        };
        let v2 = v1; // Copy, not move
        assert_eq!(v1, v2);
    }
}

#[cfg(test)]
mod dynamic_workspace_count_tests {
    use shell_control_schema::dynamic_workspace_count;

    #[test]
    fn returns_one_when_no_occupied_and_active_is_zero() {
        let count = dynamic_workspace_count(None, 0);
        assert_eq!(count, 1);
    }

    #[test]
    fn returns_active_plus_one_when_that_exceeds_default() {
        let count = dynamic_workspace_count(None, 5);
        assert_eq!(count, 6); // active (5) + 1
    }

    #[test]
    fn uses_max_occupied_plus_two_as_baseline() {
        let count = dynamic_workspace_count(Some(3), 0);
        assert_eq!(count, 5); // max_occupied (3) + 2
    }

    #[test]
    fn ensures_at_least_active_plus_one() {
        // When active exceeds max_occupied + 2
        let count = dynamic_workspace_count(Some(2), 10);
        assert_eq!(count, 11); // max(2 + 2, 10 + 1) = 11
    }

    #[test]
    fn handles_large_occupied_count() {
        let count = dynamic_workspace_count(Some(100), 0);
        assert_eq!(count, 102); // 100 + 2
    }

    #[test]
    fn handles_large_active_count() {
        let count = dynamic_workspace_count(None, 1000);
        assert_eq!(count, 1001); // 1000 + 1
    }

    #[test]
    fn handles_large_values() {
        let count = dynamic_workspace_count(Some(1000), 5000);
        assert_eq!(count, 5001); // max(1000 + 2, 5000 + 1)
    }

    #[test]
    fn zero_active_with_zero_occupied() {
        let count = dynamic_workspace_count(Some(0), 0);
        assert_eq!(count, 2); // 0 + 2
    }
}

#[cfg(test)]
mod frame_encoding_decoding_tests {
    use shell_control_schema::{
        decode_frame, encode_frame, DecodeError, Message, ProtocolVersion, CURRENT_VERSION,
    };

    #[test]
    fn encode_hello_message() {
        let msg = Message::Hello {
            version: CURRENT_VERSION,
        };
        let frame = encode_frame(&msg);

        // Frame should have at least 4-byte header
        assert!(frame.len() >= 4);
        // First 4 bytes are length prefix in little-endian
        let len_bytes = &frame[..4];
        let len = u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]])
            as usize;
        assert_eq!(frame.len(), len + 4);
    }

    #[test]
    fn decode_encodes_hello_message_roundtrip() {
        let original = Message::Hello {
            version: CURRENT_VERSION,
        };
        let encoded = encode_frame(&original);
        let decoded = decode_frame(&encoded);

        assert!(decoded.is_ok());
        if let Ok(Message::Hello { version }) = decoded {
            assert_eq!(version, CURRENT_VERSION);
        } else {
            panic!("Decoded message was not a Hello");
        }
    }

    #[test]
    fn decode_rejects_empty_frame() {
        let result = decode_frame(&[]);
        assert!(matches!(result, Err(DecodeError::Truncated { .. })));
    }

    #[test]
    fn decode_rejects_partial_header() {
        let result = decode_frame(&[0x01, 0x02]);
        assert!(matches!(result, Err(DecodeError::Truncated { .. })));
    }

    #[test]
    fn decode_rejects_oversize_frame() {
        // Create a frame header claiming impossibly large body
        let mut frame = vec![0xff, 0xff, 0xff, 0x7f]; // Large u32 in LE
        frame.push(0); // Minimal body
        let result = decode_frame(&frame);
        assert!(matches!(result, Err(DecodeError::Oversize { .. })));
    }

    #[test]
    fn decode_rejects_truncated_body() {
        // Header claims 100 bytes but we only provide 10
        let mut frame = vec![100, 0, 0, 0]; // 100 in LE bytes
        frame.extend_from_slice(&[0; 10]);
        let result = decode_frame(&frame);
        assert!(matches!(result, Err(DecodeError::Truncated { .. })));
    }

    #[test]
    fn decode_rejects_extra_bytes_in_body() {
        // Create a frame with trailing garbage
        let msg = Message::Hello {
            version: CURRENT_VERSION,
        };
        let mut frame = encode_frame(&msg);
        // Append extra bytes
        frame.extend_from_slice(&[255, 255]);

        let result = decode_frame(&frame);
        // Should fail due to trailing bytes in postcard body
        assert!(result.is_err());
    }

    #[test]
    fn encode_produces_valid_le_header() {
        let msg = Message::Hello {
            version: CURRENT_VERSION,
        };
        let frame = encode_frame(&msg);

        let header_bytes = &frame[..4];
        let decoded_len =
            u32::from_le_bytes([header_bytes[0], header_bytes[1], header_bytes[2], header_bytes[3]])
                as usize;

        // Body should be exactly the declared length
        let body = &frame[4..];
        assert_eq!(body.len(), decoded_len);
    }

    #[test]
    fn encode_multiple_messages_independently() {
        let msg1 = Message::Hello {
            version: CURRENT_VERSION,
        };
        let msg2 = Message::Hello {
            version: CURRENT_VERSION,
        };

        let frame1 = encode_frame(&msg1);
        let frame2 = encode_frame(&msg2);

        // Frames should be identical for identical messages
        assert_eq!(frame1, frame2);

        // Both should decode correctly
        assert!(decode_frame(&frame1).is_ok());
        assert!(decode_frame(&frame2).is_ok());
    }

    #[test]
    fn decode_incompatible_version_rejection() {
        // Create a Hello with incompatible major version
        let incompatible = ProtocolVersion {
            major: CURRENT_VERSION.major + 1,
            minor: 0,
        };
        let msg = Message::Hello {
            version: incompatible,
        };
        let frame = encode_frame(&msg);

        let result = decode_frame(&frame);
        assert!(matches!(result, Err(DecodeError::IncompatibleVersion { .. })));
    }

    #[test]
    fn encode_frame_respects_max_size() {
        // This test verifies that encode_frame panics if message is too large
        // (We can't directly create such a message via public API,
        // but we can verify the function exists and basic encoding works)
        let msg = Message::Hello {
            version: CURRENT_VERSION,
        };
        let frame = encode_frame(&msg);
        assert!(frame.len() < shell_control_schema::MAX_FRAME_BYTES);
    }
}

#[cfg(test)]
mod constants_tests {
    use shell_control_schema::{MAX_FRAME_BYTES, MAX_TITLE_LEN};

    #[test]
    fn max_frame_bytes_is_one_mib() {
        assert_eq!(MAX_FRAME_BYTES, 1024 * 1024);
    }

    #[test]
    fn max_title_length_is_reasonable() {
        assert_eq!(MAX_TITLE_LEN, 512);
        assert!(MAX_TITLE_LEN > 0);
        assert!(MAX_TITLE_LEN < MAX_FRAME_BYTES);
    }
}
