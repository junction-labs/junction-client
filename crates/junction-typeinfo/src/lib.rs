//! This crate defines an extremely basic reflection API, for use in
//! `junction-client`. It does not have a stable public API, and will not be
//! checked for breaking changes. Use it at your own risk.

use std::collections::BTreeMap;

pub use junction_typeinfo_derive::TypeInfo;

/// A kind of type.
///
/// The not intended to be complete model of Rust data types- if we need it for
/// defining a type in junction-client, it's probably here and if it's not, we
/// probably left it out. The types here are also inspired by what's available in
/// Python and Typescript.
///
/// The goal of this crate is to be able to use this data model to do some basic
/// reflection on Rust structs, convert to a language-specific data model, and
/// then generate code based on that language specific model. See
/// junction-api-gen for language specific data models and codegen.
///
/// This is also not the same meaning of "kind" that gets used type-theory, it's
/// here instead of "type" because it doesn't conflict with reserved identifiers
/// in Rust.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kind {
    Bool,
    String,
    Int,
    Float,
    Duration,
    Union(&'static str, Vec<Variant>),
    Tuple(Vec<Kind>),
    Array(Box<Kind>),
    Map(Box<Kind>, Box<Kind>),
    Object(&'static str),
}

/// A trait for types that can be reflected on.
///
/// All of the methods here except for `kind` have default impls for simple
/// types and should not be overridden unless you know what you're doing.
pub trait TypeInfo {
    /// The [Kind] of this type.
    fn kind() -> Kind;

    /// Any documentation for this type. Mostly set for derive macros.
    fn doc() -> Option<&'static str> {
        None
    }

    /// Any fields that should be treated as part of this struct. Only
    /// really makes sense to derive.
    fn fields() -> Vec<Field> {
        Vec::new()
    }

    /// Whether or not this field can be nullable.
    ///
    /// This should only ever be set for [Option], and doing otherwise is a
    /// crime.
    fn nullable() -> bool {
        false
    }

    /// Bundle all of the info about this type into an [Item].
    fn item() -> Item {
        Item {
            kind: Self::kind(),
            fields: Self::fields(),
            nullable: Self::nullable(),
            doc: Self::doc(),
        }
    }

    /// Any fields defined in Union variants. This must be empty for any types
    /// with a [Kind] that is not [Kind::Union].
    fn variant_fields() -> Vec<Field> {
        let mut fields: BTreeMap<&str, Vec<_>> = BTreeMap::new();

        if let Kind::Union(_, variants) = Self::kind() {
            for variant in variants {
                if let Variant::Struct(v) = variant {
                    for field in v.fields {
                        let fields: &mut Vec<Field> = fields.entry(field.name).or_default();

                        match fields.first_mut() {
                            Some(f) => {
                                merge_fields(f, field);
                            }
                            _ => fields.push(field),
                        }
                    }
                }
            }
        }

        let mut fields: Vec<_> = fields.into_values().flatten().collect();
        fields.sort_by_key(|f| f.name);
        fields.dedup_by_key(|f| f.name);
        fields
    }

    /// The fields that should be included when this type is flattened with
    /// `serde(flatten)`.
    fn flatten_fields() -> Vec<Field> {
        match Self::kind() {
            Kind::Union(_, _) => Self::variant_fields(),
            Kind::Object(_) => Self::fields(),
            _ => Vec::new(),
        }
    }
}

fn merge_fields(a: &mut Field, b: Field) {
    // short-circuit. if the fields are exactly identical, do nothing
    if a.kind == b.kind {
        return;
    }

    // if this field isn't already a Union, make it one. drop the doc for now
    match &mut a.kind {
        Kind::Union(_, _) => (),
        other => {
            a.doc = None;
            a.kind = Kind::Union(a.name, vec![Variant::Newtype(other.clone())]);
        }
    }
    // get a mutable ref to the inside of this union type
    let a_vars = match &mut a.kind {
        Kind::Union(_, vars) => vars,
        _ => unreachable!(),
    };
    // extend the possibilities
    let b_vars = match b.kind {
        Kind::Union(_, vars) => vars,
        other => vec![Variant::Newtype(other)],
    };
    a_vars.extend(b_vars);
    a_vars.dedup();
}

/// A type's [Kind], fields, nullability, and documentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub kind: Kind,
    pub fields: Vec<Field>,
    pub nullable: bool,
    pub doc: Option<&'static str>,
}

/// The field in a `struct` or a struct-like `enum` variant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub kind: Kind,
    pub nullable: bool,
    pub doc: Option<&'static str>,
}

/// A variant in a an enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Variant {
    Literal(&'static str),
    Newtype(Kind),
    Tuple(Vec<Kind>),
    Struct(StructVariant),
}

