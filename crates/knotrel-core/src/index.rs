//! Compact optional arena index and generic Euler-tour AVL forest representations.
//!
//! Replaces standard `Option<usize>` with a packed `Option<NonZeroUsize>` using
//! an `index + 1` encoding. This enables compiler niche-filling optimizations,
//! allowing optional indices to occupy the same space as a single pointer word
//! (e.g. 8 bytes on 64-bit architectures instead of 16 bytes with standard discriminant padding).
//!
//! Also provides [`GenericForest`], the shared arena-backed AVL tree infrastructure
//! powering both deterministic spanning forests ([`crate::ForestGraph`]) and HDT
//! hierarchical forests ([`crate::HdtGraph`]).

use std::num::NonZeroUsize;

/// Optional arena index encoded as `index + 1`, reserving zero to denote absence.
///
/// Because stored indices reference elements of an in-memory `Vec`, valid indices
/// never equal `usize::MAX`. The checked encoding explicitly guards against overflow
/// and rejects `usize::MAX` without wrapping.
///
/// Encoding, access, and clearing are worst-case $O(1)$ operations with zero allocation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Index(Option<NonZeroUsize>);

impl Index {
    /// Sentinel constant representing an empty (absent) index.
    pub(crate) const NONE: Self = Self(None);

    /// Encodes an optional 0-based arena index into an `Index`.
    ///
    /// # Panics
    ///
    /// Panics if `value` equals `Some(usize::MAX)`, preventing integer overflow
    /// and sentinel collision.
    pub(crate) fn from(value: Option<usize>) -> Self {
        Self(value.map(|i| {
            NonZeroUsize::new(i.checked_add(1).expect("arena index overflow"))
                .expect("encoded index is nonzero")
        }))
    }

    /// Decodes the index to an optional 0-based arena offset.
    #[inline]
    pub(crate) fn get(self) -> Option<usize> {
        self.0.map(|i| i.get() - 1)
    }

    /// Takes the value out of the index, leaving [`Index::NONE`] in its place.
    #[inline]
    pub(crate) fn take(&mut self) -> Option<usize> {
        let old = self.get();
        *self = Self::NONE;
        old
    }
}

/// Compact 64-byte AVL tour token representing a directed edge or permanent vertex arc.
///
/// Stores left, right, and parent tree indices as compact 8-byte [`Index`] instances,
/// alongside subtree height, size, vertex counters, and candidate flags of type `C`.
///
/// On 64-bit architectures, this struct occupies exactly 64 bytes (one L1 cache line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Token<C> {
    pub(crate) left: Index,
    pub(crate) right: Index,
    pub(crate) parent: Index,
    pub(crate) height: usize,
    pub(crate) size: usize,
    pub(crate) vertices: usize,
    pub(crate) vertex: Index,
    pub(crate) candidate: C,
    pub(crate) has_candidates: C,
}

impl<C: Default> Token<C> {
    /// Creates a newly initialized leaf token with unit size and height.
    #[inline]
    pub(crate) fn new(vertex: Option<usize>) -> Self {
        Self {
            left: Index::NONE,
            right: Index::NONE,
            parent: Index::NONE,
            height: 1,
            size: 1,
            vertices: usize::from(vertex.is_some()),
            vertex: Index::from(vertex),
            candidate: C::default(),
            has_candidates: C::default(),
        }
    }
}

/// Trait for aggregating candidate or marked vertex flags across AVL Euler-tour subtrees.
pub(crate) trait CandidateTracker:
    Copy + Default + PartialEq + Eq + std::fmt::Debug
{
    /// Combines node flags with aggregated child flags.
    fn combine(self, left: Self, right: Self) -> Self;

    /// Returns `true` if any candidate or mark flags are active.
    #[cfg(test)]
    fn has_any(self) -> bool;
}

impl CandidateTracker for bool {
    #[inline]
    fn combine(self, left: Self, right: Self) -> Self {
        self || left || right
    }

    #[cfg(test)]
    #[inline]
    fn has_any(self) -> bool {
        self
    }
}

impl CandidateTracker for u8 {
    #[inline]
    fn combine(self, left: Self, right: Self) -> Self {
        self | left | right
    }

    #[cfg(test)]
    #[inline]
    fn has_any(self) -> bool {
        self != 0
    }
}

