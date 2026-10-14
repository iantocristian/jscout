use std::fs;
use std::time::Duration;

use super::*;
use crate::checker::{Backend, launch};

const TIMEOUT: Duration = Duration::from_secs(60);

fn fixture(files: &[(&str, &str)]) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    for (name, text) in files {
        let path = directory.path().join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    directory
}

fn query(source: &str, file: &str, call: &str, receiver: &str, property: &str) -> MemberQuery {
    let start = source.rfind(call).unwrap();
    let receiver_start = start + call.find(receiver).unwrap();
    let property_start = start + call.find(property).unwrap();
    MemberQuery {
        file: file.into(),
        indexed_hash: blake3::hash(source.as_bytes()).to_hex().to_string(),
        call_start: start as i64,
        call_end: (start + call.len()) as i64,
        receiver_start: receiver_start as i64,
        receiver_end: (receiver_start + receiver.len()) as i64,
        property_start: property_start as i64,
        property_end: (property_start + property.len()) as i64,
    }
}

#[test]
fn embedded_queries_match_typescript_on_shared_fixtures() {
    let cases = [
        (
            "interface",
            "/** 😀 */ interface A { ping(): void } declare const receiver: A; receiver.ping();",
            "receiver.ping()",
            1,
        ),
        (
            "union",
            "interface A { ping(): void } interface B { ping(): void } declare const receiver: A | B; receiver.ping();",
            "receiver.ping()",
            2,
        ),
        (
            "implementation",
            "class A { ping(x: string): void; ping(x: number): void; ping(x: string | number) {} } declare const receiver: A; receiver.ping(1);",
            "receiver.ping(1)",
            1,
        ),
        (
            "signatures",
            "interface A { ping(x: string): void; ping(x: number): void } declare const receiver: A; receiver.ping(1);",
            "receiver.ping(1)",
            1,
        ),
        (
            "inheritance",
            "class Base { ping() {} } class A extends Base {} declare const receiver: A; receiver.ping();",
            "receiver.ping()",
            1,
        ),
        (
            "optional",
            "interface A { ping(): void } declare const receiver: A | undefined; receiver?.ping();",
            "receiver?.ping()",
            1,
        ),
        (
            "any",
            "declare const receiver: any; receiver.ping();",
            "receiver.ping()",
            0,
        ),
        (
            "unknown",
            "declare const receiver: unknown; receiver.ping();",
            "receiver.ping()",
            0,
        ),
        (
            "import",
            "import { A as Alias } from './service'; declare const receiver: Alias; receiver.ping();",
            "receiver.ping()",
            1,
        ),
    ];
    let sidecar = Path::new(env!("CARGO_MANIFEST_DIR")).join("checker/src/main.mjs");
    let node_available = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("checker/node_modules/typescript/lib/typescript.js")
        .is_file();
    for (label, source, call, count) in cases {
        let directory = fixture(&[
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"strict":true,"noLib":true},"files":["main.ts","service.ts"]}"#,
            ),
            ("main.ts", source),
            ("service.ts", "export interface A { ping(): void }"),
        ]);
        let query = query(source, "main.ts", call, "receiver", "ping");
        let mut embedded = launch(
            directory.path(),
            Some(Path::new("/missing-sidecar")),
            None,
            "/missing-node",
            Backend::Tsr,
        )
        .unwrap();
        let plan = embedded
            .plan_members(vec!["main.ts".into()], TIMEOUT)
            .unwrap();
        assert_eq!(plan.files[0].project_ids, ["tsconfig.json"], "{label}");
        let result = embedded
            .resolve_members("tsconfig.json".into(), vec![], vec![query.clone()], TIMEOUT)
            .unwrap();
        assert_eq!(
            result.results[0].answer.declarations.len(),
            count,
            "{label}"
        );
        let validation = embedded
            .validate_project(
                "tsconfig.json".into(),
                result.checker_input_fingerprint.clone(),
                TIMEOUT,
            )
            .unwrap();
        assert!(validation.valid, "{label}");
        let repeated = embedded
            .resolve_members("tsconfig.json".into(), vec![], vec![query.clone()], TIMEOUT)
            .unwrap();
        assert_eq!(
            result.checker_input_fingerprint, repeated.checker_input_fingerprint,
            "{label}"
        );
        if node_available {
            let mut node = launch(
                directory.path(),
                Some(&sidecar),
                None,
                "node",
                Backend::TypeScript,
            )
            .unwrap();
            let reference = node
                .resolve_members("tsconfig.json".into(), vec![], vec![query], TIMEOUT)
                .unwrap();
            let answer = &result.results[0].answer;
            let expected = &reference.results[0].answer;
            assert_eq!(answer.status, expected.status, "{label}");
            assert_eq!(answer.receiver_type, expected.receiver_type, "{label}");
            assert_eq!(
                serde_json::to_value(&answer.declarations).unwrap(),
                serde_json::to_value(&expected.declarations).unwrap(),
                "{label}"
            );
        }
    }
}

