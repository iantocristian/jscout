use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::Launch;
use crate::{agent, config};

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
pub enum Client {
    Codex,
    Claude,
}

impl Client {
    pub(super) fn launch_value(self, value: &str) -> Result<String> {
        match self {
            Self::Codex => Ok(value.to_string()),
            Self::Claude => expand_environment(value, |name| std::env::var(name).ok()),
        }
    }

    pub(super) fn config_path(self) -> &'static str {
        match self {
            Self::Codex => ".codex/config.toml",
            Self::Claude => ".mcp.json",
        }
    }

    pub(super) fn skill_destination(self) -> agent::Destination {
        match self {
            Self::Codex => agent::Destination::Agents,
            Self::Claude => agent::Destination::Claude,
        }
    }

    pub(super) fn entry(self, launch: &Launch, runtime: &config::RuntimeConfig) -> Result<Value> {
        let mut entry = serde_json::to_value(launch)?;
        if matches!(self, Self::Codex) {
            // Forward names, never capture secret values into repository configuration.
            let mut names = vec!["NODE_EXTRA_CA_CERTS".to_string()];
            if runtime.effective.embedding.provider.is_some()
                && let Some(name) = &runtime.effective.embedding.api_key_env
            {
                names.push(name.clone());
            }
            names.sort();
            names.dedup();
            entry["env_vars"] = json!(names);
        }
        Ok(entry)
    }

    pub(super) fn render(self, entry: &Value) -> Result<String> {
        match self {
            Self::Codex => Ok(toml::to_string_pretty(
                &json!({"mcp_servers": {"jscout": entry}}),
            )?),
            Self::Claude => Ok(format!(
                "{}\n",
                serde_json::to_string_pretty(&json!({"mcpServers": {"jscout": entry}}))?
            )),
        }
    }

    pub(super) fn prepare(self, root: &Path, entry: &Value) -> Result<(FileUpdate, Value)> {
        let path = root.join(self.config_path());
        check_local_target(root, &path)?;
        let original = read_optional(&path)?;
        let replacement = self.merge(original.as_deref(), entry)
            .with_context(|| format!("merge {}; existing jscout entries are never overwritten; use setup --print-config to inspect the required entry", path.display()))?;
        let registered: Value =
            match self {
                Self::Codex => serde_json::to_value(toml::from_str::<toml::Value>(&replacement)?)?
                    ["mcp_servers"]["jscout"]
                    .clone(),
                Self::Claude => {
                    serde_json::from_str::<Value>(&replacement)?["mcpServers"]["jscout"].clone()
                }
            };
        Ok((FileUpdate::new(path, original, replacement), registered))
    }

    fn merge(self, original: Option<&str>, entry: &Value) -> Result<String> {
        let Some(original) = original else {
            return self.render(entry);
        };
        match self {
            Self::Codex => {
                let parsed: toml::Value = toml::from_str(original)?;
                if let Some(existing) = parsed.get("mcp_servers").and_then(|v| v.get("jscout")) {
                    check_existing(&serde_json::to_value(existing)?, entry)?;
                    return Ok(original.to_string());
                }
                let mut document: toml_edit::DocumentMut = original.parse()?;
                if !document.contains_key("mcp_servers") {
                    document["mcp_servers"] = toml_edit::Item::Table(toml_edit::Table::new());
                }
                let fragment: toml_edit::DocumentMut = self.render(entry)?.parse()?;
                let mut item = fragment["mcp_servers"]["jscout"].clone();
                if document["mcp_servers"].is_inline_table() {
                    item = toml_edit::Item::Value(
                        item.into_value()
                            .map_err(|_| anyhow::anyhow!("invalid generated MCP entry"))?,
                    );
                }
                let table = document["mcp_servers"]
                    .as_table_like_mut()
                    .context("mcp_servers must be a table")?;
                table.insert("jscout", item);
                Ok(document.to_string())
            }
            Self::Claude => {
                let mut document: Value = serde_json::from_str(original)?;
                let object = document
                    .as_object_mut()
                    .context("MCP configuration must be an object")?;
                let servers = object
                    .entry("mcpServers")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .context("mcpServers must be an object")?;
                if let Some(existing) = servers.get("jscout") {
                    check_existing(existing, entry)?;
                    return Ok(original.to_string());
                }
                servers.insert("jscout".to_string(), entry.clone());
                Ok(format!("{}\n", serde_json::to_string_pretty(&document)?))
            }
        }
    }
}

