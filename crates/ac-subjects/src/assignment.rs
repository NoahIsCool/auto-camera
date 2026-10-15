//! Optimal one-to-one assignment (the Hungarian method).
//!
//! Association is a bipartite matching problem: detections on one side,
//! subjects on the other, an affinity for each pair and a gate below which a
//! pair is not allowed at all. Greedy "best IoU first" matching swaps
//! identities when two subjects cross, because the locally best pair can force
//! a globally worse one. The Hungarian method finds the assignment with the
//! minimum total cost in `O(n² m)` time for `n ≤ m`.
//!
//! [`match_pairs`] solves the *gated* problem: it maximises the number of
//! feasible pairs first and minimises their total cost second. It does so by
//! giving forbidden pairs a cost larger than any difference between feasible
//! assignments, then discarding forbidden pairs from the result.
//!
//! The solver is deterministic: equal-cost alternatives resolve the same way
//! for the same input order.

/// Minimum-cost assignment of every row of a `rows × cols` matrix with
/// `rows <= cols`, using the shortest-augmenting-path form with potentials.
///
/// `cost(i, j)` must be finite. Returns the column assigned to each row.
fn solve(rows: usize, cols: usize, cost: &dyn Fn(usize, usize) -> f64) -> Vec<usize> {
    debug_assert!(rows <= cols);
    // 1-based indices with a virtual column 0, as in the classic formulation.
    let mut u = vec![0.0; rows + 1];
    let mut v = vec![0.0; cols + 1];
    // owner[j] is the row assigned to column j (0 = none).
    let mut owner = vec![0usize; cols + 1];
    let mut way = vec![0usize; cols + 1];
    for i in 1..=rows {
        owner[0] = i;
        let mut j0 = 0;
        let mut minv = vec![f64::INFINITY; cols + 1];
        let mut used = vec![false; cols + 1];
        loop {
            used[j0] = true;
            let i0 = owner[j0];
            let mut delta = f64::INFINITY;
            let mut j1 = 0;
            for j in 1..=cols {
                if used[j] {
                    continue;
                }
                let reduced = cost(i0 - 1, j - 1) - u[i0] - v[j];
                if reduced < minv[j] {
                    minv[j] = reduced;
                    way[j] = j0;
                }
                if minv[j] < delta {
                    delta = minv[j];
                    j1 = j;
                }
            }
            for j in 0..=cols {
                if used[j] {
                    u[owner[j]] += delta;
                    v[j] -= delta;
                } else {
                    minv[j] -= delta;
                }
            }
            j0 = j1;
            if owner[j0] == 0 {
                break;
            }
        }
        loop {
            let j1 = way[j0];
            owner[j0] = owner[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut assigned = vec![0; rows];
    for j in 1..=cols {
        if owner[j] != 0 {
            assigned[owner[j] - 1] = j - 1;
        }
    }
    assigned
}

/// Gated optimal matching.
///
/// `cost(row, col)` returns `None` for a forbidden pair and `Some(c)` with a
/// finite `c` otherwise; non-finite costs are treated as forbidden. The result
/// has the largest possible number of allowed pairs and, among those, the
/// smallest total cost. Pairs are returned sorted by row.
pub(crate) fn match_pairs(
    rows: usize,
    cols: usize,
    cost: impl Fn(usize, usize) -> Option<f64>,
) -> Vec<(usize, usize)> {
    if rows == 0 || cols == 0 {
        return Vec::new();
    }
    let allowed = |i: usize, j: usize| cost(i, j).filter(|c| c.is_finite());
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for i in 0..rows {
        for j in 0..cols {
            if let Some(c) = allowed(i, j) {
                lo = lo.min(c);
                hi = hi.max(c);
            }
        }
    }
    if lo > hi {
        return Vec::new();
    }
    // Any assignment with one more forbidden pair costs more than every
    // assignment with fewer: k pairs can differ by at most k * (hi - lo).
    let k = rows.min(cols) as f64;
    let forbidden = hi + (k + 1.0) * (hi - lo + 1.0);
    let full = |i: usize, j: usize| allowed(i, j).unwrap_or(forbidden);
    let mut pairs: Vec<(usize, usize)> = if rows <= cols {
        solve(rows, cols, &full).into_iter().enumerate().collect()
    } else {
        solve(cols, rows, &|j, i| full(i, j))
            .into_iter()
            .enumerate()
            .map(|(j, i)| (i, j))
            .collect()
    };
    pairs.retain(|&(i, j)| allowed(i, j).is_some());
    pairs.sort_unstable();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// Exhaustive reference: best (pair count, -cost) over all injective maps.
    fn brute(rows: usize, cols: usize, m: &[Option<f64>]) -> (usize, f64) {
        fn go(
            i: usize,
            rows: usize,
            cols: usize,
            m: &[Option<f64>],
            used: &mut Vec<bool>,
            acc: (usize, f64),
            best: &mut (usize, f64),
        ) {
            if i == rows {
                if acc.0 > best.0 || (acc.0 == best.0 && acc.1 < best.1) {
                    *best = acc;
                }
                return;
            }
            go(i + 1, rows, cols, m, used, acc, best);
            for j in 0..cols {
                if let (false, Some(c)) = (used[j], m[i * cols + j]) {
                    used[j] = true;
                    go(i + 1, rows, cols, m, used, (acc.0 + 1, acc.1 + c), best);
                    used[j] = false;
                }
            }
        }
        let mut best = (0, 0.0);
        go(
            0,
            rows,
            cols,
            m,
            &mut vec![false; cols],
            (0, 0.0),
            &mut best,
        );
        best
    }

    #[test]
    fn empty_inputs_match_nothing() {
        assert!(match_pairs(0, 3, |_, _| Some(0.0)).is_empty());
        assert!(match_pairs(3, 0, |_, _| Some(0.0)).is_empty());
        assert!(match_pairs(2, 2, |_, _| None).is_empty());
    }

    #[test]
    fn global_optimum_beats_greedy() {
        // Greedy would take (0, 0) at cost 0.1 and then be forced into (1, 1)
        // at cost 0.9; the optimum is (0, 1) + (1, 0) at 0.4 total.
        let m = [[0.1, 0.2], [0.2, 0.9]];
        assert_eq!(
            match_pairs(2, 2, |i, j| Some(m[i][j])),
            vec![(0, 1), (1, 0)]
        );
    }

    #[test]
    fn more_pairs_win_over_cheaper_pairs() {
        // Row 1 can only take column 0; taking it lets both rows match.
        let m = [[Some(0.0), Some(0.5)], [Some(0.9), None]];
        assert_eq!(match_pairs(2, 2, |i, j| m[i][j]), vec![(0, 1), (1, 0)]);
    }

    #[test]
    fn rectangular_and_non_finite_inputs() {
        let tall = [[0.5], [0.1], [0.3]];
        assert_eq!(match_pairs(3, 1, |i, j| Some(tall[i][j])), vec![(1, 0)]);
        let wide = [[0.5, f64::NAN, 0.2]];
        assert_eq!(match_pairs(1, 3, |i, j| Some(wide[i][j])), vec![(0, 2)]);
    }

    proptest! {
        #[test]
        fn matches_exhaustive_search(
            rows in 1usize..5,
            cols in 1usize..5,
            raw in proptest::collection::vec(proptest::option::weighted(0.7, 0.0f64..2.0), 16),
        ) {
            let m = &raw[..rows * cols];
            let pairs = match_pairs(rows, cols, |i, j| m[i * cols + j]);
            let mut seen_rows = std::collections::HashSet::new();
            let mut seen_cols = std::collections::HashSet::new();
            let mut total = 0.0;
            for &(i, j) in &pairs {
                prop_assert!(seen_rows.insert(i) && seen_cols.insert(j));
                total += m[i * cols + j].expect("only allowed pairs are returned");
            }
            let (count, cost) = brute(rows, cols, m);
            prop_assert_eq!(pairs.len(), count);
            prop_assert!((total - cost).abs() < 1e-9, "{} vs {}", total, cost);
        }
    }
}