/// Generic arena-backed Euler-tour forest parameterized over candidate flag type `C`.
///
/// Each vertex has one permanent token. Each tree edge has two directed tokens.
/// Released edge tokens are returned to a free list and reused before the arena grows.
#[derive(Debug)]
pub(crate) struct GenericForest<C: CandidateTracker> {
    pub(crate) tokens: Vec<Token<C>>,
    pub(crate) free: Vec<usize>,
    pub(crate) vertex_tokens: Vec<usize>,
}

impl<C: CandidateTracker> Default for GenericForest<C> {
    fn default() -> Self {
        Self {
            tokens: Vec::new(),
            free: Vec::new(),
            vertex_tokens: Vec::new(),
        }
    }
}

impl<C: CandidateTracker> GenericForest<C> {
    pub(crate) fn add_vertex(&mut self) {
        let vertex = self.vertex_tokens.len();
        let token = self.allocate(Some(vertex));
        self.vertex_tokens.push(token);
    }

    pub(crate) fn allocate(&mut self, vertex: Option<usize>) -> usize {
        if let Some(i) = self.free.pop() {
            self.tokens[i] = Token::new(vertex);
            i
        } else {
            let i = self.tokens.len();
            self.tokens.push(Token::new(vertex));
            i
        }
    }

    pub(crate) fn height(&self, root: Option<usize>) -> usize {
        root.map_or(0, |i| self.tokens[i].height)
    }

    pub(crate) fn size(&self, root: Option<usize>) -> usize {
        root.map_or(0, |i| self.tokens[i].size)
    }

    pub(crate) fn vertices(&self, root: Option<usize>) -> usize {
        root.map_or(0, |i| self.tokens[i].vertices)
    }

    pub(crate) fn has_candidates(&self, root: Option<usize>) -> C {
        root.map_or(C::default(), |i| self.tokens[i].has_candidates)
    }

    pub(crate) fn pull(&mut self, i: usize) {
        let (l, r) = (self.tokens[i].left.get(), self.tokens[i].right.get());
        self.tokens[i].height = 1 + self.height(l).max(self.height(r));
        self.tokens[i].size = 1 + self.size(l) + self.size(r);
        self.tokens[i].vertices = usize::from(self.tokens[i].vertex.get().is_some())
            + self.vertices(l)
            + self.vertices(r);
        self.tokens[i].has_candidates = self.tokens[i]
            .candidate
            .combine(self.has_candidates(l), self.has_candidates(r));
        if let Some(l) = l {
            self.tokens[l].parent = Index::from(Some(i));
        }
        if let Some(r) = r {
            self.tokens[r].parent = Index::from(Some(i));
        }
        self.tokens[i].parent = Index::from(None);
    }

    pub(crate) fn take_left(&mut self, i: usize) -> Option<usize> {
        let child = self.tokens[i].left.take();
        if let Some(c) = child {
            self.tokens[c].parent = Index::from(None);
        }
        child
    }

    pub(crate) fn take_right(&mut self, i: usize) -> Option<usize> {
        let child = self.tokens[i].right.take();
        if let Some(c) = child {
            self.tokens[c].parent = Index::from(None);
        }
        child
    }

    pub(crate) fn rotate_left(&mut self, i: usize) -> usize {
        let r = self.take_right(i).expect("right child");
        self.tokens[i].right = Index::from(self.take_left(r));
        self.pull(i);
        self.tokens[r].left = Index::from(Some(i));
        self.pull(r);
        r
    }

    pub(crate) fn rotate_right(&mut self, i: usize) -> usize {
        let l = self.take_left(i).expect("left child");
        self.tokens[i].left = Index::from(self.take_right(l));
        self.pull(i);
        self.tokens[l].right = Index::from(Some(i));
        self.pull(l);
        l
    }

