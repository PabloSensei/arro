use std::collections::HashSet;
use std::fs;
use std::path::Path;

use toml::{Table, Value};

use crate::Result;

const STAGES: &[&str] = &[
    "M1", "M2", "M3", "M4", "M5a", "M5b", "M6", "M7", "M8", "M9", "M10", "M11", "M12", "M13",
    "M14", "M15", "M16",
];

pub fn run(root: &Path) -> Result {
    let text = fs::read_to_string(root.join("budgets.toml"))?;
    let problems = validate(&text)?;
    if problems.is_empty() {
        return Ok(());
    }
    for problem in &problems {
        eprintln!("error: budgets.toml: {problem}");
    }
    Err(format!("{} problem(s) in budgets.toml", problems.len()).into())
}

fn validate(text: &str) -> Result<Vec<String>> {
    let table: Table = text.parse()?;
    let metrics = table
        .get("metric")
        .and_then(Value::as_array)
        .ok_or("no [[metric]] entries")?;

    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    for (index, metric) in metrics.iter().enumerate() {
        let id = metric.get("id").and_then(Value::as_str);
        let Some(id) = id else {
            problems.push(format!("metric #{index} has no id"));
            continue;
        };
        if !seen.insert(id) {
            problems.push(format!("{id}: duplicate id"));
        }
        problems.extend(validate_metric(metric).map(|problem| format!("{id}: {problem}")));
    }
    Ok(problems)
}

fn validate_metric(metric: &Value) -> Option<String> {
    let stage = metric.get("applies_from").and_then(Value::as_str);
    match stage {
        None => return Some("no applies_from".into()),
        Some(stage) if !STAGES.contains(&stage) => {
            return Some(format!("unknown stage {stage}"));
        }
        Some(_) => {}
    }
    if metric.get("unit").and_then(Value::as_str).is_none() {
        return Some("no unit".into());
    }

    let has = |key| metric.get(key).is_some();
    if metric.get("observe").and_then(Value::as_bool) == Some(true) {
        let bounded = has("target") || has("limit") || has("pending");
        return bounded.then(|| "observe takes no target, limit or pending".into());
    }

    let number = |key| {
        let value = metric.get(key)?;
        value
            .as_float()
            .or(value.as_integer().map(|int| int as f64))
    };
    let pending = has("pending");
    match (number("target"), number("limit")) {
        (None, None) if pending => None,
        (Some(target), Some(limit)) if !pending => {
            (target > limit).then(|| format!("target {target} is above limit {limit}"))
        }
        _ => Some("needs target and limit, pending or observe".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn accepts_values_and_pending() {
        let text = r#"
            [[metric]]
            id = "a"
            unit = "ms"
            target = 0.5
            limit = 1
            applies_from = "M1"

            [[metric]]
            id = "b"
            unit = "MB"
            pending = "S9"
            applies_from = "M5a"
        "#;
        assert_eq!(validate(text).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn reports_missing_applies_from() {
        let text = r#"
            [[metric]]
            id = "a"
            unit = "ms"
            target = 1
            limit = 2
        "#;
        assert_eq!(validate(text).unwrap(), ["a: no applies_from"]);
    }

    #[test]
    fn reports_target_above_limit_and_duplicates() {
        let text = r#"
            [[metric]]
            id = "a"
            unit = "ms"
            target = 3
            limit = 2
            applies_from = "M1"

            [[metric]]
            id = "a"
            unit = "ms"
            pending = "S1"
            applies_from = "M99"
        "#;
        assert_eq!(
            validate(text).unwrap(),
            [
                "a: target 3 is above limit 2",
                "a: duplicate id",
                "a: unknown stage M99"
            ]
        );
    }

    #[test]
    fn reports_metric_without_values() {
        let text = r#"
            [[metric]]
            id = "a"
            unit = "ms"
            applies_from = "M1"
        "#;
        assert_eq!(
            validate(text).unwrap(),
            ["a: needs target and limit, pending or observe"]
        );
    }

    #[test]
    fn reports_missing_unit() {
        let text = r#"
            [[metric]]
            id = "a"
            target = 3
            limit = 2
            applies_from = "M1"
        "#;
        assert_eq!(validate(text).unwrap(), ["a: no unit"]);
    }

    #[test]
    fn accepts_observe_without_values() {
        let text = r#"
            [[metric]]
            id = "a"
            unit = "MB"
            observe = true
            applies_from = "M1"
        "#;
        assert_eq!(validate(text).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn observe_has_no_limit() {
        for extra in ["target = 1", "limit = 1", "pending = \"S1\""] {
            let text = format!(
                r#"
                [[metric]]
                id = "a"
                unit = "MB"
                observe = true
                {extra}
                applies_from = "M1"
                "#
            );
            assert_eq!(
                validate(&text).unwrap(),
                ["a: observe takes no target, limit or pending"],
                "{extra}"
            );
        }
    }

    #[test]
    fn observe_false_does_not_replace_values() {
        let text = r#"
            [[metric]]
            id = "a"
            unit = "MB"
            observe = false
            applies_from = "M1"
        "#;
        assert_eq!(
            validate(text).unwrap(),
            ["a: needs target and limit, pending or observe"]
        );
    }
}
