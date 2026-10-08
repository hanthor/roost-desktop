//! Required original-epoch scanout receipts; allocation is never completion.
use crate::monitor_refresh::Ticket;

/// Immutable identity captured after the original queue actually succeeds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Queued {
    pub primary: bool,
    pub layout_generation: u64,
    pub monitor_epoch: Option<u64>,
    pub cookie: std::num::NonZeroU64,
    pub crtc: u32,
    pub device: u64,
    pub commit_epoch: u64,
}
/// Actual original-device event and accepted pending-commit facts.
#[derive(Clone, Copy)]
pub struct KernelCommit {
    pub user_data: u64,
    pub crtc: u32,
    pub device: u64,
    pub commit_epoch: Option<u64>,
    pub layout_generation: u64,
    pub pending_cookie: Option<std::num::NonZeroU64>,
    pub original_fd: bool,
}
impl Queued {
    pub fn matches_pending_commit(&self, event: &KernelCommit) -> bool {
        event.original_fd
            && event.user_data == self.cookie.get()
            && event.pending_cookie == Some(self.cookie)
            && event.crtc == self.crtc
            && event.device == self.device
            && event.commit_epoch == Some(self.commit_epoch)
    }
    pub fn matches_commit(&self, event: &KernelCommit) -> bool {
        self.matches_pending_commit(event) && event.layout_generation == self.layout_generation
    }
    pub fn primary_for(&self, completed: bool, current_layout: u64) -> bool {
        completed && self.primary && self.layout_generation == current_layout
    }
}

/// Historical receipt from a matched successful GBM completion, not current
/// output authority or physical-pixel proof. No scene or input payload.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct KernelCompletion {
    pub cookie: u64,
    pub crtc: u32,
    pub device: u64,
    pub commit_epoch: u64,
    pub layout_generation: u64,
    pub sequence: u32,
    pub timestamp_clock: &'static str,
    pub timestamp_secs: u64,
    pub timestamp_subsec_ns: u32,
}

/// Actual kernel current-state readback. Agreement is independent buffer/mode
/// state evidence; it does not identify an untagged callback's originating commit.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Readback {
    pub crtc: u32,
    pub plane: Option<u32>,
    pub plane_crtc: Option<u32>,
    pub framebuffer: Option<u32>,
    pub mode_matches: bool,
}
impl Readback {
    pub fn agrees(&self, crtc: u32, plane: Option<u32>, framebuffer: u32) -> bool {
        self.crtc == crtc
            && self.plane == plane
            && self.mode_matches
            && self.framebuffer == Some(framebuffer)
            && (plane.is_none() || self.plane_crtc == Some(crtc))
    }
}

