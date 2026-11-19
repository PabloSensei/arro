//! The shape of the editor area: groups of tabs, the splits between them and the sizes
//! of those splits. Pure bookkeeping, rendering lives in `chrome`.

use serde::{Deserialize, Serialize};

/// The share of a split its first part takes when the split appears, and the smallest
/// share a drag leaves to either part.
const HALF: f32 = 0.5;
const LIMIT: f32 = 0.1;

/// The saved shape of the editor area. The groups of `Layout::panes` fill the leaves in
/// the order of the tree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SplitLayout {
    Group,
    Split {
        /// The parts sit one under another instead of side by side.
        down: bool,
        /// The share of the first part, from zero to one.
        ratio: f32,
        first: Box<SplitLayout>,
        second: Box<SplitLayout>,
    },
}

/// A node of the editor area: a group of tabs or a split of two nodes.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SplitNode {
    Group(usize),
    Split {
        down: bool,
        ratio: f32,
        first: Box<SplitNode>,
        second: Box<SplitNode>,
    },
}

/// The tree of the groups of the window. The group ids are the ids of `Workspace::panes`,
/// and they run in the order of the leaves, so the groups of `open_tabs` keep that order.
#[derive(Clone, Debug, PartialEq)]
pub struct SplitTree {
    root: SplitNode,
}

impl SplitTree {
    pub fn single() -> Self {
        Self {
            root: SplitNode::Group(0),
        }
    }

    /// The tree a saved shape names, when it has a leaf for every group `count`.
    pub fn from_layout(layout: &SplitLayout, count: usize) -> Option<Self> {
        let mut next = 0;
        let root = node_of(layout, &mut next);
        (next == count && count > 0).then_some(Self { root })
    }

    /// `count` groups in one row, or in one column when `down`: the shape of a layout
    /// saved before the tree, every part of it equal.
    pub fn line(count: usize, down: bool) -> Self {
        let count = count.max(1);
        Self {
            root: line_of(0, count, down),
        }
    }

    /// The top of the tree, for the drawing of the editor area.
    pub(crate) fn root(&self) -> &SplitNode {
        &self.root
    }

    /// The group ids from the left or the top to the right or the bottom.
    pub fn groups(&self) -> Vec<usize> {
        let mut groups = Vec::new();
        collect_groups(&self.root, &mut groups);
        groups
    }

    /// The node at `path`, where every step is the child taken at that split.
    fn node(&self, path: &[usize]) -> Option<&SplitNode> {
        let mut node = &self.root;
        for step in path {
            let SplitNode::Split { first, second, .. } = node else {
                return None;
            };
            node = match step {
                0 => first,
                1 => second,
                _ => return None,
            };
        }
        Some(node)
    }

    fn node_mut(&mut self, path: &[usize]) -> Option<&mut SplitNode> {
        let mut node = &mut self.root;
        for step in path {
            let SplitNode::Split { first, second, .. } = node else {
                return None;
            };
            node = match step {
                0 => first,
                1 => second,
                _ => return None,
            };
        }
        Some(node)
    }

    /// Splits group `at` in two. The new group takes the place after it and gets the id
    /// `at + 1`; the ids above it move up.
    pub fn split(&mut self, at: usize, down: bool) -> Option<usize> {
        let node = self.node_mut(&[])?;
        if !contains(node, at) {
            return None;
        }
        shift_after(&mut self.root, at, 1);
        let node = self.find_mut(at)?;
        *node = SplitNode::Split {
            down,
            ratio: HALF,
            first: Box::new(SplitNode::Group(at)),
            second: Box::new(SplitNode::Group(at + 1)),
        };
        Some(at + 1)
    }

    /// Takes group `at` away and joins its parent split into the other side. Returns the
    /// group that takes the focus, unless `at` was the only one.
    pub fn remove(&mut self, at: usize) -> Option<usize> {
        let focus = remove_leaf(&mut self.root, at)?;
        shift_after(&mut self.root, at, -1);
        Some(if focus > at { focus - 1 } else { focus })
    }

    /// The direction of the split the group `at` sits in, nearest first.
    pub fn direction(&self, at: usize) -> Option<bool> {
        let mut node = &self.root;
        let mut found = None;
        loop {
            let SplitNode::Split {
                down,
                first,
                second,
                ..
            } = node
            else {
                return found;
            };
            if contains(first, at) {
                found = Some(*down);
                node = first;
            } else if contains(second, at) {
                found = Some(*down);
                node = second;
            } else {
                return found;
            }
        }
    }

    /// The group on the other side of the split nearest to `at`: the last group of the
    /// left or upper part, or the first group of the right or lower one.
    pub fn opposite(&self, at: usize) -> Option<usize> {
        opposite_in(&self.root, at)
    }

