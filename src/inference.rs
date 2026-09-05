use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub fn base_url(settings: &crate::config::InferenceSettings) -> String {
    settings.url.trim_end_matches('/').to_string()
}

pub fn serve(
    project: Option<&Path>,
    inference: &crate::config::InferenceSettings,
    embedding: &crate::config::EmbeddingSettings,
    reranker: &crate::config::RerankerSettings,
) -> Result<()> {
    let project = resolve_project(project, inference.project.as_deref())?;
    let uv = &inference.uv;
    let script = project.path.join("service.py");
    let mut command = Command::new(uv);
    command.args(["run", "--project"]).arg(&project.path);
    if project.bundled {
        // Global npm/archive installs may be read-only. Keep the installed
        // lockfile unchanged and put the environment in the user's cache.
        command.arg("--locked");
        if std::env::var_os("UV_PROJECT_ENVIRONMENT").is_none_or(|value| value.is_empty()) {
            command.env(
                "UV_PROJECT_ENVIRONMENT",
                bundled_environment(&project.path, &cache_root()?)?,
            );
        }
    }
    command
        .arg("python")
        .arg(&script)
        .env("JSCOUT_INFERENCE_HOST", &inference.host)
        .env("JSCOUT_INFERENCE_PORT", inference.port.to_string())
        .env(
            "JSCOUT_INFERENCE_ALLOW_REMOTE",
            if inference.allow_remote { "1" } else { "0" },
        )
        .env(
            "JSCOUT_INFERENCE_BATCH_SIZE",
            inference.batch_size.to_string(),
        )
        .env(
            "JSCOUT_INFERENCE_MAX_LENGTH",
            inference.max_length.to_string(),
        )
        .env("JSCOUT_RERANK_MODEL", &reranker.model);
    if let Some(model) = &embedding.model {
        command.env("JSCOUT_EMBED_MODEL", model);
    }
    if let Some(revision) = &embedding.revision {
        command.env("JSCOUT_EMBED_REVISION", revision);
    }
    if let Some(revision) = &reranker.revision {
        command.env("JSCOUT_RERANK_REVISION", revision);
    }
    if let Some(cache) = &inference.model_cache_root {
        command.env("JSCOUT_MODEL_CACHE_ROOT", cache);
    }
    let status = command.status().with_context(|| {
        format!("failed to launch `{uv}`; install uv or configure inference.uv")
    })?;
    if !status.success() {
        bail!("local inference service exited with {status}");
    }
    Ok(())
}

pub fn doctor(url: Option<&str>, settings: &crate::config::InferenceSettings) -> Result<()> {
    let base = url.map_or_else(
        || base_url(settings),
        |value| value.trim_end_matches('/').to_string(),
    );
    let health = get_json(&format!("{base}/health"))
        .with_context(|| format!("local inference is not reachable at {base}"))?;
    let configuration = get_json(&format!("{base}/configuration"))?;

    println!("endpoint: {base}");
    println!("health: {}", health["status"].as_str().unwrap_or("unknown"));
    println!(
        "provider: {}",
        configuration["provider"].as_str().unwrap_or("unknown")
    );
    println!(
        "device: {}",
        configuration["device"].as_str().unwrap_or("unknown")
    );
    println!(
        "embedding: {} @ {} ({} dimensions)",
        configuration["embedding"]["model"]
            .as_str()
            .unwrap_or("unknown"),
        configuration["embedding"]["revision"]
            .as_str()
            .unwrap_or("unresolved"),
        configuration["embedding"]["dimensions"]
            .as_u64()
            .map_or_else(|| "unknown".to_string(), |value| value.to_string())
    );
    println!(
        "reranker: {} @ {}",
        configuration["reranker"]["model"]
            .as_str()
            .unwrap_or("unknown"),
        configuration["reranker"]["revision"]
            .as_str()
            .unwrap_or("unresolved")
    );
    Ok(())
}

