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

    pub(super) fn prepare(
        self,
        root: &Path,
        entry: &Value,
        replace: bool,
    ) -> Result<(FileUpdate, Value)> {
        let path = root.join(self.config_path());
        check_local_target(root, &path)?;
        let original = read_optional(&path)?;
        let replacement = self.merge(original.as_deref(), entry, replace)
            .with_context(|| format!("merge {}; existing jscout entries are never overwritten without --replace; use setup --print-config to inspect the required entry", path.display()))?;
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

    fn merge(self, original: Option<&str>, entry: &Value, replace: bool) -> Result<String> {
        let Some(original) = original else {
            return self.render(entry);
        };
        match self {
            Self::Codex => {
                let parsed: toml::Value = toml::from_str(original)?;
                if let Some(existing) = parsed.get("mcp_servers").and_then(|v| v.get("jscout")) {
                    let existing = serde_json::to_value(existing)?;
                    let updated = update_entry(&existing, entry, replace)?;
                    if updated == existing {
                        return Ok(original.to_string());
                    }
                    // Patch only changed fields, retaining user settings and TOML comments.
                    let mut document: toml_edit::DocumentMut = original.parse()?;
                    let fragment: toml_edit::DocumentMut = self.render(&updated)?.parse()?;
                    let table = document["mcp_servers"]["jscout"]
                        .as_table_like_mut()
                        .context("jscout must be a table")?;
                    for key in ["command", "args", "env_vars"] {
                        if updated.get(key) != existing.get(key) {
                            let mut item = fragment["mcp_servers"]["jscout"][key].clone();
                            if let Some(previous) =
                                table.get(key).and_then(toml_edit::Item::as_value)
                                && let Some(value) = item.as_value_mut()
                            {
                                *value.decor_mut() = previous.decor().clone();
                            }
                            table.insert(key, item);
                        }
                    }
                    return Ok(document.to_string());
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
                    let updated = update_entry(existing, entry, replace)?;
                    if updated == *existing {
                        return Ok(original.to_string());
                    }
                    servers.insert("jscout".to_string(), updated);
                } else {
                    servers.insert("jscout".to_string(), entry.clone());
                }
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

fn update_entry(existing: &Value, expected: &Value, replace: bool) -> Result<Value> {
    ensure!(
        existing.get("url").is_none() && existing.get("type").is_none_or(|kind| kind == "stdio"),
        "existing jscout transport is not local stdio"
    );
    ensure!(
        existing.get("experimental_environment").is_none(),
        "existing jscout launch uses a different executor environment"
    );
    ensure!(
        existing.get("enabled") != Some(&Value::Bool(false)),
        "existing jscout server is disabled"
    );
    let matching_launch = ["command", "args"]
        .iter()
        .all(|key| existing.get(key) == expected.get(key));
    if replace && !matching_launch {
        check_replace_target(existing, expected)?;
    } else if !replace {
        for key in ["command", "args"] {
            ensure!(
                existing.get(key) == expected.get(key),
                "existing jscout {key} differs; rerun with --replace to refresh this repository's registration"
            );
        }
    }
    let mut updated = existing.clone();
    for key in ["command", "args"] {
        updated[key] = expected[key].clone();
    }
    if let Some(required) = expected["env_vars"].as_array() {
        let mut names = match existing.get("env_vars") {
            Some(value) => value
                .as_array()
                .context("existing jscout env_vars must be an array of strings")?
                .clone(),
            None => Vec::new(),
        };
        ensure!(
            names.iter().all(Value::is_string),
            "existing jscout env_vars must be an array of strings"
        );
        if replace {
            for name in required {
                if !names.contains(name) {
                    names.push(name.clone());
                }
            }
            updated["env_vars"] = Value::Array(names);
        } else {
            ensure!(
                required.iter().all(|name| names.contains(name)),
                "existing jscout env_vars omits a configured environment name; rerun with --replace"
            );
        }
    }
    // Client-side timeouts, tool policy, and other user settings remain untouched.
    Ok(updated)
}

/// Recognize generated native/npm launch shapes without reading or executing the
/// previous installation (it may no longer exist). This scopes replacement; it
/// does not establish trust in other settings from the repository.
fn check_replace_target(existing: &Value, expected: &Value) -> Result<()> {
    let command = Path::new(
        existing["command"]
            .as_str()
            .context("MCP command missing")?,
    );
    let args = existing["args"]
        .as_array()
        .context("MCP args must be an array")?
        .iter()
        .map(|arg| arg.as_str().context("MCP args must be strings"))
        .collect::<Result<Vec<_>>>()?;
    let prefix = match args.as_slice() {
        [prefix @ .., "mcp", root]
            if Some(*root)
                == expected["args"]
                    .as_array()
                    .and_then(|args| args.last())
                    .and_then(Value::as_str) =>
        {
            prefix
        }
        _ => anyhow::bail!("--replace requires a jscout registration targeting this repository"),
    };
    let prefix = match command.file_name().and_then(|name| name.to_str()) {
        Some("jscout" | "jscout.exe") => prefix,
        Some("node" | "node.exe") => match prefix {
            [wrapper, rest @ ..]
                if Path::new(wrapper).is_absolute()
                    && Path::new(wrapper).ends_with("bin/jscout.mjs") =>
            {
                rest
            }
            _ => anyhow::bail!("--replace does not recognize this npm launcher"),
        },
        _ => anyhow::bail!("--replace does not recognize this jscout launcher"),
    };
    let known_options = match prefix {
        [] => true,
        ["--config", path] => Path::new(path).is_absolute(),
        _ => false,
    };
    ensure!(
        command.is_absolute() && known_options,
        "--replace only refreshes supported local jscout launch arguments"
    );
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
