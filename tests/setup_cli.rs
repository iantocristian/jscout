//! Executable onboarding recipes: use the real binary, SQLite index, and MCP boundary.
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn run(root: &Path, arguments: &[&str]) -> Output {
    run_with_env(root, arguments, &[])
}

fn run_with_env(root: &Path, arguments: &[&str], environment: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jscout"));
    command.env_clear().env("HOME", root).current_dir(root);
    if let Some(path) = std::env::var_os("PATH") {
        command.env("PATH", path);
    }
    command.envs(environment.iter().copied());
    command.args(arguments).output().expect("run jscout")
}

fn success(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn fixture() -> tempfile::TempDir {
    let directory = tempfile::Builder::new()
        .prefix("jscout setup with spaces ")
        .tempdir()
        .unwrap();
    fs::write(
        directory.path().join("greeting.ts"),
        "export function greeting(name: string) { return `Hello ${name}`; }\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("README.md"),
        "# Greeting\n\nA greeting function returns a salutation.\n",
    )
    .unwrap();
    directory
}

fn client_config(root: &Path, client: &str) -> std::path::PathBuf {
    root.join(if client == "codex" {
        ".codex/config.toml"
    } else {
        ".mcp.json"
    })
}

fn parse_client(client: &str, text: &str) -> Value {
    if client == "codex" {
        serde_json::to_value(toml::from_str::<toml::Value>(text).unwrap()).unwrap()
    } else {
        serde_json::from_str(text).unwrap()
    }
}

fn servers_key(client: &str) -> &'static str {
    if client == "codex" {
        "mcp_servers"
    } else {
        "mcpServers"
    }
}

fn write_client(root: &Path, client: &str, config: &Value) -> String {
    let text = if client == "codex" {
        toml::to_string_pretty(config).unwrap()
    } else {
        serde_json::to_string_pretty(config).unwrap()
    };
    let path = client_config(root, client);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, &text).unwrap();
    text
}

#[test]
fn print_config_is_valid_and_has_no_side_effects() {
    let directory = fixture();
    let root = directory.path();
    for client in ["codex", "claude"] {
        let text = success(run(root, &["setup", "--client", client, "--print-config"]));
        let parsed: Value = if client == "codex" {
            serde_json::to_value(toml::from_str::<toml::Value>(&text).unwrap()).unwrap()
        } else {
            serde_json::from_str(&text).unwrap()
        };
        let key = if client == "codex" {
            "mcp_servers"
        } else {
            "mcpServers"
        };
        assert_eq!(
            parsed[key]["jscout"]["args"][1],
            root.canonicalize().unwrap().to_str().unwrap()
        );
        assert_eq!(fs::read_dir(root).unwrap().count(), 2);
    }
}

#[test]
fn setup_indexes_installs_registers_and_reruns_for_both_clients() {
    for (client, config, skill) in [
        (
            "codex",
            ".codex/config.toml",
            ".agents/skills/jscout/SKILL.md",
        ),
        ("claude", ".mcp.json", ".claude/skills/jscout/SKILL.md"),
    ] {
        let directory = fixture();
        let root = directory.path();
        let output = success(run(root, &["setup", "--client", client]));
        assert!(
            output.contains("verified MCP initialization and 7 tools"),
            "{output}"
        );
        assert_eq!(
            fs::read_to_string(root.join(".jscout.toml")).unwrap(),
            "version = 1\n"
        );
        assert!(root.join(skill).is_file());
        let registration = fs::read(root.join(config)).unwrap();
        let query = success(run(
            root,
            &["search", ".", "greeting", "--lexical-only", "--json"],
        ));
        let result: Value = serde_json::from_str(&query).unwrap();
        assert_eq!(result["hits"][0]["symbol"], "greeting");
        let docs = success(run(
            root,
            &[
                "docs",
                "search",
                ".",
                "salutation",
                "--lexical-only",
                "--json",
            ],
        ));
        assert_eq!(
            serde_json::from_str::<Value>(&docs).unwrap()["hits"][0]["path"],
            "README.md"
        );
        success(run(root, &["setup", "--client", client]));
        assert_eq!(fs::read(root.join(config)).unwrap(), registration);
        let repeated = success(run(
            root,
            &["search", ".", "greeting", "--lexical-only", "--json"],
        ));
        assert_eq!(
            serde_json::from_str::<Value>(&repeated).unwrap()["snapshot"],
            result["snapshot"]
        );
    }
}

