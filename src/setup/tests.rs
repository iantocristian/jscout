use std::fs;

use anyhow::Result;
use serde_json::json;

use super::{Client, client};

#[test]
fn watch_command_quotes_shell_metacharacters_and_keeps_explicit_config() -> Result<()> {
    let launch = super::Launch {
        command: "/bin/echo".into(),
        args: vec![
            "--config".into(),
            "/tmp/a'b$HOME".into(),
            "mcp".into(),
            "/tmp/$(pwd)`id`".into(),
        ],
    };
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", &launch.watch_command()?])
        .output()?;
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout)?,
        "--config /tmp/a'b$HOME watch /tmp/$(pwd)`id`\n"
    );
    Ok(())
}

#[test]
fn registration_preserves_unrelated_client_settings_and_is_idempotent() -> Result<()> {
    let entry = json!({"command":"/bin/jscout", "args":["mcp", "/repo with spaces"]});
    for (client, text) in [
        (
            Client::Codex,
            "# keep my comments\nmodel = 'custom-model'\n[mcp_servers.other]\ncommand = 'other'\n",
        ),
        (
            Client::Codex,
            "# inline parent\nmcp_servers = { other = { command = 'other' } }\n",
        ),
        (
            Client::Claude,
            "{\"otherSetting\":42,\"mcpServers\":{\"other\":{\"command\":\"other\"}}}\n",
        ),
    ] {
        let root = tempfile::tempdir()?;
        let path = root.path().join(client.config_path());
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, text)?;
        client.prepare(root.path(), &entry)?.0.write(root.path())?;
        let written = fs::read_to_string(&path)?;
        assert!(written.contains("other"));
        if text.starts_with('#') {
            assert!(written.contains(text.lines().next().unwrap()));
        }
        client.prepare(root.path(), &entry)?.0.write(root.path())?;
        assert_eq!(fs::read_to_string(path)?, written);
    }
    Ok(())
}

#[test]
fn conflicting_entry_fails_without_overwriting() -> Result<()> {
    let entry = json!({"command":"/bin/jscout", "args":["mcp", "/repo"]});
    for client in [Client::Codex, Client::Claude] {
        let root = tempfile::tempdir()?;
        let path = root.path().join(client.config_path());
        fs::create_dir_all(path.parent().unwrap())?;
        let existing = client.render(&json!({"command":"other", "args":[]}))?;
        fs::write(&path, &existing)?;
        assert!(client.prepare(root.path(), &entry).is_err());
        assert_eq!(fs::read_to_string(path)?, existing);
    }
    Ok(())
}

#[test]
fn matching_entry_retains_timeouts_and_env_overrides() -> Result<()> {
    let entry = json!({"command":"/bin/jscout", "args":["mcp", "/repo"]});
    let mut existing = entry.clone();
    existing["tool_timeout_sec"] = json!(90);
    existing["env"] = json!({"LANG":"C"});
    for client in [Client::Codex, Client::Claude] {
        let root = tempfile::tempdir()?;
        let path = root.path().join(client.config_path());
        fs::create_dir_all(path.parent().unwrap())?;
        let text = client.render(&existing)?;
        fs::write(&path, &text)?;
        let (update, actual) = client.prepare(root.path(), &entry)?;
        assert_eq!(actual, existing);
        update.write(root.path())?;
        assert_eq!(fs::read_to_string(path)?, text);
    }
    Ok(())
}

#[test]
fn atomic_write_refuses_concurrent_edits() -> Result<()> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("config");
    fs::write(&path, "original")?;
    let update =
        client::FileUpdate::new(path.clone(), Some("original".into()), "replacement".into());
    fs::write(&path, "user edit")?;
    assert!(update.write(root.path()).is_err());
    assert_eq!(fs::read_to_string(path)?, "user edit");
    Ok(())
}

#[test]
fn matching_launcher_with_different_transport_is_not_treated_as_ready() -> Result<()> {
    let entry = json!({"command":"/bin/jscout", "args":["mcp", "/repo"]});
    for (key, value) in [
        ("type", json!("http")),
        ("url", json!("https://example.com/mcp")),
        ("experimental_environment", json!("remote")),
        ("enabled", json!(false)),
    ] {
        let root = tempfile::tempdir()?;
        let mut existing = entry.clone();
        existing[key] = value;
        fs::write(
            root.path().join(".mcp.json"),
            Client::Claude.render(&existing)?,
        )?;
        assert!(
            Client::Claude.prepare(root.path(), &entry).is_err(),
            "accepted {key}"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn project_registration_refuses_symlinked_global_target() -> Result<()> {
    let root = tempfile::tempdir()?;
    let outside = tempfile::tempdir()?;
    std::os::unix::fs::symlink(outside.path(), root.path().join(".codex"))?;
    let entry = json!({"command":"/bin/jscout","args":[]});
    assert!(Client::Codex.prepare(root.path(), &entry).is_err());
    assert!(!outside.path().join("config.toml").exists());
    Ok(())
}
