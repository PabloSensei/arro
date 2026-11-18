//! The plan of a statement: the text of an `EXPLAIN` and, when the product answers with
//! a structure, the tree of the steps that run.
//!
//! The statement of the plan is the one of the console under the caret. A product that
//! answers with rows of its own is read as a tree by the numbers that link a step to its
//! parent; every other answer is shown as the text of its rows.

use serde_json::Value as Json;

use crate::data::Dialect;
use crate::protocol::Value;
use crate::schema::Column;

/// One step of a plan: what it does and the steps under it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExplainNode {
    pub label: String,
    pub children: Vec<ExplainNode>,
}

/// An answered explain: a tree when the product gave one, the text of its rows otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExplainPlan {
    pub nodes: Vec<ExplainNode>,
    pub text: String,
    /// The rows of the plan, as the product wrote them.
    pub rows: usize,
}

impl ExplainPlan {
    /// Whether the plan is drawn as a tree.
    pub fn is_tree(&self) -> bool {
        !self.nodes.is_empty()
    }
}

/// The statement that explains `statement`, as the dialect writes it.
pub fn explain_statement(dialect: Dialect, statement: &str) -> String {
    let statement = statement.trim().trim_end_matches(';');
    match dialect {
        // The JSON of this product carries the steps and the numbers of each one.
        Dialect::PostgreSql => format!("EXPLAIN (FORMAT JSON) {statement}"),
        // This product names a step and its parent in the rows of the answer.
        Dialect::Sqlite => format!("EXPLAIN QUERY PLAN {statement}"),
        Dialect::H2 | Dialect::MySql | Dialect::MariaDb | Dialect::Oracle | Dialect::SqlServer => {
            format!("EXPLAIN {statement}")
        }
    }
}

/// The answer of an explain as a plan: the tree a product gave, or the text of its rows.
pub fn plan_of(dialect: Dialect, columns: &[Column], rows: &[Vec<Value>]) -> ExplainPlan {
    let nodes = match dialect {
        Dialect::PostgreSql => json_nodes(rows).unwrap_or_default(),
        Dialect::Sqlite => query_plan_nodes(rows),
        _ => Vec::new(),
    };
    let text = match nodes.is_empty() {
        true => text_of(columns, rows),
        false => String::new(),
    };
    ExplainPlan {
        nodes,
        text,
        rows: rows.len(),
    }
}

