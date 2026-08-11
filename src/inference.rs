use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

pub const DEFAULT_URL: &str = "http://127.0.0.1:8792";
const PROJECT_ENV: &str = "JSCOUT_INFERENCE_PROJECT";

pub fn base_url() -> String {
    if let Ok(url) = std::env::var("JSCOUT_INFERENCE_URL") {
        return url.trim_end_matches('/').to_string();
    }
    if std::env::var_os("JSCOUT_INFERENCE_HOST").is_none()
        && std::env::var_os("JSCOUT_INFERENCE_PORT").is_none()
    {
        return DEFAULT_URL.to_string();
    }
    let host = std::env::var("JSCOUT_INFERENCE_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let client_host = match host.as_str() {
        "0.0.0.0" => "127.0.0.1".to_string(),
        "::" => "[::1]".to_string(),
        value if value.contains(':') && !value.starts_with('[') => format!("[{value}]"),
        value => value.to_string(),
    };
    let port = std::env::var("JSCOUT_INFERENCE_PORT").unwrap_or_else(|_| "8792".to_string());
    format!("http://{client_host}:{port}")
}

pub fn serve(project: Option<&Path>) -> Result<()> {
    let project = resolve_project(project)?;
    let uv = std::env::var("JSCOUT_UV").unwrap_or_else(|_| "uv".to_string());
    let script = project.join("service.py");
    let status = Command::new(&uv)
        .args(["run", "--project"])
        .arg(&project)
        .arg("python")
        .arg(&script)
        .status()
        .with_context(|| {
            format!("failed to launch `{uv}`; install uv or set JSCOUT_UV to its absolute path")
        })?;
    if !status.success() {
        bail!("local inference service exited with {status}");
    }
    Ok(())
}

pub fn doctor(url: Option<&str>) -> Result<()> {
    let base = url
        .map(|value| value.trim_end_matches('/').to_string())
        .unwrap_or_else(base_url);
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
        "embedding: {} ({} dimensions)",
        configuration["embedding"]["model"]
            .as_str()
            .unwrap_or("unknown"),
        configuration["embedding"]["dimensions"]
            .as_u64()
            .map(|value| value.to_string())
            .unwrap_or_else(|| "unknown".to_string())
    );
    println!(
        "reranker: {}",
        configuration["reranker"]["model"]
            .as_str()
            .unwrap_or("unknown")
    );
    Ok(())
}

pub fn get_json(url: &str) -> Result<serde_json::Value> {
    let mut response = ureq::get(url).call()?;
    let text = response.body_mut().read_to_string()?;
    serde_json::from_str(&text).with_context(|| format!("invalid JSON from {url}"))
}

fn resolve_project(explicit: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return validate_project(path, "--project");
    }
    if let Some(path) = std::env::var_os(PROJECT_ENV) {
        return validate_project(Path::new(&path), PROJECT_ENV);
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
        "local inference project not found; run from the jscout checkout, pass --project, or set {PROJECT_ENV}"
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
    use super::validate_project;

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