#[test]
fn jsdoc_and_bundled_libraries_work_without_a_sidecar() {
    for (file, source, call, property, context) in [
        (
            "main.js",
            "class A { ping() {} }\n/** @param {A} receiver */\nfunction visit(receiver) { receiver.ping(); }",
            "receiver.ping()",
            "ping",
            "repo",
        ),
        (
            "main.ts",
            "const receiver: number[] = []; receiver.map(x => x);",
            "receiver.map(x => x)",
            "map",
            "lib",
        ),
    ] {
        let config = format!(
            r#"{{"compilerOptions":{{"allowJs":true,"checkJs":true}},"files":["{file}"]}}"#
        );
        let directory = fixture(&[("tsconfig.json", &config), (file, source)]);
        let mut checker = EmbeddedChecker::new(directory.path()).unwrap();
        let result = checker
            .resolve_members(
                "tsconfig.json".into(),
                vec![],
                vec![query(source, file, call, "receiver", property)],
                TIMEOUT,
            )
            .unwrap();
        assert_eq!(result.results[0].answer.declarations.len(), 1);
        assert_eq!(
            result.results[0].answer.declarations[0].context.as_deref(),
            Some(context)
        );
        assert!(
            !checker
                .built
                .as_ref()
                .unwrap()
                .inputs
                .iter()
                .any(|input| input.path.starts_with("bundled:"))
        );
    }
}

#[test]
fn ownership_honors_extends_and_tooling_without_building_programs() {
    let directory = fixture(&[
        (
            "tsconfig.base.json",
            r#"{"compilerOptions":{"noLib":true,"strict":true}}"#,
        ),
        (
            "tsconfig.json",
            r#"{"extends":"./tsconfig.base.json","include":["src/*.ts"]}"#,
        ),
        (
            "tsconfig.eslint.json",
            r#"{"extends":"./tsconfig.base.json","include":["src/*.ts","extra.ts"]}"#,
        ),
        ("src/main.ts", "export const a = 1;"),
        ("extra.ts", "export const b = 1;"),
    ]);
    let mut checker = EmbeddedChecker::new(directory.path()).unwrap();
    let plan = checker
        .plan_members(vec!["src/main.ts".into(), "extra.ts".into()], TIMEOUT, true)
        .unwrap();
    assert!(checker.built.is_none());
    assert_eq!(plan.projects.len(), 2);
    let main = plan
        .files
        .iter()
        .find(|file| file.file == "src/main.ts")
        .unwrap();
    assert_eq!(main.project_ids, ["tsconfig.json"]);
    assert_eq!(main.excluded_project_ids, ["tsconfig.eslint.json"]);
    assert!(
        plan.files
            .iter()
            .find(|file| file.file == "extra.ts")
            .unwrap()
            .tooling_fallback
    );
}

