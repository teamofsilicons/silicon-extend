//! 2.0: accounts. Every Carbon and Silicon is a personal Silicon Accounts account: a permanent
//! `uuid` (short and case-sensitive, like `zQo`) that Extend keys everything on, and a public id
//! (`c:ada`, `si:scout`) that people see and that can change. A Silicon always has one custodian,
//! a Carbon. These shapes are what API v2 (`/api/v2/…`) answers; the device wire never uses them.

use serde::{Deserialize, Serialize};

use crate::model::MemberKind;

/// A Carbon or Silicon, by uuid and current public id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AccountRef {
    /// The permanent Silicon Accounts uuid.
    pub uuid: String,
    /// The current public id (`c:ada`, `si:scout`).
    pub id: String,
    #[serde(rename = "type")]
    pub kind: MemberKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// The profile photo Silicon Accounts serves for the account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
}

impl AccountRef {
    pub fn new(uuid: impl Into<String>, id: impl Into<String>, kind: MemberKind) -> Self {
        Self {
            uuid: uuid.into(),
            id: id.into(),
            kind,
            display_name: None,
            pfp_url: None,
        }
    }

    pub fn display_name(mut self, name: Option<String>) -> Self {
        self.display_name = name.filter(|n| !n.trim().is_empty());
        self
    }

    pub fn pfp_url(mut self, url: Option<String>) -> Self {
        self.pfp_url = url.filter(|u| !u.trim().is_empty());
        self
    }
}

/// `GET /api/v2/me`, envelope type `me`: who the access token belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AccountMe {
    pub uuid: String,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: MemberKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// A Silicon's custodian.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custodian: Option<AccountRef>,
}

impl AccountMe {
    pub fn new(account: AccountRef, custodian: Option<AccountRef>) -> Self {
        Self {
            uuid: account.uuid,
            id: account.id,
            kind: account.kind,
            display_name: account.display_name,
            pfp_url: account.pfp_url,
            custodian,
        }
    }
}

/// `GET /api/v2/accounts` (no sign-in), envelope type `accounts`: where Extend's accounts come
/// from and where everything else is. A CLI uses it to sign in (device flow, or a short-lived
/// token a Silicon got with `silicon-accounts login --app <app_id> -q`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct AccountsInfo {
    /// Extend's app id in Silicon Accounts (`extend`).
    pub app_id: String,
    /// The Silicon Accounts public URL (the `iss` of every access token Extend accepts).
    pub accounts_url: String,
    pub api_base_url: String,
    pub website_url: String,
    pub docs_url: String,
    pub repository_url: String,
    /// Whether this service delivers notifications through Ting. When false, requests and wake
    /// requests are only on the website, in the CLI and on the device.
    #[serde(default)]
    pub ting_enabled: bool,
}

impl AccountsInfo {
    pub fn new(
        app_id: impl Into<String>,
        accounts_url: impl Into<String>,
        api_base_url: impl Into<String>,
        website_url: impl Into<String>,
        docs_url: impl Into<String>,
        repository_url: impl Into<String>,
        ting_enabled: bool,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            accounts_url: accounts_url.into(),
            api_base_url: api_base_url.into(),
            website_url: website_url.into(),
            docs_url: docs_url.into(),
            repository_url: repository_url.into(),
            ting_enabled,
        }
    }
}

/// `POST /api/v2/auth/logout`, envelope type `logout`: signs this sign-in out of Extend. With
/// `refresh_token` (the one the CLI or website holds), Extend also revokes it in Silicon Accounts.
/// A Silicon's running sessions end; a Carbon's sign-out ends the sessions of the Silicons they
/// gave access to, through their own pairs only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SignOut {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
}

impl SignOut {
    pub fn new(refresh_token: Option<String>) -> Self {
        Self { refresh_token }
    }
}

/// `GET /api/v2/silicons`, envelope type `silicons` (a list): the Silicons a Carbon can give
/// access to without typing an id: the ones they look after (they are the custodian), and the
/// ones they gave access to before.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SiliconSummary {
    pub uuid: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pfp_url: Option<String>,
    /// The caller is this Silicon's custodian: they can see its sessions, grants, files and
    /// requests in Extend (never act as it).
    #[serde(default)]
    pub looked_after: bool,
    /// Grants the caller gave this Silicon.
    #[serde(default)]
    pub granted_by_you: i64,
    /// Custodian view only: every grant it has, from any Carbon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grants: Option<i64>,
    /// Custodian view only: sessions running now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub running_sessions: Option<i64>,
}

impl SiliconSummary {
    pub fn new(account: AccountRef) -> Self {
        Self {
            uuid: account.uuid,
            id: account.id,
            display_name: account.display_name,
            pfp_url: account.pfp_url,
            looked_after: false,
            granted_by_you: 0,
            grants: None,
            running_sessions: None,
        }
    }
}
