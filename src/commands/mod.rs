mod core;
mod docs;
mod scout;

use std::path::Path;

use anyhow::Result;

use crate::cli::{
    CheckerCommand, Command, ConfigCommand, DocsCommand, InferenceCommand, LlmCommand, ScoutCommand,
};
use crate::{
    agent, calls, checker, config, embed, inference, llm, mcp, scout as source_view, scouting,
    search, semantic, semantic_query, structural, surface, watch,
};

use self::core::{
    EmbedCommandOptions, cmd_calls, cmd_chunks, cmd_embed, cmd_events, cmd_index, cmd_neighborhood,
    cmd_search, cmd_stats, cmd_who_uses, open_database_for_write, open_database_read_only,
};
use self::scout::{
    RepositoryScoutCommandOptions, cmd_scout_cards, cmd_scout_concepts, cmd_scout_refresh,
    cmd_scout_repository, cmd_scout_summaries, cmd_scout_workflows,
};

pub(super) fn run_config_command(command: ConfigCommand, explicit: Option<&Path>) -> Result<()> {
    match command {
        ConfigCommand::Show { root, json } => {
            let config = config::RuntimeConfig::load(Some(&root), explicit)?;
            if json {
                println!("{}", config.show_json()?);
            } else {
                println!("{}", config.show_text());
            }
            Ok(())
        }
        ConfigCommand::Validate { root } => {
            let config = config::RuntimeConfig::load(Some(&root), explicit)?;
            println!(
                "configuration valid: {} ({})",
                config
                    .config_path
                    .as_deref()
                    .map_or_else(|| "<none>".to_string(), |path| path.display().to_string()),
                config.fingerprint
            );
            Ok(())
        }
        ConfigCommand::Init { root } => {
            let path = config::init(&root, explicit)?;
            println!("created {}", path.display());
            Ok(())
        }
    }
}

impl Command {
    pub(super) fn root(&self) -> Option<&Path> {
        match self {
            Self::Stats { root }
            | Self::Chunks { root, .. }
            | Self::Index { root, .. }
            | Self::Embed { root, .. }
            | Self::Search { root, .. }
            | Self::Events { root, .. }
            | Self::Calls { root, .. }
            | Self::Mcp { root, .. }
            | Self::Annotate { root, .. }
            | Self::Memory { root, .. }
            | Self::Overview { root, .. }
            | Self::WorkflowCandidates { root, .. }
            | Self::Watch { root, .. }
            | Self::WhoUses { root, .. }
            | Self::Neighborhood { root, .. }
            | Self::Enrich { root, .. } => Some(root),
            Self::Docs { command } => Some(command.root()),
            Self::Checker {
                command: CheckerCommand::Doctor { root, .. },
            } => Some(root),
            Self::Scout { command } => Some(command.root()),
            Self::AgentGuide {
                install: Some(root),
                ..
            } => Some(root),
            Self::AgentGuide {
                update: Some(root), ..
            } => Some(root),
            Self::Llm { .. } | Self::Inference { .. } => Some(Path::new(".")),
            Self::AgentGuide {
                install: None,
                update: None,
            } => None,
            Self::Config { command } => Some(command.root()),
        }
    }
}

impl DocsCommand {
    fn root(&self) -> &Path {
        match self {
            Self::Embed { root, .. } | Self::Search { root, .. } | Self::Status { root, .. } => {
                root
            }
        }
    }
}

impl ConfigCommand {
    fn root(&self) -> &Path {
        match self {
            Self::Show { root, .. } | Self::Validate { root } | Self::Init { root } => root,
        }
    }
}

impl ScoutCommand {
    fn root(&self) -> &Path {
        match self {
            Self::Repository { root, .. }
            | Self::Workflows { root, .. }
            | Self::Cards { root, .. }
            | Self::Summaries { root, .. }
            | Self::Concepts { root, .. }
            | Self::Refresh { root, .. } => root,
        }
    }
}