#[test]
fn malformed_configs_are_reported_and_orphans_are_bounded() {
    let directory = fixture(&[
        ("tsconfig.json", "{ definitely not valid JSON"),
        ("package.json", r#"{"type":"module"}"#),
    ]);
    let files = (0..151)
        .map(|index| {
            let file = format!("source-{index:03}.ts");
            fs::write(directory.path().join(&file), "export const a = 1;").unwrap();
            file
        })
        .collect::<Vec<_>>();
    let mut checker = EmbeddedChecker::new(directory.path()).unwrap();
    let plan = checker.plan_members(files, TIMEOUT, true).unwrap();
    assert!(!plan.configuration_problems.is_empty());
    assert_eq!(plan.files.len(), 151);
    assert_eq!(plan.projects.len(), 2);
    assert!(plan.projects.iter().all(|project| project.file_count <= 150
        && project.project_id.starts_with("inferred:.#node-esm/")));
    assert!(checker.built.is_none());
}

#[test]
fn inherited_include_does_not_turn_an_intermediate_base_into_an_owner() {
    let directory = fixture(&[
        ("tsconfig.base.json", r#"{"include":["main.ts"]}"#),
        (
            "tsconfig.middle.json",
            r#"{"extends":"./tsconfig.base.json"}"#,
        ),
        ("tsconfig.json", r#"{"extends":"./tsconfig.middle.json"}"#),
        ("main.ts", "export {};"),
    ]);
    let mut checker = EmbeddedChecker::new(directory.path()).unwrap();
    let plan = checker
        .plan_members(vec!["main.ts".into()], TIMEOUT, true)
        .unwrap();
    assert_eq!(
        plan.files[0].project_ids,
        ["tsconfig.base.json", "tsconfig.json"]
    );
}

#[test]
fn prepared_batches_share_one_manifest_and_deadlines_retire_loaded_programs() {
    let source = "import { A } from './service'; declare const receiver: A; receiver.ping();";
    let directory = fixture(&[
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"noLib":true},"include":["*.ts"]}"#,
        ),
        ("first.ts", source),
        ("second.ts", source),
        ("service.ts", "export interface A { ping(): void }"),
    ]);
    let queries = ["first.ts", "second.ts"]
        .map(|file| query(source, file, "receiver.ping()", "receiver", "ping"));
    let mut checker = EmbeddedChecker::new(directory.path()).unwrap();
    checker
        .prepare_project("tsconfig.json", &[], &queries, TIMEOUT)
        .unwrap();
    let mut fingerprints = Vec::new();
    for query in queries {
        let result = checker
            .resolve_members("tsconfig.json".into(), vec![], vec![query], TIMEOUT)
            .unwrap();
        assert_eq!(result.results[0].answer.declarations.len(), 1);
        fingerprints.push(result.checker_input_fingerprint);
    }
    assert_eq!(fingerprints[0], fingerprints[1]);
    assert!(
        checker
            .validate_project("tsconfig.json".into(), fingerprints[0].clone(), TIMEOUT)
            .unwrap()
            .valid
    );
    assert!(matches!(
        checker.capabilities(Duration::ZERO),
        Err(CheckerError::Timeout(_))
    ));
    assert!(checker.built.is_none());
    assert!(checker.prepared.is_none());
}

#[test]
fn source_edits_new_inputs_and_invalid_spans_are_rejected() {
    let source = "import { A } from './service'; declare const receiver: A; receiver.ping();";
    let directory = fixture(&[
        (
            "tsconfig.json",
            r#"{"compilerOptions":{"noLib":true},"include":["*.ts"]}"#,
        ),
        ("main.ts", source),
        ("service.ts", "export interface A { ping(): void }"),
    ]);
    let query = query(source, "main.ts", "receiver.ping()", "receiver", "ping");
    let mut checker = EmbeddedChecker::new(directory.path()).unwrap();
    let first = checker
        .resolve_members("tsconfig.json".into(), vec![], vec![query.clone()], TIMEOUT)
        .unwrap();
    fs::write(directory.path().join("new.ts"), "export {};").unwrap();
    assert!(
        !checker
            .validate_project(
                "tsconfig.json".into(),
                first.checker_input_fingerprint,
                TIMEOUT
            )
            .unwrap()
            .valid
    );
    let mut bad_span = query.clone();
    bad_span.receiver_start += 1;
    assert!(
        matches!(checker.resolve_members("tsconfig.json".into(), vec![], vec![bad_span], TIMEOUT), Err(CheckerError::Remote { code, .. }) if code == "span_mismatch")
    );
    fs::write(directory.path().join("main.ts"), format!("{source}\n")).unwrap();
    assert!(
        matches!(checker.resolve_members("tsconfig.json".into(), vec![], vec![query], TIMEOUT), Err(CheckerError::Remote { code, .. }) if code == "hash_mismatch")
    );
}

#[test]
fn deadlines_and_panics_discard_sessions() {
    let directory = fixture(&[]);
    let mut checker = EmbeddedChecker::new(directory.path()).unwrap();
    assert!(matches!(
        checker.capabilities(Duration::ZERO),
        Err(CheckerError::Timeout(_))
    ));
    let result: Result<(), CheckerError> =
        checker.request(TIMEOUT, |_, _| panic!("compiler probe failure"));
    assert!(
        matches!(result, Err(CheckerError::Remote { code, message }) if code == "checker_failure" && message.contains("compiler probe failure"))
    );
    assert!(checker.built.is_none());
    assert!(checker.capabilities(TIMEOUT).unwrap().projects.is_empty());
}

