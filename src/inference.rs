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
    let script = project.join("service.py");
    let mut command = Command::new(uv);
    command
        .args(["run", "--project"])
        .arg(&project)
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

fn resolve_project(explicit: Option<&Path>, configured: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return validate_project(path, "--project");
    }
    if let Some(path) = configured {
        return validate_project(path, "inference.project");
    }

    let cwd = std::env::current_dir()?;
    for ancestor in cwd.ancestors() {
        let candidate = ancestor.join("inference");
        if candidate.join("pyproject.toml").is_file() && candidate.join("service.py").is_file() {
            return Ok(candidate);
        }
    }
    if let Ok(executable) = std::env::current_exe()
        && let Some(parent) = executable.parent()
    {
        for ancestor in parent.ancestors() {
            let candidate = ancestor.join("inference");
            if candidate.join("pyproject.toml").is_file() && candidate.join("service.py").is_file()
            {
                return Ok(candidate);
            }
        }
    }
    bail!(
        "local inference project not found; run from the jscout checkout, pass --project, or configure inference.project"
    )
}

fn validate_project(path: &Path, source: &str) -> Result<PathBuf> {
    let path = path.canonicalize().with_context(|| {
        format!(
            "{source} inference project does not exist: {}",
            path.display()
        )
    })?;
    if !path.join("pyproject.toml").is_file() || !path.join("service.py").is_file() {
        bail!(
            "{source} must name a directory containing pyproject.toml and service.py: {}",
            path.display()
        );
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::{get_json, validate_project};

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
}
