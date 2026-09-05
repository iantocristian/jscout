use super::{
    FILE_NAME, RuntimeConfig, SCHEMA_VERSION, TEMPLATE, ValueSource, init,
    parse_legacy_compatible_providers,
};
use std::collections::BTreeSet;
use std::path::Path;

fn write_config(root: &Path, text: &str) -> anyhow::Result<()> {
    std::fs::write(root.join(FILE_NAME), text)?;
    Ok(())
}

#[test]
fn absent_file_preserves_current_search_and_database_defaults() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let config = RuntimeConfig::load(Some(root.path()), None)?;
    assert!(!config.config_loaded);
    assert_eq!(
        config.effective.database.path,
        root.path().canonicalize()?.join(".jscout.db")
    );
    assert_eq!(config.effective.docs.include, ["**/*.md", "**/*.mdx"]);
    assert!(config.effective.docs.enabled);
    assert!(config.effective.docs.exclude.is_empty());
    assert!(config.effective.docs.search.vector);
    assert!(config.effective.docs.search.rerank);
    assert!(!config.effective.docs.search.freshness);
    assert_eq!(config.effective.docs.search.max_rank_movement, 2);
    assert_eq!(config.effective.docs.search.limit, 10);
    assert_eq!(config.effective.docs.search.response_bytes, 24_000);
    assert!(config.effective.search.vector);
    assert!(config.effective.search.rerank);
    assert!(!config.effective.search.attach_memory);
    assert_eq!(config.effective.search.response_bytes, 30_000);
    assert_eq!(config.effective.llm.max_concurrency, 1);
    assert_eq!(config.effective.search.expansion.mode, "paths");
    assert_eq!(config.effective.search.expansion.paths, 8);
    assert_eq!(config.sources["search.rerank"], ValueSource::Builtin);
    assert_eq!(config.sources["docs.include"], ValueSource::Builtin);
    assert_eq!(config.sources["docs.enabled"], ValueSource::Builtin);
    Ok(())
}

#[test]
fn repository_config_resolves_paths_and_records_sources() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        r#"version = 1
[database]
path = "state/index.db"
[docs]
include = ["**/*.md", ".github/*.md"]
exclude = ["archive/**"]
[docs.search]
vector = false
rerank = false
freshness = true
max_rank_movement = 3
limit = 7
response_bytes = 12000
[search]
rerank = false
attach_memory = false
response_bytes = 12000
[telemetry]
file = "logs/mcp.jsonl"
"#,
    )?;
    let config = RuntimeConfig::load(Some(root.path()), None)?;
    assert!(config.config_loaded);
    assert_eq!(
        config.effective.database.path,
        root.path().canonicalize()?.join("state/index.db")
    );
    assert_eq!(config.effective.docs.include, ["**/*.md", ".github/*.md"]);
    assert_eq!(config.effective.docs.exclude, ["archive/**"]);
    assert!(!config.effective.docs.search.vector);
    assert!(!config.effective.docs.search.rerank);
    assert!(config.effective.docs.search.freshness);
    assert_eq!(config.effective.docs.search.max_rank_movement, 3);
    assert_eq!(config.effective.docs.search.limit, 7);
    assert_eq!(config.effective.docs.search.response_bytes, 12_000);
    assert_eq!(
        config.effective.telemetry.file,
        Some(root.path().canonicalize()?.join("logs/mcp.jsonl"))
    );
    assert!(!config.effective.search.rerank);
    assert!(!config.effective.search.attach_memory);
    assert_eq!(config.effective.search.response_bytes, 12_000);
    assert_eq!(config.sources["search.response_bytes"], ValueSource::Config);
    assert_eq!(config.sources["search.rerank"], ValueSource::Config);
    assert_eq!(config.sources["docs.include"], ValueSource::Config);
    assert_eq!(config.sources["docs.search.limit"], ValueSource::Config);
    let shown = config.show_text();
    assert!(shown.contains("docs.enabled = true [builtin]"));
    assert!(shown.contains("docs.search.freshness = true [config]"));
    assert!(shown.contains("docs.search.max_rank_movement = 3 [config]"));
    let json = config.show_json()?;
    assert!(json.contains("\"docs\""));
    Ok(())
}