pub struct Pending {
    pub ticket: Ticket,
    required: Vec<(u32, bool)>,
    failed: bool,
}
impl Pending {
    pub fn new(ticket: Ticket, crtcs: &[u32]) -> Result<Self, &'static str> {
        if crtcs.len() > 64
            || crtcs.contains(&0)
            || crtcs
                .iter()
                .enumerate()
                .any(|(index, id)| crtcs[..index].contains(id))
        {
            return Err("invalid required scanout CRTCs");
        }
        Ok(Self {
            ticket,
            required: crtcs.iter().map(|id| (*id, false)).collect(),
            failed: false,
        })
    }
    /// Caller supplies only a real successful frame_submitted receipt after
    /// checking original active FD, current ticket and actual completion time.
    pub fn presented(&mut self, crtc: u32, queued_epoch: Option<u64>, admitted: bool) {
        if self.failed || !admitted || queued_epoch != Some(self.ticket.epoch) {
            return;
        }
        if let Some((_, presented)) = self.required.iter_mut().find(|(id, _)| *id == crtc) {
            *presented = true;
        }
    }
    pub fn requires(&self, crtc: u32) -> bool {
        self.required
            .iter()
            .any(|(id, presented)| *id == crtc && !*presented)
    }

    pub fn fail(&mut self) {
        self.failed = true;
    }
    pub fn failed(&self) -> bool {
        self.failed
    }
    pub fn complete(&self) -> bool {
        !self.failed && self.required.iter().all(|(_, presented)| *presented)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    fn ticket() -> Ticket {
        Ticket {
            device: 7,
            epoch: 3,
            started: Instant::now(),
        }
    }
    #[test]
    fn allocation_and_one_output_never_satisfy_all_required_scanouts() {
        let mut pending = Pending::new(ticket(), &[39, 42]).unwrap();
        assert!(!pending.complete());
        assert!(pending.requires(39));
        assert!(!pending.requires(77));
        pending.presented(39, Some(3), true);
        assert!(!pending.requires(39));
        assert!(pending.requires(42));
        pending.presented(39, Some(3), true);
        assert!(!pending.complete());
        pending.presented(42, Some(3), true);
        assert!(pending.complete());
    }
    #[test]
    fn foreign_crtc_epoch_unstamped_and_unadmitted_receipts_never_qualify() {
        let mut pending = Pending::new(ticket(), &[39]).unwrap();
        for (crtc, epoch, admitted) in [
            (42, Some(3), true),
            (39, Some(2), true),
            (39, None, true),
            (39, Some(3), false),
        ] {
            pending.presented(crtc, epoch, admitted);
            assert!(!pending.complete());
        }
    }
    #[test]
    fn actual_empty_disable_can_complete_but_failed_receipts_never_recover() {
        assert!(Pending::new(ticket(), &[]).unwrap().complete());
        let mut pending = Pending::new(ticket(), &[39]).unwrap();
        pending.fail();
        pending.presented(39, Some(3), true);
        assert!(pending.failed());
        assert!(!pending.complete());
    }
    #[test]
    fn immutable_queued_primary_never_follows_reordered_index_or_new_layout() {
        let original_primary = Queued {
            primary: true,
            layout_generation: 4,
            monitor_epoch: None,
            cookie: std::num::NonZeroU64::new(1).unwrap(),
            crtc: 39,
            device: 226,
            commit_epoch: 3,
        };
        let original_secondary = Queued {
            primary: false,
            layout_generation: 4,
            monitor_epoch: None,
            cookie: std::num::NonZeroU64::new(1).unwrap(),
            crtc: 39,
            device: 226,
            commit_epoch: 3,
        };
        // Reordering the output inventory cannot promote the old secondary
        // receipt, or attach a retired primary scene to newly queued inputs.
        assert!(!original_secondary.primary_for(true, 4));
        assert!(!original_primary.primary_for(true, 5));
        assert!(!original_primary.primary_for(false, 4));
        assert!(original_primary.primary_for(true, 4));
        let current_primary = Queued {
            primary: true,
            layout_generation: 5,
            monitor_epoch: None,
            cookie: std::num::NonZeroU64::new(1).unwrap(),
            crtc: 39,
            device: 226,
            commit_epoch: 3,
        };
        assert!(current_primary.primary_for(true, 5));
    }
    #[test]
    fn independent_current_state_requires_exact_crtc_plane_framebuffer_and_mode() {
        let expected = Readback {
            crtc: 39,
            plane: Some(41),
            plane_crtc: Some(39),
            framebuffer: Some(101),
            mode_matches: true,
        };
        assert!(expected.agrees(39, Some(41), 101));
        for changed in [
            Readback {
                crtc: 40,
                ..expected
            },
            Readback {
                plane: Some(42),
                ..expected
            },
            Readback {
                plane_crtc: None,
                ..expected
            },
            Readback {
                plane_crtc: Some(40),
                ..expected
            },
            Readback {
                framebuffer: Some(100),
                ..expected
            },
            Readback {
                framebuffer: None,
                ..expected
            },
            Readback {
                mode_matches: false,
                ..expected
            },
        ] {
            assert!(!changed.agrees(39, Some(41), 101));
        }
        let legacy = Readback {
            plane: None,
            plane_crtc: None,
            ..expected
        };
        assert!(legacy.agrees(39, None, 101));
        assert!(!legacy.agrees(39, Some(41), 101));
    }
    #[test]
    fn duplicate_zero_or_oversized_required_sets_refuse() {
        assert!(Pending::new(ticket(), &[39, 39]).is_err());
        assert!(Pending::new(ticket(), &[0]).is_err());
        assert!(Pending::new(ticket(), &(1..=65).collect::<Vec<_>>()).is_err());
    }
    #[test]
    fn completion_requires_successful_pending_cookie_and_every_original_owner_stamp() {
        let cookie = std::num::NonZeroU64::new(0x1_0000_0001).unwrap();
        let queued = Queued {
            primary: true,
            layout_generation: 4,
            monitor_epoch: Some(3),
            cookie,
            crtc: 39,
            device: 226,
            commit_epoch: 3,
        };
        let mut event = KernelCommit {
            user_data: cookie.get(),
            crtc: 39,
            device: 226,
            commit_epoch: Some(3),
            layout_generation: 4,
            pending_cookie: Some(cookie),
            original_fd: true,
        };
        assert!(queued.matches_commit(&event));
        event.pending_cookie = None;
        assert!(!queued.matches_commit(&event)); // queued-only or failed ioctl
        event.pending_cookie = Some(cookie);
        event.user_data += 1;
        assert!(!queued.matches_commit(&event));
        event.user_data = cookie.get();
        event.crtc = 40;
        assert!(!queued.matches_commit(&event));
        event.crtc = 39;
        event.device += 1;
        assert!(!queued.matches_commit(&event));
        event.device = 226;
        event.commit_epoch = Some(4);
        assert!(!queued.matches_commit(&event));
        event.commit_epoch = None;
        assert!(!queued.matches_commit(&event));
        event.commit_epoch = Some(3);
        event.layout_generation = 5;
        assert!(!queued.matches_commit(&event));
        event.layout_generation = 4;
        event.original_fd = false;
        assert!(!queued.matches_commit(&event));
        event.original_fd = true;
        assert!(queued.matches_commit(&event));
    }
}

