/// Maps character positions in [`crate::LexedQuery::db_statement`] back to the source file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SourceMap {
    /// Source (line, col), both 1-based, for each char of the DB statement, then one entry for
    /// the position just past its end.
    pub(crate) points: Vec<(u32, u32)>,
}

impl SourceMap {
    /// Map a Postgres error position (1-based, in characters) to the source (line, col).
    /// Chars of a `$n` placeholder (or an expanded tuple) map to the `:` of the `:param`.
    /// Out-of-range positions clamp to the end of the statement.
    pub fn db_char_pos_to_source(&self, pos_1based_chars: u32) -> (u32, u32) {
        let i = pos_1based_chars.saturating_sub(1) as usize;
        self.points
            .get(i)
            .or(self.points.last())
            .copied()
            .unwrap_or((1, 1))
    }
}
