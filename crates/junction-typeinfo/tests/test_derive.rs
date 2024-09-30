use junction_typeinfo::{Field, Kind, StructVariant, TypeInfo, Variant};
use serde::Serialize;

// tests for defining TypeInfo on foreign types.
//
// Tests for compilation failures of derive(TypeInfo) are in lib.rs because
// testing compilation failures is easily done through doctests, but doctests
// aren't supported in binary targets.

#[test]
fn test_struct() {
    /// This struct has docs.
    #[allow(unused)]
    #[derive(TypeInfo)]
    struct Foo {
        /// One doc.
        one: i32,
        /** two doc */
        two: String,
        three: bool,
    }

    assert_eq!(Foo::kind(), junction_typeinfo::Kind::Object("Foo"));
    assert!(!Foo::nullable());
    assert_eq!(Foo::doc(), Some("This struct has docs."));
    assert_eq!(
        Foo::fields(),
        vec![
            Field {
                name: "one",
                nullable: false,
                kind: Kind::Int,
                doc: Some("One doc."),
            },
            Field {
                name: "two",
                nullable: false,
                kind: Kind::String,
                doc: Some("two doc"),
            },
            Field {
                name: "three",
                nullable: false,
                kind: Kind::Bool,
                doc: None,
            },
        ]
    );
}

#[test]
fn test_struct_doc_hidden() {
    /// This struct has docs.
    #[allow(unused)]
    #[derive(TypeInfo)]
    struct Foo {
        /// One doc.
        one: i32,
        /** two doc */
        two: String,
        #[doc(hidden)]
        three: bool,
    }

    assert_eq!(Foo::kind(), junction_typeinfo::Kind::Object("Foo"));
    assert!(!Foo::nullable());
    assert_eq!(Foo::doc(), Some("This struct has docs."));
    assert_eq!(
        Foo::fields(),
        vec![
            Field {
                name: "one",
                nullable: false,
                kind: Kind::Int,
                doc: Some("One doc."),
            },
            Field {
                name: "two",
                nullable: false,
                kind: Kind::String,
                doc: Some("two doc"),
            },
        ]
    );
}

#[test]
fn test_tuple_struct() {
    /// This is a tuple struct.
    #[allow(unused)]
    #[derive(TypeInfo)]
    struct Foo(u32, f32);

    assert_eq!(
        Foo::kind(),
        junction_typeinfo::Kind::Tuple(vec![Kind::Int, Kind::Float])
    );
    assert!(!Foo::nullable());
    assert_eq!(Foo::fields(), Vec::new(),);
    assert_eq!(Foo::doc(), Some("This is a tuple struct."));
}

#[test]
fn test_enum() {
    #[allow(unused)]
    #[derive(TypeInfo)]
    struct Bar {
        baz: String,
    }

    #[allow(unused)]
    #[derive(TypeInfo)]
    enum Foo {
        One,
        Two(),
        Three(i32),
        Four(Bar),
        Five(f64, Bar),
        Six {},
        /// This is some variant, that is for sure.
        Seven {
            named: String,
        },
        Eight {
            named: String,
            with_many: bool,
        },
    }

    assert_eq!(
        Foo::kind(),
        junction_typeinfo::Kind::Union(
            "Foo",
            vec![
                Variant::Literal("One"),
                Variant::Literal("Two"),
                Variant::Newtype(Kind::Int),
                Variant::Newtype(Kind::Object("Bar")),
                Variant::Tuple(vec![Kind::Float, Kind::Object("Bar"),]),
                Variant::Struct(StructVariant {
                    parent: "Foo",
                    name: "Six",
                    doc: None,
                    fields: vec![],
                }),
                Variant::Struct(StructVariant {
                    parent: "Foo",
                    name: "Seven",
                    doc: Some("This is some variant, that is for sure."),
                    fields: vec![Field {
                        name: "named",
                        nullable: false,
                        kind: Kind::String,
                        doc: None,
                    }],
                }),
                Variant::Struct(StructVariant {
                    parent: "Foo",
                    name: "Eight",
                    doc: None,
                    fields: vec![
                        Field {
                            name: "named",
                            nullable: false,
                            kind: Kind::String,
                            doc: None,
                        },
                        Field {
                            name: "with_many",
                            nullable: false,
                            kind: Kind::Bool,
                            doc: None,
                        },
                    ]
                }),
            ]
        )
    );
    assert!(!Foo::nullable());
    assert_eq!(Foo::fields(), Vec::new());
}

#[test]
fn test_escaped_name() {
    #[allow(unused)]
    #[derive(TypeInfo)]
    struct Foo {
        r#type: String,
    }

    assert_eq!(Foo::kind(), junction_typeinfo::Kind::Object("Foo"));
    assert!(!Foo::nullable());
    assert_eq!(
        Foo::fields(),
        vec![Field {
            name: "type",
            kind: Kind::String,
            nullable: false,
            doc: None,
        }]
    )
}

