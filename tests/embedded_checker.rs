use std::{fs, path::Path, process::Command};

use serde_json::Value;

fn run(root: &Path, args: &[&str], without_node: bool) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jscout"));
    command.arg("--no-progress").args(args).arg(root);
    if without_node {
        command.env("PATH", "/jscout-test-no-executables");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("progress [") && !stderr.contains("checker preparation:"),
        "{stderr}"
    );
    serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
}

#[test]
fn embedded_cli_publishes_without_node_reuses_and_invalidates_inputs() {
    let repo = tempfile::tempdir().unwrap();
    fs::write(
        repo.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"strict":true,"noLib":true},"include":["*.ts"]}"#,
    )
    .unwrap();
    fs::write(repo.path().join("main.ts"), "export class Service { ping() {} } export function run(service: Service) { service.ping(); }").unwrap();
    run(repo.path(), &["index"], true);
    let args = [
        "enrich",
        "--experimental-tsr",
        "--sidecar-path",
        "/missing-sidecar",
        "--all",
    ];
    let first = run(repo.path(), &args, true);
    assert_eq!(first["checker_source"], "tsr-embedded");
    assert_eq!(first["facts_published"], 1);
    assert_eq!(first["exact_batch_reused"], false);
    let repeated = run(repo.path(), &args, true);
    assert_eq!(repeated["exact_batch_reused"], true);
    assert_eq!(first["batch_id"], repeated["batch_id"]);
    for name in ["README.md", ".DS_Store", "image.png", "data.bin"] {
        let path = repo.path().join(name);
        fs::write(&path, "noise").unwrap();
        let reused = run(repo.path(), &args, true);
        assert_eq!(reused["exact_batch_reused"], true, "{name}");
        assert_eq!(reused["batch_id"], first["batch_id"], "{name}");
        fs::remove_file(path).unwrap();
    }
    // Directory observations detect new ambient inputs without reindexing.
    fs::write(
        repo.path().join("ambient.d.ts"),
        "interface Added { value: string }",
    )
    .unwrap();
    let changed = run(repo.path(), &args, true);
    assert_eq!(changed["exact_batch_reused"], false);
    assert_eq!(changed["facts_published"], 1);

    let sidecar = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/src/main.mjs");
    if sidecar
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("node_modules/typescript/lib/typescript.js")
        .is_file()
    {
        let node = run(
            repo.path(),
            &[
                "enrich",
                "--all",
                "--sidecar-path",
                sidecar.to_str().unwrap(),
            ],
            false,
        );
        assert_ne!(node["checker_source"], first["checker_source"]);
        assert_eq!(node["exact_batch_reused"], false);
        assert_eq!(node["facts_published"], 1);
        let switched = run(repo.path(), &args, true);
        assert_eq!(switched["exact_batch_reused"], false);
        assert_eq!(switched["facts_published"], 1);
    }
}

#[cfg(unix)]
#[test]
fn embedded_discovery_honors_sigint() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let repo = tempfile::tempdir().unwrap();
    for index in 0..512 {
        let directory = repo.path().join(format!("project-{index}"));
        fs::create_dir(&directory).unwrap();
        fs::write(directory.join("tsconfig.json"), r#"{"files":["main.ts"]}"#).unwrap();
        fs::write(directory.join("main.ts"), "export {};").unwrap();
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_jscout"))
        .args(["--experimental-tsr", "checker", "doctor"])
        .arg(repo.path())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    stdout.read_line(&mut line).unwrap();
    assert!(line.contains("tsr-embedded")); // The interrupt handler is now registered.
    // SAFETY: signal only the live child just spawned by this test.
    assert_eq!(
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) },
        0
    );
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("interrupted"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(unix)]
#[test]
fn embedded_discovery_skips_unreadable_subdirectories() {
    use std::os::unix::fs::PermissionsExt;
    struct RestorePermissions(std::path::PathBuf, fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, self.1.clone());
        }
    }
    let repo = tempfile::tempdir().unwrap();
    fs::write(
        repo.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"noLib":true},"include":["**/*.ts"]}"#,
    )
    .unwrap();
    fs::write(repo.path().join("main.ts"), "export class Service { ping() {} } export function run(service: Service) { service.ping(); }").unwrap();
    let unreadable = repo.path().join("unreadable");
    fs::create_dir(&unreadable).unwrap();
    fs::write(unreadable.join("tsconfig.json"), "{}").unwrap();
    let _restore = RestorePermissions(
        unreadable.clone(),
        fs::metadata(&unreadable).unwrap().permissions(),
    );
    fs::set_permissions(&unreadable, fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read_dir(&unreadable).is_ok() {
        eprintln!("permission fixture requires an unprivileged user");
        return;
    }
    run(
        repo.path(),
        &["--experimental-tsr", "checker", "doctor"],
        true,
    );
    run(repo.path(), &["index"], true);
    let enriched = run(
        repo.path(),
        &["--experimental-tsr", "enrich", "--all"],
        true,
    );
    assert_eq!(enriched["facts_published"], 1);
}