    /// The share of the first part of the split at `path`, for tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn ratio(&self, path: &[usize]) -> Option<f32> {
        match self.node(path)? {
            SplitNode::Split { ratio, .. } => Some(*ratio),
            SplitNode::Group(_) => None,
        }
    }

    /// Sets the share of the first part of the split at `path`, within the limits.
    /// Returns whether the split is there and the share moved.
    pub fn set_ratio(&mut self, path: &[usize], ratio: f32) -> bool {
        let ratio = ratio.clamp(LIMIT, 1. - LIMIT);
        let Some(SplitNode::Split { ratio: saved, .. }) = self.node_mut(path) else {
            return false;
        };
        if (*saved - ratio).abs() < f32::EPSILON {
            return false;
        }
        *saved = ratio;
        true
    }

    /// The saved shape of the tree, with the groups that `keep` drops taken away. `None`
    /// when no group is left.
    pub fn layout(&self, keep: &[bool]) -> Option<SplitLayout> {
        layout_of(&self.root, keep)
    }

    /// The direction of the split at `path`.
    pub fn split_direction(&self, path: &[usize]) -> Option<bool> {
        match self.node(path)? {
            SplitNode::Split { down, .. } => Some(*down),
            SplitNode::Group(_) => None,
        }
    }

    fn find_mut(&mut self, at: usize) -> Option<&mut SplitNode> {
        find_group_mut(&mut self.root, at)
    }
}

fn node_of(layout: &SplitLayout, next: &mut usize) -> SplitNode {
    match layout {
        SplitLayout::Group => {
            let id = *next;
            *next += 1;
            SplitNode::Group(id)
        }
        SplitLayout::Split {
            down,
            ratio,
            first,
            second,
        } => SplitNode::Split {
            down: *down,
            ratio: ratio.clamp(LIMIT, 1. - LIMIT),
            first: Box::new(node_of(first, next)),
            second: Box::new(node_of(second, next)),
        },
    }
}

fn line_of(first: usize, count: usize, down: bool) -> SplitNode {
    if count <= 1 {
        return SplitNode::Group(first);
    }
    SplitNode::Split {
        down,
        // Every part takes the same share of what the split leaves.
        ratio: 1. / count as f32,
        first: Box::new(SplitNode::Group(first)),
        second: Box::new(line_of(first + 1, count - 1, down)),
    }
}

fn collect_groups(node: &SplitNode, groups: &mut Vec<usize>) {
    match node {
        SplitNode::Group(id) => groups.push(*id),
        SplitNode::Split { first, second, .. } => {
            collect_groups(first, groups);
            collect_groups(second, groups);
        }
    }
}

fn contains(node: &SplitNode, at: usize) -> bool {
    match node {
        SplitNode::Group(id) => *id == at,
        SplitNode::Split { first, second, .. } => contains(first, at) || contains(second, at),
    }
}

fn find_group_mut(node: &mut SplitNode, at: usize) -> Option<&mut SplitNode> {
    match node {
        SplitNode::Group(id) if *id == at => Some(node),
        SplitNode::Group(_) => None,
        SplitNode::Split { first, second, .. } => {
            find_group_mut(first, at).or_else(|| find_group_mut(second, at))
        }
    }
}

/// Moves every id above `at` by `delta`, so that an id can be put in or taken out.
fn shift_after(node: &mut SplitNode, at: usize, delta: isize) {
    match node {
        SplitNode::Group(id) => {
            if *id > at {
                *id = id.checked_add_signed(delta).unwrap_or(*id);
            }
        }
        SplitNode::Split { first, second, .. } => {
            shift_after(first, at, delta);
            shift_after(second, at, delta);
        }
    }
}

/// Takes the leaf `at` away and returns the group of the other side that takes the
/// focus. The place of the leaf is the parent split, which becomes that other side.
fn remove_leaf(node: &mut SplitNode, at: usize) -> Option<usize> {
    let SplitNode::Split { first, second, .. } = node else {
        return None;
    };
    if matches!(**first, SplitNode::Group(id) if id == at) {
        let kept = std::mem::replace(second, Box::new(SplitNode::Group(usize::MAX)));
        let focus = leftmost(&kept)?;
        *node = *kept;
        return Some(focus);
    }
    if matches!(**second, SplitNode::Group(id) if id == at) {
        let kept = std::mem::replace(first, Box::new(SplitNode::Group(usize::MAX)));
        let focus = rightmost(&kept)?;
        *node = *kept;
        return Some(focus);
    }
    remove_leaf(first, at).or_else(|| remove_leaf(second, at))
}

fn leftmost(node: &SplitNode) -> Option<usize> {
    match node {
        SplitNode::Group(id) => Some(*id),
        SplitNode::Split { first, .. } => leftmost(first),
    }
}

fn rightmost(node: &SplitNode) -> Option<usize> {
    match node {
        SplitNode::Group(id) => Some(*id),
        SplitNode::Split { second, .. } => rightmost(second),
    }
}

fn opposite_in(node: &SplitNode, at: usize) -> Option<usize> {
    let SplitNode::Split { first, second, .. } = node else {
        return None;
    };
    if contains(first, at) {
        opposite_in(first, at).or_else(|| leftmost(second))
    } else if contains(second, at) {
        opposite_in(second, at).or_else(|| rightmost(first))
    } else {
        None
    }
}

