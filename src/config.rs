use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::PathBuf;

/// A browser session: the `token_v2` cookie plus the user it acts as.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Session {
    pub token_v2: String,
    pub user_id: String,
    #[serde(default = "default_base")]
    pub base_url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

pub fn default_base() -> String {
    "https://www.notion.so".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub name: String,
    #[serde(default)]
    pub email: String,
    /// Default workspace (space) for search
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<String>,
    #[serde(flatten)]
    pub session: Session,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_account: Option<String>,
    #[serde(default)]
    pub accounts: BTreeMap<String, Account>,
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os("NTN_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("ntn"));
    }
    let home = std::env::var_os("HOME").ok_or_else(|| AppError::config("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".config").join("ntn"))
}

impl Config {
    pub fn load() -> Result<Self> {
        let path = config_dir()?.join("accounts.json");
        match std::fs::read_to_string(&path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map_err(|e| AppError::config(format!("{} is not valid: {e}", path.display()))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Atomic write: temp file in the same dir (0600), then rename.
    pub fn save(&self) -> Result<()> {
        let dir = config_dir()?;
        std::fs::create_dir_all(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut tmp = tempfile::NamedTempFile::new_in(&dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tmp.as_file()
                .set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        tmp.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        tmp.as_file().sync_all()?;
        tmp.persist(dir.join("accounts.json"))
            .map_err(|e| AppError::from(e.error))?;
        Ok(())
    }

    /// Resolve a selector (profile key, user id, name or email) to a stored account.
    pub fn select(&self, selector: Option<&str>) -> Result<(String, Account)> {
        let selector = match selector.or(self.default_account.as_deref()) {
            Some(s) => s,
            None if self.accounts.len() == 1 => {
                let (k, a) = self.accounts.iter().next().unwrap();
                return Ok((k.clone(), a.clone()));
            }
            None if self.accounts.is_empty() => {
                return Err(AppError::auth(
                    "no account configured; run `pbpaste | ntn auth parse-curl --login`",
                ));
            }
            None => {
                return Err(AppError::input(
                    "several accounts configured; pass --account or run `ntn auth set-default`",
                ));
            }
        };
        if let Some(a) = self.accounts.get(selector) {
            return Ok((selector.to_string(), a.clone()));
        }
        let lower = selector.to_lowercase();
        let hits: Vec<_> = self
            .accounts
            .iter()
            .filter(|(_, a)| {
                a.session.user_id == selector
                    || a.name.to_lowercase() == lower
                    || a.email.to_lowercase() == lower
            })
            .collect();
        match hits.as_slice() {
            [(k, a)] => Ok(((*k).clone(), (*a).clone())),
            [] => Err(AppError::not_found(format!(
                "account '{selector}' not found; see `ntn auth list`"
            ))),
            many => Err(AppError::ambiguous(
                format!("account '{selector}' matches several profiles"),
                many.iter()
                    .map(|(k, _)| k.as_str())
                    .collect::<Vec<_>>()
                    .into(),
            )),
        }
    }
}

/// The session cookie must only ever go to Notion over https.
pub fn normalize_base_url(input: &str) -> Result<String> {
    let url = url::Url::parse(input)
        .map_err(|_| AppError::input(format!("invalid Notion url: {input}")))?;
    let host = url.host_str().unwrap_or_default();
    let ok_host = ["notion.so", "notion.com"]
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if url.scheme() != "https" || !ok_host {
        return Err(AppError::input(format!(
            "Notion url must be https on notion.so or notion.com, got {input}"
        )));
    }
    Ok(format!("https://{host}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn acct(user: &str, name: &str) -> Account {
        Account {
            name: name.into(),
            email: format!("{name}@x.io"),
            space_id: None,
            session: Session {
                token_v2: "t".into(),
                user_id: user.into(),
                base_url: default_base(),
                user_agent: None,
            },
        }
    }

    #[test]
    fn select_by_key_user_name_email() {
        let mut cfg = Config::default();
        cfg.accounts.insert("work".into(), acct("u-1", "ana"));
        cfg.accounts.insert("home".into(), acct("u-2", "bob"));
        assert_eq!(cfg.select(Some("work")).unwrap().0, "work");
        assert_eq!(cfg.select(Some("u-2")).unwrap().0, "home");
        assert_eq!(cfg.select(Some("ANA@x.io")).unwrap().0, "work");
        assert_eq!(cfg.select(None).unwrap_err().code, "invalid_input");
        cfg.default_account = Some("home".into());
        assert_eq!(cfg.select(None).unwrap().0, "home");
    }

    #[test]
    fn base_url_must_be_notion_https() {
        assert_eq!(
            normalize_base_url("https://www.notion.so/api/v3/search").unwrap(),
            "https://www.notion.so"
        );
        assert!(normalize_base_url("https://app.notion.com").is_ok());
        assert!(normalize_base_url("http://www.notion.so").is_err());
        assert!(normalize_base_url("https://notion.so.evil.com").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn save_writes_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        // SAFETY: test-only env mutation; this is the only test touching NTN_CONFIG_DIR.
        unsafe { std::env::set_var("NTN_CONFIG_DIR", dir.path()) };
        let mut cfg = Config::default();
        cfg.accounts.insert("work".into(), acct("u-1", "ana"));
        cfg.save().unwrap();
        let meta = std::fs::metadata(dir.path().join("accounts.json")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        assert_eq!(Config::load().unwrap().accounts.len(), 1);
    }
}
