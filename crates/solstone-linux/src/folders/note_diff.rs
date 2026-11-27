// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Net-change computation for tracked notes inside a five-minute window.
//! First sight sends the whole note; subsequent edits send only the net line
//! diff against the baseline established before the window.

/// Compute a compact net line diff between `before` and `after`.
/// Returns `None` when the text is unchanged.
#[must_use]
pub fn net_line_diff(before: &str, after: &str) -> Option<String> {
    if before == after {
        return None;
    }
    let old_lines: Vec<&str> = before.lines().collect();
    let new_lines: Vec<&str> = after.lines().collect();

    // Trim matching prefix and suffix so common single-paragraph edits stay O(k).
    let mut prefix = 0usize;
    while prefix < old_lines.len()
        && prefix < new_lines.len()
        && old_lines[prefix] == new_lines[prefix]
    {
        prefix += 1;
    }
    let mut old_end = old_lines.len();
    let mut new_end = new_lines.len();
    while old_end > prefix && new_end > prefix && old_lines[old_end - 1] == new_lines[new_end - 1] {
        old_end -= 1;
        new_end -= 1;
    }

    let old_mid = &old_lines[prefix..old_end];
    let new_mid = &new_lines[prefix..new_end];

    let mut out = String::new();
    // Use DP LCS on bounded middle ranges; fall back to direct -/+ block for
    // huge rewrites so memory stays bounded.
    if old_mid.len().saturating_mul(new_mid.len()) <= 64_000 {
        let m = old_mid.len();
        let n = new_mid.len();
        let mut dp = vec![0u32; (m + 1) * (n + 1)];
        let idx = |i: usize, j: usize| i * (n + 1) + j;
        for i in (0..m).rev() {
            for j in (0..n).rev() {
                dp[idx(i, j)] = if old_mid[i] == new_mid[j] {
                    dp[idx(i + 1, j + 1)] + 1
                } else {
                    dp[idx(i + 1, j)].max(dp[idx(i, j + 1)])
                };
            }
        }
        let (mut i, mut j) = (0usize, 0usize);
        while i < m && j < n {
            if old_mid[i] == new_mid[j] {
                i += 1;
                j += 1;
            } else if dp[idx(i + 1, j)] >= dp[idx(i, j + 1)] {
                out.push_str("- ");
                out.push_str(old_mid[i]);
                out.push('\n');
                i += 1;
            } else {
                out.push_str("+ ");
                out.push_str(new_mid[j]);
                out.push('\n');
                j += 1;
            }
        }
        while i < m {
            out.push_str("- ");
            out.push_str(old_mid[i]);
            out.push('\n');
            i += 1;
        }
        while j < n {
            out.push_str("+ ");
            out.push_str(new_mid[j]);
            out.push('\n');
            j += 1;
        }
    } else {
        for line in old_mid {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
        for line in new_mid {
            out.push_str("+ ");
            out.push_str(line);
            out.push('\n');
        }
    }

    if out.is_empty() {
        // Only trailing-newline difference.
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unchanged_or_reverted_edit_produces_none() {
        assert_eq!(net_line_diff("a\nb\n", "a\nb\n"), None);
    }

    #[test]
    fn added_and_removed_lines_appear_cleanly() {
        let before = "first\nsecond\nthird\n";
        let after = "first\nsecond revised\nthird\nfourth\n";
        let diff = net_line_diff(before, after).unwrap();
        assert!(diff.contains("- second\n"));
        assert!(diff.contains("+ second revised\n"));
        assert!(diff.contains("+ fourth\n"));
        assert!(!diff.contains("first"));
        assert!(!diff.contains("third"));
    }
}
