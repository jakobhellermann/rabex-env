use anyhow::Result;
use rayon::iter::{IntoParallelIterator, ParallelIterator as _};

pub use merge::Merge;

pub fn par_fold_reduce<Acc, T>(
    iter: impl IntoParallelIterator<Item = T>,
    f: impl Fn(&mut Acc, T) -> Result<()> + Send + Sync,
) -> Result<Acc>
where
    Acc: Merge + Default + Send + Sync,
{
    iter.into_par_iter()
        .try_fold(Acc::default, |mut acc, item| -> Result<_> {
            f(&mut acc, item)?;
            Ok(acc)
        })
        .try_reduce(Acc::default, |mut acc, item| {
            Merge::merge(&mut acc, item);
            Ok(acc)
        })
}

mod merge {
    use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
    use std::hash::{BuildHasher, Hash};

    pub trait Merge {
        fn merge(&mut self, other: Self);
    }

    impl Merge for () {
        fn merge(&mut self, (): Self) {}
    }

    impl Merge for usize {
        fn merge(&mut self, other: Self) {
            *self += other;
        }
    }

    impl<T> Merge for Vec<T> {
        fn merge(&mut self, other: Self) {
            self.extend(other);
        }
    }

    impl<T, S> Merge for HashSet<T, S>
    where
        T: Eq + Hash,
        S: BuildHasher,
    {
        fn merge(&mut self, other: Self) {
            self.extend(other);
        }
    }
    impl<T> Merge for BTreeSet<T>
    where
        T: Ord,
    {
        fn merge(&mut self, other: Self) {
            self.extend(other);
        }
    }

    impl<K, V, S> Merge for HashMap<K, V, S>
    where
        K: Eq + Hash,
        V: Merge + Default,
        S: BuildHasher,
    {
        fn merge(&mut self, other: Self) {
            use std::collections::hash_map::Entry;
            for (item, value) in other {
                match self.entry(item) {
                    Entry::Vacant(entry) => drop(entry.insert(value)),
                    Entry::Occupied(mut entry) => entry.get_mut().merge(value),
                }
            }
        }
    }
    impl<K, V> Merge for BTreeMap<K, V>
    where
        K: Ord,
        V: Merge + Default,
    {
        fn merge(&mut self, other: Self) {
            use std::collections::btree_map::Entry;
            for (item, value) in other {
                match self.entry(item) {
                    Entry::Vacant(entry) => drop(entry.insert(value)),
                    Entry::Occupied(mut entry) => entry.get_mut().merge(value),
                }
            }
        }
    }
}
