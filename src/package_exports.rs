//! Shared package.json `exports` condition selection.

/// Export conditions enabled by the module resolver. Declaration order in a
/// condition object is semantically significant; the first active condition
/// commits the branch without backtracking.
pub(crate) const RESOLVE_CONDITIONS: &[&str] = &["import", "require", "node", "default"];

/// Collect target strings from one export value using the resolver's active
/// conditions. Arrays preserve fallback order; inactive-only objects and
/// `null` produce no targets. Requires `serde_json`'s `preserve_order` feature.
pub(crate) fn collect_active_targets(value: &serde_json::Value, out: &mut Vec<String>) {
    match value {
        serde_json::Value::String(value) => out.push(value.clone()),
        serde_json::Value::Array(values) => {
            for value in values {
                collect_active_targets(value, out);
            }
        }
        serde_json::Value::Object(map) => {
            for (condition, value) in map {
                if RESOLVE_CONDITIONS.contains(&condition.as_str()) {
                    collect_active_targets(value, out);
                    return;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::collect_active_targets;

    #[test]
    fn active_condition_commits_in_declaration_order() {
        let value = serde_json::json!({
            "default": "./fallback.js",
            "import": "./module.js"
        });
        let mut targets = Vec::new();
        collect_active_targets(&value, &mut targets);
        assert_eq!(targets, ["./fallback.js"]);
    }
}
