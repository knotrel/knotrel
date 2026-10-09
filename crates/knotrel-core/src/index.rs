//! Compact optional arena index representation for tree and forest nodes.
//!
//! Replaces standard `Option<usize>` with a packed `Option<NonZeroUsize>` using
//! an `index + 1` encoding. This enables compiler niche-filling optimizations,
//! allowing optional indices to occupy the same space as a single pointer word
//! (e.g. 8 bytes on 64-bit architectures instead of 16 bytes with standard discriminant padding).

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

#[cfg(test)]
mod tests {
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
