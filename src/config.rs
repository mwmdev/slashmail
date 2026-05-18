use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls: Option<bool>,
    pub user: Option<String>,
    pub trash_folder: Option<String>,
    pub default_folder: Option<String>,
    pub default_account: Option<String>,
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub name: String,
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls: Option<bool>,
    pub user: Option<String>,
    pub pass_env: Option<String>,
    pub trash_folder: Option<String>,
    pub default_folder: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAccount {
    pub name: Option<String>,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub user: String,
    pub pass_env: Option<String>,
    pub trash_folder: String,
    pub default_folder: String,
}

impl ResolvedAccount {
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.user)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSelector<'a> {
    Default,
    Named(&'a str),
    All,
}

#[derive(Debug, Default)]
pub struct ConnectionOverrides {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls: Option<bool>,
    pub user: Option<String>,
    pub user_explicit: bool,
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let path = match path {
            Some(p) => {
                // Explicit path must exist
                let content = std::fs::read_to_string(p)
                    .with_context(|| format!("Failed to read config file: {}", p.display()))?;
                return toml::from_str(&content)
                    .with_context(|| format!("Failed to parse config file: {}", p.display()));
            }
            None => match Self::default_path() {
                Some(p) if p.exists() => p,
                _ => return Ok(Self::default()),
            },
        };

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))
    }

    pub fn default_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("slashmail").join("config.toml"))
    }

    pub fn resolve_accounts(
        &self,
        selector: AccountSelector<'_>,
        overrides: &ConnectionOverrides,
    ) -> Result<Vec<ResolvedAccount>> {
        self.validate_accounts()?;

        match selector {
            AccountSelector::All => {
                self.reject_named_overrides(overrides)?;
                if self.accounts.is_empty() {
                    anyhow::bail!("--all-accounts requires at least one [[accounts]] entry");
                }
                self.accounts
                    .iter()
                    .map(|account| self.resolve_named_account(account))
                    .collect()
            }
            AccountSelector::Named(name) => {
                self.reject_named_overrides(overrides)?;
                let account = self
                    .accounts
                    .iter()
                    .find(|account| account.name == name)
                    .ok_or_else(|| anyhow::anyhow!("No account named '{name}' in config"))?;
                Ok(vec![self.resolve_named_account(account)?])
            }
            AccountSelector::Default => {
                if self.accounts.is_empty() {
                    Ok(vec![self.resolve_legacy_account(overrides)?])
                } else {
                    self.reject_named_overrides(overrides)?;
                    let account = if let Some(default_name) = &self.default_account {
                        self.accounts
                            .iter()
                            .find(|account| &account.name == default_name)
                            .ok_or_else(|| {
                                anyhow::anyhow!(
                                    "default_account '{default_name}' does not match any [[accounts]] entry"
                                )
                            })?
                    } else {
                        &self.accounts[0]
                    };
                    Ok(vec![self.resolve_named_account(account)?])
                }
            }
        }
    }

    fn validate_accounts(&self) -> Result<()> {
        let mut names = HashSet::new();
        for account in &self.accounts {
            if account.name.is_empty() {
                anyhow::bail!("Account name cannot be empty");
            }
            if !is_valid_account_name(&account.name) {
                anyhow::bail!(
                    "Invalid account name '{}'. Use only letters, numbers, dot, underscore, or hyphen.",
                    account.name
                );
            }
            if !names.insert(account.name.clone()) {
                anyhow::bail!("Duplicate account name '{}'", account.name);
            }
            if account.host.as_deref().unwrap_or("").is_empty() {
                anyhow::bail!(
                    "Account '{}' is missing required field 'host'",
                    account.name
                );
            }
            if account.user.as_deref().unwrap_or("").is_empty() {
                anyhow::bail!(
                    "Account '{}' is missing required field 'user'",
                    account.name
                );
            }
        }

        if let Some(default_name) = &self.default_account {
            if self.accounts.is_empty() {
                anyhow::bail!("default_account is set but no [[accounts]] entries exist");
            }
            if !self
                .accounts
                .iter()
                .any(|account| &account.name == default_name)
            {
                anyhow::bail!(
                    "default_account '{default_name}' does not match any [[accounts]] entry"
                );
            }
        }

        Ok(())
    }

    fn reject_named_overrides(&self, overrides: &ConnectionOverrides) -> Result<()> {
        if overrides.host.is_some()
            || overrides.port.is_some()
            || overrides.tls.is_some()
            || overrides.user_explicit
        {
            anyhow::bail!(
                "Connection overrides (--host, --port, --tls, -u/--user) cannot be used with named accounts"
            );
        }
        Ok(())
    }

    fn resolve_named_account(&self, account: &AccountConfig) -> Result<ResolvedAccount> {
        let tls = account.tls.unwrap_or(false);
        let port = account.port.unwrap_or(if tls { 993 } else { 1143 });
        let host = account.host.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "Account '{}' is missing required field 'host'",
                account.name
            )
        })?;
        let user = account.user.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "Account '{}' is missing required field 'user'",
                account.name
            )
        })?;

        Ok(ResolvedAccount {
            name: Some(account.name.clone()),
            host,
            port,
            tls,
            user,
            pass_env: account.pass_env.clone(),
            trash_folder: account
                .trash_folder
                .clone()
                .or_else(|| self.trash_folder.clone())
                .unwrap_or_else(|| "Trash".to_string()),
            default_folder: account
                .default_folder
                .clone()
                .or_else(|| self.default_folder.clone())
                .unwrap_or_else(|| "INBOX".to_string()),
        })
    }

    fn resolve_legacy_account(&self, overrides: &ConnectionOverrides) -> Result<ResolvedAccount> {
        let tls = overrides.tls.or(self.tls).unwrap_or(false);
        let host = overrides
            .host
            .clone()
            .or_else(|| self.host.clone())
            .unwrap_or_else(|| "127.0.0.1".to_string());
        let port = overrides
            .port
            .or(self.port)
            .unwrap_or(if tls { 993 } else { 1143 });
        let user = overrides
            .user
            .clone()
            .or_else(|| self.user.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("IMAP username required (use -u/--user or SLASHMAIL_USER env)")
            })?;

        Ok(ResolvedAccount {
            name: None,
            host,
            port,
            tls,
            user,
            pass_env: None,
            trash_folder: self
                .trash_folder
                .clone()
                .unwrap_or_else(|| "Trash".to_string()),
            default_folder: self
                .default_folder
                .clone()
                .unwrap_or_else(|| "INBOX".to_string()),
        })
    }
}