    pub(crate) fn balance(&mut self, i: usize) -> usize {
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
    pub(crate) fn join(
        &mut self,
        left: Option<usize>,
        pivot: usize,
        right: Option<usize>,
    ) -> usize {
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
    pub(crate) fn split(
        &mut self,
        root: Option<usize>,
        k: usize,
    ) -> (Option<usize>, Option<usize>) {
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

    pub(crate) fn concat(&mut self, left: Option<usize>, right: Option<usize>) -> Option<usize> {
        match (left, right) {
            (None, r) => r,
            (l, None) => l,
            (Some(l), Some(r)) => {
                let (a, pivot) = self.split(Some(l), self.tokens[l].size - 1);
                Some(self.join(a, pivot.expect("last token"), Some(r)))
            }
        }
    }

    pub(crate) fn root(&self, mut i: usize) -> usize {
        while let Some(parent) = self.tokens[i].parent.get() {
            i = parent;
        }
        i
    }

    pub(crate) fn rank(&self, mut i: usize) -> usize {
        let mut rank = self.size(self.tokens[i].left.get());
        while let Some(parent) = self.tokens[i].parent.get() {
            if self.tokens[parent].right.get() == Some(i) {
                rank += 1 + self.size(self.tokens[parent].left.get());
            }
            i = parent;
        }
        rank
    }

    pub(crate) fn reroot(&mut self, token: usize) -> Option<usize> {
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

    /// Update the vertex's incidence flag, repairing only affected ancestors.
    /// O(log n) worst case; stop when the subtree aggregate is unchanged.
    pub(crate) fn update_candidate_flag(&mut self, vertex: usize, value: C) {
        let token = self.vertex_tokens[vertex];
        if self.tokens[token].candidate == value {
            return;
        }
        self.tokens[token].candidate = value;
        let mut current = Some(token);
        while let Some(i) = current {
            let new_agg = self.tokens[i].candidate.combine(
                self.has_candidates(self.tokens[i].left.get()),
                self.has_candidates(self.tokens[i].right.get()),
            );
            if self.tokens[i].has_candidates == new_agg {
                break;
            }
            self.tokens[i].has_candidates = new_agg;
            current = self.tokens[i].parent.get();
        }
    }

    #[cfg(test)]
    pub(crate) fn validate_base(&self) {
        use std::collections::BTreeSet;
        let free: BTreeSet<_> = self.free.iter().copied().collect();
        assert_eq!(free.len(), self.free.len());
        let roots: BTreeSet<_> = self.vertex_tokens.iter().map(|&i| self.root(i)).collect();
        let mut seen = BTreeSet::new();
        fn visit<C: CandidateTracker>(
            f: &GenericForest<C>,
            i: usize,
            parent: Option<usize>,
            seen: &mut BTreeSet<usize>,
        ) -> (usize, usize, usize, C) {
            assert!(seen.insert(i));
            let t = &f.tokens[i];
            assert_eq!(t.parent.get(), parent);
            let l = t
                .left
                .get()
                .map_or((0, 0, 0, C::default()), |l| visit(f, l, Some(i), seen));
            let r = t
                .right
                .get()
                .map_or((0, 0, 0, C::default()), |r| visit(f, r, Some(i), seen));
            assert!(l.0.abs_diff(r.0) <= 1);
            assert_eq!(
                (t.height, t.size, t.vertices),
                (
                    1 + l.0.max(r.0),
                    1 + l.1 + r.1,
                    usize::from(t.vertex.get().is_some()) + l.2 + r.2
                )
            );
            assert!(!t.candidate.has_any() || t.vertex.get().is_some());
            assert_eq!(t.has_candidates, t.candidate.combine(l.3, r.3));
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
    use super::{GenericForest, Index};

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

    #[test]
    fn token_layout_occupies_cache_line() {
        assert_eq!(size_of::<super::Token<bool>>(), 64);
        assert_eq!(size_of::<super::Token<u8>>(), 64);
    }

    #[test]
    fn rotations_splits_and_reused_arcs_preserve_invariants() {
        let mut f = GenericForest::<bool>::default();
        for _ in 0..100 {
            f.add_vertex();
        }
        let mut edges = Vec::new();
        for i in 1..100 {
            edges.push(f.link(i - 1, i));
            f.validate_base();
        }
        for round in 0..20 {
            for (i, edge) in edges.iter_mut().enumerate() {
                f.cut(*edge);
                f.validate_base();
                assert!(!f.connected(i, i + 1));
                *edge = if round % 2 == 0 {
                    f.link(i + 1, i)
                } else {
                    f.link(i, i + 1)
                };
                f.validate_base();
            }
        }
        assert_eq!(f.tokens.len(), 100 + 198);
    }
}