#[test]
fn documentation_can_be_disabled_without_conflating_vector_search_policy() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        r#"version = 1
[docs]
enabled = false
include = ["handbook/**/*.md", "handbook/**/*.mdx"]
exclude = ["handbook/archive/**"]
[docs.search]
vector = true
freshness = true
"#,
    )?;

    let config = RuntimeConfig::load(Some(root.path()), None)?;

    assert!(!config.effective.docs.enabled);
    assert!(config.effective.docs.search.vector);
    assert!(config.effective.docs.search.freshness);
    assert!(!config.effective.docs.indexing_freshness());
    assert!(config.effective.docs.indexing_include().is_empty());
    assert!(config.effective.docs.indexing_exclude().is_empty());
    assert_eq!(
        config.effective.docs.include,
        ["handbook/**/*.md", "handbook/**/*.mdx"]
    );
    assert_eq!(config.sources["docs.enabled"], ValueSource::Config);
    assert_eq!(config.sources["docs.search.vector"], ValueSource::Config);
    assert!(config.show_text().contains("docs.enabled = false [config]"));
    Ok(())
}

#[test]
fn documentation_policy_changes_the_shared_runtime_fingerprint() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        "version = 1\n[database]\npath = \"state/code.db\"\n",
    )?;
    let defaults = RuntimeConfig::load(Some(root.path()), None)?;

    write_config(
        root.path(),
        "version = 1\n[database]\npath = \"state/code.db\"\n[docs]\nexclude = [\"archive/**\"]\n",
    )?;

    let config = RuntimeConfig::load(Some(root.path()), None)?;
    assert_ne!(defaults.fingerprint, config.fingerprint);
    Ok(())
}

#[test]
fn repository_roots_resolve_independent_database_and_provider_policy() -> anyhow::Result<()> {
    let first_root = tempfile::tempdir()?;
    let second_root = tempfile::tempdir()?;
    write_config(
        first_root.path(),
        "version = 1\n[database]\npath = \"state/first.db\"\n[search]\nrerank = false\n",
    )?;
    write_config(
        second_root.path(),
        "version = 1\n[database]\npath = \"state/second.db\"\n[search]\nrerank = true\n",
    )?;

    let first = RuntimeConfig::load(Some(first_root.path()), None)?;
    let second = RuntimeConfig::load(Some(second_root.path()), None)?;
    assert_ne!(
        first.effective.database.path,
        second.effective.database.path
    );
    assert!(!first.effective.search.rerank);
    assert!(second.effective.search.rerank);
    assert_ne!(first.fingerprint, second.fingerprint);
    Ok(())
}

#[test]
fn unknown_fields_and_versions_fail_with_the_configuration_path() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(root.path(), "version = 1\n[search]\nrerak = false\n")?;
    let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
    assert!(error.to_string().contains(FILE_NAME));

    write_config(root.path(), "version = 1\n[docs]\nfreshnes = false\n")?;
    let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
    assert!(error.to_string().contains(FILE_NAME));

    write_config(root.path(), "version = 99\n")?;
    let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported jscout configuration version")
    );
    Ok(())
}

#[test]
fn documentation_globs_and_positive_bounds_fail_during_load() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    for (text, expected) in [
        (
            "version = 1\n[docs]\ninclude = [\"!private/**\"]\n",
            "documentation include/exclude patterns",
        ),
        (
            "version = 1\n[docs.search]\nlimit = 0\n",
            "docs.search.limit",
        ),
        (
            "version = 1\n[docs.search]\nresponse_bytes = 0\n",
            "docs.search.response_bytes",
        ),
        (
            "version = 1\n[docs.search]\nmax_rank_movement = 4\n",
            "docs.search.max_rank_movement",
        ),
    ] {
        write_config(root.path(), text)?;
        let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected `{expected}` in `{error:#}`"
        );
    }
    Ok(())
}

#[test]
fn expansion_projection_and_path_limit_fail_closed() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        "version = 1\n[search.expansion]\nmode = \"dense\"\n",
    )?;
    assert!(
        RuntimeConfig::load(Some(root.path()), None)
            .unwrap_err()
            .to_string()
            .contains("search.expansion.mode")
    );

    write_config(root.path(), "version = 1\n[search.expansion]\npaths = 51\n")?;
    assert!(
        RuntimeConfig::load(Some(root.path()), None)
            .unwrap_err()
            .to_string()
            .contains("search.expansion.paths must be at most 50")
    );
    Ok(())
}

