//! Minimal process runner, used to drive the real `jscout` binary and other
//! external tools without panicking on failure (tests assert on the outcome).

use std::path::Path;

#[derive(Debug, Clone)]
pub struct CmdOut {
    pub ok: bool,
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

impl CmdOut {
    /// stdout and stderr concatenated; convenient for message assertions.
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Run `bin` with `args` in `cwd`. Never panics on a non-zero exit; the caller
/// inspects [`CmdOut::ok`]. `env` entries are applied on top of the inherited
/// environment.
pub fn run(bin: &Path, args: &[&str], cwd: &Path, env: &[(&str, &str)]) -> CmdOut {
    let mut command = std::process::Command::new(bin);
    command.args(args).current_dir(cwd);
    for (key, value) in env {
        command.env(key, value);
    }
    match command.output() {
        Ok(output) => CmdOut {
            ok: output.status.success(),
            code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        },
        Err(error) => CmdOut {
            ok: false,
            code: None,
            stdout: String::new(),
            stderr: format!("spawn failed: {error}"),
        },
    }
}

/// Path to the `jscout` binary built from this checkout, resolved relative to
/// this crate rather than to any particular machine. `JSCOUT_BIN` overrides it.
/// Release is preferred over debug. Tests that need the real binary skip
/// themselves when it is absent, so the suite still runs on a fresh clone.
pub fn jscout_binary() -> Option<std::path::PathBuf> {
    if let Ok(explicit) = std::env::var("JSCOUT_BIN") {
        let path = std::path::PathBuf::from(explicit);
        return path.is_file().then_some(path);
    }
    // CARGO_MANIFEST_DIR is <repo>/eval/g24-harness.
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?;
    ["release", "debug"]
        .iter()
        .map(|profile| repo_root.join("target").join(profile).join("jscout"))
        .find(|candidate| candidate.is_file())
}
