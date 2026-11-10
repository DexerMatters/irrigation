//! Key enumeration for keyed producers.
//!
//! A keyed component needs a set of occurrence identities. This trait is the
//! one place a collection handle exposes that enumeration without making the
//! underlying storage shape part of the user API.

use std::hash::Hash;

use crate::{
    DerivedListRef, DerivedSetRef, EdgeKey, GraphRef, MapRef, NodeKey, SourceListRef, SourceSetRef,
};

impl<T> KeySet<T> for Vec<T>
where
    T: Clone,
{
    fn keys(&self) -> Vec<T> {
        self.clone()
    }
}

impl<K, V> KeySet<K> for Vec<(K, V)>
where
    K: Clone,
{
    fn keys(&self) -> Vec<K> {
        self.iter().map(|(key, _)| key.clone()).collect()
    }
}

/// Enumerates the keys of one collection handle in that family's order.
pub trait KeySet<K> {
    /// The current keys.
    fn keys(&self) -> Vec<K>;
}

impl<K, V> KeySet<K> for MapRef<K, V>
where
    K: Clone + Ord + Hash + std::fmt::Debug + Send + Sync + 'static,
    V: Clone + PartialEq + Send + Sync + 'static,
{
    fn keys(&self) -> Vec<K> {
        MapRef::keys(self)
    }
}

impl<T> KeySet<T> for SourceSetRef<T>
where
    T: Clone + Ord + Hash + std::fmt::Debug + Send + Sync + 'static,
{
    fn keys(&self) -> Vec<T> {
        self.members().iter().cloned().collect()
    }
}

impl<T> KeySet<T> for DerivedSetRef<T>
where
    T: Clone + Ord + Hash + std::fmt::Debug + Send + Sync + 'static,
{
    fn keys(&self) -> Vec<T> {
        self.members().iter().cloned().collect()
    }
}

impl<T> KeySet<u64> for SourceListRef<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    fn keys(&self) -> Vec<u64> {
        self.ids().to_vec()
    }
}

impl<T> KeySet<u64> for DerivedListRef<T>
where
    T: Clone + PartialEq + Send + Sync + 'static,
{
    fn keys(&self) -> Vec<u64> {
        self.ids().to_vec()
    }
}

impl<N, E> KeySet<NodeKey> for GraphRef<N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    fn keys(&self) -> Vec<NodeKey> {
        self.nodes().into_iter().map(NodeKey).collect()
    }
}

impl<N, E> KeySet<EdgeKey> for GraphRef<N, E>
where
    N: Clone + PartialEq + Send + Sync + 'static,
    E: Clone + PartialEq + Send + Sync + 'static,
{
    fn keys(&self) -> Vec<EdgeKey> {
        self.edges().into_iter().map(EdgeKey).collect()
    }
}
