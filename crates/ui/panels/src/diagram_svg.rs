//! The diagram as a standalone SVG file: the same nodes, members and edges the window
//! draws, with the text escaped and no font of its own.

use lang_api::{
    GraphEdge, GraphEdgeKind, GraphMember, GraphMemberKind, GraphType, GraphVisibility, TypeGraph,
};

use crate::diagram_layout::Layout;

/// What the exported picture shows, as the toolbar of the diagram sets it.
#[derive(Clone, Copy, Debug)]
pub struct SvgOptions {
    pub fields: bool,
    pub methods: bool,
    pub properties: bool,
    pub dependencies: bool,
    pub public_only: bool,
}

impl Default for SvgOptions {
    fn default() -> Self {
        Self {
            fields: true,
            methods: true,
            properties: false,
            dependencies: true,
            public_only: false,
        }
    }
}

const HEADER_HEIGHT: f32 = 28.;
const ROW_HEIGHT: f32 = 20.;
const PADDING: f32 = 10.;

/// The picture of one diagram.
pub fn render(graph: &TypeGraph, layout: &Layout, options: &SvgOptions) -> String {
    let mut svg = String::new();
    svg.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.0}\" height=\"{:.0}\" viewBox=\"0 0 {:.0} {:.0}\">\n",
        layout.width, layout.height, layout.width, layout.height
    ));
    svg.push_str("<style>text{font-family:'Segoe UI',sans-serif;fill:#1c1b1a}.header{font-weight:600;font-size:13px}.member{font-size:12px}.section{stroke:#d6d1c9;stroke-width:1}.edge{fill:none;stroke:#5e5a54;stroke-width:1.2}</style>\n");
    svg.push_str(&format!(
        "<rect class=\"background\" x=\"0\" y=\"0\" width=\"{:.0}\" height=\"{:.0}\" fill=\"#fdfcfa\"/>\n",
        layout.width, layout.height
    ));
    for edge in &graph.edges {
        if edge.kind == GraphEdgeKind::Association && !options.dependencies {
            continue;
        }
        if let Some(markup) = edge_markup(graph, layout, edge) {
            svg.push_str(&markup);
        }
    }
    for (index, node) in graph.nodes.iter().enumerate() {
        svg.push_str(&node_markup(index, node, layout, options));
    }
    svg.push_str("</svg>\n");
    svg
}

fn node_markup(index: usize, node: &GraphType, layout: &Layout, options: &SvgOptions) -> String {
    let Some(placed) = layout.nodes.get(index) else {
        return String::new();
    };
    let mut out = String::new();
    let members = members_of(node, options);
    let height = placed.height.max(HEADER_HEIGHT + PADDING * 2.);
    let (fill, stroke) = if node.external {
        ("#f4f1ec", "#a8a39a")
    } else {
        ("#ffffff", "#5e5a54")
    };
    out.push_str(&format!(
        "<g class=\"node\" data-type=\"{}\">\n",
        escape(&node.qualified)
    ));
    out.push_str(&format!(
        "<rect x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\" rx=\"4\" fill=\"{fill}\" stroke=\"{stroke}\"/>\n",
        placed.x, placed.y, placed.width, height
    ));
    let name = if node.abstract_type {
        format!("{{{}}}", node.name)
    } else {
        node.name.clone()
    };
    out.push_str(&format!(
        "<text class=\"header\" x=\"{:.1}\" y=\"{:.1}\">{}</text>\n",
        placed.x + PADDING,
        placed.y + 18.,
        escape(&name)
    ));
    out.push_str(&format!(
        "<line class=\"section\" x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\"/>\n",
        placed.x,
        placed.y + HEADER_HEIGHT,
        placed.x + placed.width,
        placed.y + HEADER_HEIGHT
    ));
    let mut row = 0;
    let mut last_kind: Option<GraphMemberKind> = None;
    for member in members {
        if last_kind.is_some_and(|kind| kind != member.kind) {
            let y = placed.y + HEADER_HEIGHT + 3. + row as f32 * ROW_HEIGHT;
            out.push_str(&format!(
                "<line class=\"section\" x1=\"{:.1}\" y1=\"{:.1}\" x2=\"{:.1}\" y2=\"{:.1}\"/>\n",
                placed.x,
                y,
                placed.x + placed.width,
                y
            ));
            row += 1;
        }
        last_kind = Some(member.kind);
        let y = placed.y + HEADER_HEIGHT + 3. + row as f32 * ROW_HEIGHT + 14.;
        out.push_str(&format!(
            "<text class=\"member\" x=\"{:.1}\" y=\"{:.1}\">{}</text>\n",
            placed.x + PADDING,
            y,
            escape(&member_label(member))
        ));
        row += 1;
    }
    out.push_str("</g>\n");
    out
}

