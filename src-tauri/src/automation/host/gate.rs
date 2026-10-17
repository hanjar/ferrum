//! The gate (Automation design §10.4, SE-4): everything that must be true immediately before a run
//! touches a file, and the ONLY place that can make a [`GateOk`].
//!
//! `GateOk` is the proof the engine call needs. It has private fields, no public constructor and
//! no `Default`/`Clone`, so the host, the commands and the tests cannot fabricate one: the only
//! way to hold it is to have passed [`gate`]. [`HostDeps::run_engine`](super::HostDeps) takes a
//! `&GateOk`, and a source test keeps the engine's `run` out of every other file of this module.
//! That makes "every run path went through the gate" a property of the types, not of discipline:
//! scheduled and watch runs, catch-ups, pending reruns, retries, rechecks and Run now all become
//! queue entries (or a recheck request) and reach the engine through the same two functions.
//!
//! **`enabled` is not an authorization.** Step 4 calls [`policy::verify`] itself, on every call,
//! against the rows as they are right now. Nothing here infers "approved" from the `enabled`
//! flag, the queue row, the runtime row or an earlier verdict. The approval is checked again after
//! the lease is taken (the config can change while the roots are probed), and the run uses the
//! policy of THAT snapshot.
//!
//! Order (design §10.4): load in one read; enabled; runtime matches the host; approval valid;
//! attention hold; optional conditions; roots exist with matching identity; delete-policy
//! prerequisites; lease. A refusal never touches a file and says why with one [`GateRefusal`].
//!
//! The store closure below only READS rows (and `verify`, which reads); the root probes, the
//! conditions and the lease run after it has ended (the store mutex is not re-entrant).

use std::sync::Arc;

use crate::automation::dpapi::ApprovalKeys;
use crate::automation::lease::{GuardOutcome, LeaseEnv, LeaseGuard};
use crate::automation::model::{AutomationPolicy, Conditions, ProfilePolicy, Runtime};
use crate::automation::policy::{self, NeedsReview, Verdict};
use crate::automation::store::leases::{BusyHolder, HolderKind};
use crate::automation::store::queue::{HostClass, TriggerKind};
use crate::automation::store::runtime::Attention;
use crate::automation::store::{Store, StoreError, approvals, automations, profiles, runtime};
use crate::sync_engine::delete_policy::{GateError, RunRoots, check_gate};
use crate::sync_engine::env::EngineEnv;

/// What the caller wants to do. Only a `Run` takes a lease and may change files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateKind {
    /// A queued run of any trigger. `retry_of` is set for the resolving retry only.
    Run {
        trigger: TriggerKind,
        retry_of: Option<i64>,
    },
    /// A read-only fresh comparison of the run an attention hold names (RQ-10.1).
    Recheck { run_id: i64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Run,
    Retry { of: i64 },
    Recheck { of: i64 },
}

/// Everything the gate reads or calls outside the store.
pub struct GateCtx<'a> {
    pub store: &'a Arc<Store>,
    pub keys: &'a dyn ApprovalKeys,
    pub class: HostClass,
    pub lease_env: &'a LeaseEnv,
    pub env: &'a EngineEnv,
    /// `Some(reason)` while an optional condition (OP-1) keeps the run waiting.
    pub unmet_condition: &'a dyn Fn(&Conditions) -> Option<&'static str>,
}

/// The proof that the gate passed. Nothing outside this file can build one.
pub struct GateOk {
    automation: automations::AutomationRecord,
    policy: AutomationPolicy,
    profile_id: String,
    profile_revision: i64,
    profile: ProfilePolicy,
    policy_hash: String,
    roots: RunRoots,
    lease: Option<LeaseGuard>,
    mode: RunMode,
}

impl GateOk {
    pub fn automation_id(&self) -> &str {
        &self.automation.id
    }

    pub fn automation_name(&self) -> &str {
        &self.automation.name
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }

    pub fn profile_revision(&self) -> i64 {
        self.profile_revision
    }

    /// The profile policy of the verified snapshot - the one the approval covers.
    pub fn profile(&self) -> &ProfilePolicy {
        &self.profile
    }

    pub fn policy(&self) -> &AutomationPolicy {
        &self.policy
    }

    /// The approved policy hash this run was cleared under.
    pub fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    pub fn roots(&self) -> &RunRoots {
        &self.roots
    }

    /// `None` for a recheck (read only, no lease).
    pub fn lease(&self) -> Option<&LeaseGuard> {
        self.lease.as_ref()
    }

    pub fn mode(&self) -> RunMode {
        self.mode
    }
}