#[test]
fn unsafe_endpoints_and_remote_binds_fail_during_configuration_load() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        "version = 1\n[inference]\nurl = \"https://example.test/v1?token=x\"\n",
    )?;
    assert!(
        RuntimeConfig::load(Some(root.path()), None)
            .unwrap_err()
            .to_string()
            .contains("query string or fragment")
    );

    write_config(
        root.path(),
        "version = 1\n[inference]\nhost = \"0.0.0.0\"\n",
    )?;
    assert!(
        RuntimeConfig::load(Some(root.path()), None)
            .unwrap_err()
            .to_string()
            .contains("inference.allow_remote")
    );

    for (text, expected) in [
        (
            "version = 1\n[search]\nmemory_depth = 9\n",
            "search.memory_depth",
        ),
        ("version = 1\n[reranker]\ntop = 101\n", "reranker.top"),
        (
            "version = 1\n[watch]\nenrich_timeout_seconds = 0\n",
            "watch.enrich_timeout_seconds",
        ),
        (
            "version = 1\n[llm]\nmax_concurrency = 0\n",
            "llm.max_concurrency",
        ),
    ] {
        write_config(root.path(), text)?;
        assert!(
            RuntimeConfig::load(Some(root.path()), None)
                .unwrap_err()
                .to_string()
                .contains(expected)
        );
    }
    Ok(())
}

#[test]
fn llm_scout_concurrency_is_explicit_and_uncapped() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(root.path(), "version = 1\n[llm]\nmax_concurrency = 128\n")?;
    let config = RuntimeConfig::load(Some(root.path()), None)?;
    assert_eq!(config.effective.llm.max_concurrency, 128);
    assert_eq!(config.sources["llm.max_concurrency"], ValueSource::Config);
    Ok(())
}

#[test]
fn fingerprint_changes_with_runtime_policy_not_source_labels() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(root.path(), "version = 1\n[search]\nrerank = false\n")?;
    let first = RuntimeConfig::load(Some(root.path()), None)?;
    write_config(root.path(), "version = 1\n[search]\nrerank = true\n")?;
    let second = RuntimeConfig::load(Some(root.path()), None)?;
    assert_ne!(first.fingerprint, second.fingerprint);
    Ok(())
}

#[test]
fn init_refuses_to_overwrite_and_emits_the_current_schema() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let path = init(root.path(), None)?;
    assert_eq!(path, root.path().canonicalize()?.join(FILE_NAME));
    assert!(std::fs::read_to_string(&path)?.contains(&format!("version = {SCHEMA_VERSION}")));
    let loaded = RuntimeConfig::load(Some(root.path()), None)?;
    assert!(loaded.config_loaded);
    assert!(loaded.effective.docs.enabled);
    assert_eq!(loaded.effective.docs.include, ["**/*.md", "**/*.mdx"]);
    assert_eq!(loaded.effective.mcp.profile, "core");
    assert_eq!(loaded.effective.mcp.result_transport, "auto");
    assert!(init(root.path(), None).is_err());
    assert!(TEMPLATE.contains("rerank = true"));
    assert!(TEMPLATE.contains("[docs]"));
    assert!(TEMPLATE.contains("enabled = true"));
    assert!(TEMPLATE.contains("\"**/*.mdx\""));
    assert!(!TEMPLATE.contains(".jscout-docs.db"));
    Ok(())
}

#[test]
fn template_active_settings_match_builtin_defaults() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let defaults = RuntimeConfig::load(Some(root.path()), None)?;
    init(root.path(), None)?;
    let initialized = RuntimeConfig::load(Some(root.path()), None)?;
    assert_eq!(
        serde_json::to_value(&initialized.effective)?,
        serde_json::to_value(&defaults.effective)?,
        "active template settings must not silently select a different policy"
    );
    assert_eq!(initialized.fingerprint, defaults.fingerprint);
    assert_eq!(
        serde_json::to_value(super::SearchSettings::default())?,
        serde_json::to_value(defaults.effective.search)?
    );
    Ok(())
}

#[test]
fn template_inference_client_follows_port_without_an_override() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(root.path(), &TEMPLATE.replace("port = 8792", "port = 8793"))?;
    let config = RuntimeConfig::load(Some(root.path()), None)?;
    assert_eq!(config.effective.inference.url, "http://127.0.0.1:8793");
    assert_eq!(config.sources["inference.url"], ValueSource::Builtin);
    Ok(())
}