/// The rows of a plan as its text: the cells of a row in the order of the columns, and
/// one line per row.
fn text_of(columns: &[Column], rows: &[Vec<Value>]) -> String {
    if columns.len() <= 1 {
        return rows
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|value| !matches!(value, Value::Null))
                    .map(Value::text)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    rows.iter()
        .map(|row| row.iter().map(Value::text).collect::<Vec<_>>().join(" | "))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The steps of the plan this product answers in one JSON cell:
/// `[{"Plan": {"Node Type": "Seq Scan", ...}}]`.
fn json_nodes(rows: &[Vec<Value>]) -> Option<Vec<ExplainNode>> {
    let text = rows.iter().find_map(|row| {
        row.iter().find_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
    })?;
    let value: Json = serde_json::from_str(&text).ok()?;
    let plans = match &value {
        Json::Array(items) => items
            .iter()
            .filter_map(|item| item.get("Plan").map(json_node))
            .collect(),
        Json::Object(_) => vec![json_node(&value)],
        _ => return None,
    };
    (!plans.is_empty()).then_some(plans)
}

fn json_node(value: &Json) -> ExplainNode {
    let object = value.as_object();
    let field = |key: &str| object.and_then(|object| object.get(key));
    let mut label = field("Node Type")
        .and_then(Json::as_str)
        .unwrap_or("Plan")
        .to_owned();
    if let Some(relation) = field("Relation Name").and_then(Json::as_str) {
        label.push_str(" on ");
        label.push_str(relation);
    }
    if let Some(index) = field("Index Name").and_then(Json::as_str) {
        label.push_str(" using ");
        label.push_str(index);
    }
    if let Some(join) = field("Join Type").and_then(Json::as_str) {
        label.push_str(" (");
        label.push_str(join);
        label.push(')');
    }
    let cost = field("Total Cost").and_then(Json::as_f64);
    let cost = cost.map(|cost| format!("cost={cost:.2}"));
    let rows = field("Plan Rows").and_then(Json::as_u64);
    let rows = rows.map(|rows| format!("rows={rows}"));
    let numbers: Vec<String> = [cost, rows].into_iter().flatten().collect();
    if !numbers.is_empty() {
        label.push_str("  ");
        label.push_str(&numbers.join(" "));
    }
    let children = field("Plans")
        .and_then(Json::as_array)
        .map(|plans| plans.iter().map(json_node).collect())
        .unwrap_or_default();
    ExplainNode { label, children }
}

/// The steps of the query plan this product answers as the columns of the step, its
/// parent and its text.
fn query_plan_nodes(rows: &[Vec<Value>]) -> Vec<ExplainNode> {
    if rows.is_empty() || rows.iter().any(|row| row.len() < 4) {
        return Vec::new();
    }
    let (mut ids, mut parents, mut labels) = (Vec::new(), Vec::new(), Vec::new());
    for row in rows {
        let (Some(id), Some(parent)) = (count_of(&row[0]), count_of(&row[1])) else {
            return Vec::new();
        };
        ids.push(id);
        parents.push(parent);
        labels.push(row[3].text());
    }
    // A step of a plan comes after its parent, so the steps are read from the last one:
    // the children of a step are complete when it takes them.
    let mut children: Vec<Vec<ExplainNode>> = (0..ids.len()).map(|_| Vec::new()).collect();
    let mut roots: Vec<ExplainNode> = Vec::new();
    for index in (0..ids.len()).rev() {
        let node = ExplainNode {
            label: labels[index].clone(),
            children: std::mem::take(&mut children[index]),
        };
        match ids
            .iter()
            .position(|id| *id == parents[index])
            .filter(|parent| *parent < index)
        {
            Some(parent) => children[parent].insert(0, node),
            None => roots.insert(0, node),
        }
    }
    roots
}

fn count_of(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(integer) => Some(*integer),
        Value::Decimal(text) => text.parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::ColumnKind;

    fn column(name: &str) -> Column {
        Column {
            name: name.into(),
            type_name: "TEXT".into(),
            kind: ColumnKind::Text,
            nullable: true,
            primary_key: false,
        }
    }

    #[test]
    fn every_dialect_writes_its_own_explain() {
        let statement = "SELECT * FROM OWNERS WHERE ID = 1;";

        assert_eq!(
            explain_statement(Dialect::PostgreSql, statement),
            "EXPLAIN (FORMAT JSON) SELECT * FROM OWNERS WHERE ID = 1"
        );
        assert_eq!(
            explain_statement(Dialect::Sqlite, statement),
            "EXPLAIN QUERY PLAN SELECT * FROM OWNERS WHERE ID = 1"
        );
        for dialect in [
            Dialect::H2,
            Dialect::MySql,
            Dialect::MariaDb,
            Dialect::Oracle,
            Dialect::SqlServer,
        ] {
            assert_eq!(
                explain_statement(dialect, statement),
                "EXPLAIN SELECT * FROM OWNERS WHERE ID = 1",
                "{dialect:?}"
            );
        }
    }

    #[test]
    fn a_plan_with_a_parent_column_becomes_a_tree() {
        let columns = vec![
            column("id"),
            column("parent"),
            column("unused"),
            column("detail"),
        ];
        let rows = vec![
            vec![
                Value::Integer(2),
                Value::Integer(0),
                Value::Integer(0),
                Value::Text("SCAN OWNERS".into()),
            ],
            vec![
                Value::Integer(3),
                Value::Integer(2),
                Value::Integer(0),
                Value::Text("SEARCH USING INDEX ID".into()),
            ],
            vec![
                Value::Integer(7),
                Value::Integer(0),
                Value::Integer(0),
                Value::Text("USE TEMP B-TREE".into()),
            ],
        ];

        let plan = plan_of(Dialect::Sqlite, &columns, &rows);
        assert!(plan.is_tree());
        assert_eq!(plan.nodes.len(), 2);
        assert_eq!(plan.nodes[0].label, "SCAN OWNERS");
        assert_eq!(plan.nodes[0].children.len(), 1);
        assert_eq!(plan.nodes[0].children[0].label, "SEARCH USING INDEX ID");
        assert_eq!(plan.nodes[1].label, "USE TEMP B-TREE");
        assert!(plan.text.is_empty());
    }

    #[test]
    fn a_json_plan_becomes_a_tree_with_the_numbers_of_its_steps() {
        let columns = vec![column("QUERY PLAN")];
        let rows = vec![vec![Value::Text(
            r#"[{"Plan":{"Node Type":"Aggregate","Total Cost":11.5,"Plan Rows":3,
                 "Plans":[{"Node Type":"Seq Scan","Relation Name":"owners",
                           "Total Cost":1.1,"Plan Rows":10}]}}]"#
                .into(),
        )]];

        let plan = plan_of(Dialect::PostgreSql, &columns, &rows);
        assert_eq!(plan.nodes.len(), 1);
        assert_eq!(plan.nodes[0].label, "Aggregate  cost=11.50 rows=3");
        assert_eq!(
            plan.nodes[0].children[0].label,
            "Seq Scan on owners  cost=1.10 rows=10"
        );
        assert_eq!(plan.rows, 1);
    }

    #[test]
    fn a_plan_of_text_stays_text() {
        let columns = vec![column("PLAN")];
        let rows = vec![
            vec![Value::Text("SELECT\n    OWNERS.ID".into())],
            vec![Value::Text("    /* PUBLIC.OWNERS.tableScan */".into())],
        ];

        let plan = plan_of(Dialect::H2, &columns, &rows);
        assert!(!plan.is_tree());
        assert_eq!(
            plan.text,
            "SELECT\n    OWNERS.ID\n    /* PUBLIC.OWNERS.tableScan */"
        );
    }
}
