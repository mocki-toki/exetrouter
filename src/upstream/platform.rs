//! Same-model Platform fallback is a quota exception, not an authentication or
//! operational-backoff bypass. Native tools continue using Codex-only selection.
use super::*;
use crate::platform::routes;

impl Upstream {
    pub(crate) async fn select_excluding(
        &self,
        model: &str,
        affinity: Option<&str>,
        excluded: &[i64],
    ) -> std::result::Result<Selection, SelectError> {
        if crate::catalog::embedding_model(model) {
            return Err(SelectError::ModelUnavailable);
        }
        let native = self.select_codex_excluding(model, affinity, excluded).await;
        if matches!(&native, Err(SelectError::Backoff(_))) {
            return native;
        }
        if let Ok(account) = &native {
            if !self.quota_exhausted(account.info.id).await? {
                return native;
            }
        }
        let requested = model.to_owned();
        let eligible = self
            .db
            .call(move |conn| routes::fallback_eligible(conn, &requested, Utc::now().timestamp()))
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        if !eligible {
            return native;
        }
        self.select_platform(model, affinity, excluded)
            .await
            .or(native)
    }

    pub(crate) async fn select_embedding(
        &self,
        model: &str,
    ) -> std::result::Result<Selection, SelectError> {
        if !crate::catalog::embedding_model(model) {
            return Err(SelectError::ModelUnavailable);
        }
        crate::platform::route_catalog(&self.db, &self.vault, &self.config.platform_base)
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        self.select_platform(model, None, &[]).await
    }

    async fn select_platform(
        &self,
        model: &str,
        affinity: Option<&str>,
        excluded: &[i64],
    ) -> std::result::Result<Selection, SelectError> {
        let vault = self.vault.clone();
        let requested = model.to_owned();
        let excluded = excluded.to_vec();
        let (mut candidates, wait) = self
            .db
            .call(move |conn| {
                let now = Utc::now().timestamp();
                let mut candidates = Vec::new();
                let mut wait = None;
                for id in routes::owners(conn)? {
                    if excluded.contains(&id) {
                        continue;
                    }
                    let Ok(account) = routes::account(conn, &vault, id, now) else {
                        continue;
                    };
                    if routes::models(conn, id, now)
                        .is_ok_and(|models| models.iter().any(|m| m.id == requested))
                    {
                        if let Some((until, quota)) = account
                            .info
                            .cooldown_until
                            .map(|t| (t, true))
                            .into_iter()
                            .chain(account.info.health_until.map(|t| (t, false)))
                            .max()
                        {
                            if wait.is_none_or(|(old, _)| until < old) {
                                wait = Some((until, quota));
                            }
                            continue;
                        }
                        candidates.push(crate::pool::Candidate {
                            id,
                            quota: None,
                            routing: Default::default(),
                        });
                    }
                }
                Ok((candidates, wait))
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        while let Some(reservation) = self.pool.reserve_affinity(&candidates, affinity) {
            let id = reservation.id;
            candidates.retain(|c| c.id != id);
            if let Ok(account) = self.account(id).await {
                if self.validate_model(&account, model).await.is_ok() {
                    return Ok(Selection {
                        account,
                        _reservation: reservation,
                    });
                }
            }
        }
        Err(if wait.is_some() {
            wait_error(wait)
        } else {
            SelectError::ModelUnavailable
        })
    }
}
