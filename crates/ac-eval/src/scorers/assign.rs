//! Least-cost one-to-one assignment (the Hungarian method).
//!
//! Both kinds of identity matching need an optimal assignment: a keyframe's
//! labelled people to the subjects found there (most matches, then most
//! overlap), and IDF1's people to subjects over the whole clip (most frames
//! in common). Greedy matching is not optimal for either, and a greedy
//! IDF1 can come out lower than the true one.
//!
//! [`minimize`] is the Kuhn–Munkres algorithm with row and column
//! potentials (Jonker–Volgenant's shortest augmenting paths), `O(n² m)` for
//! `n` rows and `m >= n` columns; a matrix with more rows than columns is
//! solved transposed. Pairs a caller must not make are given a cost larger
//! than the sum of every allowed one ([`FORBIDDEN`] for IoU costs), so the
//! least total first makes the fewest forbidden pairs, which the caller
//! then drops: that is a maximum matching of allowed pairs, and the least
//! costly among those.
//!
//! # Invariants
//!
//! * Every cost is finite (the caller's duty; NaN would break the
//!   comparisons). The result pairs each row of the shorter side exactly
//!   once, sorted by row, with no column twice.
//! * The result is a function of the matrix: equal inputs give equal
//!   pairs, whatever the ties.

/// A cost for a pair that must not be made, far above the sum of any
/// matrix of costs in `0..=1` the scorers build.
pub(crate) const FORBIDDEN: f64 = 1e9;

/// The pairs `(row, column)` of least total cost in `cost`, a rectangular
/// matrix (every row the same length); one pair per row when there are no
/// more rows than columns, else one per column. Sorted by row.
pub(crate) fn minimize(cost: &[Vec<f64>]) -> Vec<(usize, usize)> {
    let rows = cost.len();
    let cols = cost.first().map_or(0, Vec::len);
    if rows == 0 || cols == 0 {
        return Vec::new();
    }
    if rows > cols {
        let transposed: Vec<Vec<f64>> = (0..cols)
            .map(|c| (0..rows).map(|r| cost[r][c]).collect())
            .collect();
        let mut pairs: Vec<(usize, usize)> = minimize(&transposed)
            .into_iter()
            .map(|(c, r)| (r, c))
            .collect();
        pairs.sort_unstable();
        return pairs;
    }
    // 1-based arrays, index 0 being the virtual start column of each
    // augmenting path. `owner[j]` is the row assigned to column `j` (0 for
    // none), `u` and `v` the row and column potentials.
    let mut u = vec![0.0; rows + 1];
    let mut v = vec![0.0; cols + 1];
    let mut owner = vec![0usize; cols + 1];
    let mut way = vec![0usize; cols + 1];
    for row in 1..=rows {
        owner[0] = row;
        let mut j0 = 0;
        let mut min_to = vec![f64::INFINITY; cols + 1];
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
                let reduced = cost[i0 - 1][j - 1] - u[i0] - v[j];
                if reduced < min_to[j] {
                    min_to[j] = reduced;
                    way[j] = j0;
                }
                if min_to[j] < delta {
                    delta = min_to[j];
                    j1 = j;
                }
            }
            for j in 0..=cols {
                if used[j] {
                    u[owner[j]] += delta;
                    v[j] -= delta;
                } else {
                    min_to[j] -= delta;
                }
            }
            j0 = j1;
            if owner[j0] == 0 {
                break;
            }
        }
        // Flip the augmenting path back to its start.
        loop {
            let j1 = way[j0];
            owner[j0] = owner[j1];
            j0 = j1;
            if j0 == 0 {
                break;
            }
        }
    }
    let mut pairs: Vec<(usize, usize)> = (1..=cols)
        .filter(|&j| owner[j] != 0)
        .map(|j| (owner[j] - 1, j - 1))
        .collect();
    pairs.sort_unstable();
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The total cost of `pairs` in `cost`.
    fn total(cost: &[Vec<f64>], pairs: &[(usize, usize)]) -> f64 {
        pairs.iter().map(|&(r, c)| cost[r][c]).sum()
    }

    #[test]
    fn finds_the_optimum_that_greedy_misses() {
        // Greedy takes the cheapest cell, (0, 0) at 1, and must then pay 10
        // for (1, 1): 11. The optimum crosses over: (0, 1) + (1, 0) = 2 + 3.
        let cost = vec![vec![1.0, 2.0], vec![3.0, 10.0]];
        let pairs = minimize(&cost);
        assert_eq!(pairs, [(0, 1), (1, 0)]);
        assert_eq!(total(&cost, &pairs), 5.0);
    }

    #[test]
    fn solves_rectangles_either_way_round() {
        // Two rows, three columns: rows take columns 2 and 0 (1 + 1).
        let wide = vec![vec![5.0, 4.0, 1.0], vec![1.0, 6.0, 7.0]];
        assert_eq!(minimize(&wide), [(0, 2), (1, 0)]);
        // The same transposed: every column gets a row, sorted by row.
        let tall = vec![vec![5.0, 1.0], vec![4.0, 6.0], vec![1.0, 7.0]];
        assert_eq!(minimize(&tall), [(0, 1), (2, 0)]);
    }

    #[test]
    fn forbidden_pairs_lose_to_any_allowed_matching() {
        // Row 0 may only take column 0 and row 1 only column 1; the cheap
        // forbidden-free crossing does not exist, so both allowed pairs win.
        let cost = vec![vec![0.4, FORBIDDEN], vec![FORBIDDEN, 0.2]];
        assert_eq!(minimize(&cost), [(0, 0), (1, 1)]);
        // With one row able to match nothing, the matching still covers
        // every row; the caller drops the forbidden pair.
        let cost = vec![vec![FORBIDDEN, FORBIDDEN], vec![0.3, FORBIDDEN]];
        let pairs = minimize(&cost);
        assert!(pairs.contains(&(1, 0)));
        assert_eq!(
            pairs
                .iter()
                .filter(|&&(r, c)| cost[r][c] < FORBIDDEN)
                .count(),
            1
        );
    }

    #[test]
    fn empty_matrices_have_no_pairs() {
        assert!(minimize(&[]).is_empty());
        assert!(minimize(&[vec![], vec![]]).is_empty());
    }
}