/// Resolve a `--flag` / `--no-flag` pair against its configured default.
///
/// Disabling overrides a configured `true`, enabling overrides a configured
/// `false`, and with neither flag present the configured value stands. Clap
/// rejects passing both explicit forms together.
pub(super) const fn resolve_flag(enable: bool, disable: bool, configured: bool) -> bool {
    if disable { false } else { enable || configured }
}

/// Command-line list arguments replace their configured default wholesale
/// rather than appending to it; an empty list means "not specified".
pub(super) fn or_configured<T: Clone>(explicit: Vec<T>, configured: &[T]) -> Vec<T> {
    if explicit.is_empty() {
        configured.to_vec()
    } else {
        explicit
    }
}

pub(super) fn effective_search_response_byte_limit(
    requested: Option<usize>,
    configured: usize,
    debug_json: bool,
) -> usize {
    requested.unwrap_or(if debug_json { usize::MAX } else { configured })
}

pub(super) fn resolve_search_provider(
    vector: bool,
    include_memory: bool,
    formats: &[String],
    embedding: &config::EmbeddingSettings,
    inference: &config::InferenceSettings,
) -> Result<Option<embed::Provider>> {
    search::validate_code_formats(formats)?;
    if !vector || (!include_memory && !search::format_scope_supports_code_vectors(formats)) {
        return Ok(None);
    }
    embed::Provider::from_settings(embedding, inference)
}

#[cfg(test)]
pub(super) fn render_cli_neighborhood(
    neighborhood: &structural::Neighborhood,
    response_bytes: Option<usize>,
    debug_json: bool,
) -> Result<String> {
    core::render_cli_neighborhood(neighborhood, response_bytes, debug_json)
}

#[cfg(test)]
pub(super) fn render_semantic_memory_text(
    artifacts: &[semantic::SemanticArtifact],
) -> Result<String> {
    core::render_semantic_memory_text(artifacts)
}

