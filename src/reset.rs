//! Bounded live subscription metadata and automatic, idempotent reset redemption.
mod automatic;
mod ledger;
use crate::{
    doctor::ServerReport,
    oauth::{self, Account},
    upstream::{account_headers, Upstream},
    Result,
};
use chrono::{DateTime, Utc};
use futures_util::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashMap, time::Duration};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credit {
    pub title: String,
    pub expires_at: Option<i64>,
    #[serde(skip)]
    id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credits {
    pub available_count: i64,
    pub credits: Vec<Credit>,
    pub details_available: bool,
    pub observed_at: i64,
}
#[derive(Default)]
pub(crate) struct State {
    pending: HashMap<i64, automatic::Pending>,
}
struct Live {
    account: Account,
    order: i64,
    quota: crate::quota::Summary,
    credits: Option<Credits>,
    allowed: Option<bool>,
}

fn usage_observation(value: &Value, now: i64) -> crate::quota::Observation {
    let mut limits = json!({});
    for kind in ["primary", "secondary"] {
        let w = &value["rate_limit"][format!("{kind}_window")];
        let seconds = w["limit_window_seconds"].as_i64().filter(|s| *s > 0);
        limits[kind] = json!({"used_percent":w["used_percent"],"window_minutes":seconds.map(|s|s.saturating_add(59)/60),"reset_at":w["reset_at"]});
    }
    crate::quota::event(
        &json!({"type":"codex.rate_limits","rate_limits":limits}),
        now,
    )
    .with_live_allowed(value["rate_limit"]["allowed"].as_bool() == Some(true))
    .with_live_absent_windows(
        ["primary", "secondary"]
            .into_iter()
            .filter(|kind| {
                value["rate_limit"]
                    .get(format!("{kind}_window"))
                    .is_some_and(Value::is_null)
            })
            .collect(),
    )
}
fn parse_credits(usage: &Value, details: Option<&Value>, now: i64) -> Option<Credits> {
    let count = details
        .and_then(|v| v["available_count"].as_i64())
        .or_else(|| usage["rate_limit_reset_credits"]["available_count"].as_i64())?;
    let mut credits: Vec<_> = details
        .and_then(|v| v["credits"].as_array())
        .into_iter()
        .flatten()
        .filter(|c| c["status"] == "available" && c["reset_type"] == "codex_rate_limits")
        .filter_map(|c| {
            let expires = if c["expires_at"].is_null() {
                None
            } else {
                Some(
                    DateTime::parse_from_rfc3339(c["expires_at"].as_str()?)
                        .ok()?
                        .timestamp(),
                )
            };
            if expires.is_some_and(|at| at <= now) {
                return None;
            }
            let id = c["id"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 512)?;
            Some(Credit {
                id: id.into(),
                title: "Full reset".into(),
                expires_at: expires,
            })
        })
        .collect();
    // Identical ordering on independent hosts, including tied expirations.
    credits.sort_by(|a, b| {
        a.expires_at
            .unwrap_or(i64::MAX)
            .cmp(&b.expires_at.unwrap_or(i64::MAX))
            .then_with(|| a.id.cmp(&b.id))
    });
    Some(Credits {
        available_count: count.max(0),
        credits,
        details_available: details.is_some_and(|v| v["credits"].is_array()),
        observed_at: now,
    })
}

impl Upstream {
    fn limits_url(&self, path: &str) -> String {
        let base = if self.config.base_url == crate::upstream::BACKEND {
            "https://chatgpt.com/backend-api/wham"
        } else {
            &self.config.base_url
        };
        format!("{base}/{path}")
    }
    async fn limits_get(&self, account: &Account, path: &str, seconds: u64) -> Result<Value> {
        let response = self
            .client
            .get(self.limits_url(path))
            .headers(account_headers(account)?)
            .timeout(Duration::from_secs(seconds))
            .send()
            .await
            .map_err(|_| "Subscription limits unavailable")?;
        if !response.status().is_success() {
            return Err("Subscription limits rejected by upstream".into());
        }
        oauth::read_json(response, 262_144).await
    }
    async fn live_limits(&self, id: i64) -> Result<Live> {
        let account = self.account(id).await?;
        let generation = account.info.generation;
        let order = self.db.call(|conn| crate::health::next(conn)).await?;
        let usage = self.limits_get(&account, "usage", 8).await?;
        let now = Utc::now().timestamp();
        let observation = usage_observation(&usage, now);
        let mut quota = self
            .db
            .call(move |conn| {
                crate::quota::save(conn, id, generation, order, observation, now)?;
                crate::quota::summary(conn, id, now)
            })
            .await?;
        // Redemption can use only windows returned by this exact GET, never old rows.
        quota.windows = ["primary", "secondary"]
            .into_iter()
            .filter_map(|kind| {
                let w = &usage["rate_limit"][format!("{kind}_window")];
                let used = w["used_percent"]
                    .as_f64()
                    .filter(|v| v.is_finite() && *v >= 0.0)?;
                let seconds = w["limit_window_seconds"].as_i64().filter(|s| *s > 0);
                let reset = w["reset_at"]
                    .as_i64()
                    .filter(|at| (0..=253_402_300_799).contains(at));
                Some(crate::quota::Window {
                    kind: kind.into(),
                    used_percent: used,
                    remaining_percent: (100.0 - used).max(0.0),
                    window_minutes: seconds.map(|s| s.saturating_add(59) / 60),
                    reset_at: reset,
                    observed_at: now,
                    status: if reset.is_some_and(|at| at <= now) {
                        "reset_elapsed"
                    } else {
                        "current"
                    }
                    .into(),
                })
            })
            .collect();
        let details = self
            .limits_get(&account, "rate-limit-reset-credits", 3)
            .await
            .ok();
        let credits = parse_credits(&usage, details.as_ref(), now);
        // An explicit, well-formed live weekly-only payload proves absence of a short window.
        let valid_week = quota
            .windows
            .iter()
            .any(|w| w.window_minutes == Some(10080));
        let short = if valid_week
            && ["primary_window", "secondary_window"].iter().all(|key| {
                let w = &usage["rate_limit"][*key];
                usage["rate_limit"].get(*key).is_some()
                    && (w.is_null() || w["limit_window_seconds"].as_i64().is_some_and(|s| s > 0))
            }) {
            Some(["primary_window", "secondary_window"].iter().any(|kind| {
                usage["rate_limit"][*kind]["limit_window_seconds"]
                    .as_i64()
                    .is_some_and(|s| (1..604800).contains(&s))
            }))
        } else {
            None
        };
        let saved = credits.clone();
        self.db
            .call(move |conn| {
                crate::routing::save(conn, id, generation, order, now, short, saved.as_ref())
            })
            .await?;
        Ok(Live {
            account,
            order,
            quota,
            credits,
            allowed: usage["rate_limit"]["allowed"].as_bool(),
        })
    }

    /// Diagnostics only read metadata; automatic mutations belong to the runtime worker.
    pub(crate) async fn account_metadata(&self, report: &mut ServerReport, live: bool, user: i64) {
        let activated = std::sync::atomic::AtomicBool::new(false);
        let activated = &activated;
        stream::iter(report.quota_accounts.iter_mut()).for_each_concurrent(Some(4),|row|async move{
            let id=row.id;
            let vault=self.vault.clone();
            if let Ok(Some(email))=self.db.call(move |conn|Ok(oauth::load(conn,&vault,id).ok().and_then(|a|oauth::token_email(&a.credentials.access_token)))).await {row.label=email;}
            row.reset_credits=self.db.call(move |conn|Ok(conn.query_row("SELECT m.credits FROM account_routing_metadata m JOIN oauth_accounts a ON a.id=m.account_id AND a.generation=m.generation WHERE m.account_id=?1",[id],|r|r.get::<_,Option<String>>(0)).ok().flatten().and_then(|s|serde_json::from_str(&s).ok()))).await.ok().flatten();
            if live {
                match self.live_limits(id).await {
                    Ok(current)=>{
                        row.quota=current.quota;
                        row.reset_credits=current.credits;
                        if let Some(status)=self.activate_weekly_window(user,id,&row.quota).await {
                            activated.store(true,std::sync::atomic::Ordering::Relaxed);
                            row.weekly_activation=Some(status);
                            if let Ok(updated)=self.live_limits(id).await {row.quota=updated.quota;row.reset_credits=updated.credits;}
                        }
                    },
                    Err(_)=>row.refresh_error=Some("Live subscription limits unavailable; showing stored observations".into()),
                }
                row.quota_history = self.db.call(move |conn| crate::quota_history::snapshot(conn, id, chrono::Utc::now().timestamp())).await.ok();
            }
        }).await;
        if activated.load(std::sync::atomic::Ordering::Relaxed) {
            report.inference = "weekly_activation_attempted".into();
        }
        if live && report.quota_accounts.len() == 1 {
            report.quota = Some(report.quota_accounts[0].quota.clone());
        }
    }
}
