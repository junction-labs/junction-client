use askama::Template;

// TODO: To make mypy happy, we have to make TypedDict keys optional. This
// requires shaving some yaks around typing: we can use typing.NotRequired on
// any individual field that's nullable (or has a default), but for python<3.11
// that involves installing typing_extensions and some backport fun.
//
// See: https://peps.python.org/pep-0655/

/// Generate Python type information for Junction API config.
///
/// The generated code is suitable for saving as a .pyi stub file. See
/// [PEP-484](PEP-484) for details on what may/may not be included.
///
/// The types generated here should be types that can't be exposed through PyO3
/// or types that need a native-looking Python implementation that can be
/// included in docs, or browsed as source.
///
/// [PEP-484]: https://peps.python.org/pep-0484/#stub-files
pub fn generate(
    w: &mut impl std::fmt::Write,
    items: Vec<junction_typeinfo::Item>,
) -> anyhow::Result<()> {
    let required_packages = ["typing", "datetime"];

    writeln!(w, "{}", MODULE_HEADER.trim())?;

    for package in required_packages {
        writeln!(w, "import {package}")?;
    }
    write!(w, "\n\n")?;

    PyDef::duration().render(w)?;
    writeln!(w)?;

    for item in items {
        for py_def in into_pydefs(item) {
            py_def.render(w)?;
            writeln!(w)?;
        }
    }

    Ok(())
}

const MODULE_HEADER: &str = r#"
"""Junction client configuration."""

# This file is automatically generated with junction-api-gen. Do not edit!
"#;

/// A python definition that we'd like to generate.
///
/// 99% of this module exists to support generated `typing.TypedDict`s for our
/// config APIs, which means we need to generate either dicts or unions that
/// represent a member field of one of those dicts.
///
/// There are absolutely more types in Python, we simply don't care about them.
#[derive(Debug)]
enum PyDef {
    TypedDict(PyTypedDict),
    Union(PyUnion),
}

impl PyDef {
    /// PyDefs are renderable to text with templates.
    ///
    /// See [PyDict] and [PyUnion] for the definition of those templates.
    fn render(&self, w: &mut impl std::fmt::Write) -> askama::Result<()> {
        match self {
            PyDef::TypedDict(py_dict) => py_dict.render_into(w),
            PyDef::Union(py_union) => py_union.render_into(w),
        }
    }

    fn duration() -> Self {
        PyDef::Union(PyUnion {
            name: "Duration",
            doc: Some("A duration expressed as a total number of seconds. Durations should never be negative."),
            types: vec![PyType::Str, PyType::Int, PyType::Float],
        })
    }
}

#[derive(Debug, Clone, Template)]
#[template(
    source = r#"
class {{name}}(typing.TypedDict):
{%- match doc -%}
{%- when Some with (doc) %}
    """{{ doc|doc_pad(4) }}"""
{% when None %}
{%- endmatch -%}
{%- for field in fields %}
    {{ field.name }}: {{ field.py_type }}
    {%- match field.doc -%}
    {% when Some with (doc) %}
    """{{ doc|doc_pad(4) }}"""
    {% when None %}
    {%- endmatch -%}
{%- endfor -%}
    "#,
    ext = "py",
    escape = "none"
)]
struct PyTypedDict {
    name: &'static str,
    doc: Option<&'static str>,
    fields: Vec<PyDictField>,
}

#[derive(Debug, Template)]
#[template(
    source = r#"
{{name}} = typing.Union[
    {%- for type in types -%}
        {{ type }} {% if !loop.last -%},{%- endif %}
    {%- endfor -%}
]
{%- match doc -%}
{%- when Some with (doc) %}
"""{{ doc|doc_pad(4) }}"""
{% when None %}
{%- endmatch -%}
    "#,
    ext = "py",
    escape = "none"
)]
struct PyUnion {
    name: &'static str,
    doc: Option<&'static str>,
    types: Vec<PyType>,
}

// askama filters. These need to be in a module named `filters` near the
// templates that are being compiled with derive(Template).
mod filters {
    use std::borrow::Cow;

    use once_cell::sync::Lazy;
    use regex::Regex;

    pub fn doc_pad(s: &str, padding: usize) -> askama::Result<Cow<str>> {
        static RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n( |\t)").unwrap());

        let replacement = "\n".to_string() + &" ".repeat(padding);
        Ok(RE.replace_all(s, replacement))
    }
}

/// Generate python defs for some type info. Only
/// [Objects][junction_typeinfo::Kind::Object] and
/// [Unions][junction_typeinfo::Kind::Union] generate definitions.
///
/// An enum with [struct][junction_typeinfo::Variant::Struct] variants, or
/// tagged type information, will generate a [PyDict] for each variant and a
/// final [PyUnion] for the enum itself.
fn into_pydefs(item: junction_typeinfo::Item) -> Vec<PyDef> {
    let mut defs = vec![];

    match item.kind {
        junction_typeinfo::Kind::Union(name, variants) => {
            for variant in &variants {
                if let junction_typeinfo::Variant::Struct(struct_variant) = variant {
                    defs.push(PyDef::TypedDict(struct_variant.clone().into()));
                }
            }

            let types = variants.into_iter().map(|v| v.into()).collect();
            defs.push(PyDef::Union(PyUnion {
                name,
                types,
                doc: None,
            }));
        }
        junction_typeinfo::Kind::Object(name) => {
            defs.push(PyDef::TypedDict(PyTypedDict {
                name,
                doc: item.doc,
                fields: item.fields.into_iter().map(|f| f.into()).collect(),
            }));
        }
        _ => (), // Do nothing
    };

    defs
}

