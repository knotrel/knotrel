//! Euler-tour sequences over a deterministic implicit AVL tree.
//!
//! Each vertex has one permanent token. Each tree edge has two directed tokens.
//! Rotating a tour and concatenating tours implements link; cutting at the two
//! directed tokens implements cut. See Holm, de Lichtenberg and Thorup (2001),
//! section 2: <https://www.cs.princeton.edu/courses/archive/fall07/cos521/handouts/poly.pdf>
//! This module implements forest connectivity only, not HDT replacement levels.

/// Optional arena index encoded as index + 1, leaving zero for absence.
/// Every stored index names a Vec element, hence cannot be usize::MAX.
/// Checked encoding rejects invalid sentinel overflow; decoding is O(1).
#[derive(Clone, Copy, Debug, Default)]
struct Index(Option<std::num::NonZeroUsize>);
impl Index {
    const NONE: Self = Self(None);
    fn from(value: Option<usize>) -> Self {
        Self(value.map(|i| {
            std::num::NonZeroUsize::new(i.checked_add(1).expect("arena index overflow"))
                .expect("encoded index is nonzero")
        }))
    }
    fn get(self) -> Option<usize> {
        self.0.map(|i| i.get() - 1)
    }
    fn take(&mut self) -> Option<usize> {
        let old = self.get();
        *self = Self::NONE;
        old
    }
}

