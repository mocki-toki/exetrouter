//! Durable, account-scoped recovery and final-day cohort fences.
use super::{automatic::digest, Live};
use crate::{
    quota::Window,
    routing::{fresh, weekly, DAY},
    Result,
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// A weekly reset deadline identifies the upstream quota cycle, independent of
/// which credit is still visible on a particular host. Never include a local ID,
/// a clock bucket, remaining percentages or the selected credit in this key.
pub(super) fn cycle(current: &Live, now: i64) -> Option<[u8; 32]> {
    cycle_for(&current.account.info.account_id, &current.quota, now)
}
fn cycle_for(account: &str, quota: &crate::quota::Summary, now: i64) -> Option<[u8; 32]> {
    let week = weekly(quota).filter(|w| valid(w, now))?;
    let mut hash = Sha256::new();
    hash.update(b"exetrouter-reset-cycle-v1\0");
    hash.update(account.as_bytes());
    hash.update(week.reset_at?.to_be_bytes());
    Some(hash.finalize().into())
}

fn valid(w: &Window, now: i64) -> bool {
    w.status == "current"
        && fresh(w.observed_at, now)
        && w.window_minutes.is_some()
        && w.reset_at.is_some_and(|at| at > now)
}

fn recovered(before: &[Window], current: &Observation, now: i64) -> bool {
    let old_week = before.iter().find(|w| w.window_minutes == Some(10080));
    let new_week = current
        .windows
        .iter()
        .find(|w| w.window_minutes == Some(10080));
    current.allowed == Some(true)
        && !current.windows.is_empty()
        && current
            .windows
            .iter()
            .all(|w| valid(w, now) && w.remaining_percent > 1.0)
        && old_week
            .zip(new_week)
            .is_some_and(|(old, new)| old.reset_at != new.reset_at)
        && before.iter().filter(|w| w.used_percent > 0.0).all(|old| {
            current.windows.iter().any(|new| {
                new.kind == old.kind
                    && new.window_minutes == old.window_minutes
                    && new.used_percent < old.used_percent
            })
        })
}

#[derive(Clone, Serialize, Deserialize)]
struct Inventory {
    digest: [u8; 32],
    expires_at: Option<i64>,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Observation {
    windows: Vec<Window>,
    allowed: Option<bool>,
    inventory: Vec<Inventory>,
    waiting: Option<Vec<Window>>,
    salvage_until: i64,
}

impl Observation {
    pub(super) fn live(current: &Live, now: i64) -> Option<Self> {
        let credits = current
            .credits
            .as_ref()
            .filter(|c| c.details_available && fresh(c.observed_at, now))?;
        Some(Self {
            windows: current.quota.windows.clone(),
            allowed: current.allowed,
            inventory: credits
                .credits
                .iter()
                .map(|c| Inventory {
                    digest: digest(&c.id),
                    expires_at: c.expires_at,
                })
                .collect(),
            waiting: None,
            salvage_until: 0,
        })
    }
    pub(super) fn cohort(&self, now: i64) -> i64 {
        self.inventory
            .iter()
            .filter_map(|c| c.expires_at)
            .filter(|at| *at > now && *at <= now + DAY)
            .max()
            .unwrap_or(0)
    }
}

pub(super) struct Gate {
    pub waiting: bool,
    pub salvage_until: i64,
}

/// Read-only preview, never observe/claim/recover. Require a worker-observed
/// inventory at the same generation/order as the current sanitized metadata.
pub(super) fn preview(conn: &Connection, id: i64, now: i64) -> Result<Option<i64>> {
    if !conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.id=?1 AND a.state='active' AND COALESCE(p.enabled,1)=1)",[id],|r|r.get::<_,bool>(0))? || crate::health::paused(conn,id,now)?.is_some() {return Ok(None);}
    let row=conn.query_row("SELECT a.account_id,m.credits,o.observation FROM oauth_accounts a JOIN account_routing_metadata m ON m.account_id=a.id AND m.generation=a.generation JOIN automatic_reset_observations o ON o.account_id=a.id AND o.request_order=m.request_order WHERE a.id=?1 AND m.observed_at<=?2 AND m.observed_at>?2-60",params![id,now],|r|Ok((r.get::<_,String>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,String>(2)?))).optional()?;
    let Some((account, Some(credits), observation)) = row else {
        return Ok(None);
    };
    let credits: super::Credits = serde_json::from_str(&credits)?;
    let observation: Observation = serde_json::from_str(&observation)?;
    let quota = crate::quota::summary(conn, id, now)?;
    if quota.cooldown_until.is_some() && quota.cooldown_source.as_deref() != Some("upstream_reset")
        || observation.waiting.is_some()
    {
        return Ok(None);
    }
    let waiting:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM automatic_reset_attempts WHERE account_id=?1 AND recovered_at IS NULL AND status IN ('submitted','unknown','reset','already_redeemed'))",[id],|r|r.get(0))?;
    let salvage:i64=conn.query_row("SELECT COALESCE(MAX(salvage_until),0) FROM automatic_reset_attempts WHERE account_id=?1 AND status NOT IN ('nothing_to_reset','no_credit','rejected')",[id],|r|r.get(0))?;
    if waiting
        || salvage.max(observation.salvage_until) > now
            && !weekly(&quota).is_some_and(|w| w.remaining_percent <= 1.0)
    {
        return Ok(None);
    }
    let Some(credit) = super::automatic::eligible_metadata(&quota, &credits, now) else {
        return Ok(None);
    };
    let Some(inventory) = observation
        .inventory
        .first()
        .filter(|c| c.expires_at == credit.expires_at)
    else {
        return Ok(None);
    };
    let Some(cycle) = cycle_for(&account, &quota, now) else {
        return Ok(None);
    };
    let attempt = Attempt {
        cycle,
        credit: inventory.digest,
        snapshot: quota.windows,
        salvage_until: salvage,
    };
    Ok(available(conn, &attempt, now)?
        .then_some(credit.expires_at)
        .flatten())
}

