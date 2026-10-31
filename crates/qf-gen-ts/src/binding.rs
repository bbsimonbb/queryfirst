//! How a generated file binds a query to a runtime.

use qf_meta::QueryMeta;

pub trait RuntimeBinding {
    /// Import lines this query needs besides pgtyped's `PreparedQuery` import, which the file
    /// assembler always emits first. Each line has no trailing newline.
    fn imports(&self, meta: &QueryMeta) -> Vec<String>;

    /// The `export const` statement for the query, ending in `;\n`.
    fn query_export(&self, meta: &QueryMeta, params_alias: &str, result_alias: &str) -> String;
}
