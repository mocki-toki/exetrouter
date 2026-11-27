//! Same-model Platform fallback is a quota exception, not an authentication or
//! operational-backoff bypass. Native tools continue using Codex-only selection.
use super::*;
use crate::platform::routes;

impl Upstream {
    pub(crate) async fn replay_replacement(&self, model: &str, owner: i64) -> Option<Selection> {
        if owner < 0 {
            return self.recovered_codex(model, owner).await;
        }
        if !self.account_enabled(owner).await.ok()? {
            return None;
        }
        if self.routing_policy().await.ok()?.codex() && !self.quota_exhausted(owner).await.ok()? {
            return None;
        }
        self.linked_platform(model, owner).await.ok()
    }

    /// A client replay ticket preserves the original pair, even if another
    /// account ranks higher. It never authorizes spending or opaque context.
    pub(crate) async fn select_replay(
        &self,
        model: &str,
        owner: i64,
    ) -> std::result::Result<Selection, SelectError> {
        if owner < 0 {
            if let Some(parent) = self.recovered_codex(model, owner).await {
                return Ok(parent);
            }
            return self.select_pinned(model, owner).await;
        }
        let pinned = self.select_pinned(model, owner).await;
        if self
            .account_enabled(owner)
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?
            && (!self.routing_policy().await?.codex()
                || (matches!(pinned, Ok(_) | Err(SelectError::Cooldown(_)))
                    && self.quota_exhausted(owner).await?))
        {
            if let Ok(project) = self.linked_platform(model, owner).await {
                return Ok(project);
            }
        }
        pinned
    }

    pub(crate) async fn select_after_quota_refusal(
        &self,
        model: &str,
        previous: i64,
        excluded: &[i64],
        same_pair: bool,
    ) -> std::result::Result<Selection, SelectError> {
        if same_pair {
            let alternate = if previous > 0 {
                self.linked_platform(model, previous).await?
            } else {
                self.recovered_codex(model, previous)
                    .await
                    .ok_or(SelectError::UpstreamUnavailable)?
            };
            return if excluded.contains(&alternate.info.id) {
                Err(SelectError::UpstreamUnavailable)
            } else {
                Ok(alternate)
            };
        }
        let alternate = self.select_excluding(model, None, excluded).await?;
        if previous > 0 && alternate.info.id < 0 {
            self.linked_platform(model, previous).await
        } else {
            Ok(alternate)
        }
    }

    pub(crate) async fn linked_platform(
        &self,
        model: &str,
        parent: i64,
    ) -> std::result::Result<Selection, SelectError> {
        let model_id = model.to_owned();
        let route = self
            .db
            .call(move |conn| {
                if !routes::fallback_eligible(conn, &model_id, Utc::now().timestamp())? {
                    return Ok(None);
                }
                routes::linked_project(conn, parent)
            })
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)?;
        self.select_pinned(model, route.ok_or(SelectError::ModelUnavailable)?)
            .await
    }

    pub(crate) async fn recovered_codex(&self, model: &str, project: i64) -> Option<Selection> {
        let parent = self
            .db
            .call(move |conn| {
                let policy = crate::platform::Policy::read(conn)?;
                if !policy.codex() {
                    return Ok(None);
                }
                let parent = routes::parent(conn, project)?;
                if routes::linked_project(conn, parent)? != Some(project) {
                    return Ok(None);
                }
                let quota = crate::quota::summary(conn, parent, Utc::now().timestamp())?;
                let ready = !policy.platform()
                    || (quota.status == "observed"
                        && quota.windows.iter().all(|w| w.remaining_percent > 0.));
                Ok(ready.then_some(parent))
            })
            .await
            .ok()
            .flatten()?;
        self.select_pinned(model, parent).await.ok()
    }

    pub(crate) async fn linked_resources(&self, first: i64, second: i64) -> crate::Result<bool> {
        if (first < 0) == (second < 0) {
            return Ok(false);
        }
        let (project, parent) = (first.min(second), first.max(second));
        self.db
            .call(move |conn| Ok(routes::linked_project(conn, parent)? == Some(project)))
            .await
    }

    pub(super) async fn routing_policy(
        &self,
    ) -> std::result::Result<crate::platform::Policy, SelectError> {
        self.db
            .call(|conn| crate::platform::Policy::read(conn))
            .await
            .map_err(|_| SelectError::UpstreamUnavailable)
    }

    pub(crate) async fn select_excluding(
        &self,
        model: &str,
        affinity: Option<&str>,
        excluded: &[i64],
    ) -> std::result::Result<Selection, SelectError> {
        if crate::catalog::embedding_model(model) {
            return Err(SelectError::ModelUnavailable);
        }
        let policy = self.routing_policy().await?;
        if !policy.codex() {
            return self
                .select_platform_discover(model, affinity, excluded)
                .await;
        }
        let native = self.select_codex_excluding(model, affinity, excluded).await;
        if !policy.platform() || matches!(&native, Err(SelectError::Backoff(_))) {
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
        self.select_platform_discover(model, affinity, excluded)
            .await
            .or(native)
    }

    pub(crate) async fn select_embedding(
        &self,
        model: &str,
    ) -> std::result::Result<Selection, SelectError> {
        if !crate::catalog::embedding_model(model) || !self.routing_policy().await?.platform() {
            return Err(SelectError::ModelUnavailable);
        }
        self.select_platform_discover(model, None, &[]).await
    }

    async fn select_platform_discover(
        &self,
        model: &str,
        affinity: Option<&str>,
        excluded: &[i64],
    ) -> std::result::Result<Selection, SelectError> {
        match self.select_platform(model, affinity, excluded).await {
            Err(SelectError::ModelUnavailable) => {
                {
                    let _guard = self.catalog_lock.lock().await;
                    crate::platform::route_catalog(
                        &self.db,
                        &self.vault,
                        &self.config.platform_base,
                    )
                    .await
                    .map_err(|_| SelectError::UpstreamUnavailable)?;
                }
                self.select_platform(model, affinity, excluded).await
            }
            result => result,
        }
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
                if !crate::platform::Policy::read(conn)?.platform_model(conn, &requested, now)? {
                    return Ok((candidates, wait));
                }
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
