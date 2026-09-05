use std::path::PathBuf;

use anyhow::Result;
use serde_json::json;

use super::{
    Cli, Command, ConfigCommand, ScoutCommand, effective_search_response_byte_limit, or_configured,
    render_cli_neighborhood, render_semantic_memory_text, resolve_flag, resolve_search_provider,
};
use crate::{
    cli::DocsCommand,
    config::{EmbeddingSettings, InferenceSettings},
    semantic::SemanticArtifact,
    structural,
};
use clap::Parser;

#[test]
fn flag_resolution_covers_every_truth_table_row() {
    let cases = [
        (false, false, false, false),
        (false, false, true, true),
        (true, false, false, true),
        (true, false, true, true),
        (false, true, false, false),
        (false, true, true, false),
        (true, true, false, false),
        (true, true, true, false),
    ];

    for (enable, disable, configured, expected) in cases {
        assert_eq!(
            resolve_flag(enable, disable, configured),
            expected,
            "enable={enable}, disable={disable}, configured={configured}"
        );
    }
}

#[test]
fn list_resolution_uses_config_only_when_cli_is_empty() {
    let configured = vec!["configured".to_string()];

    assert_eq!(or_configured(Vec::new(), &configured), configured);
    assert_eq!(
        or_configured(vec!["explicit".to_string()], &configured),
        vec!["explicit".to_string()]
    );
}

#[test]
fn config_commands_and_global_selector_parse() {
    let cli = Cli::try_parse_from([
        "jscout",
        "--config",
        "/tmp/jscout.toml",
        "config",
        "show",
        ".",
        "--json",
    ])
    .expect("config show parses");

    assert_eq!(cli.config, Some(PathBuf::from("/tmp/jscout.toml")));
    let Command::Config {
        command: ConfigCommand::Show { root, json },
    } = cli.command
    else {
        panic!("expected config show")
    };
    assert_eq!(root, PathBuf::from("."));
    assert!(json);

    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "config", "validate", "."]).expect("config validate parses");
    assert!(matches!(
        command,
        Command::Config {
            command: ConfigCommand::Validate { .. }
        }
    ));
}

#[test]
fn agent_guide_update_is_explicit_rooted_and_exclusive_with_install() {
    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "agent-guide", "--update", "/tmp/repo"])
            .expect("agent-guide update parses");
    assert_eq!(command.root(), Some(std::path::Path::new("/tmp/repo")));
    assert!(matches!(
        command,
        Command::AgentGuide {
            install: None,
            update: Some(root),
            ..
        } if root == std::path::Path::new("/tmp/repo")
    ));

    assert!(
        Cli::try_parse_from([
            "jscout",
            "agent-guide",
            "--install",
            "/tmp/repo",
            "--update",
            "/tmp/repo",
        ])
        .is_err()
    );

    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "agent-guide"]).expect("agent-guide print parses");
    assert!(command.root().is_none());
}

#[test]
fn text_search_memory_is_renderable_without_code_hits() -> Result<()> {
    let rendered = render_semantic_memory_text(&[SemanticArtifact {
        id: 7,
        supersedes: None,
        artifact_type: "workflow".into(),
        name: Some("invoice settlement".into()),
        trust: "untrusted".into(),
        body: json!({ "description": "settles an invoice" }),
        model: "test".into(),
        prompt_version: "test/v1".into(),
        confidence: "likely".into(),
        source_snapshot: "snapshot".into(),
        created_at: "now".into(),
        freshness: "fresh".into(),
        supports: Vec::new(),
        retrieval_score: None,
    }])?;
    assert!(rendered.contains("semantic memory (untrusted; verify in source)"));
    assert!(rendered.contains("invoice settlement [fresh]"));
    Ok(())
}

#[test]
fn lexical_only_and_rerank_controls_parse_independently() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "search",
        ".",
        "query",
        "--no-vector",
        "--no-rerank",
    ])
    .expect("explicit controls parse");
    let Command::Search {
        no_vector,
        no_rerank,
        lexical_only,
        ..
    } = command
    else {
        panic!("expected search")
    };
    assert!(no_vector);
    assert!(no_rerank);
    assert!(!lexical_only);

    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "search", ".", "query", "--lexical-only"])
            .expect("lexical shortcut parses");
    let Command::Search { lexical_only, .. } = command else {
        panic!("expected search")
    };
    assert!(lexical_only);

    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "search",
        ".",
        "query",
        "--vector",
        "--rerank",
        "--memory",
        "--no-expand",
    ])
    .expect("positive repository-default overrides parse");
    let Command::Search {
        vector,
        rerank,
        memory,
        no_expand,
        limit,
        ..
    } = command
    else {
        panic!("expected search")
    };
    assert!(vector);
    assert!(rerank);
    assert!(memory);
    assert!(no_expand);
    assert_eq!(limit, None);

    assert!(
        Cli::try_parse_from(["jscout", "search", ".", "query", "--vector", "--no-vector"]).is_err()
    );

    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "search",
        ".",
        "query",
        "--expand",
        "--expand-mode",
        "neighborhood",
        "--expand-paths",
        "12",
    ])
    .expect("expansion projection controls parse");
    let Command::Search {
        expand,
        expand_mode,
        expand_paths,
        ..
    } = command
    else {
        panic!("expected search")
    };
    assert!(expand);
    assert_eq!(expand_mode.as_deref(), Some("neighborhood"));
    assert_eq!(expand_paths, Some(12));
}

