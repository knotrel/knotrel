//! Euler-tour sequences over a deterministic implicit AVL tree.
//!
//! Each vertex has one permanent token. Each tree edge has two directed tokens.
//! Rotating a tour and concatenating tours implements link; cutting at the two
//! directed tokens implements cut. See Holm, de Lichtenberg and Thorup (2001),
//! section 2: <https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf>
//! Dedicated HDT variant of our one-forest AVL implementation. Separate tree
//! and non-tree incidence bits allow O(log V) marked-vertex lookups without
//! enumerating a component. The original forest stays unchanged for comparisons.

#[derive(Debug)]
struct Token {
    left: Option<usize>,
    right: Option<usize>,
    parent: Option<usize>,
    height: usize,
    size: usize,
    vertices: usize,
    vertex: Option<usize>,
    candidate: u8,
    has_candidates: u8,
}
impl Token {
    fn new(vertex: Option<usize>) -> Self {
        Self {
            left: None,
            right: None,
            parent: None,
            height: 1,
            size: 1,
            vertices: usize::from(vertex.is_some()),
            vertex,
            candidate: 0,
            has_candidates: 0,
        }
    }
}

/// Stable handles; released edge tokens are reused before the arena grows.
#[derive(Debug, Default)]
pub(crate) struct Forest {
    tokens: Vec<Token>,
    free: Vec<usize>,
    vertex_tokens: Vec<usize>,
}
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

    pub(crate) fn add_vertex(&mut self) {
        let vertex = self.vertex_tokens.len();
        let token = self.allocate(Some(vertex));
        self.vertex_tokens.push(token);
    }
    fn allocate(&mut self, vertex: Option<usize>) -> usize {
        if let Some(i) = self.free.pop() {
            self.tokens[i] = Token::new(vertex);
            i
        } else {
            let i = self.tokens.len();
            self.tokens.push(Token::new(vertex));
            i
        }
    }
    fn height(&self, root: Option<usize>) -> usize {
        root.map_or(0, |i| self.tokens[i].height)
    }
    fn size(&self, root: Option<usize>) -> usize {
        root.map_or(0, |i| self.tokens[i].size)
    }
    fn vertices(&self, root: Option<usize>) -> usize {
        root.map_or(0, |i| self.tokens[i].vertices)
    }
    fn has_candidates(&self, root: Option<usize>) -> u8 {
        root.map_or(0, |i| self.tokens[i].has_candidates)
    }
    fn pull(&mut self, i: usize) {
        let (l, r) = (self.tokens[i].left, self.tokens[i].right);
        self.tokens[i].height = 1 + self.height(l).max(self.height(r));
        self.tokens[i].size = 1 + self.size(l) + self.size(r);
        self.tokens[i].vertices =
            usize::from(self.tokens[i].vertex.is_some()) + self.vertices(l) + self.vertices(r);
        self.tokens[i].has_candidates =
            self.tokens[i].candidate | self.has_candidates(l) | self.has_candidates(r);
        if let Some(l) = l {
            self.tokens[l].parent = Some(i);
        }
        if let Some(r) = r {
            self.tokens[r].parent = Some(i);
        }
        self.tokens[i].parent = None;
    }
    fn take_left(&mut self, i: usize) -> Option<usize> {
        let child = self.tokens[i].left.take();
        if let Some(c) = child {
            self.tokens[c].parent = None;
        }
        child
    }
    fn take_right(&mut self, i: usize) -> Option<usize> {
        let child = self.tokens[i].right.take();
        if let Some(c) = child {
            self.tokens[c].parent = None;
        }
        child
    }
    fn rotate_left(&mut self, i: usize) -> usize {
        let r = self.take_right(i).expect("right child");
        self.tokens[i].right = self.take_left(r);
        self.pull(i);
        self.tokens[r].left = Some(i);
        self.pull(r);
        r
    }
    fn rotate_right(&mut self, i: usize) -> usize {
        let l = self.take_left(i).expect("left child");
        self.tokens[i].left = self.take_right(l);
        self.pull(i);
        self.tokens[l].right = Some(i);
        self.pull(l);
        l
    }
    fn balance(&mut self, i: usize) -> usize {
        self.pull(i);
        let (l, r) = (self.tokens[i].left, self.tokens[i].right);
        if self.height(l) > self.height(r) + 1 {
            let left = l.expect("left heavy");
            if self.height(self.tokens[left].right) > self.height(self.tokens[left].left) {
                self.tokens[i].left = Some(self.rotate_left(left));
            }
            self.rotate_right(i)
        } else if self.height(r) > self.height(l) + 1 {
            let right = r.expect("right heavy");
            if self.height(self.tokens[right].left) > self.height(self.tokens[right].right) {
                self.tokens[i].right = Some(self.rotate_right(right));
            }
            self.rotate_left(i)
        } else {
            i
        }
    }
    /// AVL join descends the taller spine until heights differ by at most one.
    /// Rotations preserve sequence order. Cost O(1 + absolute height difference).
    fn join(&mut self, left: Option<usize>, pivot: usize, right: Option<usize>) -> usize {
        if self.height(left) > self.height(right) + 1 {
            let l = left.expect("taller left");
            let child = self.take_right(l);
            let root = self.join(child, pivot, right);
            self.tokens[l].right = Some(root);
            self.balance(l)
        } else if self.height(right) > self.height(left) + 1 {
            let r = right.expect("taller right");
            let child = self.take_left(r);
            let root = self.join(left, pivot, child);
            self.tokens[r].left = Some(root);
            self.balance(r)
        } else {
            self.tokens[pivot].left = left;
            self.tokens[pivot].right = right;
            self.pull(pivot);
            pivot
        }
    }
    /// Split before rank k; joining the untouched subtrees restores AVL balance.
    /// Join height differences telescope along the search path: O(log n) time.
    fn split(&mut self, root: Option<usize>, k: usize) -> (Option<usize>, Option<usize>) {
        let Some(i) = root else {
            debug_assert_eq!(k, 0);
            return (None, None);
        };
        let size = self.size(self.tokens[i].left);
        let left = self.take_left(i);
        let right = self.take_right(i);
        if k <= size {
            let (a, b) = self.split(left, k);
            let r = self.join(b, i, right);
            (a, Some(r))
        } else {
            let (a, b) = self.split(right, k - size - 1);
            let l = self.join(left, i, a);
            (Some(l), b)
        }
    }
    fn concat(&mut self, left: Option<usize>, right: Option<usize>) -> Option<usize> {
        match (left, right) {
            (None, r) => r,
            (l, None) => l,
            (Some(l), Some(r)) => {
                let (a, pivot) = self.split(Some(l), self.tokens[l].size - 1);
                Some(self.join(a, pivot.expect("last token"), Some(r)))
            }
        }
    }
    fn root(&self, mut i: usize) -> usize {
        while let Some(parent) = self.tokens[i].parent {
            i = parent;
        }
        i
    }
    fn rank(&self, mut i: usize) -> usize {
        let mut rank = self.size(self.tokens[i].left);
        while let Some(parent) = self.tokens[i].parent {
            if self.tokens[parent].right == Some(i) {
                rank += 1 + self.size(self.tokens[parent].left);
            }
            i = parent;
        }
        rank
    }
    fn reroot(&mut self, token: usize) -> Option<usize> {
        let root = self.root(token);
        let rank = self.rank(token);
        let (left, right) = self.split(Some(root), rank);
        self.concat(right, left)
    }
    pub(crate) fn connected(&self, a: usize, b: usize) -> bool {
        self.root(self.vertex_tokens[a]) == self.root(self.vertex_tokens[b])
    }
    pub(crate) fn link(&mut self, a: usize, b: usize) -> (usize, usize) {
        debug_assert!(!self.connected(a, b));
        let left = self.reroot(self.vertex_tokens[a]);
        let right = self.reroot(self.vertex_tokens[b]);
        let ab = self.allocate(None);
        let ba = self.allocate(None);
        // The new directed tokens are pivots: preserve the tour sequence
        // left, ab, right, ba without splitting tours to recover pivots.
        // Both joins retain AVL balance and O(log V) worst-case link cost.
        let tour = self.join(left, ab, right);
        self.join(Some(tour), ba, None);
        (ab, ba)
    }
    pub(crate) fn cut(&mut self, arcs: (usize, usize)) {
        let tour = self.reroot(arcs.0);
        let (_, rest) = self.split(tour, 1);
        let position = self.rank(arcs.1);
        let (_, tail) = self.split(rest, position);
        self.split(tail, 1);
        for i in [arcs.0, arcs.1] {
            debug_assert_eq!(self.tokens[i].size, 1);
            self.free.push(i);
        }
    }
    /// Update exact-level incidence bits and repair ancestor aggregates.
    /// Only vertices carry flags; structural operations preserve their OR.
    /// Cost O(log V), with early exit if an aggregate remains unchanged.
    pub(crate) fn set_marks(&mut self, vertex: usize, tree: bool, non_tree: bool) {
        let token = self.vertex_tokens[vertex];
        let value = (u8::from(tree) << 1) | u8::from(non_tree);
        if self.tokens[token].candidate == value {
            return;
        }
        self.tokens[token].candidate = value;
        let mut current = Some(token);
        while let Some(i) = current {
            let value = self.tokens[i].candidate
                | self.has_candidates(self.tokens[i].left)
                | self.has_candidates(self.tokens[i].right);
            if self.tokens[i].has_candidates == value {
                break;
            }
            self.tokens[i].has_candidates = value;
            current = self.tokens[i].parent;
        }
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
                return token.vertex;
            }
            current = if self.has_candidates(token.right) & mask != 0 {
                token.right.expect("marked right subtree")
            } else {
                token.left.expect("marked left subtree")
            };
        }
    }

    /// Number of vertices in this component, O(log V) worst case.
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
        use std::collections::BTreeSet;
        let free: BTreeSet<_> = self.free.iter().copied().collect();
        assert_eq!(free.len(), self.free.len());
        let roots: BTreeSet<_> = self.vertex_tokens.iter().map(|&i| self.root(i)).collect();
        let mut seen = BTreeSet::new();
        fn visit(
            f: &Forest,
            i: usize,
            parent: Option<usize>,
            seen: &mut BTreeSet<usize>,
        ) -> (usize, usize, usize, u8) {
            assert!(seen.insert(i));
            let t = &f.tokens[i];
            assert_eq!(t.parent, parent);
            let l = t.left.map_or((0, 0, 0, 0), |l| visit(f, l, Some(i), seen));
            let r = t.right.map_or((0, 0, 0, 0), |r| visit(f, r, Some(i), seen));
            assert!(l.0.abs_diff(r.0) <= 1);
            assert_eq!(
                (t.height, t.size, t.vertices),
                (
                    1 + l.0.max(r.0),
                    1 + l.1 + r.1,
                    usize::from(t.vertex.is_some()) + l.2 + r.2
                )
            );
            assert!(t.candidate == 0 || t.vertex.is_some());
            assert_eq!(t.has_candidates, t.candidate | l.3 | r.3);
            (t.height, t.size, t.vertices, t.has_candidates)
        }
        for root in roots {
            visit(self, root, None, &mut seen);
        }
        assert!(seen.is_disjoint(&free));
        assert_eq!(seen.len() + free.len(), self.tokens.len());
    }
}

#[cfg(test)]
mod tests {
    use super::Forest;
    #[test]
    fn rotations_splits_and_reused_arcs_preserve_invariants() {
        let mut f = Forest::default();
        for _ in 0..100 {
            f.add_vertex();
        }
        f.set_marks(0, true, false);
        f.set_marks(99, false, true);
        let mut edges = Vec::new();
        for i in 1..100 {
            edges.push(f.link(i - 1, i));
            f.validate();
        }
        for round in 0..20 {
            for (i, edge) in edges.iter_mut().enumerate() {
                f.cut(*edge);
                f.validate();
                assert!(!f.connected(i, i + 1));
                *edge = if round % 2 == 0 {
                    f.link(i + 1, i)
                } else {
                    f.link(i, i + 1)
                };
                f.validate();
            }
        }
        assert_eq!(f.tokens.len(), 100 + 198);
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