#[test]
fn each_commented_embedding_recipe_is_independently_valid() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let mut recipes = Vec::new();
    let mut current = None;
    for line in TEMPLATE.lines() {
        if let Some(name) = line.strip_prefix("# BEGIN embedding recipe: ") {
            assert!(current.is_none(), "nested recipe {name}");
            current = Some((name, "version = 1\n".to_string()));
        } else if line == "# END embedding recipe" {
            recipes.push(current.take().expect("recipe end must have a beginning"));
        } else if let Some((_, text)) = current.as_mut() {
            text.push_str(line.strip_prefix("# ").expect("recipe must be commented"));
            text.push('\n');
        }
    }
    assert!(current.is_none(), "unterminated embedding recipe");
    assert_eq!(recipes.len(), 4);
    for (name, text) in recipes {
        write_config(root.path(), &text)?;
        let config = RuntimeConfig::load(Some(root.path()), None)
            .unwrap_or_else(|error| panic!("embedding recipe {name}: {error:#}"));
        assert!(config.effective.embedding.provider.is_some(), "{name}");
        assert!(config.effective.embedding.model.is_some(), "{name}");
        assert_eq!(
            config.effective.embedding.url.is_some(),
            name == "openai-compatible",
            "only the custom OpenAI recipe should override its endpoint"
        );
    }
    Ok(())
}

// Runtime serialization is the shared source of truth for display and coverage.
// Array-table members use their TOML spelling, while lists of scalar values
// remain a single key. A provider fixture expands the otherwise-empty array.
fn configuration_keys(value: &serde_json::Value, prefix: &str, keys: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            for (field, value) in fields {
                let field = match field.as_str() {
                    "baseUrl" => "base_url",
                    "apiKeyEnv" => "api_key_env",
                    "contextWindow" => "context_window",
                    "maxTokens" => "max_tokens",
                    field => field,
                };
                let key = if prefix.is_empty() {
                    field.to_string()
                } else {
                    format!("{prefix}.{field}")
                };
                configuration_keys(value, &key, keys);
            }
        }
        serde_json::Value::Array(values) if values.first().is_some_and(|v| v.is_object()) => {
            configuration_keys(&values[0], prefix, keys);
        }
        _ => {
            keys.insert(prefix.to_string());
        }
    }
}

#[test]
fn text_display_and_configuration_docs_cover_every_setting() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let config = RuntimeConfig::load(Some(root.path()), None)?;
    let value = serde_json::to_value(&config.effective)?;
    let mut keys = BTreeSet::new();
    configuration_keys(&value, "", &mut keys);
    assert_eq!(keys, config.sources.keys().cloned().collect());
    let shown = config.show_text();
    let reference = include_str!("../../docs/configuration.md");
    let analysis = include_str!("../../docs/codebase-analysis/15-configuration.md");
    for key in &keys {
        let value = key.split('.').fold(&value, |value, part| &value[part]);
        assert!(
            shown.contains(&format!("{key} = {value} [")),
            "missing value: {key}"
        );
        assert!(
            reference.contains(&format!("| `{key}` |")),
            "undocumented key: {key}"
        );
        assert!(
            analysis.contains(&format!("| `{key}` |")),
            "stale analysis table: {key}"
        );
    }
    Ok(())
}

#[test]
fn template_and_reference_cover_nested_provider_fields_too() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        r#"version = 1
[[llm.openai_compatible_providers]]
id = "local"
base_url = "http://127.0.0.1:1234/v1"
api_key_env = "LOCAL_LLM_API_KEY"
[[llm.openai_compatible_providers.models]]
id = "example"
"#,
    )?;
    let config = RuntimeConfig::load(Some(root.path()), None)?;
    let mut expected = BTreeSet::from(["version".to_string()]);
    configuration_keys(&serde_json::to_value(config.effective)?, "", &mut expected);

    let mut template_keys = BTreeSet::new();
    let mut section = "";
    for line in TEMPLATE.lines() {
        let line = line.strip_prefix("# ").unwrap_or(line).trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line.trim_matches(['[', ']']);
        } else if let Some((key, _)) = line.split_once('=') {
            let key = key.trim();
            if !key.is_empty()
                && key
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            {
                template_keys.insert(if section.is_empty() {
                    key.to_string()
                } else {
                    format!("{section}.{key}")
                });
            }
        }
    }
    assert_eq!(
        template_keys, expected,
        "template must cover every TOML field"
    );
    let reference = include_str!("../../docs/configuration.md");
    for key in expected {
        assert!(
            reference.contains(&format!("| `{key}` |")),
            "undocumented field: {key}"
        );
    }
    Ok(())
}

