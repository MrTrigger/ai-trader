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
}