#[derive(Debug)]
struct Token {
    left: Index,
    right: Index,
    parent: Index,
    height: usize,
    size: usize,
    vertices: usize,
    vertex: Index,
    candidate: bool,
    has_candidates: bool,
}
impl Token {
    fn new(vertex: Option<usize>) -> Self {
        Self {
            left: Index::NONE,
            right: Index::NONE,
            parent: Index::NONE,
            height: 1,
            size: 1,
            vertices: usize::from(vertex.is_some()),
            vertex: Index::from(vertex),
            candidate: false,
            has_candidates: false,
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
    fn has_candidates(&self, root: Option<usize>) -> bool {
        root.is_some_and(|i| self.tokens[i].has_candidates)
    }
    fn pull(&mut self, i: usize) {
        let (l, r) = (self.tokens[i].left.get(), self.tokens[i].right.get());
        self.tokens[i].height = 1 + self.height(l).max(self.height(r));
        self.tokens[i].size = 1 + self.size(l) + self.size(r);
        self.tokens[i].vertices = usize::from(self.tokens[i].vertex.get().is_some())
            + self.vertices(l)
            + self.vertices(r);
        self.tokens[i].has_candidates =
            self.tokens[i].candidate || self.has_candidates(l) || self.has_candidates(r);
        if let Some(l) = l {
            self.tokens[l].parent = Index::from(Some(i));
        }
        if let Some(r) = r {
            self.tokens[r].parent = Index::from(Some(i));
        }
        self.tokens[i].parent = Index::from(None);
    }
    fn take_left(&mut self, i: usize) -> Option<usize> {
        let child = self.tokens[i].left.take();
        if let Some(c) = child {
            self.tokens[c].parent = Index::from(None);
        }
        child
    }
    fn take_right(&mut self, i: usize) -> Option<usize> {
        let child = self.tokens[i].right.take();
        if let Some(c) = child {
            self.tokens[c].parent = Index::from(None);
        }
        child
    }
    fn rotate_left(&mut self, i: usize) -> usize {
        let r = self.take_right(i).expect("right child");
        self.tokens[i].right = Index::from(self.take_left(r));
        self.pull(i);
        self.tokens[r].left = Index::from(Some(i));
        self.pull(r);
        r
    }
    fn rotate_right(&mut self, i: usize) -> usize {
        let l = self.take_left(i).expect("left child");
        self.tokens[i].left = Index::from(self.take_right(l));
        self.pull(i);
        self.tokens[l].right = Index::from(Some(i));
        self.pull(l);
        l
    }
    fn balance(&mut self, i: usize) -> usize {
        self.pull(i);
        let (l, r) = (self.tokens[i].left.get(), self.tokens[i].right.get());
        if self.height(l) > self.height(r) + 1 {
            let left = l.expect("left heavy");
            if self.height(self.tokens[left].right.get())
                > self.height(self.tokens[left].left.get())
            {
                self.tokens[i].left = Index::from(Some(self.rotate_left(left)));
            }
            self.rotate_right(i)
        } else if self.height(r) > self.height(l) + 1 {
            let right = r.expect("right heavy");
            if self.height(self.tokens[right].left.get())
                > self.height(self.tokens[right].right.get())
            {
                self.tokens[i].right = Index::from(Some(self.rotate_right(right)));
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
            self.tokens[l].right = Index::from(Some(root));
            self.balance(l)
        } else if self.height(right) > self.height(left) + 1 {
            let r = right.expect("taller right");
            let child = self.take_left(r);
            let root = self.join(left, pivot, child);
            self.tokens[r].left = Index::from(Some(root));
            self.balance(r)
        } else {
            self.tokens[pivot].left = Index::from(left);
            self.tokens[pivot].right = Index::from(right);
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
        let size = self.size(self.tokens[i].left.get());
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
        while let Some(parent) = self.tokens[i].parent.get() {
            i = parent;
        }
        i
    }
    fn rank(&self, mut i: usize) -> usize {
        let mut rank = self.size(self.tokens[i].left.get());
        while let Some(parent) = self.tokens[i].parent.get() {
            if self.tokens[parent].right.get() == Some(i) {
                rank += 1 + self.size(self.tokens[parent].left.get());
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
        let left = self.concat(left, Some(ab));
        let left = self.concat(left, right);
        self.concat(left, Some(ba));
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
    /// Update the vertex's non-tree incidence flag, repairing only affected
    /// ancestors. O(log n) worst case; stop when the subtree OR is unchanged.
    /// Unlike structural pull, this must preserve the existing parent pointers.
    pub(crate) fn set_candidate(&mut self, vertex: usize, enabled: bool) {
        let token = self.vertex_tokens[vertex];
        if self.tokens[token].candidate == enabled {
            return;
        }
        self.tokens[token].candidate = enabled;
        let mut current = Some(token);
        while let Some(i) = current {
            let value = self.tokens[i].candidate
                || self.has_candidates(self.tokens[i].left.get())
                || self.has_candidates(self.tokens[i].right.get());
            if self.tokens[i].has_candidates == value {
                break;
            }
            self.tokens[i].has_candidates = value;
            current = self.tokens[i].parent.get();
        }
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
        let mut stack = Vec::new();
        if self.tokens[root].has_candidates {
            stack.reserve_exact(self.tokens[root].height);
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
        ) -> (usize, usize, usize, bool) {
            assert!(seen.insert(i));
            let t = &f.tokens[i];
            assert_eq!(t.parent.get(), parent);
            let l = t
                .left
                .get()
                .map_or((0, 0, 0, false), |l| visit(f, l, Some(i), seen));
            let r = t
                .right
                .get()
                .map_or((0, 0, 0, false), |r| visit(f, r, Some(i), seen));
            assert!(l.0.abs_diff(r.0) <= 1);
            assert_eq!(
                (t.height, t.size, t.vertices),
                (
                    1 + l.0.max(r.0),
                    1 + l.1 + r.1,
                    usize::from(t.vertex.get().is_some()) + l.2 + r.2
                )
            );
            assert!(!t.candidate || t.vertex.get().is_some());
            assert_eq!(t.has_candidates, t.candidate || l.3 || r.3);
            (t.height, t.size, t.vertices, t.has_candidates)
        }
        for root in roots {
            visit(self, root, None, &mut seen);
        }
        assert!(seen.is_disjoint(&free));
        assert_eq!(seen.len() + free.len(), self.tokens.len());
    }
}

/// Borrowing prevents forest rotations or candidate changes during a scan.
pub(crate) struct Candidates<'a> {
    forest: &'a Forest,
    stack: Vec<usize>,
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
    fn rotations_splits_and_reused_arcs_preserve_invariants() {
        let mut f = Forest::default();
        for _ in 0..100 {
            f.add_vertex();
        }
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
    }
}

#[cfg(all(test, target_pointer_width = "64"))]
mod compact_layout_tests {
    use super::Token;
    #[test]
    fn token_budget() {
        assert_eq!(size_of::<Token>(), 64);
    }
}

#[cfg(test)]
mod index_tests {
    use super::Index;
    #[test]
    fn optional_indices_round_trip_and_clear() {
        assert_eq!(size_of::<Index>(), size_of::<usize>());
        for value in [None, Some(0), Some(1), Some(usize::MAX - 1)] {
            let mut index = Index::from(value);
            assert_eq!(index.get(), value);
            assert_eq!(index.take(), value);
            assert_eq!(index.get(), None);
        }
    }
    #[test]
    #[should_panic(expected = "arena index overflow")]
    fn rejects_unrepresentable_index_without_wrapping() {
        Index::from(Some(usize::MAX));
    }
}