#[test]
fn test_serde_flatten_struct() {
    #[allow(unused)]
    #[derive(Serialize, TypeInfo)]
    struct Bar {
        baz: String,
    }

    #[allow(unused)]
    #[derive(Serialize, TypeInfo)]
    struct Foo {
        foo: String,

        #[serde(flatten)]
        bar: Bar,
    }

    assert_eq!(Foo::kind(), junction_typeinfo::Kind::Object("Foo"));
    assert!(!Foo::nullable());
    assert_eq!(
        Foo::fields(),
        vec![
            Field {
                name: "foo",
                kind: Kind::String,
                nullable: false,
                doc: None,
            },
            Field {
                name: "baz",
                kind: Kind::String,
                nullable: false,
                doc: None,
            },
        ]
    )
}

#[test]
fn test_serde_flatten_enum() {
    #[allow(unused)]
    #[derive(Serialize, TypeInfo)]
    #[serde(tag = "type")]
    enum Bar {
        One { value: String },
        Two { value: String },
        Three { other_thing: i32 },
    }

    #[allow(unused)]
    #[derive(Serialize, TypeInfo)]
    struct Foo {
        foo: String,

        #[serde(flatten)]
        bar: Bar,
    }

    assert_eq!(Foo::kind(), junction_typeinfo::Kind::Object("Foo"));
    assert!(!Foo::nullable());
    assert_eq!(
        Foo::fields(),
        vec![
            Field {
                name: "foo",
                kind: Kind::String,
                nullable: false,
                doc: None,
            },
            Field {
                name: "other_thing",
                kind: Kind::Int,
                nullable: false,
                doc: None,
            },
            Field {
                name: "type",
                kind: Kind::Union(
                    "type",
                    vec![
                        Variant::Literal("One"),
                        Variant::Literal("Two"),
                        Variant::Literal("Three"),
                    ]
                ),
                nullable: false,
                doc: None,
            },
            Field {
                name: "value",
                kind: Kind::String,
                nullable: false,
                doc: None,
            },
        ]
    )
}

#[test]
fn test_serde_tag() {
    #[derive(TypeInfo, Serialize)]
    struct Bar {
        bar: u32,
    }

    #[allow(unused)]
    #[derive(TypeInfo, Serialize)]
    #[serde(tag = "type")]
    enum Foo {
        One,
        Two { value: i32 },
        Three { value: String },
        Four(Bar),
    }

    fn type_kind(lit: &'static str) -> Kind {
        Kind::Union("type", vec![Variant::Literal(lit)])
    }

    assert_eq!(
        Foo::variant_fields(),
        vec![
            Field {
                name: "bar",
                kind: Kind::Int,
                nullable: false,
                doc: None,
            },
            Field {
                name: "type",
                kind: Kind::Union(
                    "type",
                    vec![
                        Variant::Literal("One"),
                        Variant::Literal("Two"),
                        Variant::Literal("Three"),
                        Variant::Literal("Four"),
                    ]
                ),
                nullable: false,
                doc: None,
            },
            Field {
                name: "value",
                kind: Kind::Union(
                    "value",
                    vec![Variant::Newtype(Kind::Int), Variant::Newtype(Kind::String)]
                ),
                nullable: false,
                doc: None,
            },
        ]
    );

    assert_eq!(
        Foo::kind(),
        junction_typeinfo::Kind::Union(
            "Foo",
            vec![
                Variant::Struct(StructVariant {
                    parent: "Foo",
                    name: "One",
                    doc: None,
                    fields: vec![Field {
                        name: "type",
                        nullable: false,
                        kind: type_kind("One"),
                        doc: None,
                    },],
                }),
                Variant::Struct(StructVariant {
                    parent: "Foo",
                    name: "Two",
                    doc: None,
                    fields: vec![
                        Field {
                            name: "type",
                            nullable: false,
                            kind: type_kind("Two"),
                            doc: None,
                        },
                        Field {
                            name: "value",
                            nullable: false,
                            kind: Kind::Int,
                            doc: None,
                        }
                    ],
                }),
                Variant::Struct(StructVariant {
                    parent: "Foo",
                    name: "Three",
                    doc: None,
                    fields: vec![
                        Field {
                            name: "type",
                            nullable: false,
                            kind: type_kind("Three"),
                            doc: None,
                        },
                        Field {
                            name: "value",
                            nullable: false,
                            kind: Kind::String,
                            doc: None,
                        }
                    ],
                }),
                Variant::Struct(StructVariant {
                    parent: "Foo",
                    name: "Four",
                    doc: None,
                    fields: vec![
                        Field {
                            name: "type",
                            nullable: false,
                            kind: type_kind("Four"),
                            doc: None,
                        },
                        Field {
                            name: "bar",
                            nullable: false,
                            kind: Kind::Int,
                            doc: None,
                        }
                    ],
                }),
            ]
        )
    );
}