/// A struct variant in an enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StructVariant {
    pub parent: &'static str,
    pub name: &'static str,
    pub doc: Option<&'static str>,
    pub fields: Vec<Field>,
}

macro_rules! impl_type {
    ($ty:ty, $kind:expr) => {
        impl crate::TypeInfo for $ty {
            fn kind() -> crate::Kind {
                $kind
            }
        }
    };
}

// bool
impl_type!(bool, Kind::Bool);

// string
impl_type!(String, Kind::String);
impl_type!(&'static str, Kind::String);

// int
//
// NOTE: u64 and i64 are going to be hard to represent in JavaScript/TypescriptA
// because f64 can't store ints past 2^53 -1. leave them out of here until we
// figure out if it makes sense to add a BigInt kind.
impl_type!(u8, Kind::Int);
impl_type!(u16, Kind::Int);
impl_type!(u32, Kind::Int);
impl_type!(i8, Kind::Int);
impl_type!(i16, Kind::Int);
impl_type!(i32, Kind::Int);

// float
impl_type!(f32, Kind::Float);
impl_type!(f64, Kind::Float);

/// An Option<T> is the same as a nullable T in our target languages.
impl<T: TypeInfo> TypeInfo for Option<T> {
    fn kind() -> Kind {
        T::kind()
    }

    fn nullable() -> bool {
        true
    }

    fn fields() -> Vec<Field> {
        T::fields()
    }
}

/// A Vec<T> wraps the Kind of the T's it contains.
impl<T: TypeInfo> TypeInfo for Vec<T> {
    fn kind() -> Kind {
        crate::Kind::Array(Box::new(T::kind()))
    }

    fn fields() -> Vec<Field> {
        Vec::new()
    }
}

impl<K: TypeInfo, V: TypeInfo> TypeInfo for BTreeMap<K, V> {
    fn kind() -> Kind {
        crate::Kind::Map(Box::new(K::kind()), Box::new(V::kind()))
    }
}

macro_rules! tuple_impl {
    ( $($type_param:tt),+ ) => {
        /// This trait is implemented for tuples up to 5 items long.
        impl<$($type_param: crate::TypeInfo),+> crate::TypeInfo for ($($type_param),+,) {
            fn kind() -> Kind {
                crate::Kind::Tuple(vec![
                    $(
                        <$type_param as crate::TypeInfo>::kind(),
                    )+
                ])
            }
        }
    };
}

tuple_impl!(T1);
tuple_impl!(T1, T2);
tuple_impl!(T1, T2, T3);
tuple_impl!(T1, T2, T3, T4);
tuple_impl!(T1, T2, T3, T4, T5);

// doctests below for compilation failures/unsupported cases in derive.
//
// these are here instead of in `tests/` because doctests are unsupported in
// binaries.
//
// https://github.com/rust-lang/cargo/issues/5477

/// ```compile_fail
/// use junction_typeinfo::TypeInfo;
///
/// #[derive(TypeInfo)]
/// struct Foo;
/// ```
#[doc(hidden)]
#[allow(unused)]
#[cfg(doctest)]
fn test_unsupported_unit_struct() {
    panic!("this function is just a doctest")
}

/// ```compile_fail
/// use junction_typeinfo::TypeInfo;
///
/// #[derive(TypeInfo)]
/// struct Foo();
/// ```
#[doc(hidden)]
#[allow(unused)]
#[cfg(doctest)]
fn test_unsupported_fieldless_tuple_struct() {
    panic!("this function is just a doctest")
}

/// ```compile_fail
/// use junction_typeinfo::TypeInfo;
///
/// #[derive(TypeInfo)]
/// struct Foo(String);
/// ```
#[doc(hidden)]
#[allow(unused)]
#[cfg(doctest)]
fn test_unsupported_newtype_struct() {
    panic!("this function is just a doctest")
}

/// ```compile_fail
/// use junction_typeinfo::TypeInfo;
///
/// #[derive(TypeInfo)]
/// union Foo {
///   a: f32,
///   b: u32,
/// }
/// ```
#[doc(hidden)]
#[allow(unused)]
#[cfg(doctest)]
fn test_unsupported_union() {
    panic!("this function is just a doctest")
}

/// ```compile_fail
/// use junction_typeinfo::TypeInfo;
/// use serde::Serialize;
///
/// #[derive(TypeInfo, Serialize)]
/// #[serde(tag = "type")]
/// enum Foo {
///   String(String),
///   Int(usize),
/// }
/// ```
#[cfg(doctest)]
fn test_unsupported_serde_flatten() {
    panic!("this function is just a doctest")
}
