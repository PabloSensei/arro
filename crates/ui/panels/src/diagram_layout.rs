//! The layout of a type diagram: one layer for every generation of a hierarchy, the
//! ancestors above their descendants, and the order of a layer chosen by the barycenter
//! of its neighbours. It is a small Sugiyama layout without external dependencies.

/// The space between two nodes of one layer.
const HORIZONTAL_GAP: f32 = 40.;
/// The space between two layers.
const VERTICAL_GAP: f32 = 64.;
/// The margin around the whole diagram.
const MARGIN: f32 = 32.;
/// How many times the order of the layers is refined.
const PASSES: usize = 4;
/// The width the caller gives a node that measures nothing.
const MIN_WIDTH: f32 = 80.;
const MIN_HEIGHT: f32 = 40.;

/// The size a node needs.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Measure {
    pub width: f32,
    pub height: f32,
}

impl Measure {
    pub fn new(width: f32, height: f32) -> Self {
        Self {
            width: width.max(MIN_WIDTH),
            height: height.max(MIN_HEIGHT),
        }
    }
}

/// A node where the layout put it, in diagram units.
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The layer the node is in: zero for the highest one.
    pub layer: usize,
}

#[cfg(test)]
impl Placed {
    pub fn right(&self) -> f32 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f32 {
        self.y + self.height
    }

    pub fn intersects(&self, other: &Self) -> bool {
        self.x < other.right()
            && other.x < self.right()
            && self.y < other.bottom()
            && other.y < self.bottom()
    }
}

/// The placement of every node and the size of the whole diagram.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Layout {
    pub nodes: Vec<Placed>,
    /// The nodes of every layer, from the highest one.
    pub layers: Vec<Vec<usize>>,
    pub width: f32,
    pub height: f32,
}

/// Puts the nodes on layers and places them. An edge goes from a node to one of its
/// ancestors, so the node it points at is placed above it.
pub fn compute(measures: &[Measure], edges: &[(usize, usize)]) -> Layout {
    if measures.is_empty() {
        return Layout::default();
    }
    let parents = parents_of(measures.len(), edges);
    let layers = assign_layers(measures.len(), &parents);
    let mut order = layers;
    let before = crossings(&order, edges);
    let mut best = order.clone();
    let mut best_crossings = before;
    for pass in 0..PASSES {
        if pass % 2 == 0 {
            order_by_parents(&mut order, &parents, edges);
        } else {
            order_by_children(&mut order, &parents, edges);
        }
        let found = crossings(&order, edges);
        if found < best_crossings {
            best_crossings = found;
            best = order.clone();
        }
    }
    place(measures, &best)
}

/// The ancestors of every node, without repeats.
fn parents_of(count: usize, edges: &[(usize, usize)]) -> Vec<Vec<usize>> {
    let mut parents = vec![Vec::new(); count];
    for (from, to) in edges {
        if from == to || *from >= count || *to >= count {
            continue;
        }
        if !parents[*from].contains(to) {
            parents[*from].push(*to);
        }
    }
    parents
}

/// The layer of every node: one below its deepest ancestor.
fn assign_layers(count: usize, parents: &[Vec<usize>]) -> Vec<Vec<usize>> {
    fn depth(
        node: usize,
        parents: &[Vec<usize>],
        found: &mut [Option<usize>],
        visiting: &mut [bool],
    ) -> usize {
        if let Some(depth) = found[node] {
            return depth;
        }
        if visiting[node] {
            // A hierarchy of the sources may be read as a cycle by a broken file.
            return 0;
        }
        visiting[node] = true;
        let mut depth_of_node = 0;
        for parent in &parents[node] {
            depth_of_node = depth_of_node.max(depth(*parent, parents, found, visiting) + 1);
        }
        visiting[node] = false;
        found[node] = Some(depth_of_node);
        depth_of_node
    }

    let mut found = vec![None; count];
    let mut visiting = vec![false; count];
    let mut deepest = 0;
    for node in 0..count {
        deepest = deepest.max(depth(node, parents, &mut found, &mut visiting));
    }
    let mut layers = vec![Vec::new(); deepest + 1];
    for node in 0..count {
        layers[found[node].unwrap_or(0)].push(node);
    }
    for layer in &mut layers {
        layer.sort_unstable();
    }
    layers
}

/// The position of every node inside its layer.
fn positions(layers: &[Vec<usize>]) -> Vec<usize> {
    let count = layers.iter().map(Vec::len).sum();
    let mut positions = vec![0; count];
    for layer in layers {
        for (index, node) in layer.iter().enumerate() {
            positions[*node] = index;
        }
    }
    positions
}

/// Orders every layer, from the second one down, by the average position of the parents
/// of its nodes.
fn order_by_parents(layers: &mut [Vec<usize>], parents: &[Vec<usize>], _: &[(usize, usize)]) {
    for layer in 1..layers.len() {
        let positions = positions(layers);
        let mut wanted: Vec<(f64, usize)> = layers[layer]
            .iter()
            .map(|node| {
                let neighbours: Vec<usize> = parents[*node]
                    .iter()
                    .map(|parent| positions[*parent])
                    .collect();
                (barycenter(&neighbours), *node)
            })
            .collect();
        wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        layers[layer] = wanted.into_iter().map(|(_, node)| node).collect();
    }
}