fn is_valid_account_name(name: &str) -> bool {
    name.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_full_config() {
        let toml = r#"
            host = "imap.example.com"
            port = 993
            tls = true
            user = "alice@example.com"
            trash_folder = "[Gmail]/Trash"
            default_folder = "INBOX"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.host.as_deref(), Some("imap.example.com"));
        assert_eq!(config.port, Some(993));
        assert_eq!(config.tls, Some(true));
        assert_eq!(config.user.as_deref(), Some("alice@example.com"));
        assert_eq!(config.trash_folder.as_deref(), Some("[Gmail]/Trash"));
        assert_eq!(config.default_folder.as_deref(), Some("INBOX"));
        assert!(config.accounts.is_empty());
    }

    #[test]
    fn parse_partial_config() {
        let toml = r#"
            host = "mail.example.com"
            tls = true
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.host.as_deref(), Some("mail.example.com"));
        assert_eq!(config.port, None);
        assert_eq!(config.tls, Some(true));
        assert_eq!(config.user, None);
    }

    #[test]
    fn parse_empty_config() {
        let config: Config = toml::from_str("").unwrap();
        assert!(config.host.is_none());
        assert!(config.port.is_none());
        assert!(config.tls.is_none());
        assert!(config.user.is_none());
        assert!(config.accounts.is_empty());
    }

    #[test]
    fn load_none_does_not_error() {
        // Should succeed whether or not a config file exists at the default path
        Config::load(None).unwrap();
    }

    #[test]
    fn explicit_missing_file_errors() {
        let result = Config::load(Some(Path::new("/nonexistent/config.toml")));
        assert!(result.is_err());
    }

    #[test]
    fn parse_multi_account_config() {
        let toml = r#"
            default_account = "personal"

            [[accounts]]
            name = "personal"
            host = "imap.example.com"
            port = 993
            tls = true
            user = "alice@example.com"
            pass_env = "SLASHMAIL_PERSONAL_PASS"

            [[accounts]]
            name = "work"
            host = "imap.work.test"
            user = "alice@work.test"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.default_account.as_deref(), Some("personal"));
        assert_eq!(
            config.accounts[0].pass_env.as_deref(),
            Some("SLASHMAIL_PERSONAL_PASS")
        );
    }

    #[test]
    fn resolve_default_named_account() {
        let toml = r#"
            default_account = "work"
            default_folder = "Inbox"
            trash_folder = "Deleted"

            [[accounts]]
            name = "personal"
            host = "imap.example.com"
            user = "alice@example.com"

            [[accounts]]
            name = "work"
            host = "imap.work.test"
            port = 993
            tls = true
            user = "alice@work.test"
            default_folder = "Work"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let accounts = config
            .resolve_accounts(AccountSelector::Default, &ConnectionOverrides::default())
            .unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].name.as_deref(), Some("work"));
        assert_eq!(accounts[0].port, 993);
        assert!(accounts[0].tls);
        assert_eq!(accounts[0].default_folder, "Work");
        assert_eq!(accounts[0].trash_folder, "Deleted");
    }

    #[test]
    fn resolve_all_accounts() {
        let toml = r#"
            [[accounts]]
            name = "a"
            host = "imap.a.test"
            user = "a@test"

            [[accounts]]
            name = "b"
            host = "imap.b.test"
            user = "b@test"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let accounts = config
            .resolve_accounts(AccountSelector::All, &ConnectionOverrides::default())
            .unwrap();
        assert_eq!(accounts.len(), 2);
        assert_eq!(accounts[0].name.as_deref(), Some("a"));
        assert_eq!(accounts[1].name.as_deref(), Some("b"));
    }

    #[test]
    fn resolve_legacy_account_uses_overrides() {
        let config: Config = toml::from_str("").unwrap();
        let overrides = ConnectionOverrides {
            host: Some("mail.test".into()),
            port: Some(1993),
            tls: Some(true),
            user: Some("me@test".into()),
            user_explicit: true,
        };
        let accounts = config
            .resolve_accounts(AccountSelector::Default, &overrides)
            .unwrap();
        assert_eq!(
            accounts[0],
            ResolvedAccount {
                name: None,
                host: "mail.test".into(),
                port: 1993,
                tls: true,
                user: "me@test".into(),
                pass_env: None,
                trash_folder: "Trash".into(),
                default_folder: "INBOX".into(),
            }
        );
    }

    #[test]
    fn duplicate_account_names_error() {
        let toml = r#"
            [[accounts]]
            name = "dup"
            host = "imap.a.test"
            user = "a@test"

            [[accounts]]
            name = "dup"
            host = "imap.b.test"
            user = "b@test"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let result = config.resolve_accounts(AccountSelector::All, &ConnectionOverrides::default());
        assert!(result.is_err());
    }

    #[test]
    fn invalid_default_account_errors() {
        let toml = r#"
            default_account = "missing"

            [[accounts]]
            name = "personal"
            host = "imap.example.com"
            user = "alice@example.com"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let result =
            config.resolve_accounts(AccountSelector::Default, &ConnectionOverrides::default());
        assert!(result.is_err());
    }

    #[test]
    fn named_accounts_reject_connection_overrides() {
        let toml = r#"
            [[accounts]]
            name = "personal"
            host = "imap.example.com"
            user = "alice@example.com"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let overrides = ConnectionOverrides {
            host: Some("other.test".into()),
            ..ConnectionOverrides::default()
        };
        let result = config.resolve_accounts(AccountSelector::Default, &overrides);
        assert!(result.is_err());
    }

    #[test]
    fn invalid_account_name_errors() {
        let toml = r#"
            [[accounts]]
            name = "bad name"
            host = "imap.example.com"
            user = "alice@example.com"
        "#;
        let config: Config = toml::from_str(toml).unwrap();
        let result = config.resolve_accounts(AccountSelector::All, &ConnectionOverrides::default());
        assert!(result.is_err());
    }
}