/// Observe inventory changes even on a host that did not submit the reset.
/// A disappearing unexpired credit fences the next one until real recovery;
/// the already-expiring cohort receives only one above-1% salvage.
pub(super) fn observe(
    conn: &mut Connection,
    id: i64,
    order: i64,
    mut current: Observation,
    now: i64,
) -> Result<Gate> {
    let tx = conn.transaction()?;
    let previous = tx.query_row("SELECT request_order,observation FROM automatic_reset_observations WHERE account_id=?1", [id], |r| Ok((r.get::<_,i64>(0)?, r.get::<_,String>(1)?))).optional()?;
    if let Some((previous_order, json)) = previous {
        if previous_order > order {
            return Ok(Gate {
                waiting: true,
                salvage_until: 0,
            });
        }
        let previous: Observation = serde_json::from_str(&json)?;
        current.waiting = previous.waiting.clone();
        current.salvage_until = previous.salvage_until;
        if previous.inventory.iter().any(|old| {
            old.expires_at.is_some_and(|at| at > now)
                && !current.inventory.iter().any(|new| new.digest == old.digest)
        }) {
            current
                .waiting
                .get_or_insert_with(|| previous.windows.clone());
            current.salvage_until = current.salvage_until.max(previous.cohort(now));
        }
    }
    if current
        .waiting
        .as_ref()
        .is_some_and(|before| recovered(before, &current, now))
    {
        current.waiting = None;
    }
    let rows = {
        let mut stmt = tx.prepare("SELECT cycle_digest,quota_snapshot FROM automatic_reset_attempts WHERE account_id=?1 AND recovered_at IS NULL AND status IN ('submitted','unknown','reset','already_redeemed') AND (status<>'submitted' OR attempted_at<=?2)")?;
        let rows = stmt
            .query_map(params![id, now - 120], |r| {
                Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        rows
    };
    for (cycle, snapshot) in rows {
        if recovered(
            &serde_json::from_str::<Vec<Window>>(&snapshot)?,
            &current,
            now,
        ) {
            tx.execute("UPDATE automatic_reset_attempts SET recovered_at=?1,status=CASE WHEN status IN ('submitted','unknown') THEN 'recovered' ELSE status END WHERE cycle_digest=?2", params![now,cycle])?;
        }
    }
    tx.execute("INSERT INTO automatic_reset_observations(account_id,request_order,observation) VALUES(?1,?2,?3) ON CONFLICT(account_id) DO UPDATE SET request_order=excluded.request_order,observation=excluded.observation", params![id,order,serde_json::to_string(&current)?])?;
    let pending: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM automatic_reset_attempts WHERE account_id=?1 AND recovered_at IS NULL AND status IN ('submitted','unknown','reset','already_redeemed'))", [id], |r| r.get(0))?;
    let salvage: i64 = tx.query_row("SELECT COALESCE(MAX(salvage_until),0) FROM automatic_reset_attempts WHERE account_id=?1 AND status NOT IN ('nothing_to_reset','no_credit','rejected')", [id], |r| r.get(0))?;
    let gate = Gate {
        waiting: pending || current.waiting.is_some(),
        salvage_until: salvage.max(current.salvage_until),
    };
    tx.commit()?;
    Ok(gate)
}

pub(super) struct Attempt {
    pub cycle: [u8; 32],
    pub credit: [u8; 32],
    pub snapshot: Vec<Window>,
    pub salvage_until: i64,
}

pub(super) fn available(conn: &Connection, attempt: &Attempt, now: i64) -> Result<bool> {
    let credited: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM automatic_reset_attempts WHERE credit_digest=?1 AND status IN ('submitted','unknown','reset','recovered'))", [attempt.credit.as_slice()], |r| r.get(0))?;
    if credited {
        return Ok(false);
    }
    let previous = conn.query_row("SELECT status,attempted_at,quota_snapshot FROM automatic_reset_attempts WHERE cycle_digest=?1", [attempt.cycle.as_slice()], |r| Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,String>(2)?))).optional()?;
    let Some((status, at, snapshot)) = previous else {
        return Ok(true);
    };
    if !matches!(
        status.as_str(),
        "nothing_to_reset" | "no_credit" | "rejected"
    ) || now - at < 30
    {
        return Ok(false);
    }
    // A confirmed no-op is not a spent credit. Retry only after fresh evidence
    // of NEW consumption, with the SAME upstream cycle key, never on each poll.
    let before: Vec<Window> = serde_json::from_str(&snapshot)?;
    Ok(attempt.snapshot.iter().any(|new| {
        valid(new, now)
            && before.iter().any(|old| {
                new.kind == old.kind
                    && new.window_minutes == old.window_minutes
                    && new.reset_at == old.reset_at
                    && new.used_percent > old.used_percent
            })
    }))
}

