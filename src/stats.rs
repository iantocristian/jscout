use std::path::Path;

use oxc_ast::ast::*;
use oxc_ast_visit::Visit;

#[derive(Default, Debug)]
pub struct FileStats {
    pub functions: usize,
    pub arrow_functions: usize,
    pub classes: usize,
    pub methods: usize,
    pub jsx_components_defined: usize,
    pub imports: usize,
    pub exports: usize,
    pub type_only_nodes: usize,
}

struct StatsVisitor {
    stats: FileStats,
    is_jsx_file: bool,
}

impl<'a> Visit<'a> for StatsVisitor {
    fn visit_function(&mut self, func: &Function<'a>, flags: oxc_syntax::scope::ScopeFlags) {
        self.stats.functions += 1;
        if self.is_jsx_file
            && let Some(id) = &func.id
            && id
                .name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
        {
            self.stats.jsx_components_defined += 1;
        }
        oxc_ast_visit::walk::walk_function(self, func, flags);
    }

    fn visit_arrow_function_expression(&mut self, arrow: &ArrowFunctionExpression<'a>) {
        self.stats.arrow_functions += 1;
        oxc_ast_visit::walk::walk_arrow_function_expression(self, arrow);
    }

    fn visit_class(&mut self, class: &Class<'a>) {
        self.stats.classes += 1;
        oxc_ast_visit::walk::walk_class(self, class);
    }

    fn visit_method_definition(&mut self, def: &MethodDefinition<'a>) {
        self.stats.methods += 1;
        oxc_ast_visit::walk::walk_method_definition(self, def);
    }

    fn visit_ts_interface_declaration(&mut self, decl: &TSInterfaceDeclaration<'a>) {
        self.stats.type_only_nodes += 1;
        oxc_ast_visit::walk::walk_ts_interface_declaration(self, decl);
    }

    fn visit_ts_type_alias_declaration(&mut self, decl: &TSTypeAliasDeclaration<'a>) {
        self.stats.type_only_nodes += 1;
        oxc_ast_visit::walk::walk_ts_type_alias_declaration(self, decl);
    }
}

pub fn file_stats(path: &Path, source: &str) -> anyhow::Result<FileStats> {
    crate::parse::with_parsed(source, path, |ret, _semantic| {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mut v = StatsVisitor {
            stats: FileStats::default(),
            is_jsx_file: matches!(ext, "jsx" | "tsx"),
        };
        v.visit_program(&ret.program);
        v.stats.imports = ret.module_record.import_entries.len();
        v.stats.exports = ret.module_record.local_export_entries.len()
            + ret.module_record.indirect_export_entries.len()
            + ret.module_record.star_export_entries.len();
        v.stats
    })
}
