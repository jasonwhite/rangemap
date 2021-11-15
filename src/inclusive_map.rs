use super::range_wrapper::RangeInclusiveStartWrapper;
use crate::std_ext::*;
use alloc::collections::BTreeMap;
use core::fmt::{self, Debug};
use core::iter::FromIterator;
use core::marker::PhantomData;
use core::ops::RangeInclusive;
use core::prelude::v1::*;

#[cfg(feature = "serde1")]
use serde::{
    de::{Deserialize, Deserializer, SeqAccess, Visitor},
    ser::{Serialize, Serializer},
};

/// A map whose keys are stored as ranges bounded
/// inclusively below and above `(start..=end)`.
///
/// Contiguous and overlapping ranges that map to the same value
/// are coalesced into a single range.
///
/// Successor and predecessor functions must be provided for
/// the key type `K`, so that we can detect adjacent but non-overlapping
/// (closed) ranges. (This is not a problem for half-open ranges,
/// because adjacent ranges can be detected using equality of range ends alone.)
///
/// You can provide these functions either by implementing the
/// [`StepLite`](crate::StepLite) trait for your key type `K`, or,
/// if this is impossible because of Rust's "orphan rules",
/// you can provide equivalent free functions using the `StepFnsT` type parameter.
/// [`StepLite`](crate::StepLite) is implemented for all standard integer types,
/// but not for any third party crate types.
#[derive(Clone)]
pub struct RangeInclusiveMap<K, V, StepFnsT = K> {
    // Wrap ranges so that they are `Ord`.
    // See `range_wrapper.rs` for explanation.
    pub(crate) btm: BTreeMap<RangeInclusiveStartWrapper<K>, V>,
    _phantom: PhantomData<StepFnsT>,
}

