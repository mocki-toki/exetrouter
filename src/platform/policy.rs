use crate::Result;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Resource {
    Codex,
    Platform,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "Vec<Resource>", into = "Vec<Resource>")]
pub enum Resources {
    Codex,
    Platform,
    Both,
}

impl TryFrom<Vec<Resource>> for Resources {
    type Error = &'static str;

    fn try_from(resources: Vec<Resource>) -> std::result::Result<Self, Self::Error> {
        match resources.as_slice() {
            [Resource::Codex] => Ok(Self::Codex),
            [Resource::Platform] => Ok(Self::Platform),
            [Resource::Codex, Resource::Platform] | [Resource::Platform, Resource::Codex] => {
                Ok(Self::Both)
            }
            _ => Err("select codex, platform, or both without duplicates"),
        }
    }
}

impl From<Resources> for Vec<Resource> {
    fn from(resources: Resources) -> Self {
        match resources {
            Resources::Codex => vec![Resource::Codex],
            Resources::Platform => vec![Resource::Platform],
            Resources::Both => vec![Resource::Codex, Resource::Platform],
        }
    }
}

impl Resources {
    pub fn set(self, conn: &Connection) -> Result<()> {
        let value = match self {
            Self::Codex => "codex",
            Self::Platform => "platform",
            Self::Both => "both",
        };
        conn.execute("INSERT INTO routing_resources VALUES (1,?1) ON CONFLICT(singleton) DO UPDATE SET resources=excluded.resources", [value])?;
        Ok(())
    }

    fn saved(conn: &Connection) -> Result<Option<Self>> {
        let value: Option<String> = conn
            .query_row(
                "SELECT resources FROM routing_resources WHERE singleton=1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        match value.as_deref() {
            None => Ok(None),
            Some("codex") => Ok(Some(Self::Codex)),
            Some("platform") => Ok(Some(Self::Platform)),
            Some("both") => Ok(Some(Self::Both)),
            _ => Err("invalid stored routing resources".into()),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Policy {
    pub(super) resources: Resources,
    pub(super) legacy_platform_restriction: bool,
}

impl Policy {
    pub(crate) fn read(conn: &Connection) -> Result<Self> {
        let saved = Resources::saved(conn)?;
        Ok(Self {
            resources: saved.unwrap_or(Resources::Both),
            legacy_platform_restriction: saved.is_none() && !legacy_fallback(conn)?,
        })
    }

    pub(crate) fn codex(self) -> bool {
        self.resources != Resources::Platform
    }

    pub(crate) fn platform(self) -> bool {
        self.resources != Resources::Codex
    }

    pub(crate) fn allows(self, account: i64) -> bool {
        if account < 0 {
            self.platform()
        } else {
            self.codex()
        }
    }

    pub(crate) fn shared_platform(self) -> bool {
        self.platform() && !self.legacy_platform_restriction
    }

    pub(crate) fn platform_model(self, conn: &Connection, model: &str, now: i64) -> Result<bool> {
        if !self.platform() {
            return Ok(false);
        }
        if self.shared_platform() || crate::catalog::embedding_model(model) {
            return Ok(true);
        }
        // Preserve the legacy default until the operator explicitly selects resources.
        let blocked = conn.query_row("SELECT EXISTS(SELECT 1 FROM oauth_models WHERE model=?1) OR EXISTS(SELECT 1 FROM oauth_accounts a LEFT JOIN account_policy p ON p.account_id=a.id WHERE a.state<>'disabled' AND COALESCE(p.enabled,1)=1 AND (a.state<>'active' OR a.catalog_updated_at IS NULL OR a.catalog_updated_at>?2 OR a.catalog_updated_at<=?2-?3))", rusqlite::params![model,now,crate::catalog::CACHE_TTL_SECONDS], |r|r.get::<_,bool>(0))?;
        Ok(!blocked)
    }
}

fn legacy_fallback(conn: &Connection) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT codex_fallback_allowed FROM platform_policy WHERE singleton=1",
        [],
        |r| r.get(0),
    )?)
}

// Keep reading old startup configuration until the operator sets the new policy.
pub fn apply_legacy_fallback(conn: &Connection, enabled: bool) -> Result<()> {
    conn.execute("UPDATE platform_policy SET codex_fallback_allowed=?1 WHERE singleton=1 AND NOT EXISTS(SELECT 1 FROM routing_resources)", [enabled])?;
    Ok(())
}