pub fn get_json(url: &str) -> Result<serde_json::Value> {
    let agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(10)))
        .build()
        .new_agent();
    let mut response = agent
        .get(url)
        .call()
        .with_context(|| format!("request to local inference endpoint {url} failed"))?;
    let text = response.body_mut().read_to_string()?;
    serde_json::from_str(&text).with_context(|| format!("invalid JSON from {url}"))
}

struct InferenceProject {
    path: PathBuf,
    bundled: bool,
}

fn resolve_project(explicit: Option<&Path>, configured: Option<&Path>) -> Result<InferenceProject> {
    let bundled = std::env::var_os("JSCOUT_BUNDLED_INFERENCE_PROJECT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from);
    resolve_project_from(
        explicit,
        configured,
        bundled.as_deref(),
        std::env::current_dir().ok().as_deref(),
        std::env::current_exe().ok().as_deref(),
    )
}

fn resolve_project_from(
    explicit: Option<&Path>,
    configured: Option<&Path>,
    bundled: Option<&Path>,
    cwd: Option<&Path>,
    executable: Option<&Path>,
) -> Result<InferenceProject> {
    for (source, path) in [("--project", explicit), ("inference.project", configured)] {
        if let Some(path) = path {
            return Ok(InferenceProject {
                path: validate_project(path, source)?,
                bundled: false,
            });
        }
    }
    if let Some(path) = bundled {
        return validate_bundle(path, "npm bundle");
    }
    if let Some(parent) = executable.and_then(Path::parent) {
        let candidate = parent.join("inference");
        if candidate.is_dir() {
            return validate_bundle(&candidate, "release bundle");
        }
    }
    // Installed resources win over unrelated inference/ directories in the
    // indexed repository. Source binaries retain ancestor discovery.
    for root in cwd.into_iter().chain(executable.and_then(Path::parent)) {
        for ancestor in root.ancestors() {
            let candidate = ancestor.join("inference");
            if is_project(&candidate) {
                return Ok(InferenceProject {
                    path: candidate,
                    bundled: false,
                });
            }
        }
    }
    bail!(
        "local inference project not found; reinstall the complete npm/release package, run from the jscout checkout, pass --project, or configure inference.project"
    )
}

fn is_project(path: &Path) -> bool {
    path.join("pyproject.toml").is_file() && path.join("service.py").is_file()
}

fn validate_bundle(path: &Path, source: &str) -> Result<InferenceProject> {
    let path = validate_project(path, source)?;
    if !path.join("uv.lock").is_file() {
        bail!("{source} is missing inference/uv.lock; reinstall the complete package");
    }
    Ok(InferenceProject {
        path,
        bundled: true,
    })
}

fn cache_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("XDG_CACHE_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Ok(path);
        }
    }
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .context("HOME is not set; set UV_PROJECT_ENVIRONMENT for the bundled inference service")?;
    Ok(PathBuf::from(home).join(".cache"))
}

fn bundled_environment(project: &Path, cache: &Path) -> Result<PathBuf> {
    let mut identity = blake3::Hasher::new();
    for file in ["pyproject.toml", "uv.lock"] {
        let path = project.join(file);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read bundled inference file {}", path.display()))?;
        identity.update(&(bytes.len() as u64).to_le_bytes());
        identity.update(&bytes);
    }
    Ok(cache
        .join("jscout")
        .join("inference")
        .join(identity.finalize().to_hex().as_str()))
}

