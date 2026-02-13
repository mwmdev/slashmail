use anyhow::{Context, Result};
use serde::Deserialize;
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
}
