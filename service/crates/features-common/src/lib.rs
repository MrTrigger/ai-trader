//! Instrument-agnostic feature-matrix operations shared by training and live
//! inference. Asset/session feature definitions remain in `features-*` crates.

/// Rank-normalise a point-in-time cross-section: average ascending rank mapped
/// to `[-1, 1]`, missing/non-finite values to zero.
///
/// This transformation is model input semantics. Python trainers consume its
/// output and may not reproduce or modify it.
pub fn rank_normalise(values: &[Vec<Option<f64>>]) -> Result<Vec<Vec<f64>>, String> {
    if values.is_empty() {
        return Ok(Vec::new());
    }
    let width = values[0].len();
    if values.iter().any(|row| row.len() != width) {
        return Err("ragged feature matrix".into());
    }
    let denominator = values.len().saturating_sub(1).max(1) as f64;
    let mut out = vec![vec![0.0; width]; values.len()];
    for column in 0..width {
        let mut measured: Vec<(usize, f64)> = values
            .iter()
            .enumerate()
            .filter_map(|(i, row)| row[column].filter(|v| v.is_finite()).map(|v| (i, v)))
            .collect();
        measured.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut start = 0;
        while start < measured.len() {
            let mut end = start + 1;
            while end < measured.len() && measured[end].1 == measured[start].1 {
                end += 1;
            }
            let rank = ((start + 1) as f64 + end as f64) / 2.0;
            let normalised = 2.0 * (rank - 1.0) / denominator - 1.0;
            for (row, _) in &measured[start..end] {
                out[*row][column] = normalised;
            }
            start = end;
        }
    }
    Ok(out)
}

/// Spearman rank correlation with average ranks for ties. This is shared
/// diagnostics math, not a strategy feature: callers supply the causal
/// cross-section and target whose ordering they want to audit.
pub fn spearman(left: &[f64], right: &[f64]) -> Option<f64> {
    if left.len() != right.len()
        || left.len() < 3
        || left.iter().chain(right).any(|value| !value.is_finite())
    {
        return None;
    }
    pearson(&average_ranks(left), &average_ranks(right))
}

fn average_ranks(values: &[f64]) -> Vec<f64> {
    let mut order = (0..values.len()).collect::<Vec<_>>();
    order.sort_by(|left, right| {
        values[*left]
            .total_cmp(&values[*right])
            .then_with(|| left.cmp(right))
    });
    let mut ranks = vec![0.0; values.len()];
    let mut start = 0;
    while start < order.len() {
        let mut end = start + 1;
        while end < order.len() && values[order[end]] == values[order[start]] {
            end += 1;
        }
        let rank = (start + end - 1) as f64 / 2.0;
        for index in &order[start..end] {
            ranks[*index] = rank;
        }
        start = end;
    }
    ranks
}

fn pearson(left: &[f64], right: &[f64]) -> Option<f64> {
    let n = left.len() as f64;
    let left_mean = left.iter().sum::<f64>() / n;
    let right_mean = right.iter().sum::<f64>() / n;
    let covariance = left
        .iter()
        .zip(right)
        .map(|(a, b)| (a - left_mean) * (b - right_mean))
        .sum::<f64>();
    let left_variance = left
        .iter()
        .map(|value| (value - left_mean).powi(2))
        .sum::<f64>();
    let right_variance = right
        .iter()
        .map(|value| (value - right_mean).powi(2))
        .sum::<f64>();
    let denominator = (left_variance * right_variance).sqrt();
    (denominator > 0.0).then_some(covariance / denominator)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ties_average_and_nulls_are_neutral() {
        let out = rank_normalise(&[
            vec![Some(10.0), None],
            vec![Some(20.0), Some(5.0)],
            vec![Some(20.0), Some(9.0)],
        ])
        .unwrap();
        assert_eq!(out[0], vec![-1.0, 0.0]);
        assert_eq!(out[1], vec![0.5, -1.0]);
        assert_eq!(out[2], vec![0.5, 0.0]);
    }

    #[test]
    fn refuses_ragged_rows() {
        assert!(rank_normalise(&[vec![Some(1.0)], vec![]]).is_err());
    }

    #[test]
    fn spearman_uses_average_ranks_and_rejects_constant_inputs() {
        assert_eq!(spearman(&[1.0, 2.0, 3.0], &[30.0, 20.0, 10.0]), Some(-1.0));
        assert!(spearman(&[1.0, 1.0, 1.0], &[1.0, 2.0, 3.0]).is_none());
        assert!(spearman(&[1.0, f64::NAN, 3.0], &[1.0, 2.0, 3.0]).is_none());
    }
}
