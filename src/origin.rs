use anyhow::{Result, bail};

pub const ALL: &[&str] = &["repository", "workspace", "dependency"];
pub const DEFAULT: &[&str] = &["repository", "workspace"];

pub fn defaults() -> Vec<String> {
    DEFAULT.iter().map(|origin| (*origin).to_string()).collect()
}

pub fn validate_all(origins: &[String]) -> Result<()> {
    if origins.is_empty() {
        bail!("file origin allowlist cannot be empty");
    }
    for origin in origins {
        if !ALL.contains(&origin.as_str()) {
            bail!("file origin must be one of: {}; got `{origin}`", ALL.join(", "));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{defaults, validate_all};

    #[test]
    fn defaults_exclude_dependencies_and_reject_empty_allowlists() {
        assert_eq!(defaults(), vec!["repository", "workspace"]);
        assert!(validate_all(&defaults()).is_ok());
        assert!(validate_all(&[]).is_err());
    }
}