#[test]
fn setup_preserves_runtime_policy_and_custom_skill() {
    let directory = fixture();
    let root = directory.path();
    let config = "version = 1\n[database]\npath = 'state/search.db'\n[docs]\nenabled = false\n[mcp]\nprofile = 'full'\ntools = ['definition']\n";
    fs::write(root.join(".jscout.toml"), config).unwrap();
    let skill = root.join(".agents/skills/jscout/SKILL.md");
    fs::create_dir_all(skill.parent().unwrap()).unwrap();
    fs::write(&skill, "my custom guide\n").unwrap();
    let output = success(run(root, &["setup", "--client", "codex"]));
    assert!(output.contains("verified MCP initialization and 1 tools"));
    assert!(output.contains("state/search.db and its -wal, -shm, and -journal sidecars"));
    assert_eq!(fs::read_to_string(skill).unwrap(), "my custom guide\n");
    assert_eq!(
        fs::read_to_string(root.join(".jscout.toml")).unwrap(),
        config
    );
    assert!(root.join("state/search.db").is_file());
    assert!(!root.join(".jscout.db").exists());
}

#[test]
fn client_conflict_is_reported_before_any_index_or_skill_write() {
    let directory = fixture();
    let root = directory.path();
    let existing = "{\"mcpServers\":{\"jscout\":{\"command\":\"other\",\"args\":[]}}}";
    fs::write(root.join(".mcp.json"), existing).unwrap();
    let output = run(root, &["setup", "--client", "claude"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("never overwritten"));
    assert_eq!(
        fs::read_to_string(root.join(".mcp.json")).unwrap(),
        existing
    );
    assert_eq!(fs::read_dir(root).unwrap().count(), 3);
}

#[test]
fn explicit_existing_config_is_carried_into_registration() {
    let directory = fixture();
    let root = directory.path();
    fs::write(
        root.join("custom.toml"),
        "version=1\n[mcp]\nprofile='full'\n",
    )
    .unwrap();
    let text = success(run(
        root,
        &["--config", "custom.toml", "setup", "--client", "claude"],
    ));
    assert!(text.contains("verified MCP initialization and 13 tools"));
    let registered: Value =
        serde_json::from_str(&fs::read_to_string(root.join(".mcp.json")).unwrap()).unwrap();
    assert_eq!(registered["mcpServers"]["jscout"]["args"][0], "--config");
    assert!(
        fs::read_to_string(root.join(".claude/skills/jscout/SKILL.md"))
            .unwrap()
            .contains("semantic_memory")
    );
    assert!(!root.join(".jscout.toml").exists());
}

#[test]
fn empty_effective_tool_surface_fails_before_writes() {
    let directory = fixture();
    let root = directory.path();
    fs::write(
        root.join(".jscout.toml"),
        "version=1\n[docs]\nenabled=false\n[mcp]\ntools=['documentation_search']\n",
    )
    .unwrap();
    let output = run(root, &["setup", "--client", "claude"]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("registers no tool"));
    assert_eq!(fs::read_dir(root).unwrap().count(), 3);
}

#[test]
fn legacy_policy_cannot_produce_a_misleading_client_registration() {
    let directory = fixture();
    let root = directory.path();
    let output = run_with_env(
        root,
        &["setup", "--client", "codex"],
        &[("JSCOUT_RERANK_TOP", "12")],
    );
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("migrate legacy environment settings")
    );
    assert_eq!(fs::read_dir(root).unwrap().count(), 2);
}

#[test]
fn claude_existing_environment_references_are_expanded_but_not_rewritten() {
    let directory = fixture();
    let root = directory.path();
    let config = success(run(
        root,
        &["setup", "--client", "claude", "--print-config"],
    ));
    let mut config: Value = serde_json::from_str(&config).unwrap();
    config["mcpServers"]["jscout"]["env"] =
        serde_json::json!({"JSCOUT_INFERENCE_URL":"${SERVICE_URL}"});
    let text = serde_json::to_string_pretty(&config).unwrap();
    fs::write(root.join(".mcp.json"), &text).unwrap();
    success(run_with_env(
        root,
        &["setup", "--client", "claude"],
        &[("SERVICE_URL", "http://127.0.0.1:8792")],
    ));
    assert_eq!(fs::read_to_string(root.join(".mcp.json")).unwrap(), text);
}

