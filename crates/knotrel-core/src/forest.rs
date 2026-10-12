//! Euler-tour sequences over a deterministic implicit AVL tree.
//!
//! Each vertex has one permanent token. Each tree edge has two directed tokens.
//! Rotating a tour and concatenating tours implements link; cutting at the two
//! directed tokens implements cut. See Holm, de Lichtenberg and Thorup (2001),
//! section 2: <https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf>
//! This module implements forest connectivity only, not HDT replacement levels.

pub(crate) type Forest = crate::index::GenericForest<bool>;

impl Forest {
    /// Update the vertex's non-tree incidence flag, repairing only affected
    /// ancestors. O(log n) worst case; stop when the subtree OR is unchanged.
    #[inline]
    pub(crate) fn set_candidate(&mut self, vertex: usize, enabled: bool) {
        self.update_candidate_flag(vertex, enabled);
    }

    /// Traverse only marked regions of the smaller tree, lazily. An empty tree
    /// of candidates returns without allocating. Stack space is O(log n).
    /// Yielding k vertices visits O(min(s, (k+1) log n)) tokens, where s is the
    /// smaller tree size. Dropping the iterator ends enumeration immediately.
    pub(crate) fn smaller_candidates(&self, a: usize, b: usize) -> Candidates<'_> {
        let a = self.root(self.vertex_tokens[a]);
        let b = self.root(self.vertex_tokens[b]);
        let root = if self.tokens[a].vertices <= self.tokens[b].vertices {
            a
        } else {
            b
        };
        let mut stack = tinyvec::TinyVec::new();
        if self.tokens[root].has_candidates {
            stack.push(root);
        }
        Candidates {
            forest: self,
            root,
            stack,
        }
    }

    #[cfg(test)]
    pub(crate) fn is_candidate(&self, vertex: usize) -> bool {
        self.tokens[self.vertex_tokens[vertex]].candidate
    }

    #[cfg(test)]
    pub(crate) fn validate(&self) {
        self.validate_base();
    }
}

/// Borrowing prevents forest rotations or candidate changes during a scan.
pub(crate) struct Candidates<'a> {
    forest: &'a Forest,
    root: usize,
    stack: tinyvec::TinyVec<[usize; 32]>,
}

impl Candidates<'_> {
    /// Tests membership in the fixed component selected for this scan.
    /// The immutable forest borrow keeps its root stable, even after iteration
    /// ends. Every yielded source is already in this component, so replacement
    /// search only needs the target's root. O(log V) worst case, O(1) scratch.
    pub(crate) fn contains_vertex(&self, vertex: usize) -> bool {
        self.forest.root(self.forest.vertex_tokens[vertex]) == self.root
    }
}

impl Iterator for Candidates<'_> {
    type Item = usize;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(i) = self.stack.pop() {
            let token = &self.forest.tokens[i];
            // Preserve v1's node/right/left traversal order among candidates.
            for child in [token.left.get(), token.right.get()].into_iter().flatten() {
                if self.forest.tokens[child].has_candidates {
                    self.stack.push(child);
                }
            }
            if token.candidate {
                return token.vertex.get();
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::Forest;

    #[test]
    fn membership_equals_connectivity_for_candidate_scan_incidences() {
        let mut f = Forest::default();
        for _ in 0..7 {
            f.add_vertex();
        }
        f.link(0, 1);
        f.link(1, 2);
        let bridge = f.link(2, 3);
        f.link(3, 4);
        f.link(4, 5);
        f.link(5, 6);
        f.cut(bridge);
        f.set_candidate(0, true);
        f.set_candidate(2, true);

        let mut scan = f.smaller_candidates(2, 3);
        let mut yielded = 0;
        while let Some(v) = scan.next() {
            yielded += 1;
            for w in 0..7 {
                assert_eq!(
                    scan.contains_vertex(w),
                    f.connected(v, w),
                    "source={v}, target={w}"
                );
            }
        }

        assert_eq!(yielded, 2);
        assert!(scan.contains_vertex(1)); // Internal candidate incidence.
        assert!(!scan.contains_vertex(3)); // Crossing candidate incidence.
    }

    #[test]
    fn candidate_search_iterates_only_marked_subtrees() {
        let mut f = Forest::default();
        for _ in 0..10 {
            f.add_vertex();
        }
        for i in 1..10 {
            f.link(i - 1, i);
        }
        f.set_candidate(3, true);
        f.set_candidate(7, true);
        assert!(f.is_candidate(3));
        assert!(f.is_candidate(7));
        assert!(!f.is_candidate(4));

        let candidates: Vec<_> = f.smaller_candidates(0, 9).collect();
        assert!(candidates.contains(&3));
        assert!(candidates.contains(&7));
        assert_eq!(candidates.len(), 2);

        f.set_candidate(3, false);
        let candidates: Vec<_> = f.smaller_candidates(0, 9).collect();
        assert_eq!(candidates, vec![7]);
    }
}

#[cfg(test)]
mod scan_root_tests {
    use super::Forest;
    #[test]
    fn membership_includes_unmarked_vertices_and_refreshes_after_mutations() {
        let mut f = Forest::default();
        for _ in 0..8 {
            f.add_vertex();
        }
        let ab = f.link(0, 1);
        f.link(1, 2);
        f.link(3, 4);
        f.link(4, 5);
        f.link(5, 6);
        f.set_candidate(0, true);
        {
            let mut scan = f.smaller_candidates(0, 3);
            assert!(scan.contains_vertex(2)); // Unmarked member still belongs.
            assert!(!scan.contains_vertex(3));
            assert!(!scan.contains_vertex(7));
            assert_eq!(scan.next(), Some(0));
            assert_eq!(scan.next(), None);
            assert!(scan.contains_vertex(1)); // Exhaustion does not invalidate root.
        }
        f.cut(ab);
        {
            let scan = f.smaller_candidates(0, 1);
            assert!(scan.contains_vertex(0));
            assert!(!scan.contains_vertex(1));
        }
        f.link(0, 3); // Rotate/merge, then choose the other component.
        {
            let scan = f.smaller_candidates(0, 1);
            assert!(scan.contains_vertex(2));
            assert!(!scan.contains_vertex(0));
        }
        f.link(1, 7); // The smaller component grows but remains smaller.
        let scan = f.smaller_candidates(1, 0);
        assert!(scan.contains_vertex(7));
        assert!(!scan.contains_vertex(3));
        f.validate();
    }
}
