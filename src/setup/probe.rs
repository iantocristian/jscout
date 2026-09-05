use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{Client, Launch, path_text};
use crate::{config, mcp};

const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_OUTPUT: u64 = 1_048_576;

pub(super) fn verify(
    launch: &Launch,
    client: Client,
    entry: &Value,
    root: &Path,
    runtime: &config::RuntimeConfig,
) -> Result<Vec<String>> {
    let mut command = Command::new(client.launch_value(&path_text(&launch.command)?)?);
    command
        .args(
            launch
                .args
                .iter()
                .map(|arg| client.launch_value(arg))
                .collect::<Result<Vec<_>>>()?,
        )
        .current_dir(root)
        .stdin(Stdio::piped());
    if let Some(cwd) = entry.get("cwd") {
        command
            .current_dir(client.launch_value(cwd.as_str().context("MCP cwd must be a string")?)?);
    }
    if let Some(env) = entry.get("env") {
        for (key, value) in env.as_object().context("MCP env must be an object")? {
            command.env(
                key,
                client.launch_value(value.as_str().context("MCP env values must be strings")?)?,
            );
        }
    }
    // Files avoid pipe backpressure and keep the verification bounded without
    // worker threads. The npm wrapper and Rust child share this process group.
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdout(Stdio::from(stdout.try_clone()?))
        .stderr(Stdio::from(stderr.try_clone()?));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command.spawn().context("start MCP verification command")?;
    let result = (|| -> Result<Vec<String>> {
        let mut input = child
            .stdin
            .take()
            .context("MCP verification stdin missing")?;
        for message in [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{
                "protocolVersion":"2025-06-18", "capabilities":{},
                "clientInfo":{"name":"jscout-setup","version":env!("CARGO_PKG_VERSION")}
            }}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}),
        ] {
            writeln!(input, "{message}")?;
        }
        drop(input);
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            ensure!(
                Instant::now() < deadline,
                "MCP startup verification timed out after 10 seconds"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        let output = read_output(&mut stdout)?;
        let errors = read_output(&mut stderr)?;
        ensure!(
            status.success(),
            "MCP startup verification failed: {}",
            errors.trim()
        );
        let replies = output
            .lines()
            .map(serde_json::from_str::<Value>)
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("MCP verification received invalid JSON")?;
        let hello = response(&replies, 1)?;
        ensure!(
            hello["serverInfo"]["name"] == "jscout",
            "MCP verification reached a different server"
        );
        ensure!(
            hello["serverInfo"]["database"]
                == serde_json::to_value(&runtime.effective.database.path)?,
            "MCP verification reached a different database"
        );
        ensure!(
            hello["serverInfo"]["configurationFingerprint"] == runtime.fingerprint,
            "MCP verification resolved different repository settings; check the existing client entry's env/cwd overrides"
        );
        let tools = response(&replies, 2)?;
        let actual = names(&tools["tools"])?;
        let expected = names(&mcp::allowed_tool_defs(
            mcp::ToolProfile::parse(&runtime.effective.mcp.profile)?,
            runtime.effective.docs.enabled,
            &runtime.effective.mcp.tools,
        ))?;
        ensure!(
            actual == expected,
            "MCP verification exposed a different tool set than the configured profile"
        );
        Ok(actual)
    })();
    if result.is_err() {
        #[cfg(unix)]
        // SAFETY: this is the process group created for our own verification child.
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    result
        .context("setup left the index/skill available but did not publish a new MCP registration")
}

fn read_output(file: &mut std::fs::File) -> Result<String> {
    file.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    file.take(MAX_OUTPUT + 1).read_to_string(&mut text)?;
    ensure!(
        text.len() as u64 <= MAX_OUTPUT,
        "MCP verification output exceeded 1 MiB"
    );
    Ok(text)
}

fn response(replies: &[Value], id: u64) -> Result<&Value> {
    let reply = replies
        .iter()
        .find(|reply| reply["id"] == id)
        .context("MCP verification response missing")?;
    ensure!(
        reply.get("error").is_none(),
        "MCP verification returned a protocol error"
    );
    reply
        .get("result")
        .context("MCP verification result missing")
}

fn names(tools: &Value) -> Result<Vec<String>> {
    let mut names = tools
        .as_array()
        .context("MCP verification tool list missing")?
        .iter()
        .map(|tool| {
            tool["name"]
                .as_str()
                .map(str::to_string)
                .context("MCP tool name missing")
        })
        .collect::<Result<Vec<_>>>()?;
    names.sort();
    Ok(names)
}