#[test]
fn replace_refreshes_stale_native_and_npm_registrations_without_losing_client_settings() {
    for client in ["codex", "claude"] {
        for npm in [false, true] {
            let directory = fixture();
            let root = directory.path();
            let generated = parse_client(
                client,
                &success(run(root, &["setup", "--client", client, "--print-config"])),
            );
            let key = servers_key(client);
            let mut existing = generated.clone();
            let entry = &mut existing[key]["jscout"];
            if npm {
                entry["command"] = "/old/node/bin/node".into();
                entry["args"].as_array_mut().unwrap().insert(
                    0,
                    "/old/node/lib/node_modules/@jscout/cli/bin/jscout.mjs".into(),
                );
            } else {
                entry["command"] = "/old/node/bin/jscout".into();
            }
            entry["tool_timeout_sec"] = 90.into();
            entry["disabled_tools"] = serde_json::json!(["semantic_memory"]);
            if client == "codex" {
                entry["env_vars"] = serde_json::json!(["CUSTOM_KEY"]);
            }
            existing[key]["other"] = serde_json::json!({"command":"unrelated","args":[]});
            let original = write_client(root, client, &existing);

            assert!(!run(root, &["setup", "--client", client]).status.success());
            assert_eq!(
                fs::read_to_string(client_config(root, client)).unwrap(),
                original
            );
            assert!(!root.join(".jscout.toml").exists());
            assert!(!root.join(".jscout.db").exists());
            assert!(
                !root
                    .join(if client == "codex" {
                        ".agents"
                    } else {
                        ".claude"
                    })
                    .exists()
            );

            success(run(root, &["setup", "--client", client, "--replace"]));
            let saved_text = fs::read_to_string(client_config(root, client)).unwrap();
            let saved = parse_client(client, &saved_text);
            assert_eq!(
                saved[key]["jscout"]["command"],
                generated[key]["jscout"]["command"]
            );
            assert_eq!(
                saved[key]["jscout"]["args"],
                generated[key]["jscout"]["args"]
            );
            assert_eq!(saved[key]["jscout"]["tool_timeout_sec"], 90);
            assert_eq!(
                saved[key]["jscout"]["disabled_tools"],
                existing[key]["jscout"]["disabled_tools"]
            );
            assert_eq!(saved[key]["other"], existing[key]["other"]);
            if client == "codex" {
                let names = saved[key]["jscout"]["env_vars"].as_array().unwrap();
                assert!(names.contains(&Value::from("CUSTOM_KEY")));
                assert!(names.contains(&Value::from("NODE_EXTRA_CA_CERTS")));
            }
            success(run(root, &["setup", "--client", client, "--replace"]));
            assert_eq!(
                fs::read_to_string(client_config(root, client)).unwrap(),
                saved_text
            );
        }
    }
}

#[test]
fn replace_adds_new_provider_environment_names_without_removing_custom_names() {
    let directory = fixture();
    let root = directory.path();
    success(run(root, &["setup", "--client", "codex"]));
    let path = client_config(root, "codex");
    let mut config = parse_client("codex", &fs::read_to_string(&path).unwrap());
    config["mcp_servers"]["jscout"]["env_vars"]
        .as_array_mut()
        .unwrap()
        .push("CUSTOM_KEY".into());
    let original = write_client(root, "codex", &config);
    fs::write(
        root.join(".jscout.toml"),
        "version=1\n[embedding]\nprovider='local'\nmodel='BAAI/bge-m3'\napi_key_env='NEW_PROVIDER_KEY'\n",
    )
    .unwrap();
    let refused = run(root, &["setup", "--client", "codex"]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("env_vars"));
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    success(run(root, &["setup", "--client", "codex", "--replace"]));
    let saved = parse_client("codex", &fs::read_to_string(path).unwrap());
    let names = saved["mcp_servers"]["jscout"]["env_vars"]
        .as_array()
        .unwrap();
    assert_eq!(names.len(), 3);
    for name in ["CUSTOM_KEY", "NEW_PROVIDER_KEY", "NODE_EXTRA_CA_CERTS"] {
        assert!(names.contains(&Value::from(name)), "{names:?}");
    }
}

#[test]
fn replace_refuses_foreign_other_root_remote_and_disabled_entries_before_writes() {
    for client in ["codex", "claude"] {
        for kind in ["foreign", "other-root", "remote", "disabled"] {
            let directory = fixture();
            let root = directory.path();
            let mut config = parse_client(
                client,
                &success(run(root, &["setup", "--client", client, "--print-config"])),
            );
            let entry = &mut config[servers_key(client)]["jscout"];
            match kind {
                "foreign" => entry["command"] = "/old/bin/unrelated".into(),
                "other-root" => entry["args"][1] = "/another/repository".into(),
                "remote" => entry["url"] = "https://example.com/mcp".into(),
                "disabled" => entry["enabled"] = false.into(),
                _ => unreachable!(),
            }
            let original = write_client(root, client, &config);
            let output = run(root, &["setup", "--client", client, "--replace"]);
            assert!(!output.status.success(), "{client} {kind}");
            assert_eq!(
                fs::read_to_string(client_config(root, client)).unwrap(),
                original
            );
            assert!(!root.join(".jscout.toml").exists());
            assert!(!root.join(".jscout.db").exists());
            assert!(
                !root
                    .join(if client == "codex" {
                        ".agents"
                    } else {
                        ".claude"
                    })
                    .exists()
            );
        }
    }
}

#[test]
fn replace_and_print_config_are_mutually_exclusive() {
    let directory = fixture();
    let root = directory.path();
    let output = run(
        root,
        &["setup", "--client", "codex", "--replace", "--print-config"],
    );
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("cannot be used with"), "{error}");
    assert_eq!(fs::read_dir(root).unwrap().count(), 2);
}