impl From<junction_typeinfo::StructVariant> for PyTypedDict {
    fn from(item: junction_typeinfo::StructVariant) -> Self {
        let name = format!(
            "{parent_name}{item_name}",
            parent_name = item.parent,
            item_name = item.name
        );
        let fields = item.fields.into_iter().map(|f| f.into()).collect();
        PyTypedDict {
            name: name.leak(),
            doc: item.doc,
            fields,
        }
    }
}

/// Types we'd like to represent in Python.
///
/// There are more types, but these are the ones we care about mapping
/// [junction_typeinfo::Kind]s into.
#[derive(Debug, Clone)]
enum PyType {
    /// A Python string.
    Str,

    /// A literal string. Can be used as a typehint.
    LiteralStr(&'static str),

    /// A Python int.
    Int,

    /// A Python float.
    Float,

    /// A Python bool.
    Bool,

    /// A duration. Can be expressed as a string listing hours/minutes/seconds,
    /// or as an number of seconds (int or float).
    Duration,

    /// A Python Dict with type-hints for keys and values.
    Dict(Box<PyType>, Box<PyType>),

    /// A TypedDict with a name and fields.
    TypedDict(PyTypedDict),

    /// A Python `list` of items that all share a type.
    List(Box<PyType>),

    /// A Python `tuple` of items.
    Tuple(Vec<PyType>),

    /// A typing.Union of multiple types, used as a type hint.
    Union(&'static str, Vec<PyType>),

    /// An object in this namespace with the given name.
    Object(&'static str),
}

impl PyType {
    fn as_literal_str(&self) -> Option<&'static str> {
        match self {
            PyType::LiteralStr(s) => Some(s),
            _ => None,
        }
    }
}

impl std::fmt::Display for PyType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PyType::Str => write!(f, "str"),
            PyType::LiteralStr(lit) => write!(f, "typing.Literal[\"{lit}\"]"),
            PyType::Int => write!(f, "int"),
            PyType::Float => write!(f, "float"),
            PyType::Bool => write!(f, "bool"),
            PyType::Duration => write!(f, "Duration"),
            PyType::List(ty) => write!(f, "typing.List[{ty}]"),
            PyType::Tuple(py_types) => {
                write!(f, "typing.Tuple[")?;

                for (i, py_ty) in py_types.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", py_ty)?;
                }

                write!(f, "]")
            }
            PyType::Union(name, types) => {
                if types.iter().all(|t| matches!(t, PyType::LiteralStr(_))) {
                    let lit_types: Vec<_> = types
                        .iter()
                        .map(|t| format!("typing.Literal[\"{}\"]", t.as_literal_str().unwrap()))
                        .collect();

                    match lit_types.as_slice() {
                        [t] => write!(f, "{t}"),
                        _ => write!(f, "typing.Union[{}]", lit_types.join(", ")),
                    }
                } else {
                    write!(f, "{name}")
                }
            }
            PyType::Object(name) => write!(f, "{name}"),
            PyType::Dict(k, v) => {
                write!(f, "typing.Dict[{k}, {v}]")
            }
            PyType::TypedDict(d) => write!(f, "{name}", name = d.name),
        }
    }
}

impl From<junction_typeinfo::Kind> for PyType {
    fn from(kind: junction_typeinfo::Kind) -> Self {
        match kind {
            junction_typeinfo::Kind::Bool => Self::Bool,
            junction_typeinfo::Kind::String => Self::Str,
            junction_typeinfo::Kind::Int => Self::Int,
            junction_typeinfo::Kind::Float => Self::Float,
            junction_typeinfo::Kind::Union(name, kinds) => {
                Self::Union(name, kinds.into_iter().map(|k| k.into()).collect())
            }
            junction_typeinfo::Kind::Tuple(kinds) => {
                Self::Tuple(kinds.into_iter().map(|k| k.into()).collect())
            }
            junction_typeinfo::Kind::Array(kind) => Self::List(Box::new((*kind).into())),
            junction_typeinfo::Kind::Object(name) => Self::Object(name),
            junction_typeinfo::Kind::Map(k, v) => {
                let k = (*k).into();
                let v = (*v).into();
                Self::Dict(Box::new(k), Box::new(v))
            }
            junction_typeinfo::Kind::Duration => Self::Duration,
        }
    }
}

impl From<junction_typeinfo::Variant> for PyType {
    fn from(variant: junction_typeinfo::Variant) -> Self {
        match variant {
            junction_typeinfo::Variant::Literal(lit) => PyType::LiteralStr(lit),
            junction_typeinfo::Variant::Newtype(kind) => kind.into(),
            junction_typeinfo::Variant::Tuple(kinds) => {
                PyType::Tuple(kinds.into_iter().map(|k| k.into()).collect())
            }
            junction_typeinfo::Variant::Struct(struct_variant) => {
                PyType::TypedDict(struct_variant.into())
            }
        }
    }
}

#[derive(Debug, Clone)]
struct PyDictField {
    name: &'static str,
    doc: Option<&'static str>,
    py_type: PyType,
}

impl From<junction_typeinfo::Field> for PyDictField {
    fn from(field: junction_typeinfo::Field) -> Self {
        let name = field.name;
        let py_type = field.kind.into();
        let doc = field.doc;
        Self { name, doc, py_type }
    }
}