fn validate_project(path: &Path, source: &str) -> Result<PathBuf> {
    let path = path.canonicalize().with_context(|| {
        format!(
            "{source} inference project does not exist: {}",
            path.display()
        )
    })?;
    if !is_project(&path) {
        bail!(
            "{source} must name a directory containing pyproject.toml and service.py: {}",
            path.display()
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{bundled_environment, get_json, resolve_project_from, validate_project};
    use std::path::{Path, PathBuf};

    fn project(root: &Path, name: &str) -> anyhow::Result<PathBuf> {
        let path = root.join(name);
        std::fs::create_dir_all(&path)?;
        std::fs::write(path.join("pyproject.toml"), "[project]\n")?;
        std::fs::write(path.join("service.py"), "")?;
        std::fs::write(path.join("uv.lock"), "version = 1\n")?;
        Ok(path.canonicalize()?)
    }

    #[test]
    fn endpoint_failures_name_the_url() {
        let url = "http://127.0.0.1:1/configuration";
        let error = get_json(url).expect_err("closed local port must fail");
        assert!(error.to_string().contains(url));
    }

    #[test]
    fn validates_complete_project() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        std::fs::write(directory.path().join("pyproject.toml"), "[project]\n")?;
        std::fs::write(directory.path().join("service.py"), "")?;
        assert_eq!(
            validate_project(directory.path(), "test")?,
            directory.path().canonicalize()?
        );
        Ok(())
    }

    #[test]
    fn discovery_prefers_overrides_then_bundles_then_source() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let explicit = project(root, "explicit")?;
        let configured = project(root, "configured")?;
        let bundled = project(root, "npm/inference")?;
        let archive = project(root, "archive/inference")?;
        let source = project(root, "source/inference")?;
        let executable_source = project(root, "checkout/inference")?;
        let source_cwd = source.parent().unwrap();
        let archive_exe = archive.parent().unwrap().join("jscout");
        let source_exe = executable_source
            .parent()
            .unwrap()
            .join("target/release/jscout");

        for (cli, config, bundle, cwd, exe, expected, is_bundled) in [
            (
                Some(explicit.as_path()),
                Some(configured.as_path()),
                Some(bundled.as_path()),
                source_cwd,
                Some(archive_exe.as_path()),
                &explicit,
                false,
            ),
            (
                None,
                Some(configured.as_path()),
                Some(bundled.as_path()),
                source_cwd,
                Some(archive_exe.as_path()),
                &configured,
                false,
            ),
            (
                None,
                None,
                Some(bundled.as_path()),
                source_cwd,
                Some(archive_exe.as_path()),
                &bundled,
                true,
            ),
            (
                None,
                None,
                None,
                source_cwd,
                Some(archive_exe.as_path()),
                &archive,
                true,
            ),
            (
                None,
                None,
                None,
                source_cwd,
                Some(source_exe.as_path()),
                &source,
                false,
            ),
            (
                None,
                None,
                None,
                root,
                Some(source_exe.as_path()),
                &executable_source,
                false,
            ),
        ] {
            let resolved = resolve_project_from(cli, config, bundle, Some(cwd), exe)?;
            assert_eq!(resolved.path, *expected);
            assert_eq!(resolved.bundled, is_bundled);
        }
        Ok(())
    }

    #[test]
    fn invalid_override_or_bundle_does_not_fall_back() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let source = project(directory.path(), "inference")?;
        let missing = directory.path().join("missing");
        for (explicit, configured, bundled) in [
            (Some(missing.as_path()), None, None),
            (None, Some(missing.as_path()), None),
            (None, None, Some(missing.as_path())),
        ] {
            assert!(
                resolve_project_from(explicit, configured, bundled, Some(directory.path()), None)
                    .is_err()
            );
        }
        std::fs::remove_file(source.join("uv.lock"))?;
        let error = resolve_project_from(None, None, Some(&source), Some(directory.path()), None)
            .err()
            .expect("bundles must contain their lockfile");
        assert!(error.to_string().contains("missing inference/uv.lock"));
        Ok(())
    }

    #[test]
    fn bundled_environment_follows_dependency_identity_not_install_location() -> anyhow::Result<()>
    {
        let directory = tempfile::tempdir()?;
        let first = project(directory.path(), "first")?;
        let second = project(directory.path(), "second")?;
        let cache = directory.path().join("cache");
        let expected = bundled_environment(&first, &cache)?;
        assert!(expected.starts_with(cache.join("jscout/inference")));
        assert_eq!(expected, bundled_environment(&second, &cache)?);
        std::fs::write(second.join("service.py"), "# same dependencies\n")?;
        assert_eq!(expected, bundled_environment(&second, &cache)?);
        std::fs::write(second.join("uv.lock"), "version = 2\n")?;
        assert_ne!(expected, bundled_environment(&second, &cache)?);
        Ok(())
    }
}
