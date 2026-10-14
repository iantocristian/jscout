#![cfg(unix)]

use std::{fs, path::Path, process::Command};

use serde_json::Value;

fn run(root: &Path, args: &[&str]) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_jscout"))
        .arg("--no-progress")
        .args(args)
        .arg(root)
        .output()
        .expect("run jscout");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap_or(Value::Null)
}

fn enrich(root: &Path) -> Value {
    let sidecar = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/src/main.mjs");
    run(
        root,
        &[
            "enrich",
            "--all",
            "--sidecar-path",
            sidecar.to_str().unwrap(),
        ],
    )
}

#[test]
fn native_typescript7_cli_publishes_reuses_and_invalidates() {
    let compiler = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/node_modules/typescript7");
    assert!(
        compiler.join("package.json").is_file(),
        "run npm ci --prefix checker before running the real TS7 integration test"
    );
    let repo = tempfile::tempdir().expect("tempdir");
    let root = repo.path();
    fs::create_dir(root.join("node_modules")).unwrap();
    std::os::unix::fs::symlink(&compiler, root.join("node_modules/typescript")).unwrap();
    let config = root.join("tsconfig.json");
    fs::write(
        &config,
        r#"{"compilerOptions":{"strict":true,"noLib":true},"files":["main.ts"]}"#,
    )
    .unwrap();
    let source = "export class Service { ping() {} } export function run(service: Service) { service.ping(); }";
    fs::write(root.join("main.ts"), source).unwrap();
    run(root, &["index"]);
    let first = enrich(root);
    assert_eq!(first["checker_version"], "7.0.2");
    assert_eq!(first["checker_source"], "repository");
    assert_eq!(first["facts_published"], 1);
    assert_eq!(first["exact_batch_reused"], false);

    let reused = enrich(root);
    assert_eq!(reused["exact_batch_reused"], true);
    assert_eq!(reused["batch_id"], first["batch_id"]);
    assert_eq!(reused["occurrences_queried"], 0);

    fs::write(
        root.join("main.ts"),
        source.replace("ping() {}", "ping() { return 7; }"),
    )
    .unwrap();
    run(root, &["index"]);
    let edited = enrich(root);
    assert_eq!(edited["exact_batch_reused"], false);
    assert_eq!(edited["facts_published"], 1);

    fs::write(
        config,
        r#"{"compilerOptions":{"strict":false,"noLib":true},"files":["main.ts"]}"#,
    )
    .unwrap();
    let changed_config = enrich(root);
    assert_eq!(changed_config["exact_batch_reused"], false);
    assert_eq!(changed_config["occurrences_queried"], 1);
    assert_eq!(changed_config["facts_published"], 1);
}