/// Claude expands these forms against the parent's environment before launch.
fn expand_environment(text: &str, lookup: impl Fn(&str) -> Option<String>) -> Result<String> {
    let mut output = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        output.push_str(&rest[..start]);
        let expression = &rest[start + 2..];
        let end = expression
            .find('}')
            .context("unclosed Claude MCP environment reference")?;
        let (name, fallback) = expression[..end]
            .split_once(":-")
            .map_or((&expression[..end], None), |(name, fallback)| {
                (name, Some(fallback))
            });
        ensure!(!name.is_empty(), "empty Claude MCP environment reference");
        let value = lookup(name)
            .filter(|value| !value.is_empty())
            .or_else(|| fallback.map(str::to_string))
            .with_context(|| format!("Claude MCP environment variable {name} is not set"))?;
        output.push_str(&value);
        rest = &expression[end + 1..];
    }
    output.push_str(rest);
    Ok(output)
}

fn check_existing(existing: &Value, expected: &Value) -> Result<()> {
    ensure!(
        existing.get("url").is_none() && existing.get("type").is_none_or(|kind| kind == "stdio"),
        "existing jscout transport is not local stdio"
    );
    ensure!(
        existing.get("experimental_environment").is_none(),
        "existing jscout launch uses a different executor environment"
    );
    for key in ["command", "args"] {
        ensure!(
            existing.get(key) == expected.get(key),
            "existing jscout {key} differs"
        );
    }
    ensure!(
        existing.get("enabled") != Some(&Value::Bool(false)),
        "existing jscout server is disabled"
    );
    if let Some(required) = expected["env_vars"].as_array() {
        let actual = existing["env_vars"]
            .as_array()
            .context("existing jscout entry does not forward the configured environment names")?;
        ensure!(
            required.iter().all(|name| actual.contains(name)),
            "existing jscout env_vars omits a configured environment name"
        );
    }
    // Client-side timeouts, tool policy, and other user settings remain untouched.
    Ok(())
}

pub(super) fn read_optional(path: &Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

/// Do not let project setup follow a linked client/skill directory into global settings.
pub(super) fn check_local_target(root: &Path, path: &Path) -> Result<()> {
    let mut current = root.to_path_buf();
    for component in path.strip_prefix(root)?.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "setup will not write through symlink {}; manage this target manually with --print-config",
                current.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(super) struct FileUpdate {
    path: PathBuf,
    original: Option<String>,
    replacement: String,
}

impl FileUpdate {
    pub(super) fn new(path: PathBuf, original: Option<String>, replacement: String) -> Self {
        Self {
            path,
            original,
            replacement,
        }
    }

    pub(super) fn write(self, root: &Path) -> Result<()> {
        check_local_target(root, &self.path)?;
        ensure!(
            read_optional(&self.path)? == self.original,
            "{} changed during setup; rerun setup",
            self.path.display()
        );
        if self.original.as_deref() == Some(self.replacement.as_str()) {
            return Ok(());
        }
        let parent = self.path.parent().context("setup target has no parent")?;
        fs::create_dir_all(parent)?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(self.replacement.as_bytes())?;
        if self.original.is_some() {
            temporary
                .as_file()
                .set_permissions(fs::metadata(&self.path)?.permissions())?;
        }
        temporary.as_file().sync_all()?;
        if self.original.is_some() {
            temporary.persist(&self.path)?;
        } else {
            temporary.persist_noclobber(&self.path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn claude_environment_references_use_values_defaults_and_literal_dollars() {
        let expand = |text| {
            super::expand_environment(text, |key| (key == "HOST").then(|| "localhost".to_string()))
        };
        assert_eq!(
            expand("http://${HOST}:${PORT:-8792}/$literal").unwrap(),
            "http://localhost:8792/$literal"
        );
        assert_eq!(expand("${OPTIONAL:-}").unwrap(), "");
        assert!(expand("${MISSING}").is_err());
        assert!(expand("${UNCLOSED").is_err());
        assert_eq!(
            super::Client::Codex.launch_value("${LITERAL}").unwrap(),
            "${LITERAL}"
        );
    }
}
