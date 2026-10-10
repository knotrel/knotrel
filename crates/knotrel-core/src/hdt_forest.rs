//! Euler-tour sequences over a deterministic implicit AVL tree.
//!
//! Each vertex has one permanent token. Each tree edge has two directed tokens.
//! Rotating a tour and concatenating tours implements link; cutting at the two
//! directed tokens implements cut. See Holm, de Lichtenberg and Thorup (2001),
//! section 2: <https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf>
//! Dedicated HDT variant of our one-forest AVL implementation. Separate tree
//! and non-tree incidence bits allow O(log V) marked-vertex lookups without
//! enumerating a component. The original forest stays unchanged for comparisons.

use crate::index::Token as GenericToken;

type Token = GenericToken<u8>;

pub(crate) type Forest = crate::index::GenericForest<u8>;

impl Forest {
    pub(crate) fn storage(&self) -> crate::HdtLevelStorage {
        crate::HdtLevelStorage {
            vertices: self.vertex_tokens.len(),
            tree_edges: (self.tokens.len() - self.free.len() - self.vertex_tokens.len()) / 2,
            token_capacity_bytes: self.tokens.capacity() * size_of::<Token>(),
            vertex_index_capacity_bytes: self.vertex_tokens.capacity() * size_of::<usize>(),
            free_list_capacity_bytes: self.free.capacity() * size_of::<usize>(),
            ..Default::default()
        }
    }

    /// Update exact-level incidence bits and repair ancestor aggregates.
    /// Only vertices carry flags; structural operations preserve their OR.
    /// Cost O(log V), with early exit if an aggregate remains unchanged.
    #[inline]
    pub(crate) fn set_marks(&mut self, vertex: usize, tree: bool, non_tree: bool) {
        let value = (u8::from(tree) << 1) | u8::from(non_tree);
        self.update_candidate_flag(vertex, value);
    }

    /// Return one exact-level incidence-bearing vertex in this component.
    /// Descends one marked AVL path, O(log V) worst case, no scratch allocation.
    pub(crate) fn marked_vertex(&self, vertex: usize, tree: bool) -> Option<usize> {
        let mask = if tree { 2 } else { 1 };
        let mut current = self.root(self.vertex_tokens[vertex]);
        if self.tokens[current].has_candidates & mask == 0 {
            return None;
        }
        loop {
            let token = &self.tokens[current];
            if token.candidate & mask != 0 {
                return token.vertex.get();
            }
            current = if self.has_candidates(token.right.get()) & mask != 0 {
                token.right.get().expect("marked right subtree")
            } else {
                token.left.get().expect("marked left subtree")
            };
        }
    }

    /// Number of vertices in this component, O(log V) worst case.
    #[inline]
    pub(crate) fn component_size(&self, vertex: usize) -> usize {
        self.tokens[self.root(self.vertex_tokens[vertex])].vertices
    }

    #[cfg(test)]
    pub(crate) fn marks(&self, vertex: usize) -> (bool, bool) {
        let flags = self.tokens[self.vertex_tokens[vertex]].candidate;
        (flags & 2 != 0, flags & 1 != 0)
    }

    #[cfg(test)]
    pub(crate) fn validate(&self) {
        self.validate_base();
    }
}

#[cfg(test)]
mod tests {
    use super::Forest;

    #[test]
    fn exact_level_marks_and_vertex_lookup() {
        let mut f = Forest::default();
        for _ in 0..100 {
            f.add_vertex();
        }
        f.set_marks(0, true, false);
        f.set_marks(99, false, true);
        for i in 1..100 {
            f.link(i - 1, i);
        }
        f.validate();

        assert_eq!(f.marks(0), (true, false));
        assert_eq!(f.marks(99), (false, true));
        assert_eq!(f.marked_vertex(50, true), Some(0));
        assert_eq!(f.marked_vertex(50, false), Some(99));

        f.set_marks(0, false, false);
        assert_eq!(f.marked_vertex(50, true), None);
        assert_eq!(f.marked_vertex(50, false), Some(99));

        f.set_marks(99, false, false);
        assert_eq!(f.marked_vertex(50, false), None);
        f.validate();
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod compact_layout_tests {
    use super::{Forest, Token};

    #[test]
    fn token_budget_and_storage_accounting() {
        assert_eq!(size_of::<Token>(), 64);
        let mut f = Forest::default();
        for _ in 0..17 {
            f.add_vertex();
        }
        assert_eq!(f.storage().token_capacity_bytes, f.tokens.capacity() * 64);
    }
}