impl<K, V> Default for RangeInclusiveMap<K, V, K>
where
    K: Ord + Clone + StepLite,
    V: Eq + Clone,
{
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> RangeInclusiveMap<K, V, K>
where
    K: Ord + Clone + StepLite,
    V: Eq + Clone,
{
    /// Makes a new empty `RangeInclusiveMap`.
    pub fn new() -> Self {
        Self::new_with_step_fns()
    }
}

impl<K, V, StepFnsT> RangeInclusiveMap<K, V, StepFnsT>
where
    K: Ord + Clone,
    V: Eq + Clone,
    StepFnsT: StepFns<K>,
{
    /// Makes a new empty `RangeInclusiveMap`, specifying successor and
    /// predecessor functions defined separately from `K` itself.
    ///
    /// This is useful as a workaround for Rust's "orphan rules",
    /// which prevent you from implementing `StepLite` for `K` if `K`
    /// is a foreign type.
    ///
    /// **NOTE:** This will likely be deprecated and then eventually
    /// removed once the standard library's [Step](core::iter::Step)
    /// trait is stabilised, as most crates will then likely implement [Step](core::iter::Step)
    /// for their types where appropriate.
    ///
    /// See [this issue](https://github.com/rust-lang/rust/issues/42168)
    /// for details about that stabilization process.
    pub fn new_with_step_fns() -> Self {
        Self {
            btm: BTreeMap::new(),
            _phantom: PhantomData,
        }
    }

    /// Returns a reference to the value corresponding to the given key,
    /// if the key is covered by any range in the map.
    pub fn get(&self, key: &K) -> Option<&V> {
        self.get_key_value(key).map(|(_range, value)| value)
    }

    /// Returns the range-value pair (as a pair of references) corresponding
    /// to the given key, if the key is covered by any range in the map.
    pub fn get_key_value(&self, key: &K) -> Option<(&RangeInclusive<K>, &V)> {
        use core::ops::Bound;

        // The only stored range that could contain the given key is the
        // last stored range whose start is less than or equal to this key.
        let key_as_start = RangeInclusiveStartWrapper::new(key.clone()..=key.clone());
        self.btm
            .range((Bound::Unbounded, Bound::Included(key_as_start)))
            .next_back()
            .filter(|(range_start_wrapper, _value)| {
                // Does the only candidate range contain
                // the requested key?
                range_start_wrapper.range.contains(key)
            })
            .map(|(range_start_wrapper, value)| (&range_start_wrapper.range, value))
    }

    /// Returns `true` if any range in the map covers the specified key.
    pub fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    /// Gets an iterator over all pairs of key range and value,
    /// ordered by key range.
    ///
    /// The iterator element type is `(&'a RangeInclusive<K>, &'a V)`.
    pub fn iter(&self) -> Iter<'_, K, V> {
        Iter {
            inner: self.btm.iter(),
        }
    }

    /// Insert a pair of key range and value into the map.
    ///
    /// If the inserted range partially or completely overlaps any
    /// existing range in the map, then the existing range (or ranges) will be
    /// partially or completely replaced by the inserted range.
    ///
    /// If the inserted range either overlaps or is immediately adjacent
    /// any existing range _mapping to the same value_, then the ranges
    /// will be coalesced into a single contiguous range.
    ///
    /// # Panics
    ///
    /// Panics if range `start > end`.
    pub fn insert(&mut self, range: RangeInclusive<K>, value: V) {
        use core::ops::Bound;

        // Backwards ranges don't make sense.
        // `RangeInclusive` doesn't enforce this,
        // and we don't want weird explosions further down
        // if someone gives us such a range.
        assert!(
            range.start() <= range.end(),
            "Range start can not be after range end"
        );

        // Wrap up the given range so that we can "borrow"
        // it as a wrapper reference to either its start or end.
        // See `range_wrapper.rs` for explanation of these hacks.
        let mut new_range_start_wrapper: RangeInclusiveStartWrapper<K> =
            RangeInclusiveStartWrapper::new(range);
        let new_value = value;

        // Is there a stored range either overlapping the start of
        // the range to insert or immediately preceding it?
        //
        // If there is any such stored range, it will be the last
        // whose start is less than or equal to _one less than_
        // the start of the range to insert, or the one before that
        // if both of the above cases exist.
        let mut candidates = self
            .btm
            .range((Bound::Unbounded, Bound::Included(&new_range_start_wrapper)))
            .rev()
            .take(2)
            .filter(|(stored_range_start_wrapper, _stored_value)| {
                // Does the candidate range either overlap
                // or immediately precede the range to insert?
                // (Remember that it might actually cover the _whole_
                // range to insert and then some.)
                stored_range_start_wrapper
                    .range
                    .touches::<StepFnsT>(&new_range_start_wrapper.range)
            });
        if let Some(mut candidate) = candidates.next() {
            // Or the one before it if both cases described above exist.
            if let Some(another_candidate) = candidates.next() {
                candidate = another_candidate;
            }
            let (stored_range_start_wrapper, stored_value) =
                (candidate.0.clone(), candidate.1.clone());
            self.adjust_touching_ranges_for_insert(
                stored_range_start_wrapper,
                stored_value,
                &mut new_range_start_wrapper.range,
                &new_value,
            );
        }

        // Are there any stored ranges whose heads overlap or immediately
        // follow the range to insert?
        //
        // If there are any such stored ranges (that weren't already caught above),
        // their starts will fall somewhere after the start of the range to insert,
        // and on, before, or _immediately after_ its end. To handle that last case
        // without risking arithmetic overflow, we'll consider _one more_ stored item past
        // the end of the end of the range to insert.
        //
        // REVISIT: Possible micro-optimisation: `impl Borrow<T> for RangeInclusiveStartWrapper<T>`
        // and use that to search here, to avoid constructing another `RangeInclusiveStartWrapper`.
        let second_last_possible_start = new_range_start_wrapper.range.end().clone();
        let second_last_possible_start = RangeInclusiveStartWrapper::new(
            second_last_possible_start.clone()..=second_last_possible_start,
        );
        while let Some((stored_range_start_wrapper, stored_value)) = self
            .btm
            .range((
                Bound::Included(&new_range_start_wrapper),
                // We would use something like `Bound::Included(&last_possible_start)`,
                // but making `last_possible_start` might cause arithmetic overflow;
                // instead decide inside the loop whether we've gone too far and break.
                Bound::Unbounded,
            ))
            .next()
        {
            // A couple of extra exceptions are needed at the
            // end of the subset of stored ranges we want to consider,
            // in part because we use `Bound::Unbounded` above.
            // (See comments up there, and in the individual cases below.)
            let stored_start = stored_range_start_wrapper.range.start();
            if *stored_start > *second_last_possible_start.range.start() {
                let latest_possible_start =
                    StepFnsT::add_one(second_last_possible_start.range.start());
                if *stored_start > latest_possible_start {
                    // We're beyond the last stored range that could be relevant.
                    // Avoid wasting time on irrelevant ranges, or even worse, looping forever.
                    // (`adjust_touching_ranges_for_insert` below assumes that the given range
                    // is relevant, and behaves very poorly if it is handed a range that it
                    // shouldn't be touching.)
                    break;
                }

                if *stored_start == latest_possible_start && *stored_value != new_value {
                    // We are looking at the last stored range that could be relevant,
                    // but it has a different value, so we don't want to merge with it.
                    // We must explicitly break here as well, because `adjust_touching_ranges_for_insert`
                    // below assumes that the given range is relevant, and behaves very poorly if it
                    // is handed a range that it shouldn't be touching.
                    break;
                }
            }

            let stored_range_start_wrapper = stored_range_start_wrapper.clone();
            let stored_value = stored_value.clone();

            self.adjust_touching_ranges_for_insert(
                stored_range_start_wrapper,
                stored_value,
                &mut new_range_start_wrapper.range,
                &new_value,
            );
        }

        // Insert the (possibly expanded) new range, and we're done!
        self.btm.insert(new_range_start_wrapper, new_value);
    }

    /// Removes a range from the map, if all or any of it was present.
    ///
    /// If the range to be removed _partially_ overlaps any ranges
    /// in the map, then those ranges will be contracted to no
    /// longer cover the removed range.
    ///
    ///
    /// # Panics
    ///
    /// Panics if range `start > end`.
    pub fn remove(&mut self, range: RangeInclusive<K>) {
        use core::ops::Bound;

        // Backwards ranges don't make sense.
        // `RangeInclusive` doesn't enforce this,
        // and we don't want weird explosions further down
        // if someone gives us such a range.
        assert!(
            range.start() <= range.end(),
            "Range start can not be after range end"
        );

        let range_start_wrapper: RangeInclusiveStartWrapper<K> =
            RangeInclusiveStartWrapper::new(range);
        let range = &range_start_wrapper.range;

        // Is there a stored range overlapping the start of
        // the range to insert?
        //
        // If there is any such stored range, it will be the last
        // whose start is less than or equal to the start of the range to insert.
        if let Some((stored_range_start_wrapper, stored_value)) = self
            .btm
            .range((Bound::Unbounded, Bound::Included(&range_start_wrapper)))
            .next_back()
            .filter(|(stored_range_start_wrapper, _stored_value)| {
                // Does the only candidate range overlap
                // the range to insert?
                stored_range_start_wrapper.range.overlaps(&range)
            })
            .map(|(stored_range_start_wrapper, stored_value)| {
                (stored_range_start_wrapper.clone(), stored_value.clone())
            })
        {
            self.adjust_overlapping_ranges_for_remove(
                stored_range_start_wrapper,
                stored_value,
                &range,
            );
        }

        // Are there any stored ranges whose heads overlap the range to insert?
        //
        // If there are any such stored ranges (that weren't already caught above),
        // their starts will fall somewhere after the start of the range to insert,
        // and on or before its end.
        //
        // REVISIT: Possible micro-optimisation: `impl Borrow<T> for RangeInclusiveStartWrapper<T>`
        // and use that to search here, to avoid constructing another `RangeInclusiveStartWrapper`.
        let new_range_end_as_start =
            RangeInclusiveStartWrapper::new(range.end().clone()..=range.end().clone());
        while let Some((stored_range_start_wrapper, stored_value)) = self
            .btm
            .range((
                Bound::Excluded(&range_start_wrapper),
                Bound::Included(&new_range_end_as_start),
            ))
            .next()
            .map(|(stored_range_start_wrapper, stored_value)| {
                (stored_range_start_wrapper.clone(), stored_value.clone())
            })
        {
            self.adjust_overlapping_ranges_for_remove(
                stored_range_start_wrapper,
                stored_value,
                &range,
            );
        }
    }

    fn adjust_touching_ranges_for_insert(
        &mut self,
        stored_range_start_wrapper: RangeInclusiveStartWrapper<K>,
        stored_value: V,
        new_range: &mut RangeInclusive<K>,
        new_value: &V,
    ) {
        use core::cmp::{max, min};

        if stored_value == *new_value {
            // The ranges have the same value, so we can "adopt"
            // the stored range.
            //
            // This means that no matter how big or where the stored range is,
            // we will expand the new range's bounds to subsume it,
            // and then delete the stored range.
            let new_start =
                min(new_range.start(), stored_range_start_wrapper.range.start()).clone();
            let new_end = max(new_range.end(), stored_range_start_wrapper.range.end()).clone();
            *new_range = new_start..=new_end;
            self.btm.remove(&stored_range_start_wrapper);
        } else {
            // The ranges have different values.
            if new_range.overlaps(&stored_range_start_wrapper.range) {
                // The ranges overlap. This is a little bit more complicated.
                // Delete the stored range, and then add back between
                // 0 and 2 subranges at the ends of the range to insert.
                self.btm.remove(&stored_range_start_wrapper);
                if stored_range_start_wrapper.range.start() < new_range.start() {
                    // Insert the piece left of the range to insert.
                    self.btm.insert(
                        RangeInclusiveStartWrapper::new(
                            stored_range_start_wrapper.range.start().clone()
                                ..=StepFnsT::sub_one(new_range.start()),
                        ),
                        stored_value.clone(),
                    );
                }
                if stored_range_start_wrapper.range.end() > new_range.end() {
                    // Insert the piece right of the range to insert.
                    self.btm.insert(
                        RangeInclusiveStartWrapper::new(
                            StepFnsT::add_one(new_range.end())
                                ..=stored_range_start_wrapper.range.end().clone(),
                        ),
                        stored_value,
                    );
                }
            } else {
                // No-op; they're not overlapping,
                // so we can just keep both ranges as they are.
            }
        }
    }

    fn adjust_overlapping_ranges_for_remove(
        &mut self,
        stored_range_start_wrapper: RangeInclusiveStartWrapper<K>,
        stored_value: V,
        range_to_remove: &RangeInclusive<K>,
    ) {
        // Delete the stored range, and then add back between
        // 0 and 2 subranges at the ends of the range to insert.
        self.btm.remove(&stored_range_start_wrapper);
        let stored_range = stored_range_start_wrapper.range;
        if stored_range.start() < range_to_remove.start() {
            // Insert the piece left of the range to insert.
            self.btm.insert(
                RangeInclusiveStartWrapper::new(
                    stored_range.start().clone()..=StepFnsT::sub_one(range_to_remove.start()),
                ),
                stored_value.clone(),
            );
        }
        if stored_range.end() > range_to_remove.end() {
            // Insert the piece right of the range to insert.
            self.btm.insert(
                RangeInclusiveStartWrapper::new(
                    StepFnsT::add_one(range_to_remove.end())..=stored_range.end().clone(),
                ),
                stored_value,
            );
        }
    }

    /// Gets an iterator over all the maximally-sized ranges
    /// contained in `outer_range` that are not covered by
    /// any range stored in the map.
    ///
    /// The iterator element type is `RangeInclusive<K>`.
    ///
    /// NOTE: Calling `gaps` eagerly finds the first gap,
    /// even if the iterator is never consumed.
    pub fn gaps<'a>(&'a self, outer_range: &'a RangeInclusive<K>) -> Gaps<'a, K, V, StepFnsT> {
        let mut keys = self.btm.keys().peekable();

        // Find the first potential gap.
        let mut candidate_start = outer_range.start().clone();
        // We might be already done from the start,
        // but not be able to represent it using
        // `candidate_start` alone if we're at the end
        // of the key domain.
        let mut done = false;
        while let Some(item) = keys.peek() {
            if item.range.end() < outer_range.start() {
                // This range sits entirely before the start of
                // the outer range; just skip it.
                let _ = keys.next();
            } else if item.range.start() <= outer_range.start() {
                // This range overlaps the start of the
                // outer range, so the first possible candidate
                // range begins immediately after its end.
                if item.range.end() >= outer_range.end() {
                    // There's a risk of overflowing;
                    // use our extra "done" flag to represent
                    // that the iterator is already done.
                    // (Don't worry about `candidate_start`
                    // we'll ignore everything else if `done`
                    // is `true`.)
                    done = true;
                } else {
                    candidate_start = StepFnsT::add_one(item.range.end());
                }
                let _ = keys.next();
            } else {
                // The rest of the items might contribute to gaps.
                break;
            }
        }

        Gaps {
            done,
            outer_range,
            keys,
            candidate_start,
            _phantom: PhantomData,
        }
    }
}