pub(super) fn claim(
    conn: &mut Connection,
    id: i64,
    generation: i64,
    order: i64,
    now: i64,
    attempt: Attempt,
) -> Result<bool> {
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    if !super::automatic::authorized(&tx, id, generation, now)? || !available(&tx, &attempt, now)? {
        return Ok(false);
    }
    let superseded: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM oauth_quota_windows WHERE account_id=?1 AND request_order>?2) OR EXISTS(SELECT 1 FROM oauth_quota_absences WHERE account_id=?1 AND request_order>?2) OR NOT EXISTS(SELECT 1 FROM account_routing_metadata WHERE account_id=?1 AND generation=?3 AND request_order=?2)",params![id,order,generation],|r|r.get(0))?;
    let uncertain: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM automatic_reset_attempts WHERE account_id=?1 AND recovered_at IS NULL AND status IN ('submitted','unknown','reset','already_redeemed'))", [id], |r| r.get(0))?;
    if superseded || uncertain {
        return Ok(false);
    }
    // Recheck the persisted cohort fence inside the mutation transaction.
    let observation: Option<String> = tx
        .query_row(
            "SELECT observation FROM automatic_reset_observations WHERE account_id=?1",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    let observation = observation
        .map(|s| serde_json::from_str::<Observation>(&s))
        .transpose()?;
    let salvage: i64 = tx.query_row("SELECT COALESCE(MAX(salvage_until),0) FROM automatic_reset_attempts WHERE account_id=?1 AND status NOT IN ('nothing_to_reset','no_credit','rejected')", [id], |r|r.get(0))?;
    if observation.as_ref().is_some_and(|s| s.waiting.is_some()) {
        return Ok(false);
    }
    if !attempt
        .snapshot
        .iter()
        .any(|w| w.window_minutes == Some(10080) && w.remaining_percent <= 1.0)
        && salvage.max(observation.as_ref().map_or(0, |s| s.salvage_until)) > now
    {
        return Ok(false);
    }
    tx.execute("INSERT INTO automatic_reset_attempts(cycle_digest,credit_digest,account_id,attempted_at,quota_snapshot,salvage_until,status) VALUES(?1,?2,?3,?4,?5,?6,'submitted') ON CONFLICT(cycle_digest) DO UPDATE SET credit_digest=excluded.credit_digest,attempted_at=excluded.attempted_at,quota_snapshot=excluded.quota_snapshot,salvage_until=excluded.salvage_until,status='submitted',recovered_at=NULL", params![attempt.cycle.as_slice(),attempt.credit.as_slice(),id,now,serde_json::to_string(&attempt.snapshot)?,attempt.salvage_until])?;
    tx.commit()?;
    Ok(true)
}