fn edge_markup(graph: &TypeGraph, layout: &Layout, edge: &GraphEdge) -> Option<String> {
    let from = graph
        .nodes
        .iter()
        .position(|node| node.qualified == edge.from)?;
    let to = graph
        .nodes
        .iter()
        .position(|node| node.qualified == edge.to)?;
    let from = layout.nodes.get(from)?;
    let to = layout.nodes.get(to)?;
    let start_x = from.x + from.width / 2.;
    let end_x = to.x + to.width / 2.;
    let (start_y, end_y, start_side, end_side) =
        if to.y + to.height / 2. <= from.y + from.height / 2. {
            (from.y, to.y + to.height, from.y, to.y + to.height)
        } else {
            (from.y + from.height, to.y, from.y + from.height, to.y)
        };
    let middle = (start_y + end_y) / 2.;
    let points = if (start_x - end_x).abs() < 1. {
        format!("{start_x:.1},{start_y:.1} {end_x:.1},{end_y:.1}")
    } else {
        format!(
            "{start_x:.1},{start_y:.1} {start_x:.1},{middle:.1} {end_x:.1},{middle:.1} {end_x:.1},{end_y:.1}"
        )
    };
    let class = match edge.kind {
        GraphEdgeKind::Inheritance => "inheritance",
        GraphEdgeKind::Implementation => "implementation",
        GraphEdgeKind::Association => "association",
    };
    let dash = if edge.kind == GraphEdgeKind::Implementation {
        " stroke-dasharray=\"6 4\""
    } else {
        ""
    };
    let mut out = format!("<polyline class=\"edge {class}\" points=\"{points}\"{dash}/>\n");
    // The marker at the target end points down from the target toward the source.
    let (tip_y, base_y) = if end_side <= start_side {
        (end_side, end_side - 8.)
    } else {
        (end_side, end_side + 8.)
    };
    match edge.kind {
        GraphEdgeKind::Inheritance | GraphEdgeKind::Implementation => {
            out.push_str(&format!(
                "<polygon class=\"arrow hollow\" points=\"{end_x:.1},{tip_y:.1} {:.1},{base_y:.1} {:.1},{base_y:.1}\" fill=\"#fdfcfa\" stroke=\"#5e5a54\"/>\n",
                end_x - 6.,
                end_x + 6.
            ));
        }
        GraphEdgeKind::Association => {
            out.push_str(&format!(
                "<polygon class=\"arrow\" points=\"{end_x:.1},{tip_y:.1} {:.1},{base_y:.1} {:.1},{base_y:.1}\" fill=\"#5e5a54\"/>\n",
                end_x - 4.,
                end_x + 4.,
            ));
            if let Some(multiplicity) = &edge.multiplicity {
                out.push_str(&format!(
                    "<text class=\"multiplicity\" x=\"{:.1}\" y=\"{:.1}\">{}</text>\n",
                    end_x + 6.,
                    end_y + if base_y > tip_y { 14. } else { -4. },
                    escape(multiplicity)
                ));
            }
        }
    }
    Some(out)
}

/// The members a node shows with the options of the toolbar, in section order.
fn members_of<'a>(node: &'a GraphType, options: &SvgOptions) -> Vec<&'a GraphMember> {
    let mut fields = Vec::new();
    let mut methods = Vec::new();
    for member in &node.members {
        if options.public_only && member.visibility != GraphVisibility::Public {
            continue;
        }
        match member.kind {
            GraphMemberKind::Field if options.fields => fields.push(member),
            GraphMemberKind::Method if options.methods && !is_accessor(member, options) => {
                methods.push(member)
            }
            _ => {}
        }
    }
    fields.extend(methods);
    fields
}

/// A getter or a setter, which Properties folds into the field it reads or writes.
pub fn is_accessor(member: &GraphMember, options: &SvgOptions) -> bool {
    if !options.properties || member.kind != GraphMemberKind::Method {
        return false;
    }
    let name = member.name.as_str();
    let getter =
        (name.starts_with("get") && name.len() > 3) || (name.starts_with("is") && name.len() > 2);
    let setter = name.starts_with("set") && name.len() > 3;
    (getter && member.parameters.is_empty()) || (setter && member.parameters.len() == 1)
}

/// One member line of a node.
pub fn member_label(member: &GraphMember) -> String {
    let visibility = match member.visibility {
        GraphVisibility::Public => '+',
        GraphVisibility::Protected => '#',
        GraphVisibility::Package => '~',
        GraphVisibility::Private => '-',
    };
    match member.kind {
        GraphMemberKind::Field => {
            if member.type_name.is_empty() {
                format!("{visibility} {}", member.name)
            } else {
                format!("{visibility} {}: {}", member.name, member.type_name)
            }
        }
        GraphMemberKind::Method => {
            let parameters = member.parameters.join(", ");
            let returns = if member.type_name.is_empty() {
                String::new()
            } else {
                format!(": {}", member.type_name)
            };
            let suffix = if member.abstract_member {
                " {abstract}"
            } else {
                ""
            };
            format!(
                "{visibility} {}({parameters}){returns}{suffix}",
                member.name
            )
        }
    }
}