/// An iterator over the entries of a `RangeInclusiveMap`, ordered by key range.
///
/// The iterator element type is `(&'a RangeInclusive<K>, &'a V)`.
///
/// This `struct` is created by the [`iter`] method on [`RangeInclusiveMap`]. See its
/// documentation for more.
///
/// [`iter`]: RangeInclusiveMap::iter
pub struct Iter<'a, K, V> {
    inner: alloc::collections::btree_map::Iter<'a, RangeInclusiveStartWrapper<K>, V>,
}

impl<'a, K, V> Iterator for Iter<'a, K, V>
where
    K: 'a,
    V: 'a,
{
    type Item = (&'a RangeInclusive<K>, &'a V);

    fn next(&mut self) -> Option<(&'a RangeInclusive<K>, &'a V)> {
        self.inner.next().map(|(by_start, v)| (&by_start.range, v))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

/// An owning iterator over the entries of a `RangeInclusiveMap`, ordered by key range.
///
/// The iterator element type is `(RangeInclusive<K>, V)`.
///
/// This `struct` is created by the [`into_iter`] method on [`RangeInclusiveMap`]
/// (provided by the `IntoIterator` trait). See its documentation for more.
///
/// [`into_iter`]: IntoIterator::into_iter
pub struct IntoIter<K, V> {
    inner: alloc::collections::btree_map::IntoIter<RangeInclusiveStartWrapper<K>, V>,
}

impl<K, V> IntoIterator for RangeInclusiveMap<K, V> {
    type Item = (RangeInclusive<K>, V);
    type IntoIter = IntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter {
        IntoIter {
            inner: self.btm.into_iter(),
        }
    }
}

impl<K, V> Iterator for IntoIter<K, V> {
    type Item = (RangeInclusive<K>, V);
    fn next(&mut self) -> Option<(RangeInclusive<K>, V)> {
        self.inner.next().map(|(by_start, v)| (by_start.range, v))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

// We can't just derive this automatically, because that would
// expose irrelevant (and private) implementation details.
// Instead implement it in the same way that the underlying BTreeMap does.
impl<K: Debug, V: Debug> Debug for RangeInclusiveMap<K, V>
where
    K: Ord + Clone + StepLite,
    V: Eq + Clone,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.iter()).finish()
    }
}

impl<K, V> FromIterator<(RangeInclusive<K>, V)> for RangeInclusiveMap<K, V>
where
    K: Ord + Clone + StepLite,
    V: Eq + Clone,
{
    fn from_iter<T: IntoIterator<Item = (RangeInclusive<K>, V)>>(iter: T) -> Self {
        let mut range_map = RangeInclusiveMap::new();
        range_map.extend(iter);
        range_map
    }
}

impl<K, V> Extend<(RangeInclusive<K>, V)> for RangeInclusiveMap<K, V>
where
    K: Ord + Clone + StepLite,
    V: Eq + Clone,
{
    fn extend<T: IntoIterator<Item = (RangeInclusive<K>, V)>>(&mut self, iter: T) {
        iter.into_iter().for_each(move |(k, v)| {
            self.insert(k, v);
        })
    }
}

#[cfg(feature = "serde1")]
impl<K, V> Serialize for RangeInclusiveMap<K, V>
where
    K: Ord + Clone + StepLite + Serialize,
    V: Eq + Clone + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.btm.len()))?;
        for (k, v) in self.iter() {
            seq.serialize_element(&((k.start(), k.end()), &v))?;
        }
        seq.end()
    }
}