#[test]
fn search_format_scope_is_repeatable_and_omitted_by_default() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "search",
        ".",
        "checker protocol",
        "--format",
        "javascript",
        "--format",
        "typescript",
    ])
    .expect("format allowlist parses");
    let Command::Search { formats, .. } = command else {
        panic!("expected search")
    };
    assert_eq!(formats, ["javascript", "typescript"]);

    let Cli { command, .. } = Cli::try_parse_from(["jscout", "search", ".", "checker protocol"])
        .expect("omitted format scope parses");
    let Command::Search { formats, .. } = command else {
        panic!("expected search")
    };
    assert!(formats.is_empty());
}

#[test]
fn rust_only_cli_scope_resolves_provider_only_for_vector_memory() -> Result<()> {
    let embedding = EmbeddingSettings {
        provider: Some("voyage".into()),
        model: Some("voyage-code-3".into()),
        revision: None,
        url: None,
        api_key_env: Some("JSCOUT_TEST_FORMAT_SCOPE_MISSING_VOYAGE_KEY_4C02A9".into()),
        query_prefix: None,
        batch: 64,
        origins: crate::origin::defaults(),
    };
    let inference = InferenceSettings {
        url: "http://127.0.0.1:11435".into(),
        host: "127.0.0.1".into(),
        port: 11_435,
        project: None,
        uv: "uv".into(),
        allow_remote: false,
        batch_size: 16,
        max_length: 4_096,
        model_cache_root: None,
    };

    assert!(
        resolve_search_provider(true, false, &["rust".into()], &embedding, &inference)?.is_none(),
        "Rust-only code search must not resolve vector credentials"
    );
    assert!(
        resolve_search_provider(false, true, &["rust".into()], &embedding, &inference)?.is_none(),
        "disabling vectors must keep attached semantic memory lexical"
    );
    let Err(error) = resolve_search_provider(true, true, &["rust".into()], &embedding, &inference)
    else {
        panic!("vector-enabled attached memory still requires configured credentials")
    };
    assert!(error.to_string().contains("requires secret environment"));
    let Err(error) =
        resolve_search_provider(true, false, &["javascript".into()], &embedding, &inference)
    else {
        panic!("a vector-capable format still requires configured credentials")
    };
    assert!(error.to_string().contains("requires secret environment"));
    Ok(())
}

#[test]
fn documentation_commands_and_required_vector_controls_parse() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "docs",
        "search",
        ".",
        "release guide",
        "--vector",
        "--no-rerank",
        "--no-freshness",
        "--limit",
        "7",
    ])
    .expect("documentation search parses");
    let Command::Docs {
        command:
            DocsCommand::Search {
                query,
                vector,
                no_rerank,
                no_freshness,
                limit,
                ..
            },
    } = command
    else {
        panic!("expected documentation search")
    };
    assert_eq!(query, "release guide");
    assert!(vector);
    assert!(no_rerank);
    assert!(no_freshness);
    assert_eq!(limit, Some(7));

    assert!(
        Cli::try_parse_from([
            "jscout",
            "docs",
            "search",
            ".",
            "query",
            "--vector",
            "--lexical-only",
        ])
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "jscout",
            "docs",
            "search",
            ".",
            "query",
            "--json",
            "--debug-json",
        ])
        .is_err()
    );
}

#[test]
fn exhaustive_search_parses_cursor_paging_and_rejects_explicit_stage_enables() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "search",
        ".",
        "query",
        "--exhaustive",
        "--cursor",
        "opaque",
        "--limit",
        "25",
        "--no-vector",
        "--no-rerank",
        "--no-memory",
        "--no-expand",
    ])
    .expect("exhaustive continuation parses");
    let Command::Search {
        exhaustive,
        cursor,
        limit,
        ..
    } = command
    else {
        panic!("expected search")
    };
    assert!(exhaustive);
    assert_eq!(cursor.as_deref(), Some("opaque"));
    assert_eq!(limit, Some(25));

    assert!(Cli::try_parse_from(["jscout", "search", ".", "query", "--cursor", "opaque"]).is_err());
    for stage in ["--vector", "--rerank", "--memory", "--expand"] {
        assert!(
            Cli::try_parse_from(["jscout", "search", ".", "query", "--exhaustive", stage]).is_err(),
            "{stage} must conflict with exhaustive mode",
        );
    }
}