#[cfg(test)]
mod layout_retirement_tests {
    use super::*;
    #[test]
    fn applied_layout_retires_exact_old_pending_without_current_layout_authority() {
        let cookie = std::num::NonZeroU64::new(90).unwrap();
        let origin = Queued {
            primary: true,
            layout_generation: 4,
            monitor_epoch: None,
            cookie,
            crtc: 39,
            device: 226,
            commit_epoch: 3,
        };
        let mut event = KernelCommit {
            user_data: 90,
            crtc: 39,
            device: 226,
            commit_epoch: Some(3),
            layout_generation: 5,
            pending_cookie: Some(cookie),
            original_fd: true,
        };
        assert!(origin.matches_pending_commit(&event));
        assert!(!origin.matches_commit(&event));
        // A fresh render has a distinct cookie: the old callback cannot retire it.
        let new = Queued {
            cookie: std::num::NonZeroU64::new(91).unwrap(),
            layout_generation: 5,
            ..origin
        };
        event.pending_cookie = Some(new.cookie);
        assert!(!origin.matches_pending_commit(&event));
        assert!(!new.matches_pending_commit(&event));
        event.user_data = 91;
        assert!(new.matches_commit(&event));
        for rejected in [
            KernelCommit {
                original_fd: false,
                ..event
            },
            KernelCommit {
                commit_epoch: Some(4),
                ..event
            },
            KernelCommit {
                device: 227,
                ..event
            },
            KernelCommit { crtc: 40, ..event },
        ] {
            assert!(!new.matches_pending_commit(&rejected));
        }
    }
}