#[cfg(feature = "serde1")]
impl<'de, K, V> Deserialize<'de> for RangeInclusiveMap<K, V>
where
    K: Ord + Clone + StepLite + Deserialize<'de>,
    V: Eq + Clone + Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(RangeInclusiveMapVisitor::new())
    }
}

#[cfg(feature = "serde1")]
struct RangeInclusiveMapVisitor<K, V> {
    marker: PhantomData<fn() -> RangeInclusiveMap<K, V>>,
}

#[cfg(feature = "serde1")]
impl<K, V> RangeInclusiveMapVisitor<K, V> {
    fn new() -> Self {
        RangeInclusiveMapVisitor {
            marker: PhantomData,
        }
    }
}

#[cfg(feature = "serde1")]
impl<'de, K, V> Visitor<'de> for RangeInclusiveMapVisitor<K, V>
where
    K: Ord + Clone + StepLite + Deserialize<'de>,
    V: Eq + Clone + Deserialize<'de>,
{
    type Value = RangeInclusiveMap<K, V>;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("RangeInclusiveMap")
    }

    fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut range_inclusive_map = RangeInclusiveMap::new();
        while let Some(((start, end), value)) = access.next_element()? {
            range_inclusive_map.insert(start..=end, value);
        }
        Ok(range_inclusive_map)
    }
}