#[test]
fn repository_scout_accepts_explicit_all_without_hiding_the_warning_threshold() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "scout",
        "repository",
        ".",
        "--max-calls",
        "all",
        "--max-subjects",
        "all",
        "--warn-subjects",
        "512",
    ])
    .expect("unbounded repository scout parses");
    let Command::Scout {
        command:
            ScoutCommand::Repository {
                max_calls,
                max_subjects,
                warn_subjects,
                ..
            },
    } = command
    else {
        panic!("expected repository scout")
    };
    assert_eq!(max_calls, usize::MAX);
    assert_eq!(max_subjects, usize::MAX);
    assert_eq!(warn_subjects, 512);
    assert!(
        Cli::try_parse_from(["jscout", "scout", "repository", ".", "--max-calls", "0",]).is_err()
    );
}

#[test]
fn search_and_embed_accept_external_database_paths() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "search",
        ".",
        "query",
        "--database",
        "/tmp/search.db",
    ])
    .expect("external search database parses");
    let Command::Search { database, .. } = command else {
        panic!("expected search")
    };
    assert_eq!(database, Some(PathBuf::from("/tmp/search.db")));

    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "embed", ".", "--database", "/tmp/embed.db"])
            .expect("external embed database parses");
    let Command::Embed { database, .. } = command else {
        panic!("expected embed")
    };
    assert_eq!(database, Some(PathBuf::from("/tmp/embed.db")));

    let Cli { command, .. } = Cli::try_parse_from(["jscout", "embed", ".", "--semantic-only"])
        .expect("semantic-only embedding parses");
    let Command::Embed {
        semantic,
        semantic_only,
        ..
    } = command
    else {
        panic!("expected embed")
    };
    assert!(!semantic);
    assert!(semantic_only);
    assert!(Cli::try_parse_from(["jscout", "embed", ".", "--product", "--semantic-only"]).is_err());
    assert!(Cli::try_parse_from(["jscout", "embed", ".", "--repair", "--semantic-only"]).is_err());

    let Cli { command, .. } = Cli::try_parse_from(["jscout", "embed", ".", "--repair"])
        .expect("explicit vector repair parses");
    let Command::Embed { repair, .. } = command else {
        panic!("expected embed")
    };
    assert!(repair);

    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "memory", ".", "rewrite behavior", "--no-vector"])
            .expect("lexical semantic-memory query parses");
    let Command::Memory { no_vector, .. } = command else {
        panic!("expected memory")
    };
    assert!(no_vector);

    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "memory",
        ".",
        "--artifact",
        "7",
        "--view",
        "body",
        "--supports-per-artifact",
        "3",
    ])
    .expect("semantic artifact view parses");
    let Command::Memory {
        artifact,
        view,
        supports_per_artifact,
        source_limit,
        ..
    } = command
    else {
        panic!("expected memory")
    };
    assert_eq!(artifact, Some(7));
    assert_eq!(view.as_deref(), Some("body"));
    assert_eq!(supports_per_artifact, Some(3));
    assert_eq!(source_limit, 1);
}

#[test]
fn compact_and_debug_json_modes_parse_without_ambiguity() {
    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "search", ".", "query", "--debug-json"])
            .expect("debug search output parses");
    let Command::Search {
        json,
        debug_json,
        response_bytes,
        ..
    } = command
    else {
        panic!("expected search")
    };
    assert!(!json);
    assert!(debug_json);
    assert_eq!(response_bytes, None);
    assert!(
        Cli::try_parse_from(["jscout", "search", ".", "query", "--json", "--debug-json"]).is_err()
    );

    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "neighborhood", ".", "root", "--debug-json"])
            .expect("debug neighborhood output parses");
    let Command::Neighborhood {
        debug_json,
        response_bytes,
        ..
    } = command
    else {
        panic!("expected neighborhood")
    };
    assert!(debug_json);
    assert_eq!(response_bytes, None);

    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "neighborhood",
        ".",
        "root",
        "--debug-json",
        "--response-bytes",
        "50000",
    ])
    .expect("explicit debug neighborhood budget parses");
    let Command::Neighborhood { response_bytes, .. } = command else {
        panic!("expected neighborhood")
    };
    assert_eq!(response_bytes, Some(50_000));
}

