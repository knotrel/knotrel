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
    stack: tinyvec::TinyVec<[usize; 32]>,
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