#[test]
fn documented_toml_recipes_load_without_providers_or_credentials() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    for (name, document) in [
        ("README.md", include_str!("../../README.md")),
        (
            "installation.md",
            include_str!("../../docs/installation.md"),
        ),
        ("inference.md", include_str!("../../docs/inference.md")),
        (
            "configuration.md",
            include_str!("../../docs/configuration.md"),
        ),
        (
            "documentation.md",
            include_str!("../../docs/documentation.md"),
        ),
        ("mcp.md", include_str!("../../docs/mcp.md")),
    ] {
        for (index, block) in document.split("```toml\n").skip(1).enumerate() {
            let recipe = block.split_once("```").expect("closed TOML fence").0;
            let parsed: toml::Value = toml::from_str(recipe)
                .unwrap_or_else(|error| panic!("{name} TOML recipe {index}: {error:#}"));
            if name == "mcp.md" {
                continue; // Client TOML has a different schema from repository policy.
            }
            let text = if parsed.get("version").is_some() {
                recipe.to_string()
            } else {
                format!("version = {SCHEMA_VERSION}\n{recipe}")
            };
            write_config(root.path(), &text)?;
            RuntimeConfig::load(Some(root.path()), None)
                .unwrap_or_else(|error| panic!("{name} config recipe {index}: {error:#}"));
        }
    }
    Ok(())
}

#[test]
fn mcp_tools_allowlist_rejects_unknown_names_and_explicit_empty_lists() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    std::fs::write(
        root.path().join(FILE_NAME),
        "version = 1\n[mcp]\ntools = []\n",
    )?;
    let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("mcp.tools must list at least one tool")
    );
    std::fs::write(
        root.path().join(FILE_NAME),
        "version = 1\n[mcp]\ntools = [\"definitions\"]\n",
    )?;
    let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
    assert!(error.to_string().contains("unknown tool `definitions`"));
    std::fs::write(
        root.path().join(FILE_NAME),
        "version = 1\n[mcp]\ntools = [\"definition\", \"who_uses\"]\n",
    )?;
    let loaded = RuntimeConfig::load(Some(root.path()), None)?;
    assert_eq!(loaded.effective.mcp.tools, ["definition", "who_uses"]);
    Ok(())
}

#[test]
fn mcp_result_transport_fails_closed() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        "version = 1\n[mcp]\nresult_transport = \"both\"\n",
    )?;
    let error = RuntimeConfig::load(Some(root.path()), None).unwrap_err();
    assert!(error.to_string().contains("mcp.result_transport"));
    Ok(())
}

#[test]
fn typed_compatible_providers_keep_only_secret_variable_names() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    write_config(
        root.path(),
        r"version = 1
[llm]
openai_compatible_providers = []
",
    )?;
    let empty = RuntimeConfig::load(Some(root.path()), None)?;
    assert!(empty.effective.llm.openai_compatible_providers.is_empty());
    assert_eq!(
        empty.sources["llm.openai_compatible_providers"],
        ValueSource::Config
    );

    write_config(
        root.path(),
        r#"version = 1
[[llm.openai_compatible_providers]]
id = "private"
base_url = "https://models.example.test/v1"
api_key_env = "PRIVATE_MODEL_KEY"

[[llm.openai_compatible_providers.models]]
id = "model-1"
reasoning = true
context_window = 100000
max_tokens = 10000
"#,
    )?;
    let configured = RuntimeConfig::load(Some(root.path()), None)?;
    let provider = &configured.effective.llm.openai_compatible_providers[0];
    assert_eq!(provider.name, "private");
    assert_eq!(provider.api_key_env.as_deref(), Some("PRIVATE_MODEL_KEY"));
    Ok(())
}

#[test]
fn legacy_compatible_provider_json_preserves_gateway_defaults() -> anyhow::Result<()> {
    let providers = parse_legacy_compatible_providers(
        r#"[{"id":"local","baseUrl":"http://127.0.0.1:11434/v1","models":[{"id":"smoke","contextWindow":131072,"maxTokens":32768}]}]"#,
    )?;
    let provider = &providers[0];
    assert_eq!(provider.name, "local");
    assert_eq!(provider.base_url, "http://127.0.0.1:11434/v1");
    assert_eq!(provider.models[0].name, "smoke");
    assert!(!provider.models[0].reasoning);
    assert_eq!(provider.models[0].context_window, 131_072);
    assert_eq!(provider.models[0].max_tokens, 32_768);
    Ok(())
}
