use std::borrow::Cow;

use quote::{quote, quote_spanned};
use syn::{
    spanned::Spanned, Attribute, Data, DataEnum, DataStruct, DeriveInput, Expr, Field, FieldsNamed,
    FieldsUnnamed, Ident, Meta, Variant,
};

#[proc_macro_derive(TypeInfo)]
pub fn derive_typeinfo(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);

    let name = &input.ident;
    let TypeInfo { kind, fields, doc } = match typeinfo(&input) {
        Ok(v) => v,
        Err(e) => return e.into_compile_error().into(),
    };

    let expanded = quote! {
        impl junction_typeinfo::TypeInfo for #name {
            fn kind() -> junction_typeinfo::Kind {
                #kind
            }

            fn fields() -> Vec<junction_typeinfo::Field> {
                #fields
            }

            fn doc() -> Option<&'static str> {
                #doc
            }
        }
    };

    proc_macro::TokenStream::from(expanded)
}

struct TypeInfo {
    kind: proc_macro2::TokenStream,
    fields: proc_macro2::TokenStream,
    doc: proc_macro2::TokenStream,
}

fn typeinfo(input: &DeriveInput) -> syn::Result<TypeInfo> {
    let name = &input.ident;
    let attrs = &input.attrs;
    let data = &input.data;

    let ctx = serde_derive_internals::Ctxt::new();
    let container_attrs = serde_derive_internals::attr::Container::from_ast(&ctx, input);
    ctx.check()?;

    match data {
        Data::Struct(data) => struct_typeinfo(name, attrs, data),
        Data::Enum(data) => enum_typeinfo(name, attrs, &container_attrs, data),
        Data::Union(_) => Err(syn::Error::new_spanned(
            name,
            "Deriving TypeInfo for a union is not supported.",
        )),
    }
}

