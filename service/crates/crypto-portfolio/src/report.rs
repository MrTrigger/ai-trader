//! Self-contained HTML lens over a previously computed research record.

use serde_json::Value;

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn number(value: Option<&Value>) -> Option<f64> {
    value.and_then(|value| {
        value
            .as_f64()
            .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
    })
}

fn metric(metrics: &Value, name: &str) -> String {
    match number(metrics.get(name)) {
        Some(value) if name == "total_return" || name == "max_drawdown" => {
            format!("{:+.2}%", value * 100.0)
        }
        Some(value) => format!("{value:.2}"),
        None => "—".into(),
    }
}

fn nav_svg(runs: &[Value]) -> String {
    let series = runs
        .iter()
        .filter_map(|run| {
            let values = run.get("nav")?.as_array()?;
            let points = values
                .iter()
                .filter_map(|point| {
                    point
                        .get("value")
                        .and_then(|value| number(Some(value)))
                        .or_else(|| point.as_array().and_then(|pair| number(pair.get(1))))
                })
                .collect::<Vec<_>>();
            (!points.is_empty()).then(|| {
                (
                    run.get("label").and_then(Value::as_str).unwrap_or("run"),
                    points,
                )
            })
        })
        .collect::<Vec<_>>();
    if series.is_empty() {
        return "<p>No NAV series in this record.</p>".into();
    }
    let min = series
        .iter()
        .flat_map(|(_, values)| values)
        .copied()
        .fold(f64::INFINITY, f64::min);
    let max = series
        .iter()
        .flat_map(|(_, values)| values)
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let span = (max - min).max(1e-12);
    let colors = ["#2dd4bf", "#f59e0b", "#94a3b8", "#e879f9"];
    let mut svg = String::from(
        "<svg class=chart viewBox=\"0 0 1000 320\" role=img aria-label=\"NAV series\"><path d=\"M40 10V285H990\" class=axis />",
    );
    for (series_index, (label, values)) in series.iter().enumerate() {
        let denominator = values.len().saturating_sub(1).max(1) as f64;
        let points = values
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let x = 40.0 + 950.0 * index as f64 / denominator;
                let y = 285.0 - 265.0 * (*value - min) / span;
                format!("{x:.1},{y:.1}")
            })
            .collect::<Vec<_>>()
            .join(" ");
        let color = colors[series_index % colors.len()];
        svg.push_str(&format!(
            "<polyline points=\"{points}\" style=\"stroke:{color}\"/><text x=\"{}\" y=\"310\" style=\"fill:{color}\">{}</text>",
            45 + series_index * 180,
            escape(label)
        ));
    }
    svg.push_str("</svg>");
    svg
}

pub fn render(record: &Value) -> Result<String, String> {
    let disclosures = record
        .get("disclosures")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let runs = record
        .get("runs")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let window = record
        .get("window")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" → ")
        })
        .unwrap_or_else(|| "unstated window".into());
    let mut html = format!(
        "<!doctype html><html lang=en><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\"><title>Crypto research evidence</title><style>{}</style><body><main><header><p class=kicker>RUST RESEARCH RECORD · {}</p><h1>Crypto portfolio evidence</h1><p>This page is a lens over a saved record. It computes no features, decisions, or statistics.</p></header><section class=warning><h2>Disclosures — read before any number</h2>",
        "body{margin:0;background:#081018;color:#dbe7f3;font:15px system-ui,sans-serif}main{max-width:1180px;margin:auto;padding:40px 24px}h1{font-size:42px;margin:.2em 0}h2{margin-top:0}.kicker{color:#2dd4bf;letter-spacing:.12em}.warning{background:#2a1710;border:1px solid #f59e0b;padding:20px;margin:28px 0}.card{background:#101c28;border:1px solid #263747;padding:20px;margin:20px 0;overflow:auto}table{width:100%;border-collapse:collapse}th,td{text-align:right;padding:10px;border-bottom:1px solid #263747}th:first-child,td:first-child{text-align:left}.chart{width:100%;min-width:620px}.chart polyline{fill:none;stroke-width:2}.axis{fill:none;stroke:#536579;stroke-width:1}pre{white-space:pre-wrap;word-break:break-word;color:#9fb2c5}code{font-family:ui-monospace,monospace}",
        escape(&window)
    );
    if disclosures.is_empty() {
        html.push_str(
            "<p>No top-level disclosures were recorded. Treat every result as incomplete.</p>",
        );
    } else {
        html.push_str("<ul>");
        for disclosure in disclosures {
            html.push_str(&format!(
                "<li>{}</li>",
                escape(disclosure.as_str().unwrap_or("non-text disclosure"))
            ));
        }
        html.push_str("</ul>");
    }
    html.push_str("</section><section class=card><h2>Candidate replays</h2><table><thead><tr><th>run</th><th>n</th><th>return</th><th>Sharpe</th><th>drawdown</th><th>2× return</th></tr></thead><tbody>");
    for run in &runs {
        let metrics = run.get("metrics").unwrap_or(&Value::Null);
        let stressed = run.get("stressed").unwrap_or(&Value::Null);
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(
                run.get("label")
                    .and_then(Value::as_str)
                    .unwrap_or("unnamed")
            ),
            metrics
                .get("n")
                .and_then(Value::as_u64)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
            metric(metrics, "total_return"),
            metric(metrics, "sharpe"),
            metric(metrics, "max_drawdown"),
            metric(stressed, "total_return")
        ));
    }
    html.push_str("</tbody></table></section><section class=card><h2>NAV paths</h2>");
    html.push_str(&nav_svg(&runs));
    html.push_str("</section><section class=card><h2>Information coefficient</h2><table><thead><tr><th>horizon</th><th>periods</th><th>observations</th><th>mean IC</th><th>t-stat</th><th>hit rate</th></tr></thead><tbody>");
    for row in record
        .get("ic")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        html.push_str(&format!(
            "<tr><td>{}d</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            row.get("horizon_days")
                .and_then(Value::as_i64)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
            row.get("n_periods")
                .and_then(Value::as_u64)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
            row.get("n_observations")
                .and_then(Value::as_u64)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
            metric(row, "mean_ic"),
            metric(row, "t_stat"),
            metric(row, "hit_rate")
        ));
    }
    let pretty = serde_json::to_string_pretty(record).map_err(|error| error.to_string())?;
    html.push_str(&format!(
        "</tbody></table></section><details class=card><summary>Complete machine-readable evidence</summary><pre><code>{}</code></pre></details></main></body></html>",
        escape(&pretty)
    ));
    Ok(html)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disclosures_precede_metrics_and_untrusted_text_is_escaped() {
        let record = serde_json::json!({
            "window": ["2025-01-01", "2025-12-31"],
            "disclosures": ["<script>bad()</script>"],
            "runs": [{"label": "candidate", "metrics": {"n": 4, "total_return": "0.1"}, "stressed": {"total_return": "0.0"}, "nav": []}],
            "ic": []
        });
        let html = render(&record).unwrap();
        assert!(!html.contains("<script>bad()"));
        assert!(html.find("Disclosures").unwrap() < html.find("+10.00%").unwrap());
        assert!(!html.contains("https://"));
    }
}
