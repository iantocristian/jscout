//! Literal, repository-relative primary-hit scope shared by both search corpora.

use anyhow::{Result, ensure};
use serde::Serialize;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub(crate) struct PathScope {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
}

impl PathScope {
    pub fn new(path: Option<String>, path_prefix: Option<String>) -> Result<Self> {
        Ok(Self {
            path: path.map(|value| normalize(&value, false)).transpose()?,
            path_prefix: path_prefix
                .map(|value| normalize(&value, true))
                .transpose()?,
        })
    }

    pub fn normalized(&self) -> Result<Self> {
        Self::new(self.path.clone(), self.path_prefix.clone())
    }

    pub fn is_empty(&self) -> bool {
        self.path.is_none() && self.path_prefix.is_none()
    }

    /// Bind `path`, then `path_prefix`, at the supplied parameter position.
    /// Substring equality treats `%` and `_` literally, unlike SQL LIKE.
    pub fn sql(&self, column: &str, first_parameter: usize) -> String {
        let next = first_parameter + 1;
        format!(
            "(?{first_parameter} IS NULL OR {column}=?{first_parameter}) \
             AND (?{next} IS NULL OR substr({column},1,length(?{next}))=?{next})"
        )
    }

    #[cfg(test)]
    pub fn matches(&self, path: &str) -> bool {
        self.path.as_ref().is_none_or(|exact| path == exact)
            && self
                .path_prefix
                .as_ref()
                .is_none_or(|prefix| path.starts_with(prefix))
    }
}

fn normalize(value: &str, prefix: bool) -> Result<String> {
    let field = if prefix { "path_prefix" } else { "path" };
    ensure!(
        !value.starts_with('/')
            && !value.contains('\\')
            && !value.contains('\0')
            && !(value.as_bytes().get(1) == Some(&b':')
                && value.as_bytes()[0].is_ascii_alphabetic()),
        "{field} must be a repository-relative path using / separators"
    );
    let mut parts = Vec::new();
    for part in value.split('/') {
        ensure!(
            part != "..",
            "{field} must not contain parent traversal (..)"
        );
        if !part.is_empty() && part != "." {
            parts.push(part);
        }
    }
    ensure!(
        !parts.is_empty(),
        "{field} must name a file or directory within the repository"
    );
    let mut normalized = parts.join("/");
    if prefix {
        normalized.push('/');
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_is_canonical_literal_and_component_bounded() {
        let scope = PathScope::new(None, Some("./src//nested/".into())).unwrap();
        assert_eq!(scope.path_prefix.as_deref(), Some("src/nested/"));
        assert!(scope.matches("src/nested/file.ts"));
        assert!(!scope.matches("src/nested-sibling/file.ts"));
        assert_eq!(scope, scope.normalized().unwrap());
        let scope = PathScope::new(Some("src/a_%.ts".into()), Some("src".into())).unwrap();
        assert!(scope.matches("src/a_%.ts"));
        assert!(!scope.matches("src/abc.ts"));
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let sql = format!("SELECT {}", scope.sql("?3", 1));
        for path in ["src/a_%.ts", "src/abc.ts", "other/a_%.ts"] {
            let actual: bool = conn
                .query_row(
                    &sql,
                    rusqlite::params![scope.path, scope.path_prefix, path],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(actual, scope.matches(path));
        }
    }

    #[test]
    fn scope_rejects_ambiguous_or_external_paths() {
        for value in [
            "",
            ".",
            "/src",
            "../src",
            "src/../other",
            "C:/src",
            "src\\file",
            "src\0file",
        ] {
            assert!(
                PathScope::new(Some(value.into()), None).is_err(),
                "{value:?}"
            );
            assert!(
                PathScope::new(None, Some(value.into())).is_err(),
                "{value:?}"
            );
        }
    }
}
