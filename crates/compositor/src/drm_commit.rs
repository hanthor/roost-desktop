//! Per-process non-reused scalar kernel commit identities. No pointer userdata.
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicU64, Ordering};
static NEXT: AtomicU64 = AtomicU64::new(1);
fn allocate(counter: &AtomicU64) -> Option<NonZeroU64> {
    let value = counter
        .fetch_update(Ordering::AcqRel, Ordering::Acquire, |v| v.checked_add(1))
        .ok()?;
    NonZeroU64::new(value)
}
/// Never reset when a DRM device, connector or native epoch is replaced.
pub fn next_cookie() -> Option<NonZeroU64> {
    allocate(&NEXT)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_allocator_never_wraps_or_reuses_on_device_replacement() {
        let counter = AtomicU64::new(1);
        assert_eq!(allocate(&counter).unwrap().get(), 1);
        assert_eq!(allocate(&counter).unwrap().get(), 2);
        counter.store(u64::MAX, Ordering::Release);
        assert!(allocate(&counter).is_none());
        assert!(allocate(&counter).is_none());
        assert_eq!(counter.load(Ordering::Acquire), u64::MAX);
    }
    fn event(kind: u32, length: u32, cookie: u64, crtc: u32) -> [u8; 1024] {
        let mut bytes = [0u8; 1024];
        bytes[..4].copy_from_slice(&kind.to_ne_bytes());
        bytes[4..8].copy_from_slice(&length.to_ne_bytes());
        bytes[8..16].copy_from_slice(&cookie.to_ne_bytes());
        bytes[16..20].copy_from_slice(&12u32.to_ne_bytes());
        bytes[20..24].copy_from_slice(&345u32.to_ne_bytes());
        bytes[24..28].copy_from_slice(&u32::MAX.to_ne_bytes());
        bytes[28..32].copy_from_slice(&crtc.to_ne_bytes());
        bytes
    }
    #[test]
    fn published_additive_parser_contract_preserves_high_cookie_true_crtc_timestamp_and_sequence() {
        use smithay::reexports::drm::control::{EventsWithUserData, KernelEvent};
        let cookie = 0xfedc_ba98_1234_5678;
        let bytes = event(2, 32, cookie, 39);
        let mut events = EventsWithUserData::with_event_buf(bytes, 32).unwrap();
        let KernelEvent::PageFlip(parsed) = events.next().unwrap().unwrap() else {
            panic!("page flip");
        };
        assert_eq!(parsed.user_data, cookie);
        assert_eq!(parsed.crtc_id, 39);
        assert_eq!(parsed.sequence, u32::MAX);
        assert_eq!(parsed.timestamp, std::time::Duration::new(12, 345000));
        assert!(events.next().is_none());
        let mut zero = EventsWithUserData::with_event_buf(event(2, 32, cookie, 0), 32).unwrap();
        let KernelEvent::PageFlip(parsed) = zero.next().unwrap().unwrap() else {
            panic!("page flip");
        };
        assert_eq!(parsed.crtc_id, 0); // no fallback to low cookie bits
    }
    #[test]
    fn malformed_and_overlong_records_fail_once_without_out_of_bounds_or_loops() {
        use smithay::reexports::drm::control::EventsWithUserData;
        assert!(EventsWithUserData::with_event_buf([0; 1024], 1025).is_err());
        for (length, amount) in [
            (0, 32),
            (7, 32),
            (31, 32),
            (33, 32),
            (u32::MAX, 32),
            (32, 7),
        ] {
            let mut events =
                EventsWithUserData::with_event_buf(event(2, length, 1, 39), amount).unwrap();
            assert!(events.next().unwrap().is_err());
            assert!(events.next().is_none());
        }
        let mut bytes = event(2, 32, 1, 39);
        bytes[20..24].copy_from_slice(&1_000_000u32.to_ne_bytes());
        let mut events = EventsWithUserData::with_event_buf(bytes, 32).unwrap();
        assert!(events.next().unwrap().is_err());
        assert!(events.next().is_none());
    }
    #[test]
    fn complete_mixed_unknown_and_cookie_events_retain_all_independent_fields() {
        use smithay::reexports::drm::control::{EventsWithUserData, KernelEvent};
        let original = event(2, 32, 0x1_0000_0001, 39);
        let mut bytes = [0u8; 1024];
        bytes[..4].copy_from_slice(&99u32.to_ne_bytes());
        bytes[4..8].copy_from_slice(&8u32.to_ne_bytes());
        bytes[8..40].copy_from_slice(&original[..32]);
        let mut events = EventsWithUserData::with_event_buf(bytes, 40).unwrap();
        assert!(
            matches!(events.next().unwrap().unwrap(),KernelEvent::Unknown(raw) if raw.len()==8)
        );
        assert!(
            matches!(events.next().unwrap().unwrap(),KernelEvent::PageFlip(event)
            if event.user_data==0x1_0000_0001 && event.crtc_id==39)
        );
        assert!(events.next().is_none());
    }
}
