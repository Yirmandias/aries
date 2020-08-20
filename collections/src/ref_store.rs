use itertools::Itertools;
use serde::{Serialize, Serializer};
use std::borrow::Borrow;
use std::collections::HashMap;
use std::fmt::{Debug, Error, Formatter};
use std::hash::Hash;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ops::{Index, IndexMut};

pub trait Ref: Into<usize> + From<usize> + Copy + PartialEq {}

impl<X> Ref for X where X: Into<usize> + From<usize> + Copy + PartialEq {}

#[macro_export]
macro_rules! create_ref_type {
    ($type_name:ident) => {
        #[derive(PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Hash)]
        pub struct $type_name {
            id: NonZeroU32,
        }
        impl $type_name {
            pub fn new(id: NonZeroU32) -> $type_name {
                $type_name { id }
            }
        }
        impl From<usize> for $type_name {
            fn from(u: usize) -> Self {
                unsafe {
                    $type_name {
                        id: NonZeroU32::new_unchecked(u as u32 + 1),
                    }
                }
            }
        }
        impl From<$type_name> for usize {
            fn from(v: $type_name) -> Self {
                (v.id.get() - 1) as usize
            }
        }
    };
}

create_ref_type!(X);

/// A store to generate integer references to more complex values.
/// The objective is to allow interning complex values.
///
/// A new key can be obtained by `push`ing a value into the store.
///
#[derive(Clone)]
pub struct RefPool<Key, Val> {
    internal: Vec<Val>,
    rev: HashMap<Val, Key>,
}
impl<K, V: Hash + Eq> Default for RefPool<K, V> {
    fn default() -> Self {
        RefPool {
            internal: Default::default(),
            rev: HashMap::new(),
        }
    }
}
impl<K, V: Debug> Debug for RefPool<K, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        write!(f, "{}", format!("{:?}", self.internal.iter().enumerate().format(", ")))
    }
}

impl<K, V> RefPool<K, V>
where
    K: Ref,
{
    pub fn len(&self) -> usize {
        self.internal.len()
    }

    pub fn is_empty(&self) -> bool {
        self.internal.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = K> {
        (0..self.len()).map(K::from)
    }

    pub fn last_key(&self) -> Option<K> {
        if self.is_empty() {
            None
        } else {
            Some((self.len() - 1).into())
        }
    }

    pub fn push(&mut self, v: V) -> K
    where
        V: Eq + Hash + Clone, // TODO: remove necessity of clone by storing reference to internal field
    {
        assert!(!self.rev.contains_key(&v));
        let id: K = self.internal.len().into();
        self.rev.insert(v.clone(), id);
        self.internal.push(v);
        id
    }

    pub fn get(&self, k: K) -> &V {
        &self.internal[k.into()]
    }

    pub fn get_ref<W: ?Sized>(&self, v: &W) -> Option<K>
    where
        W: Eq + Hash,
        V: Eq + Hash + Borrow<W>,
    {
        self.rev.get(v).copied()
    }
}

impl<K: Ref, V> Index<K> for RefPool<K, V> {
    type Output = V;

    fn index(&self, index: K) -> &Self::Output {
        self.get(index)
    }
}

/// Same as the pool but does not allow retrieving the ID of a previously interned item.
/// IDs are only returned upon insertion.
#[derive(Clone)]
pub struct RefStore<Key, Val> {
    internal: Vec<Val>,
    phantom: PhantomData<Key>,
}
impl<K, V: Debug> Debug for RefStore<K, V> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        write!(f, "{}", format!("{:?}", self.internal.iter().enumerate().format(", ")))
    }
}

impl<K: Ref, V> Default for RefStore<K, V> {
    fn default() -> Self {
        RefStore::new()
    }
}

impl<K, V> RefStore<K, V>
where
    K: Ref,
{
    pub fn new() -> Self {
        RefStore {
            internal: Vec::new(),
            phantom: Default::default(),
        }
    }

    pub fn initialized(len: usize, v: V) -> Self
    where
        V: Clone,
    {
        RefStore {
            internal: vec![v; len],
            phantom: Default::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.internal.len()
    }

    pub fn is_empty(&self) -> bool {
        self.internal.is_empty()
    }

    pub fn keys(&self) -> impl Iterator<Item = K> {
        (0..self.len()).map(K::from)
    }

    pub fn last_key(&self) -> Option<K> {
        if self.is_empty() {
            None
        } else {
            Some((self.len() - 1).into())
        }
    }

    pub fn push(&mut self, v: V) -> K {
        let id: K = self.internal.len().into();
        self.internal.push(v);
        id
    }

    pub fn get(&self, k: K) -> &V {
        &self.internal[k.into()]
    }

    pub fn get_mut(&mut self, k: K) -> &mut V {
        &mut self.internal[k.into()]
    }
}

impl<K: Ref, V> Index<K> for RefStore<K, V> {
    type Output = V;

    fn index(&self, index: K) -> &Self::Output {
        self.get(index)
    }
}

impl<K: Ref, V> IndexMut<K> for RefStore<K, V> {
    fn index_mut(&mut self, index: K) -> &mut Self::Output {
        self.get_mut(index)
    }
}

impl<K, V: Serialize> Serialize for RefStore<K, V> {
    fn serialize<S>(&self, serializer: S) -> Result<<S as Serializer>::Ok, <S as Serializer>::Error>
    where
        S: Serializer,
    {
        self.internal.serialize(serializer)
    }
}

pub struct RefVec<K, V> {
    values: Vec<V>,
    phantom: PhantomData<K>,
}

impl<K, V> Default for RefVec<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K, V> RefVec<K, V> {
    pub fn new() -> Self {
        RefVec {
            values: Vec::new(),
            phantom: PhantomData::default(),
        }
    }

    /// Creates a new RefVec with the given `value` repeated `num_items` times.
    pub fn with_values(num_items: usize, value: V) -> Self
    where
        V: Clone,
    {
        RefVec {
            values: vec![value; num_items],
            phantom: PhantomData::default(),
        }
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn push(&mut self, value: V) -> K
    where
        K: From<usize>,
    {
        self.values.push(value);
        K::from(self.values.len() - 1)
    }

    pub fn keys(&self) -> impl Iterator<Item = K>
    where
        K: From<usize>,
    {
        (0..(self.values.len())).map(K::from)
    }
}

impl<K: Into<usize>, V> Index<K> for RefVec<K, V> {
    type Output = V;

    fn index(&self, index: K) -> &Self::Output {
        &self.values[index.into()]
    }
}

impl<K: Into<usize>, V> IndexMut<K> for RefVec<K, V> {
    fn index_mut(&mut self, index: K) -> &mut Self::Output {
        &mut self.values[index.into()]
    }
}