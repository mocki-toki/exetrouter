//! Interactive local operator override. Eligibility differs from automation;
//! durable claims, recovery fences and the upstream mutation do not.
use super::*;
use crate::routing::{fresh, weekly, DAY};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::time::Instant;

const CONFIRMATION_LIFETIME: Duration = Duration::from_secs(120);
const MAX_WEEKLY_REMAINING: f64 = 5.0;

pub(super) struct Preview {
    id: i64,
    generation: i64,
    cycle: [u8; 32],
    credit: [u8; 32],
    inventory: Vec<[u8; 32]>,
    expires_at: i64,
    weekly_reset: i64,
    weekly_remaining: f64,
    email: Option<String>,
    pub(super) deadline: Instant,
}

pub(super) struct Outcome {
    pub status: &'static str,
    pub verified: bool,
}

pub fn require_terminal(json: bool) -> Result<()> {
    if json || !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("Reset requires an interactive local terminal; --json is not supported".into());
    }
    Ok(())
}

fn local_time(at: i64) -> String {
    DateTime::from_timestamp(at, 0).map_or_else(
        || "Unknown".into(),
        |time| {
            time.with_timezone(&chrono::Local)
                .format("%b %-d, %Y, %I:%M %p %Z")
                .to_string()
        },
    )
}

