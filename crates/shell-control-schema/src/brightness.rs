//! Session-only brightness journal messages. Full bindings remain bounded;
//! neither kernel-object identity nor these records authorize disk restoration.
use crate::NativeOutputInfo;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessBinding {
    pub output: NativeOutputInfo,
    pub ownership_generation: u64,
    pub backlight: String,
    pub device: u64,
    pub inode: u64,
    pub minimum: u32,
    pub maximum: u32,
}
/// First original kernel measurement; never committed User intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessMeasuredCandidate {
    pub level: u32,
    pub sampled_revision: u64,
    pub provider_epoch: Option<u64>,
}
/// A whole-panel no-change genuine NORMAL/Auto=-1 completion receipt.
/// The immutable original Reading binding supplies all owner/generation fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessMeasuredRestoration {
    pub level: u32,
    pub sampled_revision: u64,
    pub provider_epoch: u64,
    pub establishing_grant: BrightnessGrant,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrightnessReading {
    pub binding: BrightnessBinding,
    pub user: u32,
    /// Current readback cannot establish a committed User baseline.
    pub user_known: bool,
    pub measured_candidate: Option<BrightnessMeasuredCandidate>,
    pub measured_restoration: Option<BrightnessMeasuredRestoration>,
    pub applied: u32,
    pub ratio: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrightnessPolicy {
    pub dimming: Option<bool>,
    pub automatic: Option<f64>,
    pub idle: f64,
}
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum BrightnessPolicyUpdate {
    Dimming(bool),
    Automatic(f64),
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum BrightnessSource {
    User,
    External {
        update: BrightnessPolicyUpdate,
    },
    /// Actual invocation sender. The compositor must authenticate the original
    /// genuine GSD owner off-frame; this string itself confers no authority.
    Power {
        sender: String,
        update: BrightnessPolicyUpdate,
    },
    Idle(f64),
    Automatic,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrightnessTarget {
    pub binding: BrightnessBinding,
    pub effective: u32,
    pub user: Option<u32>,
    pub ratio: Option<f64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessObservation {
    pub binding: BrightnessBinding,
    pub actual: u32,
    pub helper_succeeded: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessGrant {
    pub transaction: u64,
    pub revision: u64,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrightnessPending {
    pub grant: BrightnessGrant,
    pub targets: Vec<BrightnessTarget>,
    pub interrupted: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessAuthority {
    pub child_generation: u64,
    pub connection_generation: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrightnessProvider {
    pub unique: String,
    pub epoch: u64,
    pub uid: u32,
    pub pid: u32,
    pub start: String,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrightnessJournalSnapshot {
    pub revision: u64,
    pub native_generation: Option<u64>,
    pub authority: Option<BrightnessAuthority>,
    pub provider: Option<BrightnessProvider>,
    pub readings: Vec<BrightnessReading>,
    /// Bounded revoked original intent; these bindings confer no restore grant.
    pub retired_readings: Vec<BrightnessReading>,
    pub policy: BrightnessPolicy,
    pub pending: Option<BrightnessPending>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BrightnessJournalError {
    Authority,
    Bounds,
    Binding,
    Busy,
    PolicyUnknown,
    Provider,
    Stale,
    Incomplete,
}