#[test]
fn non_search_cli_and_module_response_defaults_remain_24000_bytes() {
    for arguments in [
        ["jscout", "memory", ".", "needle"].as_slice(),
        ["jscout", "overview", "."].as_slice(),
    ] {
        let cli = Cli::try_parse_from(arguments).expect("non-search command parses");
        let (Command::Memory { response_bytes, .. } | Command::Overview { response_bytes, .. }) =
            cli.command
        else {
            panic!("expected memory or overview");
        };
        assert_eq!(response_bytes, 24_000);
    }
    assert_eq!(
        crate::semantic_query::QueryOptions::default().response_byte_limit,
        24_000
    );
    assert_eq!(
        crate::surface::OverviewOptions::default().response_byte_limit,
        24_000
    );
}

#[test]
fn debug_json_is_unbounded_unless_the_caller_sets_a_budget() {
    let configured = crate::config::SearchSettings::default().response_bytes;
    assert_eq!(
        effective_search_response_byte_limit(None, configured, false),
        30_000
    );
    assert_eq!(
        effective_search_response_byte_limit(Some(24_000), configured, false),
        24_000
    );
    assert_eq!(
        effective_search_response_byte_limit(None, 24_000, true),
        usize::MAX
    );
    assert_eq!(
        effective_search_response_byte_limit(Some(50_000), 24_000, true),
        50_000
    );
    assert_eq!(
        effective_search_response_byte_limit(None, 12_000, false),
        12_000
    );

    let neighborhood = structural::Neighborhood {
        snapshot: "snapshot".into(),
        publication_snapshot: "publication".into(),
        requested_anchor: "sym:src/root.ts#::root@1".into(),
        resolved_anchor: "sym:src/root.ts#::root@1".into(),
        anchor_status: "current".into(),
        nodes: Vec::new(),
        edges: Vec::new(),
        truncated: false,
    };
    let rendered = render_cli_neighborhood(&neighborhood, None, true)
        .expect("unbounded debug neighborhood renders");
    assert_eq!(
        rendered,
        serde_json::to_string(&neighborhood).expect("fixture serializes")
    );
}

#[test]
fn watch_checker_enrichment_controls_parse_independently() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "watch",
        ".",
        "--embed",
        "--product",
        "--enrich",
        "--enrich-timeout",
        "45",
        "--sidecar-path",
        "checker.mjs",
        "--database",
        "watch.db",
        "--debounce-ms",
        "750",
        "--reconcile-seconds",
        "30",
    ])
    .expect("watch enrichment controls parse");
    let Command::Watch {
        embed,
        product,
        enrich,
        enrich_timeout,
        sidecar_path,
        database,
        debounce_ms,
        reconcile_seconds,
        ..
    } = command
    else {
        panic!("expected watch")
    };
    assert!(embed);
    assert!(product);
    assert!(enrich);
    assert_eq!(enrich_timeout, Some(45));
    assert_eq!(sidecar_path, Some(PathBuf::from("checker.mjs")));
    assert_eq!(database, Some(PathBuf::from("watch.db")));
    assert_eq!(debounce_ms, Some(750));
    assert_eq!(reconcile_seconds, Some(30));

    assert!(Cli::try_parse_from(["jscout", "watch", ".", "--product"]).is_ok());
    assert!(Cli::try_parse_from(["jscout", "watch", ".", "--embed", "--no-embed"]).is_err());
}

#[test]
fn enrichment_plan_controls_parse_without_implying_a_default_cap() {
    let Cli { command, .. } = Cli::try_parse_from([
        "jscout",
        "enrich",
        ".",
        "--dry-run",
        "--file",
        "packages/core",
        "--package",
        "@scope/core",
        "--member",
        "insert",
        "--role",
        "test",
        "--max-occurrences",
        "25",
        "--all",
        "--full",
    ])
    .expect("enrichment plan controls parse");
    let Command::Enrich {
        files,
        packages,
        members,
        roles,
        max_occurrences,
        all,
        dry_run,
        full,
        ..
    } = command
    else {
        panic!("expected enrich")
    };
    assert_eq!(files, ["packages/core"]);
    assert_eq!(packages, ["@scope/core"]);
    assert_eq!(members, ["insert"]);
    assert_eq!(roles, ["test"]);
    assert_eq!(max_occurrences, Some(25));
    assert!(all);
    assert!(dry_run);
    assert!(full);

    let Cli { command, .. } =
        Cli::try_parse_from(["jscout", "enrich", "."]).expect("default enrich parses");
    let Command::Enrich {
        max_occurrences, ..
    } = command
    else {
        panic!("expected enrich")
    };
    assert_eq!(max_occurrences, None);
}
