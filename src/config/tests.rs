use super::{
    FILE_NAME, RuntimeConfig, SCHEMA_VERSION, TEMPLATE, ValueSource, init,
    parse_legacy_compatible_providers,
};
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
    assert_eq!(config.effective.docs.search.limit, 10);
    assert_eq!(config.effective.docs.search.response_bytes, 24_000);
    assert!(config.effective.search.vector);
    assert!(config.effective.search.rerank);
    assert!(!config.effective.search.attach_memory);
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
limit = 7
response_bytes = 12000
[search]
rerank = false
attach_memory = false
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
    assert_eq!(config.effective.docs.search.limit, 7);
    assert_eq!(config.effective.docs.search.response_bytes, 12_000);
    assert_eq!(
        config.effective.telemetry.file,
        Some(root.path().canonicalize()?.join("logs/mcp.jsonl"))
    );
    assert!(!config.effective.search.rerank);
    assert!(!config.effective.search.attach_memory);
    assert_eq!(config.sources["search.rerank"], ValueSource::Config);
    assert_eq!(config.sources["docs.include"], ValueSource::Config);
    assert_eq!(config.sources["docs.search.limit"], ValueSource::Config);
    let shown = config.show_text();
    assert!(shown.contains("docs: enabled=true include="));
    assert!(shown.contains("docs-search: vector=false rerank=false limit=7 response_bytes=12000"));
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
"#,
    )?;

    let config = RuntimeConfig::load(Some(root.path()), None)?;

    assert!(!config.effective.docs.enabled);
    assert!(config.effective.docs.search.vector);
    assert!(config.effective.docs.indexing_include().is_empty());
    assert!(config.effective.docs.indexing_exclude().is_empty());
    assert_eq!(
        config.effective.docs.include,
        ["handbook/**/*.md", "handbook/**/*.mdx"]
    );
    assert_eq!(config.sources["docs.enabled"], ValueSource::Config);
    assert_eq!(config.sources["docs.search.vector"], ValueSource::Config);
    assert!(config.show_text().contains("docs: enabled=false"));
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
    assert_eq!(loaded.effective.mcp.profile, "structural");
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