pub(super) fn run_command(command: Command, runtime: &config::RuntimeConfig) -> Result<()> {
    let configured_database = runtime.effective.database.path.as_path();
    match command {
        Command::Config { .. } => unreachable!("configuration commands are dispatched first"),
        Command::Docs { command } => docs::run(command, runtime),
        Command::Stats { root } => cmd_stats(&root),
        Command::Chunks { root, filter } => cmd_chunks(&root, filter.as_deref()),
        Command::Index {
            root,
            database,
            dependencies,
            no_dependencies,
        } => {
            let dependencies = if no_dependencies {
                Vec::new()
            } else if dependencies.is_empty() {
                runtime.effective.index.dependencies.clone()
            } else {
                dependencies
            };
            cmd_index(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                &dependencies,
                &runtime.effective.docs,
                &runtime.effective.diagnostics,
            )
        }
        Command::Embed {
            root,
            database,
            batch,
            file_origins,
            product,
            semantic,
            semantic_only,
            repair,
        } => cmd_embed(
            &root,
            Some(database.as_deref().unwrap_or(configured_database)),
            EmbedCommandOptions {
                batch: batch.unwrap_or(runtime.effective.embedding.batch),
                file_origins: if file_origins.is_empty() {
                    &runtime.effective.embedding.origins
                } else {
                    &file_origins
                },
                product,
                semantic,
                semantic_only,
                repair,
            },
            runtime,
        ),
        Command::Search {
            root,
            query,
            database,
            limit,
            exhaustive,
            cursor,
            file_roles,
            formats,
            file_origins,
            memory,
            no_memory,
            memory_limit,
            memory_depth,
            memory_nodes,
            response_bytes,
            vector,
            no_vector,
            rerank,
            no_rerank,
            lexical_only,
            json,
            debug_json,
            expand,
            no_expand,
            expand_depth,
            expand_mode,
            expand_seeds,
            expand_paths,
            expand_nodes,
            expand_edges,
            expand_bytes,
            expand_min_confidence,
            expand_file_roles,
        } => {
            let configured = &runtime.effective.search;
            let vector =
                !exhaustive && resolve_flag(vector, lexical_only || no_vector, configured.vector);
            let rerank =
                !exhaustive && resolve_flag(rerank, lexical_only || no_rerank, configured.rerank);
            let include_memory =
                !exhaustive && resolve_flag(memory, no_memory, configured.attach_memory);
            let expand =
                !exhaustive && resolve_flag(expand, no_expand, configured.expansion.enabled);
            let file_roles = or_configured(file_roles, &configured.file_roles);
            let file_origins = or_configured(file_origins, &configured.origins);
            let expand_file_roles =
                or_configured(expand_file_roles, &configured.expansion.file_roles);
            let provider = resolve_search_provider(
                vector,
                include_memory,
                &formats,
                &runtime.effective.embedding,
                &runtime.effective.inference,
            )?;
            cmd_search(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                &query,
                provider.as_ref(),
                json,
                debug_json,
                search::SearchOptions {
                    mode: if exhaustive {
                        search::SearchMode::Exhaustive { cursor }
                    } else {
                        search::SearchMode::Ranked
                    },
                    limit: search::resolve_search_limit(exhaustive, limit, configured.limit),
                    expand,
                    file_roles,
                    formats,
                    file_origins: file_origins.clone(),
                    include_memory,
                    memory_limit: memory_limit.unwrap_or(configured.memory_limit),
                    memory_graph_depth: memory_depth.unwrap_or(configured.memory_depth),
                    memory_graph_node_limit: memory_nodes.unwrap_or(configured.memory_nodes),
                    rerank,
                    reranker: search::Reranker::from_settings(
                        &runtime.effective.reranker,
                        &runtime.effective.embedding,
                        &runtime.effective.inference,
                    ),
                    timing: runtime.effective.diagnostics.timing,
                    compact: json,
                    include_neighborhood_followups: true,
                    response_byte_limit: effective_search_response_byte_limit(
                        response_bytes,
                        configured.response_bytes,
                        debug_json,
                    ),
                    expansion: search::ExpansionOptions {
                        projection: search::ExpansionProjection::parse(
                            expand_mode.as_deref().unwrap_or(&configured.expansion.mode),
                        )?,
                        depth: expand_depth.unwrap_or(configured.expansion.depth),
                        seed_limit: expand_seeds.unwrap_or(configured.expansion.seeds),
                        path_limit: expand_paths.unwrap_or(configured.expansion.paths),
                        node_limit: expand_nodes.unwrap_or(configured.expansion.nodes),
                        edge_limit: expand_edges.unwrap_or(configured.expansion.edges),
                        byte_limit: expand_bytes.unwrap_or(configured.expansion.bytes),
                        min_confidence: expand_min_confidence
                            .unwrap_or_else(|| configured.expansion.min_confidence.clone()),
                        file_roles: expand_file_roles,
                        file_origins,
                    },
                },
            )
        }
        Command::Events {
            root,
            name,
            file_origins,
        } => cmd_events(&root, configured_database, name.as_deref(), &file_origins),
        Command::Calls {
            root,
            method,
            args,
            arg_position,
            receiver,
            file_origins,
            limit,
            json,
            database,
        } => {
            let filters = args
                .iter()
                .map(|text| calls::ArgFilter::parse(text))
                .collect::<Result<Vec<_>>>()?;
            cmd_calls(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                &calls::CallQuery {
                    method,
                    args: filters,
                    arg_position,
                    receiver_suffix: receiver,
                    file_origins,
                    limit,
                },
                json,
            )
        }
        Command::Mcp {
            root,
            database,
            telemetry,
            request_log,
            profile,
            source_view,
            result_transport,
        } => {
            let profile = profile.as_deref().unwrap_or(&runtime.effective.mcp.profile);
            let source_view = source_view
                .as_deref()
                .unwrap_or(&runtime.effective.mcp.source_view);
            let result_transport = result_transport
                .as_deref()
                .unwrap_or(&runtime.effective.mcp.result_transport);
            mcp::serve(
                &root,
                database.as_deref().unwrap_or(configured_database),
                telemetry
                    .as_deref()
                    .or(runtime.effective.telemetry.file.as_deref()),
                request_log
                    .as_deref()
                    .or(runtime.effective.telemetry.request_log.as_deref()),
                mcp::ServeOptions {
                    profile: mcp::ToolProfile::parse(profile)?,
                    source_view: source_view::SourceView::parse(source_view)?,
                    result_transport: mcp::ResultTransportPolicy::parse(result_transport)?,
                },
                runtime,
            )
        }
        Command::Annotate {
            root,
            input,
            database,
        } => {
            let conn = open_database_for_write(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let input: semantic::AnnotateRequest = serde_json::from_slice(&std::fs::read(&input)?)?;
            let provider = embed::Provider::from_settings(
                &runtime.effective.embedding,
                &runtime.effective.inference,
            )?;
            let publication =
                semantic::annotate_request_with_provider(&root, &conn, provider.as_ref(), input)?;
            println!("{}", serde_json::to_string_pretty(&publication)?);
            Ok(())
        }
        Command::Memory {
            root,
            query,
            no_vector,
            vector,
            limit,
            artifact_types,
            freshness,
            artifact,
            view,
            debug,
            anchor,
            file,
            reconnaissance_subject,
            related_to,
            include_superseded,
            source,
            source_limit,
            source_depth,
            source_bytes,
            file_origins,
            response_bytes,
            supports_per_artifact,
            relation_limit,
            concept_tag_limit,
            database,
        } => {
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let vector = resolve_flag(vector, no_vector, runtime.effective.search.vector);
            let provider = if vector {
                embed::Provider::from_settings(
                    &runtime.effective.embedding,
                    &runtime.effective.inference,
                )?
            } else {
                None
            };
            let artifact_view = match view.as_deref() {
                Some(value) => semantic_query::ArtifactViewMode::parse(value)?,
                None if artifact.is_some() && !debug => semantic_query::ArtifactViewMode::Compact,
                None => semantic_query::ArtifactViewMode::Full,
            };
            let supports_per_artifact = supports_per_artifact.unwrap_or_else(|| {
                if artifact.is_some() && artifact_view != semantic_query::ArtifactViewMode::Full {
                    1
                } else {
                    8
                }
            });
            let result = semantic_query::query(
                &root,
                &conn,
                provider.as_ref(),
                &semantic_query::QueryOptions {
                    query,
                    artifact_id: artifact,
                    anchor,
                    file,
                    reconnaissance_subject,
                    related_to,
                    artifact_types,
                    freshness,
                    include_superseded,
                    limit,
                    include_source: source,
                    source_limit,
                    evidence_relation_depth: source_depth,
                    source_byte_limit: source_bytes,
                    file_origins,
                    response_byte_limit: response_bytes,
                    supports_per_artifact,
                    relation_limit,
                    concept_tag_limit,
                    artifact_view,
                    debug,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Command::Overview {
            root,
            file_origins,
            area_limit,
            relation_limit,
            semantic,
            semantic_limit,
            semantic_types,
            reconnaissance_limit,
            reconnaissance_subject,
            reconnaissance_detail,
            response_bytes,
            database,
        } => {
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let result = surface::overview_response(
                &conn,
                &surface::OverviewOptions {
                    file_origins,
                    area_limit,
                    relation_limit,
                    include_semantic: semantic,
                    semantic_limit,
                    semantic_types,
                    reconnaissance_limit,
                    reconnaissance_subject,
                    reconnaissance_detail,
                    response_byte_limit: response_bytes,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&result)?);
            Ok(())
        }
        Command::WorkflowCandidates {
            root,
            seeds,
            snapshot,
            depth,
            candidate_limit,
            database,
        } => {
            let conn = open_database_read_only(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
            )?;
            let candidates = semantic::workflow_candidates(
                &root,
                &conn,
                &seeds,
                &semantic::WorkflowCandidateOptions {
                    expected_snapshot: snapshot,
                    depth,
                    candidate_limit,
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&candidates)?);
            Ok(())
        }
        Command::Watch {
            root,
            database,
            embed,
            no_embed,
            product,
            no_product,
            dependencies,
            no_dependencies,
            enrich,
            no_enrich,
            enrich_timeout,
            sidecar_path,
            debounce_ms,
            reconcile_seconds,
        } => {
            let configured = &runtime.effective.watch;
            let embed = resolve_flag(embed, no_embed, configured.embed);
            let product = resolve_flag(product, no_product, configured.product);
            let enrich = resolve_flag(enrich, no_enrich, configured.enrich);
            let dependencies = if no_dependencies {
                Vec::new()
            } else if dependencies.is_empty() {
                configured.dependencies.clone()
            } else {
                dependencies
            };
            let enrich_timeout = enrich_timeout.unwrap_or(configured.enrich_timeout_seconds);
            let debounce_ms = debounce_ms.unwrap_or(configured.debounce_ms);
            let reconcile_seconds = reconcile_seconds.unwrap_or(configured.reconcile_seconds);
            if product && !embed {
                anyhow::bail!(
                    "product-only watched embedding requires embedding; enable --embed or disable product-only mode"
                );
            }
            if enrich_timeout == 0 {
                anyhow::bail!("watch enrichment timeout must be greater than zero");
            }
            if debounce_ms == 0 {
                anyhow::bail!("watch debounce must be greater than zero");
            }
            if reconcile_seconds != 0 && reconcile_seconds.saturating_mul(1_000) <= debounce_ms {
                anyhow::bail!("watch reconciliation must exceed debounce or be zero");
            }
            let provider = if embed {
                embed::Provider::from_settings(
                    &runtime.effective.embedding,
                    &runtime.effective.inference,
                )?
            } else {
                None
            };
            watch::watch(
                &root,
                &watch::WatchOptions {
                    database: Some(database.as_deref().unwrap_or(configured_database)),
                    embed_on_change: embed,
                    provider: provider.as_ref(),
                    embed_product_only: product,
                    dependencies: &dependencies,
                    docs_include: runtime.effective.docs.indexing_include(),
                    docs_exclude: runtime.effective.docs.indexing_exclude(),
                    enrich_on_change: enrich,
                    enrich_timeout: std::time::Duration::from_secs(enrich_timeout),
                    checker_sidecar: sidecar_path.as_deref().or(runtime
                        .effective
                        .sidecars
                        .checker
                        .as_deref()),
                    checker_node: &runtime.effective.sidecars.node,
                    timing: runtime.effective.diagnostics.timing,
                    debug: runtime.effective.diagnostics.debug,
                    debounce: std::time::Duration::from_millis(debounce_ms),
                    reconcile_interval: std::time::Duration::from_secs(reconcile_seconds),
                    config_fingerprint: &runtime.fingerprint,
                    config_loaded: runtime.config_loaded,
                },
            )
        }
        Command::WhoUses {
            root,
            spec,
            json,
            file_origins,
        } => cmd_who_uses(&root, configured_database, &spec, json, &file_origins),
        Command::Neighborhood {
            root,
            anchor,
            snapshot,
            depth,
            direction,
            node_limit,
            edge_limit,
            min_confidence,
            kinds,
            file_roles,
            file_origins,
            response_bytes,
            debug_json,
        } => cmd_neighborhood(
            &root,
            configured_database,
            &anchor,
            response_bytes,
            debug_json,
            structural::NeighborhoodOptions {
                expected_snapshot: snapshot,
                depth,
                direction,
                node_limit,
                edge_limit,
                min_confidence,
                kinds,
                penalize_file_roles: !file_roles.is_empty(),
                file_roles,
                file_origins,
            },
        ),
        Command::AgentGuide { install, update } => {
            if let Some(root) = install {
                let target = agent::install(&root)?;
                println!("installed {}", target.display());
            } else if let Some(root) = update {
                let target = agent::update(&root)?;
                println!("updated {}", target.display());
            } else {
                print!("{}", agent::GUIDE);
            }
            Ok(())
        }
        Command::Enrich {
            root,
            timeout,
            files,
            packages,
            members,
            roles,
            max_occurrences,
            all,
            dry_run,
            full,
            sidecar_path,
            database,
        } => {
            let report = checker::enrich(
                &root,
                &checker::EnrichOptions {
                    database: Some(database.as_deref().unwrap_or(configured_database)),
                    sidecar: sidecar_path.as_deref().or(runtime
                        .effective
                        .sidecars
                        .checker
                        .as_deref()),
                    node: &runtime.effective.sidecars.node,
                    timeout: std::time::Duration::from_secs(timeout),
                    files,
                    packages,
                    members,
                    roles,
                    max_occurrences,
                    include_all: all,
                    dry_run,
                    carry_forward: false,
                    force_full: full,
                    dirty_files: Vec::new(),
                },
            )?;
            println!("{}", serde_json::to_string_pretty(&report)?);
            Ok(())
        }
        Command::Checker { command } => match command {
            CheckerCommand::Doctor {
                root,
                timeout,
                sidecar_path,
            } => checker::doctor(
                &root,
                sidecar_path.as_deref(),
                runtime.effective.sidecars.checker.as_deref(),
                &runtime.effective.sidecars.node,
                std::time::Duration::from_secs(timeout),
            ),
        },
        Command::Llm { command } => match command {
            LlmCommand::Doctor {
                model,
                gateway_path,
            } => llm::doctor(model.as_deref(), gateway_path.as_deref(), runtime),
        },
        Command::Inference { command } => match command {
            InferenceCommand::Serve { project } => inference::serve(
                project.as_deref(),
                &runtime.effective.inference,
                &runtime.effective.embedding,
                &runtime.effective.reranker,
            ),
            InferenceCommand::Doctor { url } => {
                inference::doctor(url.as_deref(), &runtime.effective.inference)
            }
        },
        Command::Scout { command } => match command {
            ScoutCommand::Repository {
                root,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                max_subjects,
                warn_subjects,
                max_depth,
                rebuild,
                dry_run,
                checker_timeout,
                sidecar_path,
                database,
                gateway_path,
            } => cmd_scout_repository(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                gateway_path.as_deref(),
                runtime,
                RepositoryScoutCommandOptions {
                    dry_run,
                    warn_subjects,
                    planning: scouting::repository::RepositoryPlanningOptions {
                        max_subjects,
                        max_depth,
                        checker_timeout: std::time::Duration::from_secs(checker_timeout),
                        checker_sidecar: sidecar_path.as_deref().or(runtime
                            .effective
                            .sidecars
                            .checker
                            .as_deref()),
                        checker_node: &runtime.effective.sidecars.node,
                    },
                    scout: scouting::repository::RepositoryScoutOptions {
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?
                            .with_max_concurrency(runtime.effective.llm.max_concurrency)?,
                        rebuild,
                        max_subjects,
                        max_depth,
                    },
                },
            ),
            ScoutCommand::Workflows {
                root,
                seeds,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                depth,
                candidate_limit,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => {
                let max_calls = match max_calls {
                    Some(value) => value,
                    None if seeds.is_empty() => {
                        anyhow::bail!("automatic workflow scouting requires --max-calls")
                    }
                    None => 1,
                };
                cmd_scout_workflows(
                    &root,
                    Some(database.as_deref().unwrap_or(configured_database)),
                    gateway_path.as_deref(),
                    runtime,
                    dry_run,
                    scouting::WorkflowScoutOptions {
                        seeds,
                        depth,
                        candidate_limit,
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?
                            .with_max_concurrency(runtime.effective.llm.max_concurrency)?,
                        rebuild,
                        supersedes_artifact_id: None,
                    },
                )
            }
            ScoutCommand::Cards {
                root,
                anchors,
                files,
                reconnaissance_subjects,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => {
                let max_calls = match max_calls {
                    Some(value) => value,
                    None if anchors.is_empty()
                        || !files.is_empty()
                        || !reconnaissance_subjects.is_empty() =>
                    {
                        anyhow::bail!(
                            "automatic or file/subject-targeted card scouting requires --max-calls"
                        )
                    }
                    // One run per explicitly requested subject.
                    None => anchors.len(),
                };
                cmd_scout_cards(
                    &root,
                    Some(database.as_deref().unwrap_or(configured_database)),
                    gateway_path.as_deref(),
                    runtime,
                    dry_run,
                    scouting::CardScoutOptions {
                        anchors,
                        files,
                        reconnaissance_subjects,
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?
                            .with_max_concurrency(runtime.effective.llm.max_concurrency)?,
                        rebuild,
                        supersedes_artifact_id: None,
                    },
                )
            }
            ScoutCommand::Summaries {
                root,
                level,
                scopes,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => cmd_scout_summaries(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                gateway_path.as_deref(),
                runtime,
                dry_run,
                scouting::SummaryScoutOptions {
                    level,
                    scopes,
                    model: llm::config::resolve_model_setting(
                        model.as_deref(),
                        &runtime.effective.llm.model,
                    )?,
                    reasoning: llm::config::resolve_reasoning_setting(
                        reasoning.as_deref(),
                        runtime.effective.llm.reasoning.as_deref(),
                    ),
                    service_tier,
                    policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?
                        .with_max_concurrency(runtime.effective.llm.max_concurrency)?,
                    rebuild,
                    supersedes_artifact_id: None,
                },
            ),
            ScoutCommand::Concepts {
                root,
                terms,
                model,
                reasoning,
                service_tier,
                timeout,
                max_calls,
                context_bytes,
                rebuild,
                dry_run,
                database,
                gateway_path,
            } => {
                let max_calls = match max_calls {
                    Some(value) => value,
                    None if terms.is_empty() => {
                        anyhow::bail!("automatic concept scouting requires --max-calls")
                    }
                    None => terms.len(),
                };
                cmd_scout_concepts(
                    &root,
                    Some(database.as_deref().unwrap_or(configured_database)),
                    gateway_path.as_deref(),
                    runtime,
                    dry_run,
                    scouting::ConceptScoutOptions {
                        terms,
                        model: llm::config::resolve_model_setting(
                            model.as_deref(),
                            &runtime.effective.llm.model,
                        )?,
                        reasoning: llm::config::resolve_reasoning_setting(
                            reasoning.as_deref(),
                            runtime.effective.llm.reasoning.as_deref(),
                        ),
                        service_tier,
                        policy: llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?
                            .with_max_concurrency(runtime.effective.llm.max_concurrency)?,
                        rebuild,
                        supersedes_artifact_id: None,
                    },
                )
            }
            ScoutCommand::Refresh {
                root,
                artifacts,
                timeout,
                max_calls,
                context_bytes,
                dry_run,
                database,
                gateway_path,
            } => cmd_scout_refresh(
                &root,
                Some(database.as_deref().unwrap_or(configured_database)),
                gateway_path.as_deref(),
                runtime,
                &artifacts,
                dry_run,
                llm::config::RequestPolicy::new(timeout, max_calls, context_bytes)?
                    .with_max_concurrency(runtime.effective.llm.max_concurrency)?,
            ),
        },
    }
}