fn struct_typeinfo(name: &Ident, attrs: &[Attribute], data: &DataStruct) -> syn::Result<TypeInfo> {
    let doc = compile_doc(attrs);

    match &data.fields {
        syn::Fields::Named(fields) => Ok(TypeInfo {
            kind: quote! {
                junction_typeinfo::Kind::Object(stringify!(#name))
            },
            fields: struct_fields(fields, &None)?,
            doc,
        }),
        syn::Fields::Unnamed(fields) if fields.unnamed.is_empty() => Err(syn::Error::new_spanned(
            name,
            "Deriving TypeInfo for a unit struct is not supported",
        )),
        syn::Fields::Unnamed(fields) if fields.unnamed.len() == 1 => Err(syn::Error::new_spanned(
            name,
            "Deriving TypeInfo for a Newtype struct is not supported",
        )),
        syn::Fields::Unnamed(fields) => {
            let kinds = tuple_kinds(fields);
            Ok(TypeInfo {
                kind: quote! {
                    junction_typeinfo::Kind::Tuple(#kinds)
                },
                fields: empty_vec(),
                doc,
            })
        }
        syn::Fields::Unit => Err(syn::Error::new_spanned(
            name,
            "Deriving TypeInfo for a unit struct is not supported",
        )),
    }
}

fn enum_typeinfo(
    name: &Ident,
    attrs: &[Attribute],
    container_attrs: &serde_derive_internals::attr::Container,
    data: &DataEnum,
) -> syn::Result<TypeInfo> {
    let mut variants = Vec::with_capacity(data.variants.len());
    let internal_tag = match container_attrs.tag() {
        serde_derive_internals::attr::TagType::Internal { tag } => {
            Some(syn::Ident::new(tag, name.span()))
        }
        _ => None,
    };

    for v in &data.variants {
        match &v.fields {
            // a fieldless variant gets treated as a string literal
            syn::Fields::Unit => variants.push(unit_variant(name, v, &internal_tag)),
            // a tuple variant gets treated differently depending on how many
            // fields it has.
            //
            // - with zero fields it's a literal
            // - with exactly one field, it acts like a newtype struct
            // - with more than one field, it acts like a tuple.
            //
            // serde doesn't support internal tags on tuple structs, so only newtype
            // variants have to worry about it.
            //
            // we generally don't support newtype structs anywhere so just return an error
            // if an unnamed field gets mixed in when the enum is internally tagged.
            syn::Fields::Unnamed(fields) => match fields.unnamed.len() {
                0 => variants.push(unit_variant(name, v, &internal_tag)),
                1 => {
                    let field = fields.unnamed.first().expect(
                        "junction-typeinfo-derive: proc-macro has a bug: fields has no elements",
                    );
                    variants.push(newtype_variant(name, v, field, &internal_tag));
                }
                _ => {
                    if internal_tag.is_some() {
                        return Err(syn::Error::new(
                            v.span(),
                            "TypeInfo can't be derived for an internally \
                                tagged enum with Tuple variants. Either remove this variant or \
                                remove the serde(tag) attribute",
                        ));
                    }
                    let kinds = tuple_kinds(fields);
                    variants.push(quote! {
                        junction_typeinfo::Variant::Tuple(#kinds)
                    });
                }
            },
            // A named variant should always be turned into an anonymous object
            syn::Fields::Named(fields) => {
                let variant_name = &v.ident;
                let fields = struct_fields(fields, &internal_tag.as_ref().map(|t| (t, v)))?;
                let doc = compile_doc(&v.attrs);

                variants.push(quote_spanned! {v.span()=>
                    junction_typeinfo::Variant::Struct(junction_typeinfo::StructVariant {
                        parent: stringify!(#name),
                        name: stringify!(#variant_name),
                        doc: #doc,
                        fields: #fields,
                    })
                });
            }
        }
    }

    let kind = quote! {
        junction_typeinfo::Kind::Union(
            stringify!(#name),
            vec![
                #( #variants, )*
            ],
        )
    };

    let doc = compile_doc(attrs);

    Ok(TypeInfo {
        kind,
        doc,
        fields: empty_vec(),
    })
}

fn newtype_variant(
    enum_name: &Ident,
    variant: &Variant,
    field: &Field,
    tag_field: &Option<Ident>,
) -> proc_macro2::TokenStream {
    if let Some(tag_field) = tag_field {
        let field_type = &field.ty;
        let variant_name = &variant.ident;
        let tag_field = internal_tag_field(tag_field, variant);

        quote_spanned! {variant.span()=>
            {
                let mut fields = vec![#tag_field];
                fields.extend(<#field_type as junction_typeinfo::TypeInfo>::fields());

                junction_typeinfo::Variant::Struct(junction_typeinfo::StructVariant{
                    parent: stringify!(#enum_name),
                    name: stringify!(#variant_name),
                    doc: None,
                    fields,
                })
            }
        }
    } else {
        let field_type = &field.ty;
        quote_spanned! {variant.span()=>
            junction_typeinfo::Variant::Newtype(<#field_type as junction_typeinfo::TypeInfo>::kind())
        }
    }
}

fn unit_variant(
    enum_name: &Ident,
    variant: &Variant,
    tag_field: &Option<Ident>,
) -> proc_macro2::TokenStream {
    if let Some(tag_field) = tag_field {
        let variant_name = &variant.ident;
        let tag_field = internal_tag_field(tag_field, variant);
        quote_spanned! {variant.span()=>
            junction_typeinfo::Variant::Struct(junction_typeinfo::StructVariant{
                parent: stringify!(#enum_name),
                name: stringify!(#variant_name),
                doc: None,
                fields: vec![#tag_field],
            })
        }
    } else {
        let name: &Ident = &variant.ident;
        let span = variant.span();
        quote_spanned! {span=>
            junction_typeinfo::Variant::Literal(stringify!(#name))
        }
    }
}

fn struct_fields(
    fields: &FieldsNamed,
    internal_tag: &Option<(&Ident, &Variant)>,
) -> syn::Result<proc_macro2::TokenStream> {
    let mut field_stmts = vec![];

    if let Some((tag, variant)) = internal_tag {
        let tag_field = internal_tag_field(tag, variant);
        field_stmts.push(quote! {
            fields.push(#tag_field);
        });
    }

    for (i, f) in fields.named.iter().enumerate() {
        if is_hidden(f)? {
            continue;
        }

        let ctx = serde_derive_internals::Ctxt::new();
        let field_attrs = serde_derive_internals::attr::Field::from_ast(
            &ctx,
            i,
            f,
            None,
            &serde_derive_internals::attr::Default::None,
        );
        ctx.check()?;

        if field_attrs.flatten() {
            let field_type = &f.ty;
            field_stmts.push(quote! {
                fields.extend(
                <#field_type as junction_typeinfo::TypeInfo>::flatten_fields()
                );
            });
        } else {
            let field_name = f.ident.as_ref().map(strip_raw_prefix);
            let field_type = &f.ty;
            let field_doc = compile_doc(&f.attrs);

            field_stmts.push(quote_spanned! {f.span()=>
                fields.push(junction_typeinfo::Field {
                    name: stringify!(#field_name),
                    nullable: <#field_type as junction_typeinfo::TypeInfo>::nullable(),
                    kind: <#field_type as junction_typeinfo::TypeInfo>::kind(),
                    doc: #field_doc,
                });
            });
        }
    }

    Ok(quote! {
        {
            let mut fields = Vec::new();
            #( #field_stmts )*

            fields
        }
    })
}

fn is_hidden(field: &Field) -> syn::Result<bool> {
    for attr in &field.attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }

        let mut hidden = false;
        // try to parse doc(hidden). this errors out on regular rustdoc, because
        // it turns into #[doc = "your rustdoc"] attributes which is an unexpected
        // structure for this parser.
        //
        // just full steam ahead and parse, ignoring the error if we get one. it
        // wasn't doc(hidden) if it was an error.
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("hidden") {
                hidden = true;
            }
            Ok(())
        });

        if hidden {
            return Ok(true);
        }
    }

    Ok(false)
}

fn internal_tag_field(tag: &Ident, variant: &Variant) -> proc_macro2::TokenStream {
    let kind = internal_tag_kind(std::iter::once(variant));
    quote! {
        junction_typeinfo::Field {
            name: stringify!(#tag),
            nullable: false,
            kind: #kind,
            doc: None,
        }
    }
}
fn internal_tag_kind<'a>(variants: impl Iterator<Item = &'a Variant>) -> proc_macro2::TokenStream {
    let mut literals = vec![];
    for v in variants {
        let variant_name = &v.ident;
        literals.push(quote! {
            junction_typeinfo::Variant::Literal(stringify!(#variant_name))
        });
    }

    quote! {
        junction_typeinfo::Kind::Union("type", vec![#( #literals, )*])
    }
}

fn compile_doc(attrs: &[Attribute]) -> proc_macro2::TokenStream {
    let mut full_doc = String::new();
    for attr in attrs {
        let Some(doc_value) = get_doc_value(attr) else {
            continue;
        };

        match doc_value {
            Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(lit),
                ..
            }) => {
                full_doc.push('\n');
                full_doc.push_str(&lit.value());
            }
            _ => continue,
        }
    }

    if full_doc.is_empty() {
        quote! { None }
    } else {
        let doc = full_doc.trim().to_string();

        quote! {
            Some(#doc)
        }
    }
}

fn get_doc_value(attr: &Attribute) -> Option<&Expr> {
    let ident = attr.path().get_ident()?;

    if ident != "doc" {
        return None;
    }

    match &attr.meta {
        Meta::NameValue(kv) => Some(&kv.value),
        _ => None,
    }
}

fn strip_raw_prefix(ident: &Ident) -> Cow<Ident> {
    match ident.to_string().strip_prefix("r#") {
        Some(stripped) => Cow::Owned(Ident::new(stripped, ident.span())),
        None => Cow::Borrowed(ident),
    }
}

fn tuple_kinds(fields: &FieldsUnnamed) -> proc_macro2::TokenStream {
    let field_kinds = fields.unnamed.iter().map(|f| {
        let field_type = &f.ty;

        quote_spanned! {f.span()=>
            <#field_type as junction_typeinfo::TypeInfo>::kind()
        }
    });
    quote_spanned! {fields.span()=>
        vec![
            #( #field_kinds, )*
        ]
    }
}

fn empty_vec() -> proc_macro2::TokenStream {
    quote! {
        Vec::new()
    }
}