/// Why the gate said no. Each variant is one refusal reason of design §10.4.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateRefusal {
    /// The automation no longer exists.
    NotFound,
    /// Step 2: the automation is paused.
    Disabled,
    /// Step 3: it belongs to the other host class.
    WrongRuntime,
    /// Step 4: the approval does not verify.
    NeedsReview(NeedsReview),
    /// Step 5: an attention hold names a run, and this is not the resolving retry or recheck.
    Held,
    /// Step 6: an optional condition is unmet; the entry stays queued.
    Conditions(&'static str),
    /// Step 7: a folder is offline or is not the folder that was approved; stays queued.
    Device(GateError),
    /// Step 8: a delete-policy prerequisite or a protected-folder rule fails; the run fails.
    Blocked(GateError),
    /// Step 9: the folders are in use by another run; stays queued.
    LeaseBusy(BusyHolder),
    /// The approval or the profile changed while the gate was working.
    Changed,
    /// The store failed; nothing runs.
    Store(StoreError),
}

impl GateRefusal {
    /// A refusal that leaves the queue entry where it is (it may run later).
    pub fn keeps_entry(&self) -> bool {
        matches!(
            self,
            GateRefusal::Conditions(_)
                | GateRefusal::Device(_)
                | GateRefusal::LeaseBusy(_)
                | GateRefusal::Changed
                | GateRefusal::Store(_)
        )
    }

    /// Closed code used to log each refusal once.
    pub fn code(&self) -> &'static str {
        match self {
            GateRefusal::NotFound => "not_found",
            GateRefusal::Disabled => "disabled",
            GateRefusal::WrongRuntime => "wrong_runtime",
            GateRefusal::NeedsReview(_) => "needs_review",
            GateRefusal::Held => "held",
            GateRefusal::Conditions(_) => "conditions",
            GateRefusal::Device(e) | GateRefusal::Blocked(e) => e.code(),
            GateRefusal::LeaseBusy(_) => "lease_busy",
            GateRefusal::Changed => "changed",
            GateRefusal::Store(_) => "store",
        }
    }
}

impl From<StoreError> for GateRefusal {
    fn from(e: StoreError) -> Self {
        GateRefusal::Store(e)
    }
}

/// The rows the decision rests on, read in ONE store call and ONE read transaction.
struct Snapshot {
    automation: automations::AutomationRecord,
    policy: AutomationPolicy,
    profile_id: String,
    profile_revision: i64,
    profile: Option<ProfilePolicy>,
    policy_hash: String,
    config_rev: i64,
    verdict: Verdict,
    attention: Option<Attention>,
}

fn class_of(policy: &AutomationPolicy) -> HostClass {
    match policy.runtime {
        Runtime::App => HostClass::App,
        Runtime::Background => HostClass::Background,
    }
}

fn snapshot(
    store: &Store,
    keys: &dyn ApprovalKeys,
    automation_id: &str,
) -> Result<Snapshot, GateRefusal> {
    store.with_read(|conn| -> Result<Snapshot, GateRefusal> {
        // ONE real read transaction (BEGIN DEFERRED): every statement below sees the same
        // committed state, so the automation, the approval and the profile the verdict was
        // judged on are the ones the run uses. The transaction ends (rolls back) when the
        // closure returns.
        let tx = conn
            .unchecked_transaction()
            .map_err(|_| GateRefusal::Store(StoreError::Unavailable))?;
        let conn: &rusqlite::Connection = &tx;
        let automation = automations::get(conn, automation_id)?.ok_or(GateRefusal::NotFound)?;
        // An unreadable stored policy is a review, never a repair (design §4.4).
        let policy = serde_json::from_str::<AutomationPolicy>(&automation.policy);
        let verdict = policy::verify(conn, keys, automation_id)?;
        let profile_rec = profiles::get(conn, &automation.profile_id)?;
        let approval = approvals::get(conn, automation_id)?;
        let attention = runtime::get(conn, automation_id)?.and_then(|r| r.attention);
        let (Ok(policy), Some(profile_rec)) = (policy, profile_rec) else {
            // The run cannot even be described: it is a review, whatever `verify` said.
            return Err(GateRefusal::NeedsReview(match verdict {
                Verdict::NeedsReview(reason) => reason,
                Verdict::Valid => NeedsReview::PolicyChanged,
            }));
        };
        let profile = serde_json::from_str::<ProfilePolicy>(&profile_rec.policy).ok();
        // Belt and braces: the hash of the very policies that go into the proof must be the one
        // the approval carries (the verdict above already said so on the same snapshot).
        if matches!(verdict, Verdict::Valid) {
            let recomputed = profile.as_ref().and_then(|pp| {
                policy::policy_hash(
                    &automation.id,
                    &profile_rec.id,
                    profile_rec.revision,
                    pp,
                    &policy,
                )
            });
            let approved = approval.as_ref().map(|a| a.policy_hash.as_str());
            if recomputed.as_deref() != approved {
                return Err(GateRefusal::NeedsReview(NeedsReview::PolicyChanged));
            }
        }
        Ok(Snapshot {
            profile,
            profile_id: profile_rec.id.clone(),
            profile_revision: profile_rec.revision,
            policy_hash: approval.map(|a| a.policy_hash).unwrap_or_default(),
            config_rev: automation.config_rev,
            automation,
            policy,
            verdict,
            attention,
        })
    })
}