#[test]
fn input_observations_detect_missing_file_creation_and_symlink_retargeting() {
    use tsr_vfs::FileSystem;
    let directory = fixture(&[("a.ts", "a"), ("b.ts", "a")]);
    let host = host::InputFs::os(Request::new(TIMEOUT).unwrap());
    let missing = directory.path().join("missing.ts");
    assert!(
        !host
            .file_exists(path_text(&missing).unwrap().as_bytes())
            .unwrap()
    );
    let input = host.inputs().unwrap().pop().unwrap();
    assert_eq!(
        host::fresh_signature(&missing, &input.source_hash).as_deref(),
        Some(input.source_hash.as_str())
    );
    fs::write(&missing, "export {};").unwrap();
    assert_ne!(
        host::fresh_signature(&missing, &input.source_hash).as_deref(),
        Some(input.source_hash.as_str())
    );
    #[cfg(unix)]
    {
        let link = directory.path().join("link.ts");
        std::os::unix::fs::symlink("a.ts", &link).unwrap();
        host.realpath(path_text(&link).unwrap().as_bytes()).unwrap();
        let input = host
            .inputs()
            .unwrap()
            .into_iter()
            .find(|input| input.path == path_text(&link).unwrap())
            .unwrap();
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink("b.ts", &link).unwrap();
        assert_ne!(
            host::fresh_signature(&link, &input.source_hash).as_deref(),
            Some(input.source_hash.as_str())
        );
    }
}

#[test]
fn directory_spelling_variants_share_one_persistable_observation() {
    use tsr_vfs::FileSystem;
    let directory = fixture(&[("main.ts", "export {};")]);
    let path = path_text(directory.path()).unwrap();
    let host = host::InputFs::os(Request::new(TIMEOUT).unwrap());
    host.stat(path.as_bytes()).unwrap();
    host.entries(format!("{path}//./").as_bytes()).unwrap();
    let inputs = host.inputs().unwrap();
    assert_eq!(inputs.len(), 1);
    assert_eq!(
        host::fresh_signature(Path::new(&inputs[0].path), &inputs[0].source_hash),
        Some(inputs[0].source_hash.clone())
    );
}

#[test]
fn directory_observations_ignore_noise_but_retain_compiler_inputs() {
    use tsr_vfs::FileSystem;
    let directory = fixture(&[("main.ts", "export {};")]);
    let host = host::InputFs::os(Request::new(TIMEOUT).unwrap());
    host.entries(path_text(directory.path()).unwrap().as_bytes())
        .unwrap();
    let input = host.inputs().unwrap().pop().unwrap();
    for name in [
        "README.md",
        ".DS_Store",
        "image.png",
        "data.bin",
        "notes.txt",
    ] {
        let path = directory.path().join(name);
        fs::write(&path, "noise").unwrap();
        assert_eq!(
            host::fresh_signature(directory.path(), &input.source_hash),
            Some(input.source_hash.clone()),
            "{name} invalidated a directory observation"
        );
        #[cfg(unix)]
        {
            let link = directory.path().join(format!("link-{name}"));
            std::os::unix::fs::symlink(&path, &link).unwrap();
            assert_eq!(
                host::fresh_signature(directory.path(), &input.source_hash),
                Some(input.source_hash.clone()),
                "irrelevant symlink invalidated a directory observation"
            );
            fs::remove_file(link).unwrap();
        }
        fs::remove_file(path).unwrap();
    }
    for name in [
        "new.ts",
        "new.tsx",
        "new.mts",
        "new.cts",
        "new.d.ts",
        "new.js",
        "new.jsx",
        "new.mjs",
        "new.cjs",
        "data.json",
        "package.json",
    ] {
        let path = directory.path().join(name);
        fs::write(&path, "").unwrap();
        assert_ne!(
            host::fresh_signature(directory.path(), &input.source_hash),
            Some(input.source_hash.clone()),
            "{name} must invalidate directory membership"
        );
        fs::remove_file(path).unwrap();
    }
    let nested = directory.path().join("new-directory");
    fs::create_dir(&nested).unwrap();
    assert_ne!(
        host::fresh_signature(directory.path(), &input.source_hash),
        Some(input.source_hash.clone())
    );
    fs::remove_dir(nested).unwrap();
    // Explicit reads remain inputs even for nonstandard file names.
    let explicit = directory.path().join("compiler-config.txt");
    fs::write(&explicit, "before").unwrap();
    host.read_file(path_text(&explicit).unwrap().as_bytes())
        .unwrap();
    let observed = host
        .inputs()
        .unwrap()
        .into_iter()
        .find(|input| input.path == path_text(&explicit).unwrap())
        .unwrap();
    fs::write(&explicit, "after").unwrap();
    assert_ne!(
        host::fresh_signature(&explicit, &observed.source_hash),
        Some(observed.source_hash)
    );
}