pub async fn command(upstream: &Upstream, account: &str) -> Result<()> {
    require_terminal(false)?;
    let preview = upstream.preview_manual_reset(account).await?;
    let identity = preview.email.as_deref().map_or_else(
        || format!("Account {}", preview.id),
        |email| format!("{email} (account {})", preview.id),
    );
    let remaining = format!("{:.3}", preview.weekly_remaining);
    let remaining = remaining.trim_end_matches('0').trim_end_matches('.');
    println!(
        "Account: {identity}\nWeekly quota: {remaining}% left; scheduled reset {}\nCredit: Full reset, expires {}\nThis spends one credit and cannot be undone.",
        local_time(preview.weekly_reset),
        local_time(preview.expires_at),
    );
    if preview.weekly_reset.saturating_sub(Utc::now().timestamp()) < 3 * DAY {
        println!("The scheduled weekly reset is less than three days away. Consider waiting if this credit will not expire first.");
    }
    let confirmation = format!("RESET {}", preview.id);
    print!("Type {confirmation} within two minutes to confirm: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().lock().take(64).read_line(&mut answer)?;
    if !answer.ends_with('\n') || answer.trim_end_matches(['\r', '\n']) != confirmation {
        return Err("Reset cancelled; no credit submitted".into());
    }

    let result = upstream.confirm_manual_reset(preview).await?;
    match (result.status, result.verified) {
        ("reset", true) => println!("One reset credit redeemed; fresh quota and credit inventory confirm recovery."),
        ("already_redeemed", true) => println!("Upstream reports this weekly reset was already redeemed; fresh quota and inventory confirm recovery."),
        ("reset" | "already_redeemed", false) => return Err("Upstream reports a reset, but quota recovery and credit use could not both be verified. Do not repeat the reset.".into()),
        ("nothing_to_reset", _) => return Err("Upstream reported nothing to reset; no credit spent by this request".into()),
        ("no_credit", _) => return Err("Upstream reported no available credit; no credit spent by this request".into()),
        ("rejected", _) => return Err("Upstream rejected the reset request".into()),
        (_, true) => return Err("Upstream did not confirm redemption, but fresh quota recovered and the selected credit is no longer available. Do not repeat the reset.".into()),
        (_, false) => return Err("Reset outcome unknown. Inspect current limits and credit inventory; do not repeat the reset.".into()),
    }
    Ok(())
}

fn eligible(current: &Live, now: i64) -> Result<(&Credit, &crate::quota::Window)> {
    let week = weekly(&current.quota)
        .filter(|w| {
            w.status == "current"
                && fresh(w.observed_at, now)
                && w.reset_at.is_some_and(|at| at > now)
        })
        .ok_or("A current weekly quota window with a known future reset is required")?;
    if week.remaining_percent > MAX_WEEKLY_REMAINING {
        return Err("Manual reset requires 5% or less weekly quota remaining".into());
    }
    let credits = current
        .credits
        .as_ref()
        .filter(|c| c.details_available && current.inventory.is_some() && fresh(c.observed_at, now))
        .ok_or("Complete live reset-credit inventory is unavailable")?;
    let credit = credits
        .credits
        .first()
        .filter(|c| c.expires_at.is_some_and(|at| at > now))
        .ok_or("No supported reset credit with a known future expiry is available")?;
    Ok((credit, week))
}

fn ensure_authorized(conn: &rusqlite::Connection, id: i64, generation: i64) -> Result<()> {
    if !automatic::authorized(conn, id, generation, Utc::now().timestamp())? {
        return Err("The account is unavailable, paused or its credentials have changed".into());
    }
    Ok(())
}

impl Upstream {
    async fn resolve_reset_account(&self, selector: &str) -> Result<i64> {
        let numeric = selector.parse::<i64>().ok().filter(|id| *id > 0);
        if numeric.is_none() && !selector.contains('@') {
            return Err("Specify a positive local account ID or exact account email".into());
        }
        let selector = selector.to_owned();
        let vault = self.vault.clone();
        self.db
            .call(move |conn| {
                let mut matches = Vec::new();
                for info in oauth::list(conn)? {
                    let matched = if let Some(id) = numeric {
                        info.id == id
                    } else {
                        let account = oauth::load(conn, &vault, info.id)?;
                        oauth::token_email(&account.credentials.access_token).as_deref()
                            == Some(&selector)
                    };
                    if matched {
                        matches.push(info);
                    }
                }
                match matches.as_slice() {
                    [info] => {
                        ensure_authorized(conn, info.id, info.generation)?;
                        Ok(info.id)
                    }
                    [] => Err("No OAuth account matches that ID or email".into()),
                    _ => Err(
                        "More than one OAuth account has that email; use a local account ID".into(),
                    ),
                }
            })
            .await
    }

    pub(super) async fn preview_manual_reset(&self, selector: &str) -> Result<Preview> {
        let id = self.resolve_reset_account(selector).await?;
        let current = self.live_limits(id).await?;
        let generation = current.account.info.generation;
        let email = oauth::token_email(&current.account.credentials.access_token);
        if selector.contains('@') && email.as_deref() != Some(selector) {
            return Err("The account email changed during preflight; no credit submitted".into());
        }
        self.db
            .call(move |conn| ensure_authorized(conn, id, generation))
            .await?;
        let now = Utc::now().timestamp();
        let (credit, week) = eligible(&current, now)?;
        let attempt = self
            .reset_attempt(&current, now, Some(credit))
            .await?
            .ok_or(
                "A prior reset, recent quota change or credit recovery fence prevents this reset",
            )?;
        Ok(Preview {
            id,
            generation,
            cycle: attempt.cycle,
            credit: attempt.credit,
            inventory: current.inventory.clone().expect("validated inventory"),
            expires_at: credit.expires_at.expect("validated expiry"),
            weekly_reset: week.reset_at.expect("validated reset"),
            weekly_remaining: week.remaining_percent,
            email,
            deadline: Instant::now() + CONFIRMATION_LIFETIME,
        })
    }

    pub(super) async fn confirm_manual_reset(&self, preview: Preview) -> Result<Outcome> {
        let changed = "The account, weekly cycle, eligibility or credit inventory changed; no credit submitted";
        let expired = "Reset confirmation expired; no credit submitted";
        if Instant::now() >= preview.deadline {
            return Err(expired.into());
        }
        let id = preview.id;
        let generation = preview.generation;
        self.db
            .call(move |conn| ensure_authorized(conn, id, generation))
            .await?;
        let current = self.live_limits(id).await?;
        let now = Utc::now().timestamp();
        let (credit, _) = eligible(&current, now)?;
        if current.account.info.generation != generation
            || ledger::cycle(&current, now) != Some(preview.cycle)
            || automatic::digest(&credit.id) != preview.credit
            || credit.expires_at != Some(preview.expires_at)
            || current.inventory.as_ref() != Some(&preview.inventory)
        {
            return Err(changed.into());
        }
        let attempt = self
            .reset_attempt(&current, now, Some(credit))
            .await?
            .ok_or(changed)?;
        if Instant::now() >= preview.deadline {
            return Err(expired.into());
        }
        if !self.claim_reset(&current, now, attempt).await? {
            return Err(changed.into());
        }
        let status = self
            .submit_reset(&current.account, credit, preview.cycle)
            .await;
        let mut verified = false;
        if let Ok(after) = self.live_limits(id).await {
            let now = Utc::now().timestamp();
            if let (Some(inventory), Some(observation)) =
                (&after.inventory, ledger::Observation::live(&after, now))
            {
                verified = after.account.info.generation == generation
                    && now < preview.expires_at
                    && !inventory.contains(&preview.credit)
                    && ledger::recovered(&current.quota.windows, &observation, now);
                let _ = self.observe_reset(&after, now).await;
            }
        }
        let status = status.unwrap_or("unknown");
        tracing::info!(
            event = "manual_reset_finished",
            account = id,
            status,
            verified
        );
        Ok(Outcome { status, verified })
    }
}