/// Classify a failed root/prerequisite check: offline or swapped folders wait (they may come
/// back); everything else is a refusal the user has to look at.
fn classify(e: GateError) -> GateRefusal {
    match e {
        GateError::RootUnavailable(_) | GateError::IdentityMismatch(_) => GateRefusal::Device(e),
        _ => GateRefusal::Blocked(e),
    }
}

/// `gate(&ctx, automation_id, kind)` - design §10.4. Returns the proof [`GateOk`] or the one reason
/// the run may not start. No file is touched here, and nothing is written to the store (the
/// caller records the refusal).
pub fn gate(ctx: &GateCtx<'_>, automation_id: &str, kind: GateKind) -> Result<GateOk, GateRefusal> {
    // 1. Load the rows and verify the approval, in one read transaction.
    let first = snapshot(ctx.store, ctx.keys, automation_id)?;
    let recheck = matches!(kind, GateKind::Recheck { .. });
    // 2. Enabled. A recheck is a read-only comparison and may look at a paused automation.
    if !first.automation.enabled && !recheck {
        return Err(GateRefusal::Disabled);
    }
    // 3. The runtime matches this host.
    if class_of(&first.policy) != ctx.class {
        return Err(GateRefusal::WrongRuntime);
    }
    // 4. The approval verifies. Never inferred from `enabled` (the verdict is recomputed above).
    if let Verdict::NeedsReview(reason) = first.verdict {
        return Err(GateRefusal::NeedsReview(reason));
    }
    let Some(profile) = first.profile.clone() else {
        return Err(GateRefusal::NeedsReview(NeedsReview::PolicyChanged));
    };
    // 5. Attention: only the retry that resolves it, or a recheck of the run it names, may pass.
    let mode = match kind {
        GateKind::Run {
            trigger: TriggerKind::Retry,
            retry_of: Some(of),
        } => RunMode::Retry { of },
        GateKind::Run { .. } => RunMode::Run,
        GateKind::Recheck { run_id } => RunMode::Recheck { of: run_id },
    };
    if let Some(attention) = &first.attention {
        let resolves = match mode {
            RunMode::Retry { of } | RunMode::Recheck { of } => of == attention.run_id,
            RunMode::Run => false,
        };
        if !resolves {
            return Err(GateRefusal::Held);
        }
    }
    // 6. Optional conditions (OP-1) keep a run queued; they do not apply to a read-only recheck.
    if !recheck && let Some(why) = (ctx.unmet_condition)(&first.policy.conditions) {
        return Err(GateRefusal::Conditions(why));
    }
    // 7 + 8. Roots exist with the approved identity; Recycle Bin / versioning prerequisites and
    // the protected-folder rules. Probes run here, after the store closure has ended.
    let roots = check_gate(&profile, ctx.env).map_err(classify)?;
    // 9. The lease (a recheck compares read-only and needs none).
    let lease = if recheck {
        None
    } else {
        let holder = match ctx.class {
            HostClass::App => HolderKind::App,
            HostClass::Background => HolderKind::Runner,
        };
        match LeaseGuard::acquire(
            ctx.store,
            ctx.lease_env,
            holder,
            None,
            &roots.lease_roots(),
            None,
        )? {
            GuardOutcome::Acquired(guard) => Some(guard),
            GuardOutcome::Busy(who) => return Err(GateRefusal::LeaseBusy(who)),
        }
    };
    // The configuration may have changed while the folders were probed and the lease taken: verify
    // again against the rows as they are now, and require that nothing moved. The lease (if any)
    // is released by `Drop` on every early return below.
    let second = snapshot(ctx.store, ctx.keys, automation_id)?;
    if !matches!(second.verdict, Verdict::Valid)
        || second.policy_hash != first.policy_hash
        || second.profile_revision != first.profile_revision
        || second.config_rev != first.config_rev
        || second.attention != first.attention
        || (!second.automation.enabled && !recheck)
    {
        return Err(GateRefusal::Changed);
    }
    Ok(GateOk {
        automation: first.automation,
        policy: first.policy,
        profile_id: first.profile_id,
        profile_revision: first.profile_revision,
        profile,
        policy_hash: first.policy_hash,
        roots,
        lease,
        mode,
    })
}
