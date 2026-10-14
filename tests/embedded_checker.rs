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
    let diagnostics = run(
        repo.path(),
        &[args.as_slice(), &["--tsr-diagnostics"]].concat(),
        true,
    );
    assert_eq!(diagnostics["exact_batch_reused"], true);
    assert_eq!(diagnostics["batch_id"], first["batch_id"]);
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

#[test]
fn embedded_toml_selection_honors_cli_and_explicit_config_overrides() {
    let repo = tempfile::tempdir().unwrap();
    fs::write(
        repo.path().join("tsconfig.json"),
        r#"{"compilerOptions":{"noLib":true},"include":["*.ts"]}"#,
    )
    .unwrap();
    fs::write(repo.path().join("main.ts"), "export class Service { ping() {} } export function run(service: Service) { service.ping(); }").unwrap();
    let policy = repo.path().join(".jscout.toml");
    fs::write(&policy, "version = 1\n[experimental]\ntsr = true\n[sidecars]\nnode = \"jscout-test-missing-node\"\n").unwrap();
    run(repo.path(), &["index"], true);
    let args = ["enrich", "--all", "--sidecar-path", "/missing-sidecar"];
    let configured = run(repo.path(), &args, true);
    assert_eq!(configured["checker_source"], "tsr-embedded");
    assert_eq!(configured["facts_published"], 1);
    run(repo.path(), &["checker", "doctor"], true);
    let shown = run(repo.path(), &["config", "show", "--json"], true);
    assert_eq!(shown["effective"]["experimental"]["tsr"], true);
    assert_eq!(shown["sources"]["experimental.tsr"], "config");

    // Disabling TSR must select Node even when TOML enabled it. This fixture
    // deliberately has no Node executable, so that backend must fail to start.
    let disabled = Command::new(env!("CARGO_BIN_EXE_jscout"))
        .args([
            "--no-progress",
            "--experimental-tsr=false",
            "--tsr-diagnostics",
            "enrich",
            "--all",
        ])
        .arg(repo.path())
        .env("PATH", "/jscout-test-no-executables")
        .output()
        .unwrap();
    assert!(!disabled.status.success());
    assert!(String::from_utf8_lossy(&disabled.stderr).contains("jscout-test-missing-node"));

    fs::write(&policy, "version = 1\n[experimental]\ntsr = false\n").unwrap();
    let enabled = run(
        repo.path(),
        &["--experimental-tsr", "enrich", "--all"],
        true,
    );
    assert_eq!(enabled["checker_source"], "tsr-embedded");
    assert_eq!(enabled["exact_batch_reused"], true);

    // An explicit config replaces the root's policy and still needs no CLI flag.
    let external = tempfile::tempdir().unwrap();
    let override_path = external.path().join("policy.toml");
    fs::write(&override_path, "version = 1\n[experimental]\ntsr = true\n").unwrap();
    let overridden = run(
        repo.path(),
        &[
            "--config",
            override_path.to_str().unwrap(),
            "enrich",
            "--all",
        ],
        true,
    );
    assert_eq!(overridden["checker_source"], "tsr-embedded");
    assert_eq!(overridden["exact_batch_reused"], true);
}

#[test]
fn embedded_diagnostics_are_opt_in_and_errors_survive_no_progress_in_serial_and_parallel() {
    let repo = tempfile::tempdir().unwrap();
    // Select TSR through TOML to verify diagnostics do not require the selector flag.
    fs::write(
        repo.path().join(".jscout.toml"),
        "version = 1\n[experimental]\ntsr = true\n",
    )
    .unwrap();
    for project in ["first", "second"] {
        let directory = repo.path().join(project);
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("tsconfig.json"),
            r#"{"compilerOptions":{"noLib":true},"files":["main.ts"]}"#,
        )
        .unwrap();
        fs::write(directory.join("main.ts"), "export class Service { ping() {} } export function run(service: Service) { service.ping(); }").unwrap();
    }
    run(repo.path(), &["index"], true);
    // Inject a malformed indexed span, causing a deterministic real adapter error.
    // Source text stays unchanged, so the checker reaches its call-lookup operation.
    let conn = rusqlite::Connection::open(repo.path().join(".jscout.db")).unwrap();
    assert_eq!(
        conn.execute(
            "UPDATE member_calls SET receiver_start = receiver_start + 1",
            []
        )
        .unwrap(),
        2
    );
    drop(conn);
    for jobs in ["1", "2"] {
        for diagnostics in [false, true] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_jscout"));
            command
                .args([
                    "--no-progress",
                    "enrich",
                    "--all",
                    "--full",
                    "--checker-jobs",
                    jobs,
                ])
                .arg(repo.path());
            if diagnostics {
                command.arg("--tsr-diagnostics");
            }
            let output = command
                .env("PATH", "/jscout-test-no-executables")
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(!output.status.success(), "{stderr}");
            assert!(
                !stderr.contains("progress [") && !stderr.contains("checker preparation:"),
                "{stderr}"
            );
            let failures = stderr
                .lines()
                .filter(|line| line.starts_with("checker enrichment: project "))
                .collect::<Vec<_>>();
            assert_eq!(failures.len(), 2, "{stderr}");
            for project in ["first", "second"] {
                let failure = failures
                    .iter()
                    .find(|line| line.contains(&format!("project {project}/tsconfig.json failed")))
                    .unwrap();
                assert!(
                    failure.contains("disposition=terminal") && failure.contains("[span_mismatch]"),
                    "{failure}"
                );
                assert!(
                    failure.contains("exact indexed member-call occurrence not found"),
                    "{failure}"
                );
                assert_eq!(
                    failure.contains(&format!("project=\"{project}/tsconfig.json\"")),
                    diagnostics,
                    "{failure}"
                );
                assert_eq!(
                    failure.contains(&format!("file=\"{project}/main.ts\"")),
                    diagnostics,
                    "{failure}"
                );
                assert_eq!(failure.contains("call_bytes="), diagnostics, "{failure}");
                assert_eq!(
                    failure.contains("operation=call-lookup"),
                    diagnostics,
                    "{failure}"
                );
            }
        }
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
