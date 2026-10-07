//! Original connection/request-specific brightness admission. Cached metadata
//! alone never authorizes a helper; each grant must match its original await.
use roost_shell_control::{
    BrightnessAuthority, BrightnessGrant, BrightnessJournalSnapshot, BrightnessObservation,
    BrightnessReading, BrightnessSource, BrightnessTarget,
};
use roost_shell_host::control::BrightnessReply;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
const DEADLINE: Duration = Duration::from_secs(2);
#[derive(Clone)]
pub enum Command {
    Initialize {
        readings: Vec<BrightnessReading>,
        idle: f64,
    },
    Begin {
        source: BrightnessSource,
        targets: Vec<BrightnessTarget>,
    },
    Complete {
        grant: BrightnessGrant,
        observations: Vec<BrightnessObservation>,
    },
}
#[derive(Clone)]
pub struct Bridge {
    pub snapshot: Rc<dyn Fn() -> Option<BrightnessJournalSnapshot>>,
    pub send: Rc<dyn Fn(Command) -> Result<u64, String>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permit {
    pub authority: BrightnessAuthority,
    pub generation: u64,
    pub grant: BrightnessGrant,
    pub admitted: Instant,
}
pub struct Receipt {
    pub reply: BrightnessReply,
    /// Original request admission; receiving a reply never renews this budget.
    pub started: Instant,
}
type Done = Box<dyn FnOnce(Result<Receipt, String>)>;
#[derive(Clone)]
enum Expected {
    Initialize,
    Begin(Vec<BrightnessTarget>),
    Complete(BrightnessGrant),
}
struct Await {
    request: u64,
    authority: BrightnessAuthority,
    generation: u64,
    started: Instant,
    expected: Expected,
    done: Done,
}
pub struct Client {
    bridge: Bridge,
    awaiting: RefCell<Option<Await>>,
}
impl Client {
    pub fn new(bridge: Bridge) -> Rc<Self> {
        let client = Rc::new(Self {
            bridge,
            awaiting: RefCell::new(None),
        });
        let weak = Rc::downgrade(&client);
        glib::timeout_add_local(Duration::from_millis(100), move || {
            let Some(client) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            client.expire();
            glib::ControlFlow::Continue
        });
        client
    }
    pub fn snapshot(&self) -> Option<BrightnessJournalSnapshot> {
        (self.bridge.snapshot)()
    }
    pub fn pending(&self) -> bool {
        self.awaiting.borrow().is_some()
    }
    pub fn same_original(&self, permit: Permit) -> bool {
        self.snapshot().is_some_and(|v| {
            v.authority == Some(permit.authority)
                && v.native_generation == Some(permit.generation)
                && v.pending
                    .as_ref()
                    .is_some_and(|pending| pending.grant == permit.grant && !pending.interrupted)
        })
    }
    pub fn may_submit(&self, permit: Permit) -> bool {
        fresh(permit.admitted, Instant::now()) && self.same_original(permit)
    }
    pub fn request(self: &Rc<Self>, command: Command, done: Done) {
        if self.pending() {
            done(Err("brightness journal await already pending".into()));
            return;
        }
        let Some(state) = self.snapshot() else {
            done(Err("brightness journal unavailable".into()));
            return;
        };
        let (Some(authority), Some(generation)) = (state.authority, state.native_generation) else {
            done(Err("brightness native authority unavailable".into()));
            return;
        };
        if authority.child_generation == 0
            || authority.connection_generation == 0
            || generation == 0
        {
            done(Err("brightness original authority invalid".into()));
            return;
        }
        let expected = match &command {
            Command::Initialize { .. } => Expected::Initialize,
            Command::Begin { targets, .. } => Expected::Begin(targets.clone()),
            Command::Complete { grant, .. } => Expected::Complete(*grant),
        };
        let started = Instant::now();
        match (self.bridge.send)(command) {
            Ok(request) => {
                *self.awaiting.borrow_mut() = Some(Await {
                    request,
                    authority,
                    generation,
                    started,
                    expected,
                    done,
                })
            }
            Err(error) => done(Err(error)),
        }
    }
    fn expire(&self) {
        let expired = self.awaiting.borrow().as_ref().is_some_and(|v| {
            !fresh(v.started, Instant::now())
                || !self.snapshot().is_some_and(|state| {
                    state.authority == Some(v.authority)
                        && state.native_generation == Some(v.generation)
                })
        });
        if expired {
            let pending = self.awaiting.borrow_mut().take();
            if let Some(pending) = pending {
                (pending.done)(Err("brightness original await expired or revoked".into()));
            }
        }
    }
    pub fn receive(&self, reply: BrightnessReply) {
        let matches = self
            .awaiting
            .borrow()
            .as_ref()
            .is_some_and(|v| v.request == reply.request);
        if !matches {
            return;
        }
        let Some(awaiting) = self.awaiting.borrow_mut().take() else {
            return;
        };
        let result = validated_receipt(&awaiting, reply, self.snapshot().as_ref(), Instant::now());
        (awaiting.done)(result);
    }
}
pub(crate) fn fresh(started: Instant, now: Instant) -> bool {
    now.checked_duration_since(started)
        .is_some_and(|v| v <= DEADLINE)
}
fn validated_receipt(
    awaiting: &Await,
    reply: BrightnessReply,
    current: Option<&BrightnessJournalSnapshot>,
    now: Instant,
) -> Result<Receipt, String> {
    validate(awaiting, &reply, current, now)?;
    Ok(Receipt {
        reply,
        started: awaiting.started,
    })
}

fn validate(
    awaiting: &Await,
    reply: &BrightnessReply,
    current: Option<&BrightnessJournalSnapshot>,
    now: Instant,
) -> Result<(), String> {
    let fail = || "brightness original journal reply rejected".to_owned();
    if reply.request != awaiting.request || !fresh(awaiting.started, now) || reply.error.is_some() {
        return Err(fail());
    }
    let state = reply.state.as_ref().ok_or_else(fail)?;
    if state.authority != Some(awaiting.authority)
        || state.native_generation != Some(awaiting.generation)
        || !current.is_some_and(|v| {
            v.authority == Some(awaiting.authority)
                && v.native_generation == Some(awaiting.generation)
        })
    {
        return Err(fail());
    }
    match &awaiting.expected {
        Expected::Initialize if reply.grant.is_none() => Ok(()),
        Expected::Begin(targets) => {
            let grant = reply.grant.ok_or_else(fail)?;
            let pending = state.pending.as_ref().ok_or_else(fail)?;
            if grant.transaction == 0
                || grant.revision != state.revision
                || pending.grant != grant
                || pending.interrupted
                || &pending.targets != targets
            {
                return Err(fail());
            }
            Ok(())
        }
        Expected::Complete(grant)
            if reply.grant.is_none()
                && state.pending.is_none()
                && state.revision > grant.revision =>
        {
            Ok(())
        }
        _ => Err(fail()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> BrightnessJournalSnapshot {
        BrightnessJournalSnapshot {
            revision: 1,
            native_generation: Some(1),
            authority: Some(BrightnessAuthority {
                child_generation: 1,
                connection_generation: 1,
            }),
            provider: None,
            readings: Vec::new(),
            retired_readings: Vec::new(),
            policy: roost_shell_control::BrightnessPolicy {
                dimming: None,
                automatic: None,
                idle: 0.3,
            },
            pending: None,
        }
    }
    fn awaiting(started: Instant) -> Await {
        Await {
            request: 7,
            authority: state().authority.unwrap(),
            generation: 1,
            started,
            expected: Expected::Initialize,
            done: Box::new(|_| {}),
        }
    }
    #[test]
    fn exact_original_request_connection_generation_and_deadline_required() {
        let now = Instant::now();
        let original = awaiting(now);
        let mut reply = BrightnessReply {
            request: 7,
            grant: None,
            error: None,
            state: Some(state()),
        };
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_ok());
        reply.request = 8;
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_err());
        reply.request = 7;
        reply
            .state
            .as_mut()
            .unwrap()
            .authority
            .as_mut()
            .unwrap()
            .connection_generation = 2;
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_err());
        reply.state = Some(state());
        assert!(validate(&original, &reply, reply.state.as_ref(), now + DEADLINE).is_ok());
        assert!(validate(
            &original,
            &reply,
            reply.state.as_ref(),
            now + DEADLINE + Duration::from_nanos(1)
        )
        .is_err());
        assert!(validate(
            &original,
            &reply,
            reply.state.as_ref(),
            now - Duration::from_nanos(1)
        )
        .is_err());
    }
    #[test]
    fn passive_snapshot_cannot_authorize_hardware_or_complete_pending() {
        let now = Instant::now();
        let mut original = awaiting(now);
        original.expected = Expected::Begin(Vec::new());
        let mut reply = BrightnessReply {
            request: 7,
            grant: None,
            error: None,
            state: Some(state()),
        };
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_err());
        let grant = BrightnessGrant {
            transaction: 1,
            revision: 1,
        };
        reply.grant = Some(grant);
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_err());
        reply.state.as_mut().unwrap().pending = Some(roost_shell_control::BrightnessPending {
            grant,
            targets: Vec::new(),
            interrupted: false,
        });
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_ok());
        reply
            .state
            .as_mut()
            .unwrap()
            .pending
            .as_mut()
            .unwrap()
            .interrupted = true;
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_err());
        original.expected = Expected::Complete(grant);
        reply.grant = None;
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_err());
        reply.state.as_mut().unwrap().pending = None;
        reply.state.as_mut().unwrap().revision = 2;
        assert!(validate(&original, &reply, reply.state.as_ref(), now).is_ok());
    }
    #[test]
    fn late_validated_receipt_retains_original_request_start_for_helper_budget() {
        let started = Instant::now();
        let awaiting = awaiting(started);
        let reply = BrightnessReply {
            request: 7,
            grant: None,
            error: None,
            state: Some(state()),
        };
        let receipt = validated_receipt(
            &awaiting,
            reply,
            Some(&state()),
            started + Duration::from_millis(1900),
        )
        .unwrap();
        assert_eq!(receipt.started, started);
        assert!(fresh(receipt.started, started + DEADLINE));
        assert!(!fresh(
            receipt.started,
            started + DEADLINE + Duration::from_nanos(1)
        ));
    }
}