/// Orders every layer, from the one above the last, by the average position of the
/// children of its nodes.
fn order_by_children(layers: &mut [Vec<usize>], _: &[Vec<usize>], edges: &[(usize, usize)]) {
    for layer in (0..layers.len().saturating_sub(1)).rev() {
        let positions = positions(layers);
        let mut wanted: Vec<(f64, usize)> = layers[layer]
            .iter()
            .map(|node| {
                let neighbours: Vec<usize> = edges
                    .iter()
                    .filter(|(from, _)| from == node)
                    .map(|(_, to)| positions[*to])
                    .collect();
                (barycenter(&neighbours), *node)
            })
            .collect();
        wanted.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        layers[layer] = wanted.into_iter().map(|(_, node)| node).collect();
    }
}

fn barycenter(neighbours: &[usize]) -> f64 {
    if neighbours.is_empty() {
        return f64::MAX;
    }
    neighbours.iter().map(|at| *at as f64).sum::<f64>() / neighbours.len() as f64
}

/// How many pairs of edges cross between neighbouring layers.
pub fn crossings(layers: &[Vec<usize>], edges: &[(usize, usize)]) -> usize {
    let positions = positions(layers);
    let mut layer_of = vec![0usize; positions.len()];
    for (index, layer) in layers.iter().enumerate() {
        for node in layer {
            layer_of[*node] = index;
        }
    }
    let mut count = 0;
    for (index, (from_a, to_a)) in edges.iter().enumerate() {
        for (from_b, to_b) in &edges[index + 1..] {
            if from_a == from_b || to_a == to_b {
                continue;
            }
            if layer_of[*from_a] != layer_of[*from_b] || layer_of[*to_a] != layer_of[*to_b] {
                continue;
            }
            let left = (positions[*from_a] as isize - positions[*from_b] as isize)
                * (positions[*to_a] as isize - positions[*to_b] as isize);
            if left < 0 {
                count += 1;
            }
        }
    }
    count
}

/// Gives every node its place: the layers from the top down, the nodes of a layer side by
/// side, every layer centred over the widest one.
fn place(measures: &[Measure], layers: &[Vec<usize>]) -> Layout {
    let widths: Vec<f32> = layers
        .iter()
        .map(|layer| {
            layer.iter().map(|node| measures[*node].width).sum::<f32>()
                + HORIZONTAL_GAP * layer.len().saturating_sub(1) as f32
        })
        .collect();
    let widest = widths.iter().copied().fold(0., f32::max);
    let heights: Vec<f32> = layers
        .iter()
        .map(|layer| {
            layer
                .iter()
                .map(|node| measures[*node].height)
                .fold(0., f32::max)
        })
        .collect();
    let mut nodes = vec![
        Placed {
            x: 0.,
            y: 0.,
            width: 0.,
            height: 0.,
            layer: 0,
        };
        measures.len()
    ];
    let mut y = MARGIN;
    for (index, layer) in layers.iter().enumerate() {
        let mut x = MARGIN + (widest - widths[index]) / 2.;
        for node in layer {
            let measure = measures[*node];
            let centre = y + (heights[index] - measure.height) / 2.;
            nodes[*node] = Placed {
                x,
                y: centre,
                width: measure.width,
                height: measure.height,
                layer: index,
            };
            x += measure.width + HORIZONTAL_GAP;
        }
        y += heights[index] + VERTICAL_GAP;
    }
    let height = (y - VERTICAL_GAP).max(0.) + MARGIN;
    Layout {
        nodes,
        layers: layers.to_vec(),
        width: widest + MARGIN * 2.,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measures(count: usize) -> Vec<Measure> {
        (0..count).map(|_| Measure::new(200., 100.)).collect()
    }

    #[test]
    fn an_ancestor_is_placed_above_its_descendant() {
        let layout = compute(&measures(3), &[(0, 1), (1, 2)]);
        assert!(layout.nodes[2].y < layout.nodes[1].y, "the root is highest");
        assert!(layout.nodes[1].y < layout.nodes[0].y, "the child is lowest");
        assert_eq!(layout.nodes[2].layer, 0);
        assert_eq!(layout.nodes[1].layer, 1);
        assert_eq!(layout.nodes[0].layer, 2);
    }

    #[test]
    fn fifty_nodes_of_a_wide_hierarchy_do_not_overlap() {
        let mut edges = Vec::new();
        for node in 1..50 {
            edges.push((node, (node - 1) / 2));
        }
        let layout = compute(&measures(50), &edges);
        for (index, node) in layout.nodes.iter().enumerate() {
            for other in &layout.nodes[index + 1..] {
                assert!(!node.intersects(other), "{node:?} overlaps {other:?}");
            }
        }
    }

    #[test]
    fn the_barycenter_takes_the_crossings_out_of_a_crossed_graph() {
        // Two layers: the first order of the lower one crosses both edges.
        let layout = compute(&measures(4), &[(2, 1), (3, 0)]);
        assert_eq!(
            crossings(&layout.layers, &[(2, 1), (3, 0)]),
            0,
            "the order after the passes has no crossing: {:?}",
            layout.layers
        );
        let initial = vec![vec![0, 1], vec![2, 3]];
        assert!(
            crossings(&initial, &[(2, 1), (3, 0)]) > 0,
            "the test graph starts crossed"
        );
    }

    #[test]
    fn a_cycle_does_not_hang_the_layers() {
        let layout = compute(&measures(3), &[(0, 1), (1, 2), (2, 0)]);
        assert_eq!(layout.nodes.len(), 3);
    }

    #[test]
    fn the_layers_are_centred_over_the_widest_one() {
        let layout = compute(&measures(2), &[(1, 0)]);
        let top = &layout.nodes[0];
        let lower = &layout.nodes[1];
        assert!(top.y < lower.y);
        assert!(
            (top.x + top.width / 2. - (lower.x + lower.width / 2.)).abs() < 1.,
            "the only node of each layer shares the centre"
        );
    }
}
