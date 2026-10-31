use std::path::Path;

/// Split into words on non-alphanumerics and case boundaries (npm `camel-case` semantics).
fn words(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(&prev) = i.checked_sub(1).and_then(|j| chars.get(j)) {
            let lower_to_upper = (prev.is_lowercase() || prev.is_ascii_digit()) && c.is_uppercase();
            let acronym_end = prev.is_uppercase()
                && c.is_uppercase()
                && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if (lower_to_upper || acronym_end) && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub(crate) fn camel_case(s: &str) -> String {
    let mut out = String::new();
    for (i, w) in words(s).iter().enumerate() {
        let lower = w.to_lowercase();
        if i == 0 {
            out.push_str(&lower);
        } else {
            let mut cs = lower.chars();
            if let Some(f) = cs.next() {
                out.extend(f.to_uppercase());
                out.push_str(cs.as_str());
            }
        }
    }
    out
}

/// Derive a query name from a file path: `get-accounts.psql` -> `getAccounts`.
pub fn name_from_path(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    camel_case(stem)
}
