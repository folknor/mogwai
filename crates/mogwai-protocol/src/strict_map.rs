// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Map types whose decode refuses a repeated key.
//!
//! serde's own `HashMap` and `BTreeMap` visitors insert every entry they read,
//! so a document naming one key twice silently keeps the last value. A derived
//! struct refuses a repeated field and `deny_unknown_fields` refuses a stray
//! one, which left the map the one place in a strict decoder where a malformed
//! document still decoded. Refusal is a property of these types rather than of
//! an annotation on each field, so a field cannot forget it: every map inside a
//! workspace type that derives `Deserialize` is one of the two below, and a
//! source gate forbids the raw maps there.
//!
//! Both are thin newtypes. They serialize exactly as the map they wrap, so a
//! pinned output does not move, and they `Deref` and `DerefMut` to it, so a
//! reader or writer of the field calls the map's own methods. The newtype is a
//! decode contract, not a different collection, and hiding the collection
//! behind a parallel accessor surface would only be a second API to keep in
//! step with the first. Construction goes through `Default`, `From` the
//! wrapped map, or `FromIterator`; `into_inner` gives the map back by value.
//!
//! The duplicate check runs before the value is kept and names the key, so an
//! operator reading the refusal learns which entry to delete. That needs the
//! key to be `Display`, which every map key in the workspace is.

use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Display};
use std::hash::Hash;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};

use serde::de::{self, Deserialize, Deserializer, MapAccess};
use serde::ser::{Serialize, Serializer};

/// The message a repeated key is refused with, shared by both types so the
/// wording cannot diverge between them.
fn duplicate<E: de::Error>(key: &dyn Display) -> E {
    E::custom(format_args!("duplicate map key `{key}`"))
}

macro_rules! strict_map {
    (
        $(#[$doc:meta])*
        $name:ident, $inner:ident, [$($key_bound:tt)+]
    ) => {
        $(#[$doc])*
        #[derive(Clone, Debug)]
        pub struct $name<K, V>($inner<K, V>);

        // Written by hand because a derive would bound only `K: PartialEq`,
        // which the wrapped map's own comparison does not accept.
        impl<K, V> PartialEq for $name<K, V>
        where
            $inner<K, V>: PartialEq,
        {
            fn eq(&self, other: &Self) -> bool {
                self.0 == other.0
            }
        }

        impl<K, V> Eq for $name<K, V> where $inner<K, V>: Eq {}

        impl<K, V> $name<K, V> {
            /// The wrapped map, by value.
            #[must_use]
            pub fn into_inner(self) -> $inner<K, V> {
                self.0
            }

            /// Inherent so a `skip_serializing_if` path can name it; the
            /// wrapped map's own method is reached through `Deref` otherwise.
            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
        }

        impl<K, V> Default for $name<K, V> {
            fn default() -> Self {
                Self($inner::default())
            }
        }

        impl<K, V> Deref for $name<K, V> {
            type Target = $inner<K, V>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl<K, V> DerefMut for $name<K, V> {
            fn deref_mut(&mut self) -> &mut Self::Target {
                &mut self.0
            }
        }

        impl<K, V> From<$inner<K, V>> for $name<K, V> {
            fn from(map: $inner<K, V>) -> Self {
                Self(map)
            }
        }

        impl<K, V> From<$name<K, V>> for $inner<K, V> {
            fn from(map: $name<K, V>) -> Self {
                map.0
            }
        }

        impl<K: $($key_bound)+, V, const N: usize> From<[(K, V); N]> for $name<K, V> {
            fn from(entries: [(K, V); N]) -> Self {
                Self($inner::from(entries))
            }
        }

        impl<K: $($key_bound)+, V> FromIterator<(K, V)> for $name<K, V> {
            fn from_iter<I: IntoIterator<Item = (K, V)>>(iter: I) -> Self {
                Self(iter.into_iter().collect())
            }
        }

        impl<K, V> IntoIterator for $name<K, V> {
            type Item = (K, V);
            type IntoIter = <$inner<K, V> as IntoIterator>::IntoIter;

            fn into_iter(self) -> Self::IntoIter {
                self.0.into_iter()
            }
        }

        impl<'a, K, V> IntoIterator for &'a $name<K, V> {
            type Item = (&'a K, &'a V);
            type IntoIter = <&'a $inner<K, V> as IntoIterator>::IntoIter;

            fn into_iter(self) -> Self::IntoIter {
                self.0.iter()
            }
        }

        impl<'a, K, V> IntoIterator for &'a mut $name<K, V> {
            type Item = (&'a K, &'a mut V);
            type IntoIter = <&'a mut $inner<K, V> as IntoIterator>::IntoIter;

            fn into_iter(self) -> Self::IntoIter {
                self.0.iter_mut()
            }
        }

        impl<K: Serialize, V: Serialize> Serialize for $name<K, V> {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                self.0.serialize(serializer)
            }
        }

        impl<'de, K, V> Deserialize<'de> for $name<K, V>
        where
            K: Deserialize<'de> + Display + $($key_bound)+,
            V: Deserialize<'de>,
        {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                struct Unique<K, V>(PhantomData<(K, V)>);

                impl<'de, K, V> de::Visitor<'de> for Unique<K, V>
                where
                    K: Deserialize<'de> + Display + $($key_bound)+,
                    V: Deserialize<'de>,
                {
                    type Value = $name<K, V>;

                    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                        formatter.write_str("a map with no repeated key")
                    }

                    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                        let mut map = $inner::default();
                        while let Some(key) = access.next_key::<K>()? {
                            if map.contains_key(&key) {
                                return Err(duplicate(&key));
                            }
                            let value = access.next_value::<V>()?;
                            map.insert(key, value);
                        }
                        Ok($name(map))
                    }
                }

                deserializer.deserialize_map(Unique(PhantomData))
            }
        }
    };
}