fn layout_of(node: &SplitNode, keep: &[bool]) -> Option<SplitLayout> {
    match node {
        SplitNode::Group(id) => keep
            .get(*id)
            .copied()
            .unwrap_or(false)
            .then_some(SplitLayout::Group),
        SplitNode::Split {
            down,
            ratio,
            first,
            second,
        } => {
            match (layout_of(first, keep), layout_of(second, keep)) {
                (Some(first), Some(second)) => Some(SplitLayout::Split {
                    down: *down,
                    ratio: *ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                // A side without tabs goes away and the other side takes its place.
                (Some(one), None) | (None, Some(one)) => Some(one),
                (None, None) => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{SplitLayout, SplitTree};

    #[test]
    fn a_split_of_a_group_inside_a_vertical_split_goes_across_it() {
        let mut tree = SplitTree::single();
        assert_eq!(tree.split(0, false), Some(1));
        assert_eq!(tree.groups(), [0, 1]);
        assert_eq!(tree.split(1, true), Some(2));
        assert_eq!(tree.groups(), [0, 1, 2]);
        assert_eq!(tree.direction(0), Some(false));
        assert_eq!(tree.direction(1), Some(true));
        assert_eq!(tree.direction(2), Some(true));

        // The saved shape keeps the nesting: the first group is left of the other two.
        let layout = tree.layout(&[true, true, true]).unwrap();
        let back = SplitTree::from_layout(&layout, 3).unwrap();
        assert_eq!(back, tree);
    }

    #[test]
    fn a_split_in_the_middle_moves_the_ids_above_it() {
        let mut tree = SplitTree::single();
        tree.split(0, false).unwrap();
        tree.split(1, true).unwrap();
        // Three groups: the first left, the other two one under another. Split the
        // first one down, and every id above it takes the next place.
        let new = tree.split(0, true).unwrap();
        assert_eq!(new, 1);
        assert_eq!(tree.groups(), [0, 1, 2, 3]);
        assert_eq!(tree.split_direction(&[]), Some(false));
        assert_eq!(tree.direction(0), Some(true));
        assert_eq!(tree.direction(1), Some(true));
        // The split of the two groups that were there keeps its direction.
        assert_eq!(tree.direction(2), Some(true));
        assert_eq!(tree.direction(3), Some(true));
    }

    #[test]
    fn taking_a_group_away_joins_its_split_into_the_other_side() {
        let mut tree = SplitTree::single();
        tree.split(0, false).unwrap();
        tree.split(1, true).unwrap();
        // The middle group goes; the lower one takes its place and its id.
        assert_eq!(tree.remove(1), Some(1));
        assert_eq!(tree.groups(), [0, 1]);
        assert_eq!(tree.direction(1), Some(false));

        assert_eq!(tree.remove(1), Some(0));
        assert_eq!(tree.groups(), [0]);
        assert_eq!(tree.remove(0), None, "the last group stays");
    }

    #[test]
    fn the_opposite_group_is_the_other_side_of_the_nearest_split() {
        let mut tree = SplitTree::single();
        tree.split(0, false).unwrap();
        tree.split(1, true).unwrap();
        assert_eq!(tree.opposite(0), Some(1));
        assert_eq!(tree.opposite(1), Some(2));
        assert_eq!(tree.opposite(2), Some(1));
    }

    #[test]
    fn a_ratio_moves_within_its_limits_and_only_at_its_split() {
        let mut tree = SplitTree::single();
        tree.split(0, false).unwrap();
        tree.split(1, true).unwrap();
        assert_eq!(tree.ratio(&[]), Some(0.5));
        assert!(tree.set_ratio(&[], 0.7));
        assert_eq!(tree.ratio(&[]), Some(0.7));
        assert_eq!(tree.ratio(&[1]), Some(0.5), "the nested split stays");
        assert!(tree.set_ratio(&[], 0.95), "the limits hold");
        assert_eq!(tree.ratio(&[]), Some(0.9));
        assert!(!tree.set_ratio(&[], 0.95), "the share is already there");
    }

    #[test]
    fn a_layout_without_a_tree_opens_as_one_row_or_column() {
        let row = SplitTree::line(3, false);
        assert_eq!(row.groups(), [0, 1, 2]);
        assert_eq!(row.direction(0), Some(false));
        assert_eq!(row.direction(2), Some(false));
        assert!(row.ratio(&[]).unwrap() > 0.3 && row.ratio(&[]).unwrap() < 0.34);

        let column = SplitTree::line(2, true);
        assert_eq!(column.direction(0), Some(true));
        assert_eq!(column.direction(1), Some(true));
    }

    #[test]
    fn saving_and_reading_a_tree_keeps_the_groups_that_have_tabs() {
        let mut tree = SplitTree::single();
        tree.split(0, false).unwrap();
        tree.split(1, true).unwrap();
        // The middle group has no tabs: the layout joins the two that do.
        let layout = tree.layout(&[true, false, true]).unwrap();
        assert!(matches!(layout, SplitLayout::Split { down: false, .. }));
        let back = SplitTree::from_layout(&layout, 2).unwrap();
        assert_eq!(back.groups(), [0, 1]);
        assert_eq!(SplitTree::from_layout(&SplitLayout::Group, 2), None);
    }
}
