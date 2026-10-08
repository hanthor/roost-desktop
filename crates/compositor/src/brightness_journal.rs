//! Bounded in-session brightness intent, independent of the restartable shell.
//!
//! This state machine performs no hardware writes or IPC. Its callers must
//! supply supervised-peer authority, actual native bindings and genuine helper
//! completion/readback facts. A control-command acknowledgement is not a
//! completion. No value here authorizes cross-session or disk restoration.
use roost_shell_control::{
    BrightnessMeasuredCandidate as MeasuredCandidate,
    BrightnessMeasuredRestoration as MeasuredRestoration, NativeOutputInfo,
};
use std::time::{Duration, Instant};

pub const MAX_PANELS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Authority {
    pub child_generation: u64,
    pub connection_generation: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub output: NativeOutputInfo,
    pub ownership_generation: u64,
    pub backlight: String,
    pub device: u64,
    pub inode: u64,
    pub minimum: u32,
    pub maximum: u32,
}
impl Binding {
    fn valid(&self) -> bool {
        self.ownership_generation != 0
            && self.minimum < self.maximum
            && self.output.connector_id != 0
            && !self.output.name.is_empty()
            && self.output.name.len() <= 512
            && !self.output.connector_sysfs.is_empty()
            && self.output.connector_sysfs.len() <= 512
            && !self.backlight.is_empty()
            && self.backlight.len() <= 128
            && self
                .backlight
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
    }
    fn level(&self, value: u32) -> bool {
        (self.minimum..=self.maximum).contains(&value)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub binding: Binding,
    pub user: u32,
    /// False for a newly observed/rebound panel: numeric current level is not
    /// committed User intent while genuine DIM/auto state remains unknown.
    pub user_known: bool,
    pub measured_candidate: Option<MeasuredCandidate>,
    pub measured_restoration: Option<MeasuredRestoration>,
    pub applied: u32,
    pub ratio: f64,
}
impl Reading {
    fn valid(&self) -> bool {
        self.binding.valid()
            && self.binding.level(self.user)
            && self.applied <= self.binding.maximum
            && self.ratio.is_finite()
            && (0.0..=1.0).contains(&self.ratio)
    }
}

/// Each dimension is independently unknown until the original genuine power
/// provider supplies that policy. DIM does not imply an automatic reset.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Policy {
    pub dimming: Option<bool>,
    pub automatic: Option<f64>,
    pub idle: f64,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            dimming: None,
            automatic: None,
            idle: 0.3,
        }
    }
}
impl Policy {
    fn known(self) -> bool {
        self.dimming.is_some() && self.automatic.is_some()
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PolicyUpdate {
    Dimming(bool),
    Automatic(f64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Source {
    /// Explicit user intent; never inferred from an equal hardware target.
    User,
    /// Permissive external API request; it cannot establish genuine resync.
    External {
        update: PolicyUpdate,
    },
    /// Epoch comes only from the off-frame genuine installed GSD credential
    /// observer, not an app-id hint or caller-supplied process name.
    Power {
        provider_epoch: u64,
        update: PolicyUpdate,
    },
    Idle(f64),
    Automatic,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub binding: Binding,
    pub effective: u32,
    pub user: Option<u32>,
    pub ratio: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observation {
    pub binding: Binding,
    pub actual: u32,
    /// Set only after the real login1 callback succeeds. Readback by itself
    /// cannot resolve an interrupted/failed helper transaction.
    pub helper_succeeded: bool,
}

/// Off-frame original kernel readback; time and binding are stamped by the
/// compositor validator, never accepted as shell-supplied authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FreshReadback {
    pub binding: Binding,
    pub actual: u32,
    pub checked_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grant {
    pub transaction: u64,
    pub revision: u64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub grant: Grant,
    pub authority: Authority,
    pub source: Source,
    pub targets: Vec<Target>,
    pub interrupted: bool,
    started: Instant,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub revision: u64,
    pub native_generation: Option<u64>,
    pub authority: Option<Authority>,
    pub readings: Vec<Reading>,
    /// Prior intent for revoked bindings, never transferred to a new panel.
    pub retired_readings: Vec<Reading>,
    pub policy: Policy,
    pub pending: Option<Pending>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Authority,
    Bounds,
    Binding,
    Busy,
    PolicyUnknown,
    Provider,
    Stale,
    Incomplete,
}

#[derive(Default)]
pub struct Journal {
    authority: Option<Authority>,
    provider_epoch: Option<u64>,
    revision: u64,
    transaction: u64,
    readings: Vec<Reading>,
    retired_readings: Vec<Reading>,
    native_generation: Option<u64>,
    ever_bound: bool,
    policy: Policy,
    pending: Option<Pending>,
}
impl Journal {
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            revision: self.revision,
            native_generation: self.native_generation,
            authority: self.authority,
            readings: self.readings.clone(),
            retired_readings: self.retired_readings.clone(),
            policy: self.policy,
            pending: self.pending.clone(),
        }
    }
    pub fn authority(&mut self, authority: Option<Authority>) {
        if self.authority != authority {
            self.interrupt();
            self.authority = authority;
        }
    }
    pub fn provider(&mut self, epoch: Option<u64>) {
        if self.provider_epoch != epoch {
            if self.provider_epoch.is_some() {
                self.invalidate_measured();
            }
            self.provider_epoch = epoch;
            // Initial provider admission binds existing original measurements,
            // without resampling them or renewing their sampled revision.
            if let Some(epoch) = epoch {
                for reading in &mut self.readings {
                    if let Some(candidate) = &mut reading.measured_candidate {
                        if candidate.provider_epoch.is_none() {
                            candidate.provider_epoch = Some(epoch);
                        }
                    }
                }
            }
            self.policy.dimming = None;
            self.policy.automatic = None;
            self.interrupt();
        }
    }
    fn invalidate_measured(&mut self) {
        for reading in self.readings.iter_mut().chain(&mut self.retired_readings) {
            reading.measured_candidate = None;
            reading.measured_restoration = None;
        }
    }
    fn interrupt(&mut self) {
        if self.pending.is_some() {
            self.invalidate_measured();
        }
        if let Some(pending) = &mut self.pending {
            pending.interrupted = true;
            match pending.source {
                Source::Power {
                    update: PolicyUpdate::Dimming(_),
                    ..
                }
                | Source::External {
                    update: PolicyUpdate::Dimming(_),
                } => self.policy.dimming = None,
                Source::Power {
                    update: PolicyUpdate::Automatic(_),
                    ..
                }
                | Source::External {
                    update: PolicyUpdate::Automatic(_),
                } => self.policy.automatic = None,
                _ => {}
            }
        }
    }
    /// Missing client ACK never fabricates completion; an orphaned issued grant
    /// becomes interrupted after a finite window, retaining partial uncertainty.
    pub fn expire(&mut self, now: Instant) {
        if self.pending.as_ref().is_some_and(|v| {
            !v.interrupted
                && !now
                    .checked_duration_since(v.started)
                    .is_some_and(|age| age <= Duration::from_secs(10))
        }) {
            self.interrupt();
        }
    }
    fn authorized(&self, authority: Authority) -> Result<(), Error> {
        if authority.child_generation != 0
            && authority.connection_generation != 0
            && self.authority == Some(authority)
        {
            Ok(())
        } else {
            Err(Error::Authority)
        }
    }
    /// Initial kernel snapshot only. Existing intent is never overwritten by a
    /// replacement shell treating a dimmed effective value as new user intent.
    pub fn initialize(
        &mut self,
        authority: Authority,
        readings: Vec<Reading>,
    ) -> Result<(), Error> {
        self.authorized(authority)?;
        if !self.readings.is_empty() || self.pending.is_some() {
            return Err(Error::Stale);
        }
        if readings.is_empty() || readings.len() > MAX_PANELS || readings.iter().any(|r| !r.valid())
        {
            return Err(Error::Bounds);
        }
        for (index, reading) in readings.iter().enumerate() {
            if readings[..index].iter().any(|r| {
                r.binding.backlight == reading.binding.backlight
                    || r.binding.output.name == reading.binding.output.name
            }) {
                return Err(Error::Binding);
            }
        }
        let revision = self.revision.checked_add(1).ok_or(Error::Stale)?;
        self.readings = readings;
        self.ever_bound = true;
        self.revision = revision;
        Ok(())
    }
    /// Actual backend ownership invalidates grants even when connector metadata
    /// happens to match. Missing ownership never licenses a hardware submission.
    pub fn native_generation(&mut self, generation: Option<u64>) {
        let generation = generation.filter(|v| *v != 0);
        if self.native_generation != generation {
            self.invalidate_measured();
            self.interrupt();
            self.native_generation = generation;
        }
    }

    /// Fresh kernel-only import, without hardware writes or fabricated helper
    /// completion. Exact unchanged bindings retain committed original User
    /// intent; new bindings expose current effective level as explicitly unknown.
    pub fn reconcile(
        &mut self,
        authority: Authority,
        generation: u64,
        facts: &[FreshReadback],
        now: Instant,
        idle: f64,
    ) -> Result<(), Error> {
        self.authorized(authority)?;
        if self.native_generation != Some(generation) || generation == 0 {
            return Err(Error::Binding);
        }
        if facts.len() > MAX_PANELS || !idle.is_finite() || !(0.0..=1.0).contains(&idle) {
            return Err(Error::Bounds);
        }
        for (index, fact) in facts.iter().enumerate() {
            if !fact.binding.valid()
                || fact.binding.ownership_generation != generation
                || fact.actual > fact.binding.maximum
                || !now
                    .checked_duration_since(fact.checked_at)
                    .is_some_and(|v| v <= Duration::from_secs(2))
                || facts[..index].iter().any(|v| {
                    v.binding.backlight == fact.binding.backlight
                        || v.binding.output.name == fact.binding.output.name
                })
            {
                return Err(Error::Binding);
            }
        }
        let revision = self.revision.checked_add(1).ok_or(Error::Stale)?;
        if self.pending.is_some() {
            self.interrupt();
        }
        let first = !self.ever_bound && self.pending.is_none() && self.retired_readings.is_empty();
        let unexplained = self.readings.iter().any(|old| {
            facts
                .iter()
                .any(|v| v.binding == old.binding && v.actual != old.applied)
        });
        if unexplained {
            self.invalidate_measured();
            self.policy.dimming = None;
            self.policy.automatic = None;
        }
        let changed = (!self.readings.is_empty() && facts.len() != self.readings.len())
            || self
                .readings
                .iter()
                .any(|old| !facts.iter().any(|v| v.binding == old.binding));
        if changed {
            self.invalidate_measured();
            self.interrupt();
            self.retired_readings = self.readings.clone();
            // A physically different panel may have a different transient state.
            self.policy.dimming = None;
            self.policy.automatic = None;
        }
        let initial = self.readings.is_empty() && self.retired_readings.is_empty();
        let readings = facts
            .iter()
            .map(|fact| {
                if let Some(old) = self.readings.iter().find(|v| v.binding == fact.binding) {
                    Reading {
                        applied: fact.actual,
                        ..old.clone()
                    }
                } else {
                    Reading {
                        binding: fact.binding.clone(),
                        user: fact.actual.max(fact.binding.minimum),
                        user_known: false,
                        measured_candidate: if first && fact.binding.level(fact.actual) {
                            Some(MeasuredCandidate {
                                level: fact.actual,
                                sampled_revision: revision,
                                provider_epoch: self.provider_epoch,
                            })
                        } else {
                            None
                        },
                        measured_restoration: None,
                        applied: fact.actual,
                        ratio: 1.0,
                    }
                }
            })
            .collect();
        self.readings = readings;
        if !facts.is_empty() {
            self.ever_bound = true;
        }
        if initial {
            self.policy.idle = idle;
        }
        self.revision = revision;
        Ok(())
    }

    /// The journal is pending BEFORE the caller may start any hardware write.
    pub fn begin(
        &mut self,
        authority: Authority,
        source: Source,
        targets: Vec<Target>,
        fresh: &[FreshReadback],
        now: Instant,
    ) -> Result<Grant, Error> {
        self.authorized(authority)?;
        self.expire(now);
        if self.pending.as_ref().is_some_and(|p| !p.interrupted) {
            return Err(Error::Busy);
        }
        if targets.is_empty() || targets.len() > MAX_PANELS {
            return Err(Error::Bounds);
        }
        if fresh.len() != targets.len() || fresh.len() > MAX_PANELS {
            return Err(Error::Incomplete);
        }
        for (index, observation) in fresh.iter().enumerate() {
            if !targets.iter().any(|t| t.binding == observation.binding)
                || fresh[..index]
                    .iter()
                    .any(|o| o.binding == observation.binding)
                || observation.actual > observation.binding.maximum
                || !now
                    .checked_duration_since(observation.checked_at)
                    .is_some_and(|age| age <= Duration::from_secs(2))
            {
                return Err(Error::Stale);
            }
        }
        // A changed original observation is not a new restoration baseline.
        // Before any grant, revoke measurements if effects are unexplained.
        if fresh.iter().any(|fact| {
            self.readings
                .iter()
                .any(|reading| reading.binding == fact.binding && reading.applied != fact.actual)
        }) {
            self.invalidate_measured();
            self.policy.dimming = None;
            self.policy.automatic = None;
        }
        let mut next_policy = self.policy;
        if let Source::Power {
            provider_epoch,
            update,
        } = source
        {
            if self.provider_epoch != Some(provider_epoch) {
                return Err(Error::Provider);
            }
            match update {
                PolicyUpdate::Dimming(value) => next_policy.dimming = Some(value),
                PolicyUpdate::Automatic(value) if value.is_finite() => {
                    next_policy.automatic = Some(value)
                }
                _ => return Err(Error::Bounds),
            }
        }
        if let Source::Idle(value) = source {
            if !value.is_finite() || !(0.0..=1.0).contains(&value) {
                return Err(Error::Bounds);
            }
            next_policy.idle = value;
        }
        if let Source::External {
            update: PolicyUpdate::Automatic(value),
        } = source
        {
            if !value.is_finite() {
                return Err(Error::Bounds);
            }
        }
        let automatic = !matches!(source, Source::User);
        for (index, target) in targets.iter().enumerate() {
            if self.native_generation != Some(target.binding.ownership_generation) {
                return Err(Error::Binding);
            }
            let original = self
                .readings
                .iter()
                .find(|r| r.binding == target.binding)
                .ok_or(Error::Binding)?;
            if !target.binding.level(target.effective)
                || target.user.is_some_and(|u| !target.binding.level(u))
                || target
                    .ratio
                    .is_some_and(|r| !r.is_finite() || !(0.0..=1.0).contains(&r))
                || (!matches!(source, Source::User)
                    && (target.user.is_some() || target.ratio.is_some()))
                || targets[..index].iter().any(|t| t.binding == target.binding)
            {
                return Err(Error::Bounds);
            }
            let actual = fresh
                .iter()
                .find(|f| f.binding == original.binding)
                .ok_or(Error::Incomplete)?
                .actual;
            if automatic
                && target.effective > actual
                && (!(original.user_known
                    || original.measured_restoration.is_some_and(|v| {
                        self.provider_epoch == Some(v.provider_epoch) && target.effective <= v.level
                    }))
                    || !next_policy.known()
                    || (self.pending.is_some() && !matches!(source, Source::Power { .. })))
            {
                return Err(Error::PolicyUnknown);
            }
        }
        for observation in fresh {
            self.readings
                .iter_mut()
                .find(|r| r.binding == observation.binding)
                .ok_or(Error::Binding)?
                .applied = observation.actual;
        }
        self.transaction = self.transaction.checked_add(1).ok_or(Error::Stale)?;
        let grant = Grant {
            transaction: self.transaction,
            revision: self.revision,
        };
        self.pending = Some(Pending {
            grant,
            authority,
            source,
            targets,
            interrupted: false,
            started: now,
        });
        Ok(grant)
    }
    /// Exact currently-owned bindings must be supplied by the backend, and
    /// actual observations by the completion/readback path, before any commit.
    pub fn fail_completion(&mut self, authority: Authority, grant: Grant) -> Result<(), Error> {
        self.authorized(authority)?;
        if !self
            .pending
            .as_ref()
            .is_some_and(|v| v.authority == authority && v.grant == grant)
        {
            return Err(Error::Stale);
        }
        self.interrupt();
        Ok(())
    }

    pub fn complete(
        &mut self,
        authority: Authority,
        grant: Grant,
        owned: &[Binding],
        observations: Vec<Observation>,
    ) -> Result<(), Error> {
        self.authorized(authority)?;
        self.expire(Instant::now());
        let pending = self.pending.as_ref().ok_or(Error::Stale)?.clone();
        if pending.grant != grant
            || pending.authority != authority
            || pending.interrupted
            || grant.revision != self.revision
        {
            return Err(Error::Stale);
        }
        if observations.len() > MAX_PANELS || owned.len() > MAX_PANELS {
            self.interrupt();
            return Err(Error::Bounds);
        }
        let mut all = observations.len() == pending.targets.len();
        for (index, observation) in observations.iter().enumerate() {
            if observations[..index]
                .iter()
                .any(|o| o.binding == observation.binding)
                || !pending
                    .targets
                    .iter()
                    .any(|t| t.binding == observation.binding)
                || !owned.contains(&observation.binding)
                || observation.actual > observation.binding.maximum
            {
                self.interrupt();
                return Err(Error::Binding);
            }
        }
        for target in &pending.targets {
            if !owned.contains(&target.binding) {
                all = false;
                continue;
            }
            if let Some(observed) = observations.iter().find(|o| o.binding == target.binding) {
                if let Some(reading) = self
                    .readings
                    .iter_mut()
                    .find(|r| r.binding == target.binding)
                {
                    reading.applied = observed.actual;
                }
                all &= observed.helper_succeeded && observed.actual == target.effective;
            } else {
                all = false;
            }
        }
        if !all {
            self.interrupt();
            return Err(Error::Incomplete);
        }
        let revision = self.revision.checked_add(1).ok_or(Error::Stale)?;
        let pristine = self.readings.iter().all(|reading| {
            reading.measured_candidate.is_none_or(|v| {
                observations
                    .iter()
                    .any(|o| o.binding == reading.binding && o.actual == v.level)
            })
        });
        if !pristine {
            self.invalidate_measured();
        }
        // Commit ALL user intent atomically only after the complete batch.
        for target in &pending.targets {
            let reading = self
                .readings
                .iter_mut()
                .find(|r| r.binding == target.binding)
                .ok_or(Error::Binding)?;
            if let Some(user) = target.user {
                reading.user = user;
                reading.user_known = true;
                reading.measured_candidate = None;
                reading.measured_restoration = None;
            }
            if let Some(ratio) = target.ratio {
                reading.ratio = ratio;
            }
        }
        if let Source::Power {
            provider_epoch,
            update,
        } = pending.source
        {
            if self.provider_epoch != Some(provider_epoch) {
                return Err(Error::Provider);
            }
            match update {
                PolicyUpdate::Dimming(value) => self.policy.dimming = Some(value),
                PolicyUpdate::Automatic(value) => self.policy.automatic = Some(value),
            }
        }
        if let Source::External { update } = pending.source {
            self.invalidate_measured();
            match update {
                PolicyUpdate::Dimming(_) => self.policy.dimming = None,
                PolicyUpdate::Automatic(_) => self.policy.automatic = None,
            }
        }
        if let Source::Idle(value) = pending.source {
            self.policy.idle = value;
        }
        let establish = matches!(pending.source, Source::Power { .. })
            && self.policy.dimming == Some(false)
            && self.policy.automatic == Some(-1.0)
            && pending.targets.len() == self.readings.len()
            && !self.readings.is_empty()
            && self.readings.iter().all(|reading| {
                !reading.user_known
                    && reading.measured_candidate.is_some_and(|candidate| {
                        candidate.provider_epoch == self.provider_epoch
                            && pending.targets.iter().any(|target| {
                                target.binding == reading.binding
                                    && target.effective == candidate.level
                            })
                    })
            });
        if establish {
            let epoch = self.provider_epoch.ok_or(Error::Provider)?;
            for reading in &mut self.readings {
                let candidate = reading.measured_candidate.take().ok_or(Error::Incomplete)?;
                reading.measured_restoration = Some(MeasuredRestoration {
                    level: candidate.level,
                    sampled_revision: candidate.sampled_revision,
                    provider_epoch: epoch,
                    establishing_grant: grant.into(),
                });
            }
        }
        if (establish || matches!(pending.source, Source::User))
            && pending.targets.iter().all(|v| v.ratio.is_none())
        {
            let max = self
                .readings
                .iter()
                .filter_map(|v| {
                    let level = if v.user_known {
                        Some(v.user)
                    } else {
                        v.measured_restoration.map(|m| m.level)
                    }?;
                    Some(
                        f64::from(level.saturating_sub(v.binding.minimum))
                            / f64::from(v.binding.maximum - v.binding.minimum),
                    )
                })
                .fold(0.0f64, f64::max);
            if max > 0.01 {
                for reading in &mut self.readings {
                    let level = if reading.user_known {
                        Some(reading.user)
                    } else {
                        reading.measured_restoration.map(|v| v.level)
                    };
                    let Some(level) = level else { continue };
                    reading.ratio = f64::from(level.saturating_sub(reading.binding.minimum))
                        / f64::from(reading.binding.maximum - reading.binding.minimum)
                        / max;
                }
            }
        }
        self.pending = None;
        self.revision = revision;
        Ok(())
    }
}

impl From<roost_shell_control::BrightnessBinding> for Binding {
    fn from(v: roost_shell_control::BrightnessBinding) -> Self {
        Self {
            output: v.output,
            ownership_generation: v.ownership_generation,
            backlight: v.backlight,
            device: v.device,
            inode: v.inode,
            minimum: v.minimum,
            maximum: v.maximum,
        }
    }
}
impl From<Binding> for roost_shell_control::BrightnessBinding {
    fn from(v: Binding) -> Self {
        Self {
            output: v.output,
            ownership_generation: v.ownership_generation,
            backlight: v.backlight,
            device: v.device,
            inode: v.inode,
            minimum: v.minimum,
            maximum: v.maximum,
        }
    }
}
impl From<roost_shell_control::BrightnessReading> for Reading {
    fn from(v: roost_shell_control::BrightnessReading) -> Self {
        Self {
            binding: v.binding.into(),
            user: v.user,
            user_known: v.user_known,
            measured_candidate: v.measured_candidate,
            measured_restoration: v.measured_restoration,
            applied: v.applied,
            ratio: v.ratio,
        }
    }
}
impl From<Reading> for roost_shell_control::BrightnessReading {
    fn from(v: Reading) -> Self {
        Self {
            binding: v.binding.into(),
            user: v.user,
            user_known: v.user_known,
            measured_candidate: v.measured_candidate,
            measured_restoration: v.measured_restoration,
            applied: v.applied,
            ratio: v.ratio,
        }
    }
}
impl From<roost_shell_control::BrightnessTarget> for Target {
    fn from(v: roost_shell_control::BrightnessTarget) -> Self {
        Self {
            binding: v.binding.into(),
            effective: v.effective,
            user: v.user,
            ratio: v.ratio,
        }
    }
}
impl From<Target> for roost_shell_control::BrightnessTarget {
    fn from(v: Target) -> Self {
        Self {
            binding: v.binding.into(),
            effective: v.effective,
            user: v.user,
            ratio: v.ratio,
        }
    }
}
impl From<roost_shell_control::BrightnessObservation> for Observation {
    fn from(v: roost_shell_control::BrightnessObservation) -> Self {
        Self {
            binding: v.binding.into(),
            actual: v.actual,
            helper_succeeded: v.helper_succeeded,
        }
    }
}
impl From<roost_shell_control::BrightnessGrant> for Grant {
    fn from(v: roost_shell_control::BrightnessGrant) -> Self {
        Self {
            transaction: v.transaction,
            revision: v.revision,
        }
    }
}
impl From<Grant> for roost_shell_control::BrightnessGrant {
    fn from(v: Grant) -> Self {
        Self {
            transaction: v.transaction,
            revision: v.revision,
        }
    }
}
impl From<Policy> for roost_shell_control::BrightnessPolicy {
    fn from(v: Policy) -> Self {
        Self {
            dimming: v.dimming,
            automatic: v.automatic,
            idle: v.idle,
        }
    }
}
impl From<Snapshot> for roost_shell_control::BrightnessJournalSnapshot {
    fn from(v: Snapshot) -> Self {
        Self {
            revision: v.revision,
            native_generation: v.native_generation,
            authority: v
                .authority
                .map(|v| roost_shell_control::BrightnessAuthority {
                    child_generation: v.child_generation,
                    connection_generation: v.connection_generation,
                }),
            provider: None,
            readings: v.readings.into_iter().map(Into::into).collect(),
            retired_readings: v.retired_readings.into_iter().map(Into::into).collect(),
            policy: v.policy.into(),
            pending: v.pending.map(|v| roost_shell_control::BrightnessPending {
                grant: v.grant.into(),
                targets: v.targets.into_iter().map(Into::into).collect(),
                interrupted: v.interrupted,
            }),
        }
    }
}
impl From<Error> for roost_shell_control::BrightnessJournalError {
    fn from(v: Error) -> Self {
        match v {
            Error::Authority => Self::Authority,
            Error::Bounds => Self::Bounds,
            Error::Binding => Self::Binding,
            Error::Busy => Self::Busy,
            Error::PolicyUnknown => Self::PolicyUnknown,
            Error::Provider => Self::Provider,
            Error::Stale => Self::Stale,
            Error::Incomplete => Self::Incomplete,
        }
    }
}
impl From<roost_shell_control::BrightnessPolicyUpdate> for PolicyUpdate {
    fn from(v: roost_shell_control::BrightnessPolicyUpdate) -> Self {
        match v {
            roost_shell_control::BrightnessPolicyUpdate::Dimming(v) => Self::Dimming(v),
            roost_shell_control::BrightnessPolicyUpdate::Automatic(v) => Self::Automatic(v),
        }
    }
}
impl Source {
    /// The worker's pinned genuine owner must match the original invocation
    /// sender. An arbitrary same-user API invocation stays External.
    pub fn admitted(
        v: roost_shell_control::BrightnessSource,
        provider: Option<(&str, u64)>,
    ) -> Result<Self, Error> {
        use roost_shell_control::BrightnessSource as W;
        Ok(match v {
            W::User => Self::User,
            W::External { update } => Self::External {
                update: update.into(),
            },
            W::Idle(v) => Self::Idle(v),
            W::Automatic => Self::Automatic,
            W::Power { sender, update } => {
                let Some((owner, epoch)) = provider else {
                    return Err(Error::Provider);
                };
                if epoch == 0 || sender != owner || !sender.starts_with(':') {
                    return Err(Error::Provider);
                }
                Self::Power {
                    provider_epoch: epoch,
                    update: update.into(),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn authority() -> Authority {
        Authority {
            child_generation: 1,
            connection_generation: 1,
        }
    }
    fn binding(name: &str) -> Binding {
        Binding {
            output: NativeOutputInfo {
                name: name.into(),
                drm_device: 226,
                connector_id: 39,
                connector_sysfs: format!("/sys/device/card0-{name}"),
                connector_device: 1,
                connector_inode: 2,
            },
            ownership_generation: 1,
            backlight: format!("backlight_{name}"),
            device: 1,
            inode: 4,
            minimum: 0,
            maximum: 1000,
        }
    }
    fn journal() -> Journal {
        let mut journal = Journal::default();
        journal.authority(Some(authority()));
        journal.native_generation(Some(1));
        journal
            .initialize(
                authority(),
                vec![Reading {
                    binding: binding("eDP-1"),
                    user: 700,
                    user_known: true,
                    measured_candidate: None,
                    measured_restoration: None,
                    applied: 307,
                    ratio: 1.0,
                }],
            )
            .unwrap();
        journal
    }
    fn target(value: u32) -> Target {
        Target {
            binding: binding("eDP-1"),
            effective: value,
            user: None,
            ratio: None,
        }
    }
    fn observed(value: u32, success: bool) -> Observation {
        Observation {
            binding: binding("eDP-1"),
            actual: value,
            helper_succeeded: success,
        }
    }
    fn begin(journal: &mut Journal, source: Source, targets: Vec<Target>) -> Result<Grant, Error> {
        let now = Instant::now();
        let fresh: Vec<_> = targets
            .iter()
            .map(|target| FreshReadback {
                binding: target.binding.clone(),
                actual: journal
                    .readings
                    .iter()
                    .find(|r| r.binding == target.binding)
                    .map_or(0, |r| r.applied),
                checked_at: now,
            })
            .collect();
        journal.begin(authority(), source, targets, &fresh, now)
    }
    fn complete(
        journal: &mut Journal,
        grant: Grant,
        value: u32,
        success: bool,
    ) -> Result<(), Error> {
        journal.complete(
            authority(),
            grant,
            &[binding("eDP-1")],
            vec![observed(value, success)],
        )
    }
    #[test]
    fn missing_journal_never_assumes_normal_or_automatic_reset() {
        let mut journal = journal();
        assert_eq!(journal.snapshot().policy.dimming, None);
        assert_eq!(journal.snapshot().policy.automatic, None);
        assert_eq!(
            begin(&mut journal, Source::Automatic, vec![target(700)]),
            Err(Error::PolicyUnknown)
        );
        assert!(journal.snapshot().pending.is_none());
    }
    #[test]
    fn begin_precedes_write_and_equal_readback_cannot_replace_helper_success() {
        let mut journal = journal();
        let grant = begin(
            &mut journal,
            Source::User,
            vec![Target {
                user: Some(800),
                ..target(200)
            }],
        )
        .unwrap();
        assert_eq!(journal.snapshot().pending.unwrap().grant, grant);
        assert_eq!(
            complete(&mut journal, grant, 200, false),
            Err(Error::Incomplete)
        );
        assert_eq!(journal.snapshot().readings[0].user, 700);
        assert_eq!(journal.snapshot().readings[0].applied, 200);
        assert!(journal.snapshot().pending.unwrap().interrupted);
    }
    #[test]
    fn shell_replacement_preserves_baseline_and_pending_and_rejects_late_completion() {
        let mut journal = journal();
        let grant = begin(
            &mut journal,
            Source::User,
            vec![Target {
                user: Some(800),
                ..target(200)
            }],
        )
        .unwrap();
        journal.authority(None);
        let new = Authority {
            child_generation: 2,
            connection_generation: 2,
        };
        journal.authority(Some(new));
        assert_eq!(
            complete(&mut journal, grant, 200, true),
            Err(Error::Authority)
        );
        assert_eq!(
            journal.complete(new, grant, &[binding("eDP-1")], vec![observed(200, true)]),
            Err(Error::Stale)
        );
        assert_eq!(journal.snapshot().readings[0].user, 700);
        assert!(journal.snapshot().pending.unwrap().interrupted);
        assert_eq!(
            journal.initialize(
                new,
                vec![Reading {
                    binding: binding("eDP-1"),
                    user: 200,
                    user_known: true,
                    measured_candidate: None,
                    measured_restoration: None,
                    applied: 200,
                    ratio: 1.0
                }]
            ),
            Err(Error::Stale)
        );
    }
    #[test]
    fn only_full_batch_success_commits_user_intent() {
        let mut journal = Journal::default();
        journal.authority(Some(authority()));
        journal.native_generation(Some(1));
        let panels = [binding("eDP-1"), binding("DP-1")];
        journal
            .initialize(
                authority(),
                panels
                    .iter()
                    .map(|b| Reading {
                        binding: b.clone(),
                        user: 700,
                        user_known: true,
                        measured_candidate: None,
                        measured_restoration: None,
                        applied: 307,
                        ratio: 1.0,
                    })
                    .collect(),
            )
            .unwrap();
        let targets: Vec<_> = panels
            .iter()
            .map(|b| Target {
                binding: b.clone(),
                effective: 200,
                user: Some(800),
                ratio: Some(0.5),
            })
            .collect();
        let grant = begin(&mut journal, Source::User, targets.clone()).unwrap();
        let observations = panels
            .iter()
            .enumerate()
            .map(|(i, b)| Observation {
                binding: b.clone(),
                actual: 200,
                helper_succeeded: i == 0,
            })
            .collect();
        assert_eq!(
            journal.complete(authority(), grant, &panels, observations),
            Err(Error::Incomplete)
        );
        assert!(journal
            .snapshot()
            .readings
            .iter()
            .all(|r| r.user == 700 && r.ratio == 1.0 && r.applied == 200));
        let fresh = begin(&mut journal, Source::User, targets).unwrap();
        let observations = panels
            .iter()
            .map(|b| Observation {
                binding: b.clone(),
                actual: 200,
                helper_succeeded: true,
            })
            .collect();
        journal
            .complete(authority(), fresh, &panels, observations)
            .unwrap();
        assert!(journal
            .snapshot()
            .readings
            .iter()
            .all(|r| r.user == 800 && r.ratio == 0.5));
        assert!(journal.snapshot().pending.is_none());
    }
    #[test]
    fn genuine_dimensions_are_separate_and_provider_loss_revokes_both() {
        let mut journal = journal();
        journal.provider(Some(1));
        let dim = begin(
            &mut journal,
            Source::Power {
                provider_epoch: 1,
                update: PolicyUpdate::Dimming(true),
            },
            vec![target(200)],
        )
        .unwrap();
        complete(&mut journal, dim, 200, true).unwrap();
        assert_eq!(journal.snapshot().policy.dimming, Some(true));
        assert_eq!(journal.snapshot().policy.automatic, None);
        assert_eq!(
            begin(
                &mut journal,
                Source::Power {
                    provider_epoch: 1,
                    update: PolicyUpdate::Dimming(false)
                },
                vec![target(700)]
            ),
            Err(Error::PolicyUnknown)
        );
        let auto = begin(
            &mut journal,
            Source::Power {
                provider_epoch: 1,
                update: PolicyUpdate::Automatic(-1.0),
            },
            vec![target(200)],
        )
        .unwrap();
        complete(&mut journal, auto, 200, true).unwrap();
        let normal = begin(
            &mut journal,
            Source::Power {
                provider_epoch: 1,
                update: PolicyUpdate::Dimming(false),
            },
            vec![target(700)],
        )
        .unwrap();
        complete(&mut journal, normal, 700, true).unwrap();
        assert_eq!(journal.snapshot().readings[0].user, 700);
        journal.provider(None);
        assert_eq!(journal.snapshot().policy.dimming, None);
        assert_eq!(journal.snapshot().policy.automatic, None);
        assert_eq!(
            begin(
                &mut journal,
                Source::Power {
                    provider_epoch: 1,
                    update: PolicyUpdate::Dimming(false)
                },
                vec![target(700)]
            ),
            Err(Error::Provider)
        );
    }
    #[test]
    fn changed_binding_missing_or_duplicate_receipt_never_commits() {
        let mut replaced = binding("eDP-1");
        replaced.inode += 1;
        for (owned, observations, expected) in [
            (vec![replaced], vec![observed(200, true)], Error::Binding),
            (
                vec![binding("eDP-1")],
                vec![observed(200, true), observed(200, true)],
                Error::Binding,
            ),
            (vec![binding("eDP-1")], vec![], Error::Incomplete),
        ] {
            // Each guard gets its own original admitted transaction. A prior
            // rejection interrupts that grant and must not mask another case.
            let mut journal = journal();
            let grant = begin(
                &mut journal,
                Source::User,
                vec![Target {
                    user: Some(800),
                    ..target(200)
                }],
            )
            .unwrap();
            assert_eq!(
                journal.complete(authority(), grant, &owned, observations),
                Err(expected)
            );
            let snapshot = journal.snapshot();
            assert_eq!(snapshot.readings[0].user, 700);
            assert_eq!(snapshot.readings[0].applied, 307);
            assert!(snapshot.pending.unwrap().interrupted);
            // Even a later complete-looking receipt cannot reuse the grant
            // invalidated by this particular failed completion.
            assert_eq!(
                journal.complete(
                    authority(),
                    grant,
                    &[binding("eDP-1")],
                    vec![observed(200, true)]
                ),
                Err(Error::Stale)
            );
            assert_eq!(journal.snapshot().readings[0].user, 700);
        }
    }
    #[test]
    fn invalid_bounds_and_nonfinite_ratio_do_not_admit_pending() {
        let mut journal = journal();
        assert_eq!(
            begin(
                &mut journal,
                Source::User,
                vec![Target {
                    ratio: Some(f64::NAN),
                    ..target(200)
                }]
            ),
            Err(Error::Bounds)
        );
        assert_eq!(
            begin(&mut journal, Source::User, vec![target(1001)]),
            Err(Error::Bounds)
        );
        assert_eq!(
            begin(
                &mut journal,
                Source::User,
                vec![target(200); MAX_PANELS + 1]
            ),
            Err(Error::Bounds)
        );
        assert!(journal.snapshot().pending.is_none());
        assert_eq!(
            journal.begin(
                Authority {
                    child_generation: 0,
                    connection_generation: 0
                },
                Source::User,
                vec![target(200)],
                &[],
                Instant::now()
            ),
            Err(Error::Authority)
        );
    }
    #[test]
    fn explicit_external_request_does_not_fabricate_genuine_policy() {
        let mut journal = journal();
        let grant = begin(
            &mut journal,
            Source::External {
                update: PolicyUpdate::Dimming(false),
            },
            vec![target(200)],
        )
        .unwrap();
        complete(&mut journal, grant, 200, true).unwrap();
        assert_eq!(journal.snapshot().policy.dimming, None);
        assert_eq!(journal.snapshot().policy.automatic, None);
    }
    #[test]
    fn successful_zero_keeps_original_ratios() {
        let mut journal = journal();
        let grant = begin(
            &mut journal,
            Source::User,
            vec![Target {
                user: Some(0),
                ratio: Some(0.5),
                ..target(0)
            }],
        )
        .unwrap();
        complete(&mut journal, grant, 0, true).unwrap();
        let snapshot = journal.snapshot();
        assert_eq!(snapshot.readings[0].user, 0);
        assert_eq!(snapshot.readings[0].ratio, 0.5);
    }
    #[test]
    fn external_unknown_policy_never_brightens_and_only_its_dimension_is_invalidated() {
        let mut journal = journal();
        assert_eq!(
            begin(
                &mut journal,
                Source::External {
                    update: PolicyUpdate::Dimming(false)
                },
                vec![target(700)]
            ),
            Err(Error::PolicyUnknown)
        );
        journal.provider(Some(1));
        journal.policy.dimming = Some(false);
        journal.policy.automatic = Some(-1.0);
        let grant = begin(
            &mut journal,
            Source::External {
                update: PolicyUpdate::Dimming(true),
            },
            vec![target(200)],
        )
        .unwrap();
        complete(&mut journal, grant, 200, true).unwrap();
        assert_eq!(journal.snapshot().policy.dimming, None);
        assert_eq!(journal.snapshot().policy.automatic, Some(-1.0));
    }
    #[test]
    fn fresh_readback_prevents_cached_effective_value_from_admitting_upward() {
        let mut journal = journal();
        let now = Instant::now();
        let fresh = [FreshReadback {
            binding: binding("eDP-1"),
            actual: 0,
            checked_at: now,
        }];
        assert_eq!(
            journal.begin(
                authority(),
                Source::Automatic,
                vec![target(200)],
                &fresh,
                now
            ),
            Err(Error::PolicyUnknown)
        );
        assert!(journal.snapshot().pending.is_none());
        let stale = [FreshReadback {
            checked_at: now - Duration::from_secs(3),
            ..fresh[0].clone()
        }];
        assert_eq!(
            journal.begin(authority(), Source::User, vec![target(200)], &stale, now),
            Err(Error::Stale)
        );
        let future = [FreshReadback {
            checked_at: now + Duration::from_secs(1),
            ..fresh[0].clone()
        }];
        assert_eq!(
            journal.begin(authority(), Source::User, vec![target(200)], &future, now),
            Err(Error::Stale)
        );
        let mut replaced = fresh[0].clone();
        replaced.binding.ownership_generation += 1;
        assert_eq!(
            journal.begin(
                authority(),
                Source::User,
                vec![target(200)],
                &[replaced],
                now
            ),
            Err(Error::Stale)
        );
    }
    #[test]
    fn interrupted_policy_invalidates_only_the_unacknowledged_dimension() {
        let mut journal = journal();
        journal.provider(Some(1));
        journal.policy.dimming = Some(false);
        journal.policy.automatic = Some(-1.0);
        let grant = begin(
            &mut journal,
            Source::Power {
                provider_epoch: 1,
                update: PolicyUpdate::Dimming(true),
            },
            vec![target(200)],
        )
        .unwrap();
        assert_eq!(
            complete(&mut journal, grant, 200, false),
            Err(Error::Incomplete)
        );
        assert_eq!(journal.snapshot().policy.dimming, None);
        assert_eq!(journal.snapshot().policy.automatic, Some(-1.0));
        // An actually fresh genuine NORMAL action may resolve interruption;
        // it does not pretend that the old DIM helper request succeeded.
        let fresh = begin(
            &mut journal,
            Source::Power {
                provider_epoch: 1,
                update: PolicyUpdate::Dimming(false),
            },
            vec![target(700)],
        )
        .unwrap();
        assert_ne!(fresh.transaction, grant.transaction);
        complete(&mut journal, fresh, 700, true).unwrap();
        assert_eq!(journal.snapshot().readings[0].user, 700);
    }
    #[test]
    fn original_provider_fact_cannot_be_replaced_by_sender_hint() {
        use roost_shell_control::{BrightnessPolicyUpdate as U, BrightnessSource as W};
        let source = || W::Power {
            sender: ":1.4".into(),
            update: U::Dimming(false),
        };
        assert_eq!(Source::admitted(source(), None), Err(Error::Provider));
        assert_eq!(
            Source::admitted(source(), Some((":1.5", 1))),
            Err(Error::Provider)
        );
        assert_eq!(
            Source::admitted(source(), Some((":1.4", 0))),
            Err(Error::Provider)
        );
        assert_eq!(
            Source::admitted(source(), Some((":1.4", 2))),
            Ok(Source::Power {
                provider_epoch: 2,
                update: PolicyUpdate::Dimming(false)
            })
        );
    }
    #[test]
    fn observed_effective_level_is_not_a_committed_user_baseline() {
        let mut journal = journal();
        journal.readings[0].user_known = false;
        journal.policy.dimming = Some(false);
        journal.policy.automatic = Some(-1.0);
        journal.readings[0].applied = 100;
        assert_eq!(
            begin(&mut journal, Source::Automatic, vec![target(700)]),
            Err(Error::PolicyUnknown)
        );
        let grant = begin(
            &mut journal,
            Source::User,
            vec![Target {
                user: Some(700),
                ..target(700)
            }],
        )
        .unwrap();
        complete(&mut journal, grant, 700, true).unwrap();
        assert!(journal.snapshot().readings[0].user_known);
    }
    fn measured_journal(levels: &[u32]) -> Journal {
        let mut j = Journal::default();
        j.authority(Some(authority()));
        j.native_generation(Some(1));
        let now = Instant::now();
        let facts = levels
            .iter()
            .enumerate()
            .map(|(i, level)| FreshReadback {
                binding: binding(&format!("eDP-{}", i + 1)),
                actual: *level,
                checked_at: now,
            })
            .collect::<Vec<_>>();
        j.reconcile(authority(), 1, &facts, now, 0.3).unwrap();
        j
    }
    fn measured_policy(j: &mut Journal, update: PolicyUpdate) -> Result<(), Error> {
        let targets = j
            .readings
            .iter()
            .map(|r| Target {
                binding: r.binding.clone(),
                effective: r.applied,
                user: None,
                ratio: None,
            })
            .collect::<Vec<_>>();
        let grant = begin(
            j,
            Source::Power {
                provider_epoch: 7,
                update,
            },
            targets.clone(),
        )?;
        let owned = targets
            .iter()
            .map(|t| t.binding.clone())
            .collect::<Vec<_>>();
        let observations = targets
            .iter()
            .map(|t| Observation {
                binding: t.binding.clone(),
                actual: t.effective,
                helper_succeeded: true,
            })
            .collect();
        j.complete(authority(), grant, &owned, observations)
    }
    #[test]
    fn initial_two_genuine_no_change_receipts_establish_measured_not_user_baseline() {
        for reverse in [false, true] {
            let mut j = measured_journal(&[700, 350]);
            let original_revision = j.readings[0].measured_candidate.unwrap().sampled_revision;
            j.provider(Some(7));
            assert_eq!(
                j.readings[0].measured_candidate.unwrap().sampled_revision,
                original_revision
            );
            let updates = if reverse {
                [PolicyUpdate::Automatic(-1.0), PolicyUpdate::Dimming(false)]
            } else {
                [PolicyUpdate::Dimming(false), PolicyUpdate::Automatic(-1.0)]
            };
            measured_policy(&mut j, updates[0]).unwrap();
            assert!(j.readings.iter().all(|r| !r.user_known
                && r.measured_candidate.is_some()
                && r.measured_restoration.is_none()));
            measured_policy(&mut j, updates[1]).unwrap();
            assert!(j.readings.iter().all(|r| !r.user_known
                && r.measured_candidate.is_none()
                && r.measured_restoration.unwrap().provider_epoch == 7
                && r.measured_restoration.unwrap().sampled_revision == original_revision));
            assert_eq!(j.readings[0].ratio, 1.0);
            assert_eq!(j.readings[1].ratio, 0.5);
        }
    }
    #[test]
    fn genuine_dim_normal_restores_only_original_measured_cap() {
        let mut j = measured_journal(&[700]);
        j.provider(Some(7));
        measured_policy(&mut j, PolicyUpdate::Dimming(false)).unwrap();
        measured_policy(&mut j, PolicyUpdate::Automatic(-1.0)).unwrap();
        let dim = begin(
            &mut j,
            Source::Power {
                provider_epoch: 7,
                update: PolicyUpdate::Dimming(true),
            },
            vec![target(210)],
        )
        .unwrap();
        complete(&mut j, dim, 210, true).unwrap();
        assert_eq!(
            begin(
                &mut j,
                Source::Power {
                    provider_epoch: 7,
                    update: PolicyUpdate::Dimming(false)
                },
                vec![target(701)]
            ),
            Err(Error::PolicyUnknown)
        );
        let normal = begin(
            &mut j,
            Source::Power {
                provider_epoch: 7,
                update: PolicyUpdate::Dimming(false),
            },
            vec![target(700)],
        )
        .unwrap();
        complete(&mut j, normal, 700, true).unwrap();
        assert!(!j.readings[0].user_known);
        assert_eq!(j.readings[0].measured_restoration.unwrap().level, 700);
    }
    #[test]
    fn partial_initial_helper_effect_permanently_revokes_measured_eligibility() {
        let mut j = measured_journal(&[700]);
        j.provider(Some(7));
        let grant = begin(
            &mut j,
            Source::Power {
                provider_epoch: 7,
                update: PolicyUpdate::Dimming(false),
            },
            vec![target(700)],
        )
        .unwrap();
        assert_eq!(complete(&mut j, grant, 700, false), Err(Error::Incomplete));
        assert!(j.readings[0].measured_candidate.is_none());
        let now = Instant::now();
        j.reconcile(
            authority(),
            1,
            &[FreshReadback {
                binding: binding("eDP-1"),
                actual: 700,
                checked_at: now,
            }],
            now,
            0.3,
        )
        .unwrap();
        assert!(j.readings[0].measured_candidate.is_none());
    }
    #[test]
    fn provider_loss_rebind_external_unknown_and_changed_readback_cannot_resample() {
        for mode in 0..4 {
            let mut j = measured_journal(&[700]);
            j.provider(Some(7));
            measured_policy(&mut j, PolicyUpdate::Dimming(false)).unwrap();
            measured_policy(&mut j, PolicyUpdate::Automatic(-1.0)).unwrap();
            match mode {
                0 => {
                    j.provider(None);
                    j.provider(Some(7));
                }
                1 => {
                    j.native_generation(None);
                    j.native_generation(Some(1));
                }
                2 => {
                    let grant = begin(
                        &mut j,
                        Source::External {
                            update: PolicyUpdate::Dimming(false),
                        },
                        vec![target(700)],
                    )
                    .unwrap();
                    complete(&mut j, grant, 700, true).unwrap();
                }
                _ => {}
            }
            let now = Instant::now();
            j.reconcile(
                authority(),
                1,
                &[FreshReadback {
                    binding: binding("eDP-1"),
                    actual: if mode == 3 { 210 } else { 700 },
                    checked_at: now,
                }],
                now,
                0.3,
            )
            .unwrap();
            assert!(j.readings[0].measured_candidate.is_none());
            assert!(j.readings[0].measured_restoration.is_none());
            assert!(!j.readings[0].user_known);
        }
    }
    #[test]
    fn minimum_invalid_initial_measurement_never_becomes_upward_restoration() {
        let mut j = Journal::default();
        j.authority(Some(authority()));
        j.native_generation(Some(1));
        j.provider(Some(7));
        let mut original = binding("eDP-1");
        original.minimum = 10;
        let now = Instant::now();
        j.reconcile(
            authority(),
            1,
            &[FreshReadback {
                binding: original.clone(),
                actual: 0,
                checked_at: now,
            }],
            now,
            0.3,
        )
        .unwrap();
        assert!(!j.readings[0].user_known);
        assert!(j.readings[0].measured_candidate.is_none());
        assert_eq!(
            j.begin(
                authority(),
                Source::Power {
                    provider_epoch: 7,
                    update: PolicyUpdate::Dimming(false)
                },
                vec![Target {
                    binding: original.clone(),
                    effective: 10,
                    user: None,
                    ratio: None
                }],
                &[FreshReadback {
                    binding: original,
                    actual: 0,
                    checked_at: now
                }],
                now
            ),
            Err(Error::PolicyUnknown)
        );
    }
    #[test]
    fn explicit_user_commit_replaces_original_measured_restoration() {
        let mut j = measured_journal(&[700]);
        j.provider(Some(7));
        measured_policy(&mut j, PolicyUpdate::Dimming(false)).unwrap();
        measured_policy(&mut j, PolicyUpdate::Automatic(-1.0)).unwrap();
        let mut t = target(800);
        t.user = Some(800);
        let grant = begin(&mut j, Source::User, vec![t]).unwrap();
        complete(&mut j, grant, 800, true).unwrap();
        assert!(j.readings[0].user_known);
        assert_eq!(j.readings[0].user, 800);
        assert!(j.readings[0].measured_restoration.is_none());
    }
    #[test]
    fn incomplete_panel_handshake_and_late_provider_during_pending_do_not_establish() {
        let mut j = measured_journal(&[700, 350]);
        j.provider(Some(7));
        measured_policy(&mut j, PolicyUpdate::Dimming(false)).unwrap();
        let grant = begin(
            &mut j,
            Source::Power {
                provider_epoch: 7,
                update: PolicyUpdate::Automatic(-1.0),
            },
            vec![target(700)],
        )
        .unwrap();
        complete(&mut j, grant, 700, true).unwrap();
        assert!(j
            .readings
            .iter()
            .all(|r| r.measured_restoration.is_none() && r.measured_candidate.is_none()));
        let mut j = measured_journal(&[700]);
        let grant = begin(&mut j, Source::Idle(0.3), vec![target(700)]).unwrap();
        j.provider(Some(7));
        assert!(j.readings[0].measured_candidate.is_none());
        assert_eq!(complete(&mut j, grant, 700, true), Err(Error::Stale));
    }
    #[test]
    fn wrong_grant_cannot_clear_new_measured_candidate_but_matching_invalid_receipt_does() {
        let mut j = measured_journal(&[700]);
        j.provider(Some(7));
        let grant = begin(
            &mut j,
            Source::Power {
                provider_epoch: 7,
                update: PolicyUpdate::Dimming(false),
            },
            vec![target(700)],
        )
        .unwrap();
        assert_eq!(
            complete(
                &mut j,
                Grant {
                    transaction: grant.transaction + 1,
                    revision: grant.revision
                },
                700,
                true
            ),
            Err(Error::Stale)
        );
        assert!(j.readings[0].measured_candidate.is_some());
        let mut invalid = observed(700, true);
        invalid.binding.inode += 1;
        assert_eq!(
            j.complete(authority(), grant, &[binding("eDP-1")], vec![invalid]),
            Err(Error::Binding)
        );
        assert!(j.readings[0].measured_candidate.is_none());
        assert!(j.pending.as_ref().unwrap().interrupted);
    }
}