strict_map!(
    /// A `HashMap` whose decode refuses a repeated key by name.
    StrictHashMap,
    HashMap,
    [Eq + Hash]
);

strict_map!(
    /// A `BTreeMap` whose decode refuses a repeated key by name.
    StrictBTreeMap,
    BTreeMap,
    [Ord]
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_map_refuses_a_repeated_key_by_name() {
        let error =
            serde_json::from_str::<StrictHashMap<String, u64>>(r#"{"USD":1,"EUR":2,"USD":3}"#)
                .expect_err("a repeated key must not decode");
        assert!(
            error.to_string().contains("duplicate map key `USD`"),
            "{error}"
        );
    }

    #[test]
    fn btree_map_refuses_a_repeated_key_by_name() {
        let error = serde_json::from_str::<StrictBTreeMap<u32, u64>>(r#"{"7":1,"8":2,"7":3}"#)
            .expect_err("a repeated key must not decode");
        assert!(
            error.to_string().contains("duplicate map key `7`"),
            "{error}"
        );
    }

    #[test]
    fn distinct_keys_decode_and_serialize_as_the_plain_map() {
        let text = r#"{"1":"a","2":"b"}"#;
        let strict: StrictBTreeMap<u32, String> = serde_json::from_str(text).unwrap();
        let plain: BTreeMap<u32, String> = serde_json::from_str(text).unwrap();
        assert_eq!(*strict, plain);
        assert_eq!(serde_json::to_string(&strict).unwrap(), text);

        let hashed: StrictHashMap<String, u64> = serde_json::from_str(r#"{"USD":1}"#).unwrap();
        assert_eq!(serde_json::to_string(&hashed).unwrap(), r#"{"USD":1}"#);
    }

    #[test]
    fn nested_maps_refuse_a_repeated_inner_key() {
        let error = serde_json::from_str::<StrictBTreeMap<u32, StrictBTreeMap<u32, u64>>>(
            r#"{"1":{"2":1,"2":2}}"#,
        )
        .expect_err("a repeated inner key must not decode");
        assert!(
            error.to_string().contains("duplicate map key `2`"),
            "{error}"
        );
    }
}