/// An iterator over all ranges not covered by a `RangeInclusiveMap`.
///
/// The iterator element type is `RangeInclusive<K>`.
///
/// This `struct` is created by the [`gaps`] method on [`RangeInclusiveMap`]. See its
/// documentation for more.
///
/// [`gaps`]: RangeInclusiveMap::gaps
pub struct Gaps<'a, K, V, StepFnsT> {
    /// Would be redundant, but we need an extra flag to
    /// avoid overflowing when dealing with inclusive ranges.
    ///
    /// All other things here are ignored if `done` is `true`.
    done: bool,
    outer_range: &'a RangeInclusive<K>,
    keys: core::iter::Peekable<
        alloc::collections::btree_map::Keys<'a, RangeInclusiveStartWrapper<K>, V>,
    >,
    candidate_start: K,
    _phantom: PhantomData<StepFnsT>,
}

// `Gaps` is always fused. (See definition of `next` below.)
impl<'a, K, V, StepFnsT> core::iter::FusedIterator for Gaps<'a, K, V, StepFnsT>
where
    K: Ord + Clone,
    StepFnsT: StepFns<K>,
{
}

impl<'a, K, V, StepFnsT> Iterator for Gaps<'a, K, V, StepFnsT>
where
    K: Ord + Clone,
    StepFnsT: StepFns<K>,
{
    type Item = RangeInclusive<K>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.candidate_start > *self.outer_range.end() {
            // We've already passed the end of the outer range;
            // there are no more gaps to find.
            return None;
        }

        // Figure out where this gap ends.
        let (end, next_candidate_start) = if let Some(item) = self.keys.next() {
            if item.range.start() <= self.outer_range.end() {
                // The gap goes up until just before the start of the next item,
                // and the next candidate starts after it.
                (
                    StepFnsT::sub_one(item.range.start()),
                    StepFnsT::add_one(item.range.end()),
                )
            } else {
                // The item sits after the end of the outer range,
                // so this gap ends at the end of the outer range.
                // This also means there will be no more gaps.
                self.done = true;
                (
                    self.outer_range.end().clone(),
                    // This value will be ignored.
                    self.candidate_start.clone(),
                )
            }
        } else {
            // There's no next item; the end is at the
            // end of the outer range.
            // This also means there will be no more gaps.
            self.done = true;
            (
                self.outer_range.end().clone(),
                // This value will be ignored.
                self.candidate_start.clone(),
            )
        };

        // Move the next candidate gap start past the end
        // of this gap, and yield the gap we found.
        let gap = self.candidate_start.clone()..=end;
        self.candidate_start = next_candidate_start;
        Some(gap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::{format, vec, vec::Vec};

    trait RangeInclusiveMapExt<K, V> {
        fn to_vec(&self) -> Vec<(RangeInclusive<K>, V)>;
    }

    impl<K, V> RangeInclusiveMapExt<K, V> for RangeInclusiveMap<K, V, K>
    where
        K: Ord + Clone + StepLite,
        V: Eq + Clone,
    {
        fn to_vec(&self) -> Vec<(RangeInclusive<K>, V)> {
            self.iter().map(|(kr, v)| (kr.clone(), v.clone())).collect()
        }
    }

    //
    // Insertion tests
    //

    #[test]
    fn empty_map_is_empty() {
        let range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        assert_eq!(range_map.to_vec(), vec![]);
    }

    #[test]
    fn insert_into_empty_map() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(0..=50, false);
        assert_eq!(range_map.to_vec(), vec![(0..=50, false)]);
    }

    #[test]
    fn new_same_value_immediately_following_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=3, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ●---◌ ◌ ◌ ◌
        range_map.insert(4..=6, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---------◌ ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=6, false)]);
    }

    #[test]
    fn new_different_value_immediately_following_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=3, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◆---◇ ◌ ◌ ◌
        range_map.insert(4..=6, true);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        // ◌ ◌ ◌ ◌ ◆---◇ ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=3, false), (4..=6, true)]);
    }

    #[test]
    fn new_same_value_overlapping_end_of_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-----● ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=4, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ●---● ◌ ◌ ◌
        range_map.insert(4..=6, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---------● ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=6, false)]);
    }

    #[test]
    fn new_different_value_overlapping_end_of_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=3, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◆---◆ ◌ ◌ ◌ ◌
        range_map.insert(3..=5, true);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-● ◌ ◌ ◌ ◌ ◌ ◌ ◌
        // ◌ ◌ ◌ ◆---◇ ◌ ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=2, false), (3..=5, true)]);
    }

    #[test]
    fn new_same_value_immediately_preceding_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ●---● ◌ ◌ ◌ ◌
        range_map.insert(3..=5, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-● ◌ ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=2, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-------● ◌ ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=5, false)]);
    }

    #[test]
    fn new_different_value_immediately_preceding_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◆---◆ ◌ ◌ ◌ ◌
        range_map.insert(3..=5, true);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-● ◌ ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=2, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-● ◌ ◌ ◌ ◌ ◌ ◌ ◌
        // ◌ ◌ ◌ ◆---◇ ◌ ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=2, false), (3..=5, true)]);
    }

    #[test]
    fn new_same_value_wholly_inside_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-------● ◌ ◌ ◌ ◌
        range_map.insert(1..=5, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(2..=4, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-------● ◌ ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=5, false)]);
    }

    #[test]
    fn new_different_value_wholly_inside_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆-------◆ ◌ ◌ ◌ ◌
        range_map.insert(1..=5, true);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(2..=4, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆ ◌ ◌ ◌ ◌ ◌ ◌ ◌ ◌
        // ◌ ◌ ●---● ◌ ◌ ◌ ◌ ◌
        // ◌ ◌ ◌ ◌ ◌ ◆ ◌ ◌ ◌ ◌
        assert_eq!(
            range_map.to_vec(),
            vec![(1..=1, true), (2..=4, false), (5..=5, true)]
        );
    }

    #[test]
    fn replace_at_end_of_existing_range_should_coalesce() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=3, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ●---● ◌ ◌ ◌
        range_map.insert(4..=6, true);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ●---● ◌ ◌ ◌
        range_map.insert(4..=6, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---------● ◌ ◌ ◌
        assert_eq!(range_map.to_vec(), vec![(1..=6, false)]);
    }

    #[test]
    // Test every permutation of a bunch of touching and overlapping ranges.
    fn lots_of_interesting_ranges() {
        use crate::stupid_range_map::StupidU32RangeMap;
        use permutator::Permutation;

        let mut ranges_with_values = [
            (2..=3, false),
            // A duplicate range
            (2..=3, false),
            // Almost a duplicate, but with a different value
            (2..=3, true),
            // A few small ranges, some of them overlapping others,
            // some of them touching others
            (3..=5, true),
            (4..=6, true),
            (6..=7, true),
            // A really big range
            (2..=6, true),
        ];

        ranges_with_values.permutation().for_each(|permutation| {
            let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
            let mut stupid: StupidU32RangeMap<bool> = StupidU32RangeMap::new();

            for (k, v) in permutation {
                // Insert it into both maps.
                range_map.insert(k.clone(), v);
                stupid.insert(k, v);

                // At every step, both maps should contain the same stuff.
                let stupid2: StupidU32RangeMap<bool> = range_map.clone().into();
                assert_eq!(stupid, stupid2);
            }
        });
    }

    //
    // Get* tests
    //

    #[test]
    fn get() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(0..=50, false);
        assert_eq!(range_map.get(&50), Some(&false));
        assert_eq!(range_map.get(&51), None);
    }

    #[test]
    fn get_key_value() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(0..=50, false);
        assert_eq!(range_map.get_key_value(&50), Some((&(0..=50), &false)));
        assert_eq!(range_map.get_key_value(&51), None);
    }

    //
    // Removal tests
    //

    #[test]
    fn remove_from_empty_map() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.remove(0..=50);
        assert_eq!(range_map.to_vec(), vec![]);
    }

    #[test]
    fn remove_non_covered_range_before_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(25..=75, false);
        range_map.remove(0..=24);
        assert_eq!(range_map.to_vec(), vec![(25..=75, false)]);
    }

    #[test]
    fn remove_non_covered_range_after_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(25..=75, false);
        range_map.remove(76..=100);
        assert_eq!(range_map.to_vec(), vec![(25..=75, false)]);
    }

    #[test]
    fn remove_overlapping_start_of_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(25..=75, false);
        range_map.remove(0..=25);
        assert_eq!(range_map.to_vec(), vec![(26..=75, false)]);
    }

    #[test]
    fn remove_middle_of_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(25..=75, false);
        range_map.remove(30..=70);
        assert_eq!(range_map.to_vec(), vec![(25..=29, false), (71..=75, false)]);
    }

    #[test]
    fn remove_overlapping_end_of_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(25..=75, false);
        range_map.remove(75..=100);
        assert_eq!(range_map.to_vec(), vec![(25..=74, false)]);
    }

    #[test]
    fn remove_exactly_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(25..=75, false);
        range_map.remove(25..=75);
        assert_eq!(range_map.to_vec(), vec![]);
    }

    #[test]
    fn remove_superset_of_stored() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        range_map.insert(25..=75, false);
        range_map.remove(0..=100);
        assert_eq!(range_map.to_vec(), vec![]);
    }

    //
    // Test extremes of key ranges; we do addition/subtraction in
    // the range domain so I want to make sure I haven't accidentally
    // introduced some arithmetic overflow there.
    //

    #[test]
    fn no_overflow_at_key_domain_extremes() {
        let mut range_map: RangeInclusiveMap<u8, bool> = RangeInclusiveMap::new();
        range_map.insert(0..=255, false);
        range_map.insert(0..=10, true);
        range_map.insert(245..=255, true);
        range_map.remove(0..=5);
        range_map.remove(0..=5);
        range_map.remove(250..=255);
        range_map.remove(250..=255);
        range_map.insert(0..=255, true);
        range_map.remove(1..=254);
        range_map.insert(254..=254, true);
        range_map.insert(255..=255, true);
        range_map.insert(255..=255, false);
        range_map.insert(0..=0, false);
        range_map.insert(1..=1, true);
        range_map.insert(0..=0, true);
    }

    // Gaps tests

    #[test]
    fn whole_range_is_a_gap() {
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◌ ◌ ◌ ◌ ◌
        let range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆-------------◆ ◌
        let outer_range = 1..=8;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield the entire outer range.
        assert_eq!(gaps.next(), Some(1..=8));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn whole_range_is_covered_exactly() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---------● ◌ ◌ ◌
        range_map.insert(1..=6, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆---------◆ ◌ ◌ ◌
        let outer_range = 1..=6;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield no gaps.
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_before_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●---● ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=3, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆-----◆ ◌
        let outer_range = 5..=8;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield the entire outer range.
        assert_eq!(gaps.next(), Some(5..=8));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_touching_start_of_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-----● ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=4, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆-----◆ ◌
        let outer_range = 5..=8;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield the entire outer range.
        assert_eq!(gaps.next(), Some(5..=8));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_overlapping_start_of_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ●-------● ◌ ◌ ◌ ◌
        range_map.insert(1..=5, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆-----◆ ◌
        let outer_range = 5..=8;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield from just past the end of the stored item
        // to the end of the outer range.
        assert_eq!(gaps.next(), Some(6..=8));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_starting_at_start_of_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ●-● ◌ ◌ ◌
        range_map.insert(5..=6, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆-----◆ ◌
        let outer_range = 5..=8;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield from just past the item onwards.
        assert_eq!(gaps.next(), Some(7..=8));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn items_floating_inside_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◌ ●-● ◌ ◌
        range_map.insert(6..=7, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ●-● ◌ ◌ ◌ ◌ ◌
        range_map.insert(3..=4, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆-------------◆ ◌
        let outer_range = 1..=8;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield gaps at start, between items,
        // and at end.
        assert_eq!(gaps.next(), Some(1..=2));
        assert_eq!(gaps.next(), Some(5..=5));
        assert_eq!(gaps.next(), Some(8..=8));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_ending_at_end_of_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◌ ◌ ●-● ◌
        range_map.insert(7..=8, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆-----◆ ◌
        let outer_range = 5..=8;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield from the start of the outer range
        // up to just before the start of the stored item.
        assert_eq!(gaps.next(), Some(5..=6));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_overlapping_end_of_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ●---● ◌ ◌
        range_map.insert(5..=6, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◆-----◆ ◌ ◌ ◌ ◌
        let outer_range = 2..=5;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield from the start of the outer range
        // up to the start of the stored item.
        assert_eq!(gaps.next(), Some(2..=4));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_touching_end_of_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ●-----● ◌
        range_map.insert(5..=9, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆-----◆ ◌ ◌ ◌ ◌ ◌
        let outer_range = 1..=4;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield the entire outer range.
        assert_eq!(gaps.next(), Some(1..=4));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn item_after_outer_range() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◌ ●---● ◌
        range_map.insert(6..=7, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆-----◆ ◌ ◌ ◌ ◌ ◌
        let outer_range = 1..=4;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield the entire outer range.
        assert_eq!(gaps.next(), Some(1..=4));
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn zero_width_outer_range_with_items_away_from_both_sides() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆---◆ ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=3, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆---◆ ◌ ◌
        range_map.insert(5..=7, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◆ ◌ ◌ ◌ ◌ ◌
        let outer_range = 4..=4;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield a zero-width gap.
        assert_eq!(gaps.next(), Some(4..=4));
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn zero_width_outer_range_with_items_touching_both_sides() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◆-◆ ◌ ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(2..=3, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆---◆ ◌ ◌ ◌
        range_map.insert(5..=6, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◆ ◌ ◌ ◌ ◌ ◌
        let outer_range = 4..=4;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield no gaps.
        assert_eq!(gaps.next(), Some(4..=4));
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn empty_outer_range_with_item_straddling() {
        let mut range_map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◆-----◆ ◌ ◌ ◌ ◌ ◌
        range_map.insert(2..=5, ());
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◆ ◌ ◌ ◌ ◌ ◌
        let outer_range = 4..=4;
        let mut gaps = range_map.gaps(&outer_range);
        // Should yield no gaps.
        assert_eq!(gaps.next(), None);
        // Gaps iterator should be fused.
        assert_eq!(gaps.next(), None);
        assert_eq!(gaps.next(), None);
    }

    #[test]
    fn no_overflow_finding_gaps_at_key_domain_extremes() {
        // Items and outer range both at extremes.
        let mut range_map: RangeInclusiveMap<u8, bool> = RangeInclusiveMap::new();
        range_map.insert(0..=255, false);
        range_map.gaps(&(0..=255));

        // Items at extremes with gaps in middle.
        let mut range_map: RangeInclusiveMap<u8, bool> = RangeInclusiveMap::new();
        range_map.insert(0..=255, false);
        range_map.gaps(&(0..=5));
        range_map.gaps(&(250..=255));

        // Items just in from extremes.
        let mut range_map: RangeInclusiveMap<u8, bool> = RangeInclusiveMap::new();
        range_map.insert(0..=255, false);
        range_map.gaps(&(1..=5));
        range_map.gaps(&(250..=254));

        // Outer range just in from extremes,
        // items at extremes.
        let mut range_map: RangeInclusiveMap<u8, bool> = RangeInclusiveMap::new();
        range_map.insert(1..=254, false);
        range_map.gaps(&(0..=5));
        range_map.gaps(&(250..=255));
    }

    ///
    /// impl Debug
    ///

    #[test]
    fn map_debug_repr_looks_right() {
        let mut map: RangeInclusiveMap<u32, ()> = RangeInclusiveMap::new();

        // Empty
        assert_eq!(format!("{:?}", map), "{}");

        // One entry
        map.insert(2..=5, ());
        assert_eq!(format!("{:?}", map), "{2..=5: ()}");

        // Many entries
        map.insert(7..=8, ());
        map.insert(10..=11, ());
        assert_eq!(format!("{:?}", map), "{2..=5: (), 7..=8: (), 10..=11: ()}");
    }

    // impl Serialize

    #[cfg(feature = "serde1")]
    #[test]
    fn serialization() {
        let mut range_map: RangeInclusiveMap<u32, bool> = RangeInclusiveMap::new();
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◆---◆ ◌ ◌ ◌ ◌ ◌ ◌
        range_map.insert(1..=3, false);
        // 0 1 2 3 4 5 6 7 8 9
        // ◌ ◌ ◌ ◌ ◌ ◆---◆ ◌ ◌
        range_map.insert(5..=7, true);
        let output = serde_json::to_string(&range_map).expect("Failed to serialize");
        assert_eq!(output, "[[[1,3],false],[[5,7],true]]");
    }

    // impl Deserialize

    #[cfg(feature = "serde1")]
    #[test]
    fn deserialization() {
        let input = "[[[1,3],false],[[5,7],true]]";
        let range_map: RangeInclusiveMap<u32, bool> =
            serde_json::from_str(input).expect("Failed to deserialize");
        let reserialized = serde_json::to_string(&range_map).expect("Failed to re-serialize");
        assert_eq!(reserialized, input);
    }
}