/// Text as an XML text node or attribute value.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use lang_api::{
        GraphEdge, GraphEdgeKind, GraphMember, GraphMemberKind, GraphType, GraphTypeKind,
        GraphVisibility, TypeGraph,
    };

    use super::*;
    use crate::diagram_layout::{Measure, compute};

    fn node(qualified: &str, members: Vec<GraphMember>) -> GraphType {
        GraphType {
            name: qualified.rsplit('.').next().unwrap_or(qualified).to_owned(),
            qualified: qualified.to_owned(),
            kind: GraphTypeKind::Class,
            abstract_type: false,
            external: false,
            members,
            place: None,
        }
    }

    fn member(name: &str, kind: GraphMemberKind, type_name: &str) -> GraphMember {
        GraphMember {
            name: name.to_owned(),
            type_name: type_name.to_owned(),
            parameters: Vec::new(),
            kind,
            visibility: GraphVisibility::Public,
            static_member: false,
            abstract_member: false,
            place: None,
        }
    }

    #[test]
    fn the_picture_has_the_nodes_the_edges_and_the_escaped_text() {
        let graph = TypeGraph {
            nodes: vec![
                node(
                    "demo.Person<&>",
                    vec![member("name", GraphMemberKind::Field, "String")],
                ),
                node(
                    "demo.Owner \"quoted\"",
                    vec![member("friends", GraphMemberKind::Field, "List")],
                ),
            ],
            edges: vec![GraphEdge {
                from: "demo.Owner \"quoted\"".to_owned(),
                to: "demo.Person<&>".to_owned(),
                kind: GraphEdgeKind::Inheritance,
                multiplicity: None,
            }],
        };
        let layout = compute(
            &[Measure::new(220., 80.), Measure::new(220., 80.)],
            &[(1, 0)],
        );
        let svg = render(&graph, &layout, &SvgOptions::default());
        assert!(svg.contains("class=\"node\""), "the nodes are drawn");
        assert!(
            svg.contains("data-type=\"demo.Person&lt;&amp;&gt;\""),
            "the names are escaped"
        );
        assert!(svg.contains("demo.Owner &quot;quoted&quot;"));
        assert!(
            svg.contains("class=\"edge inheritance\""),
            "the edge is drawn"
        );
        assert!(svg.contains("class=\"arrow hollow\""));
        assert!(svg.contains("name: String"), "the members are drawn");
        assert_eq!(svg.matches("<g class=\"node\"").count(), 2);
    }

    #[test]
    fn the_toolbar_hides_the_fields_and_the_dependencies() {
        let graph = TypeGraph {
            nodes: vec![
                node(
                    "demo.A",
                    vec![
                        member("count", GraphMemberKind::Field, "int"),
                        member("run", GraphMemberKind::Method, "void"),
                    ],
                ),
                node("demo.B", Vec::new()),
            ],
            edges: vec![GraphEdge {
                from: "demo.A".to_owned(),
                to: "demo.B".to_owned(),
                kind: GraphEdgeKind::Association,
                multiplicity: Some("*".to_owned()),
            }],
        };
        let layout = compute(
            &[Measure::new(200., 80.), Measure::new(200., 80.)],
            &[(0, 1)],
        );
        let svg = render(
            &graph,
            &layout,
            &SvgOptions {
                fields: false,
                dependencies: false,
                ..SvgOptions::default()
            },
        );
        assert!(!svg.contains("count: int"));
        assert!(svg.contains("run()"));
        assert!(!svg.contains("class=\"edge association\""));
    }

    #[test]
    fn properties_fold_the_accessors_of_a_field() {
        let mut get = member("getName", GraphMemberKind::Method, "String");
        get.abstract_member = false;
        let mut set = member("setName", GraphMemberKind::Method, "void");
        set.parameters = vec!["String".to_owned()];
        let graph = TypeGraph {
            nodes: vec![node("demo.A", vec![get, set])],
            edges: Vec::new(),
        };
        let layout = compute(&[Measure::new(200., 80.)], &[]);
        let with = render(
            &graph,
            &layout,
            &SvgOptions {
                properties: true,
                ..SvgOptions::default()
            },
        );
        assert!(!with.contains("getName()"), "the getter is folded");
        let without = render(&graph, &layout, &SvgOptions::default());
        assert!(without.contains("getName()"), "the getter is a method");
    }
}
