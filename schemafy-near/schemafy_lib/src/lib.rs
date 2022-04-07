// This would be nice once it stabilizes:
// https://github.com/rust-lang/rust/issues/44732
// #![feature(external_doc)]
// #![doc(include = "../README.md")]

//! This is a Rust crate which can take a [json schema (draft
//! 4)](http://json-schema.org/) and generate Rust types which are
//! serializable with [serde](https://serde.rs/). No checking such as
//! `min_value` are done but instead only the structure of the schema
//! is followed as closely as possible.
//!
//! As a schema could be arbitrarily complex this crate makes no
//! guarantee that it can generate good types or even any types at all
//! for a given schema but the crate does manage to bootstrap itself
//! which is kind of cool.
//!
//! ## Example
//!
//! Generated types for VS Codes [debug server protocol][]: <https://docs.rs/debugserver-types>
//!
//! [debug server protocol]:https://code.visualstudio.com/docs/extensions/example-debuggers
//!
//! ## Usage
//!
//! Rust code is generated by providing a [`Schema`](./struct.Schema.html) struct (which can be deserialized from JSON).
//!
//! A proc macro is available in [`schemafy`](https://docs.rs/schemafy) crate
//!
//! ```rust
//! extern crate serde;
//! extern crate schemafy_core;
//! extern crate serde_json;
//!
//! use serde::{Serialize, Deserialize};
//! use schemafy_lib::Expander;
//!
//! let json = std::fs::read_to_string("src/schema.json").expect("Read schema JSON file");
//!
//! let schema = serde_json::from_str(&json).unwrap();
//! let mut expander = Expander::new(
//!     Some("Schema"),
//!     "::schemafy_core::",
//!     &schema,
//! );
//!
//! let code = expander.expand(&schema);
//! ```

#[macro_use]
extern crate serde_derive;

#[macro_use]
extern crate quote;

pub mod generator;

/// Types from the JSON Schema meta-schema (draft 4).
///
/// This module is itself generated from a JSON schema.
mod schema;

use std::{borrow::Cow, convert::TryFrom};

use inflector::Inflector;

use serde_json::Value;

use uriparse::{Fragment, URI};

pub use schema::{Schema, SimpleTypes};

pub use generator::{Generator, GeneratorBuilder};

use proc_macro2::{Span, TokenStream};

fn replace_invalid_identifier_chars(s: &str) -> String {
    s.strip_prefix('$').unwrap_or(s).replace(|c: char| !c.is_alphanumeric() && c != '_', "_")
}

fn replace_numeric_start(s: &str) -> String {
    if s.chars().next().map(|c| c.is_numeric()).unwrap_or(false) {
        format!("_{}", s)
    } else {
        s.to_string()
    }
}

fn remove_excess_underscores(s: &str) -> String {
    let mut result = String::new();
    let mut char_iter = s.chars().peekable();

    while let Some(c) = char_iter.next() {
        let next_c = char_iter.peek();
        if c != '_' || !matches!(next_c, Some('_')) {
            result.push(c);
        }
    }

    result
}

pub fn str_to_ident(s: &str) -> syn::Ident {
    if s.is_empty() {
        return syn::Ident::new("empty_", Span::call_site());
    }

    if s.chars().all(|c| c == '_') {
        return syn::Ident::new("underscore_", Span::call_site());
    }

    let s = replace_invalid_identifier_chars(s);
    let s = replace_numeric_start(&s);
    let s = remove_excess_underscores(&s);

    if s.is_empty() {
        return syn::Ident::new("invalid_", Span::call_site());
    }

    let keywords = [
        "as", "break", "const", "continue", "crate", "else", "enum", "extern", "false", "fn",
        "for", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut", "pub", "ref",
        "return", "self", "static", "struct", "super", "trait", "true", "type", "unsafe", "use",
        "where", "while", "abstract", "become", "box", "do", "final", "macro", "override", "priv",
        "typeof", "unsized", "virtual", "yield", "async", "await", "try",
    ];
    if keywords.iter().any(|&keyword| keyword == s) {
        return syn::Ident::new(&format!("{}_", s), Span::call_site());
    }

    syn::Ident::new(&s, Span::call_site())
}

fn rename_keyword(prefix: &str, s: &str) -> Option<TokenStream> {
    let n = str_to_ident(s);

    if n == s {
        return None;
    }

    if prefix.is_empty() {
        Some(quote! {
            #[serde(rename = #s)]
            #n
        })
    } else {
        let prefix = syn::Ident::new(prefix, Span::call_site());
        Some(quote! {
            #[serde(rename = #s)]
            #prefix #n
        })
    }
}

fn field(s: &str) -> TokenStream {
    if let Some(t) = rename_keyword("pub", s) {
        return t;
    }
    let snake = s.to_snake_case();
    if snake == s && !snake.contains(|c: char| c == '$' || c == '#') {
        let field = syn::Ident::new(s, Span::call_site());
        return quote!( pub #field );
    }

    let field = if snake.is_empty() {
        syn::Ident::new("underscore", Span::call_site())
    } else {
        str_to_ident(&snake)
    };

    quote! {
        #[serde(rename = #s)]
        pub #field
    }
}

fn merge_option<T, F>(mut result: &mut Option<T>, r: &Option<T>, f: F)
where
    F: FnOnce(&mut T, &T),
    T: Clone,
{
    *result = match (&mut result, r) {
        (&mut &mut Some(ref mut result), &Some(ref r)) => return f(result, r),
        (&mut &mut None, &Some(ref r)) => Some(r.clone()),
        _ => return,
    };
}

fn merge_all_of(result: &mut Schema, r: &Schema) {
    use std::collections::btree_map::Entry;

    for (k, v) in &r.properties {
        match result.properties.entry(k.clone()) {
            Entry::Vacant(entry) => {
                entry.insert(v.clone());
            }
            Entry::Occupied(mut entry) => merge_all_of(entry.get_mut(), v),
        }
    }

    if let Some(ref ref_) = r.ref_ {
        result.ref_ = Some(ref_.clone());
    }

    if let Some(ref description) = r.description {
        result.description = Some(description.clone());
    }

    merge_option(&mut result.required, &r.required, |required, r_required| {
        required.extend(r_required.iter().cloned());
    });

    result.type_.retain(|e| r.type_.contains(e));
}

const LINE_LENGTH: usize = 100;
const INDENT_LENGTH: usize = 4;

fn make_doc_comment(mut comment: &str, remaining_line: usize) -> TokenStream {
    let mut out_comment = String::new();
    out_comment.push_str("/// ");
    let mut length = 4;
    while let Some(word) = comment.split(char::is_whitespace).next() {
        if comment.is_empty() {
            break;
        }
        comment = &comment[word.len()..];
        if length + word.len() >= remaining_line {
            out_comment.push_str("\n/// ");
            length = 4;
        }
        out_comment.push_str(word);
        length += word.len();
        let mut n = comment.chars();
        match n.next() {
            Some('\n') => {
                out_comment.push('\n');
                out_comment.push_str("/// ");
                length = 4;
            }
            Some(_) => {
                out_comment.push(' ');
                length += 1;
            }
            None => (),
        }
        comment = n.as_str();
    }
    if out_comment.ends_with(' ') {
        out_comment.pop();
    }
    out_comment.push('\n');
    out_comment.parse().unwrap()
}

struct FieldExpander<'a, 'r: 'a> {
    default: bool,
    expander: &'a mut Expander<'r>,
}

impl<'a, 'r> FieldExpander<'a, 'r> {
    fn expand_fields(&mut self, type_name: &str, schema: &Schema) -> Vec<TokenStream> {
        let schema = self.expander.schema(schema);
        schema
            .properties
            .iter()
            .map(|(field_name, value)| {
                self.expander.current_field.clone_from(field_name);
                let key = field(field_name);
                let required =
                    schema.required.iter().flat_map(|a| a.iter()).any(|req| req == field_name);
                let field_type = self.expander.expand_type(type_name, required, value);
                if !field_type.typ.starts_with("Option<") {
                    self.default = false;
                }
                let typ = field_type.typ.parse::<TokenStream>().unwrap();

                let default =
                    if field_type.default { Some(quote! { #[serde(default)] }) } else { None };
                let attributes = if field_type.attributes.is_empty() {
                    None
                } else {
                    let attributes = field_type
                        .attributes
                        .iter()
                        .map(|attr| attr.parse::<TokenStream>().unwrap());
                    Some(quote! {
                        #[serde( #(#attributes),* )]
                    })
                };
                let comment = value
                    .description
                    .as_ref()
                    .map(|comment| make_doc_comment(comment, LINE_LENGTH - INDENT_LENGTH));
                quote! {
                    #comment
                    #default
                    #attributes
                    #key : #typ
                }
            })
            .collect()
    }
}

pub struct Expander<'r> {
    root_name: Option<&'r str>,
    schemafy_path: &'r str,
    root: &'r Schema,
    current_type: String,
    current_field: String,
    types: Vec<(String, TokenStream)>,
}

struct FieldType {
    typ: String,
    attributes: Vec<String>,
    default: bool,
}

impl<S> From<S> for FieldType
where
    S: Into<String>,
{
    fn from(s: S) -> FieldType {
        FieldType { typ: s.into(), attributes: Vec::new(), default: false }
    }
}

impl<'r> Expander<'r> {
    pub fn new(
        root_name: Option<&'r str>,
        schemafy_path: &'r str,
        root: &'r Schema,
    ) -> Expander<'r> {
        Expander {
            root_name,
            root,
            schemafy_path,
            current_field: "".into(),
            current_type: "".into(),
            types: Vec::new(),
        }
    }

    fn type_ref(&self, s: &str) -> String {
        // ref is supposed to be be a valid URI, however we should better have a fallback plan
        let fragment = URI::try_from(s)
            .map(|uri| uri.fragment().map(Fragment::to_owned))
            .ok()
            .flatten()
            .or({
                let s = s.strip_prefix('#').unwrap_or(s);
                Fragment::try_from(s).ok()
            })
            .map(|fragment| fragment.to_string())
            .unwrap_or_else(|| s.to_owned());

        let ref_ = if fragment.is_empty() {
            self.root_name.expect("No root name specified for schema")
        } else {
            fragment.split('/').last().expect("Component")
        };

        let ref_ = ref_.to_pascal_case();
        let ref_ = replace_invalid_identifier_chars(&ref_);
        replace_numeric_start(&ref_)
    }

    fn schema(&self, schema: &'r Schema) -> Cow<'r, Schema> {
        let schema = match schema.ref_ {
            Some(ref ref_) => self.schema_ref(ref_),
            None => schema,
        };
        match schema.all_of {
            Some(ref all_of) if !all_of.is_empty() => {
                all_of.iter().skip(1).fold(self.schema(&all_of[0]).clone(), |mut result, def| {
                    merge_all_of(result.to_mut(), &self.schema(def));
                    result
                })
            }
            _ => Cow::Borrowed(schema),
        }
    }

    fn schema_ref(&self, s: &str) -> &'r Schema {
        s.split('/').fold(self.root, |schema, comp| {
            if comp.ends_with('#') {
                self.root
            } else if comp == "definitions" {
                schema
            } else {
                schema
                    .definitions
                    .get(comp)
                    .unwrap_or_else(|| panic!("Expected definition: `{}` {}", s, comp))
            }
        })
    }

    fn expand_type(&mut self, type_name: &str, required: bool, typ: &Schema) -> FieldType {
        let saved_type = self.current_type.clone();
        let mut result = self.expand_type_(typ);
        self.current_type = saved_type;
        if type_name.to_pascal_case() == result.typ.to_pascal_case() {
            result.typ = format!("Box<{}>", result.typ)
        }
        if !required {
            if !result.default {
                result.typ = format!("Option<{}>", result.typ);
            }
            if result.typ.starts_with("Option<") {
                result.attributes.push("skip_serializing_if=\"Option::is_none\"".into());
            }
        }
        result
    }

    fn expand_type_(&mut self, typ: &Schema) -> FieldType {
        if let Some(ref ref_) = typ.ref_ {
            self.type_ref(ref_).into()
        } else if typ.any_of.as_ref().map_or(false, |a| a.len() >= 2) {
            let any_of = typ.any_of.as_ref().unwrap();
            let simple = self.schema(&any_of[0]);
            let array = self.schema(&any_of[1]);
            if !array.type_.is_empty() {
                if let SimpleTypes::Array = array.type_[0] {
                    if simple == self.schema(&array.items[0]) {
                        return FieldType {
                            typ: format!("Vec<{}>", self.expand_type_(&any_of[0]).typ),
                            attributes: vec![format!(
                                r#"with="{}one_or_many""#,
                                self.schemafy_path
                            )],
                            default: true,
                        };
                    }
                }
            }
            "serde_json::Value".into()
        } else if typ.one_of.as_ref().map_or(false, |a| a.len() >= 2) {
            let schemas = typ.one_of.as_ref().unwrap();
            let (type_name, type_def) = self.expand_one_of(schemas);
            self.types.push((type_name.clone(), type_def));
            type_name.into()
        } else if typ.type_.len() == 2 {
            if typ.type_[0] == SimpleTypes::Null || typ.type_[1] == SimpleTypes::Null {
                let mut ty = typ.clone();
                ty.type_.retain(|x| *x != SimpleTypes::Null);

                FieldType {
                    typ: format!("Option<{}>", self.expand_type_(&ty).typ),
                    attributes: vec![],
                    default: true,
                }
            } else {
                "serde_json::Value".into()
            }
        } else if typ.type_.len() == 1 {
            match typ.type_[0] {
                SimpleTypes::String => {
                    if typ.enum_.as_ref().map_or(false, |e| e.is_empty()) {
                        "serde_json::Value".into()
                    } else {
                        "String".into()
                    }
                }
                SimpleTypes::Integer => "i64".into(),
                SimpleTypes::Boolean => "bool".into(),
                SimpleTypes::Number => "f64".into(),
                // Handle objects defined inline
                SimpleTypes::Object
                    if !typ.properties.is_empty()
                        || typ.additional_properties == Some(Value::Bool(false)) =>
                {
                    let name = format!(
                        "{}{}",
                        self.current_type.to_pascal_case(),
                        self.current_field.to_pascal_case()
                    );
                    let tokens = self.expand_schema(&name, typ);
                    self.types.push((name.clone(), tokens));
                    name.into()
                }
                SimpleTypes::Object => {
                    let prop = match typ.additional_properties {
                        Some(ref props) if props.is_object() => {
                            let prop = serde_json::from_value(props.clone()).unwrap();
                            self.expand_type_(&prop).typ
                        }
                        _ => "serde_json::Value".into(),
                    };
                    let result = format!("::std::collections::BTreeMap<String, {}>", prop);
                    FieldType {
                        typ: result,
                        attributes: Vec::new(),
                        default: typ.default == Some(Value::Object(Default::default())),
                    }
                }
                SimpleTypes::Array => {
                    let item_type = typ.items.get(0).map_or("serde_json::Value".into(), |item| {
                        self.current_type = format!("{}Item", self.current_type);
                        self.expand_type_(item).typ
                    });
                    format!("Vec<{}>", item_type).into()
                }
                _ => "serde_json::Value".into(),
            }
        } else {
            "serde_json::Value".into()
        }
    }

    fn expand_one_of(&mut self, schemas: &[Schema]) -> (String, TokenStream) {
        let current_field = if self.current_field.is_empty() {
            "".to_owned()
        } else {
            str_to_ident(&self.current_field).to_string().to_pascal_case()
        };
        let saved_type = format!("{}{}", self.current_type, current_field);
        if schemas.is_empty() {
            return (saved_type, TokenStream::new());
        }
        let (variant_names, variant_types): (Vec<_>, Vec<_>) = schemas
            .iter()
            .enumerate()
            .map(|(i, schema)| {
                let name = schema.id.clone().unwrap_or_else(|| format!("Variant{}", i));
                if let Some(ref_) = &schema.ref_ {
                    let type_ = self.type_ref(ref_);
                    (format_ident!("{}", &name), format_ident!("{}", &type_))
                } else {
                    let type_name = format!("{}{}", saved_type, &name);
                    let field_type = self.expand_schema(&type_name, schema);
                    self.types.push((type_name.clone(), field_type));
                    (format_ident!("{}", &name), format_ident!("{}", &type_name))
                }
            })
            .unzip();
        let type_name_ident = syn::Ident::new(&saved_type, Span::call_site());
        let type_def = quote! {
            #[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
            #[serde(untagged)]
            pub enum #type_name_ident {
                #(#variant_names(#variant_types)),*
            }
        };
        (saved_type, type_def)
    }

    fn expand_definitions(&mut self, schema: &Schema) {
        for (name, def) in &schema.definitions {
            let type_decl = self.expand_schema(name, def);
            let definition_tokens = match def.description {
                Some(ref comment) => {
                    let t = make_doc_comment(comment, LINE_LENGTH);
                    quote! {
                        #t
                        #type_decl
                    }
                }
                None => type_decl,
            };
            self.types.push((name.to_string(), definition_tokens));
        }
    }

    fn expand_schema(&mut self, original_name: &str, schema: &Schema) -> TokenStream {
        self.expand_definitions(schema);

        let pascal_case_name = replace_invalid_identifier_chars(&original_name.to_pascal_case());
        self.current_type.clone_from(&pascal_case_name);
        let (fields, default) = {
            let mut field_expander = FieldExpander { default: true, expander: self };
            let fields = field_expander.expand_fields(original_name, schema);
            (fields, field_expander.default)
        };
        let name = syn::Ident::new(&pascal_case_name, Span::call_site());
        let is_struct =
            !fields.is_empty() || schema.additional_properties == Some(Value::Bool(false));
        let serde_rename = if name == original_name {
            None
        } else {
            Some(quote! {
                #[serde(rename = #original_name)]
            })
        };
        let is_enum = schema.enum_.as_ref().map_or(false, |e| !e.is_empty());
        let type_decl = if is_struct {
            let serde_deny_unknown = if schema.additional_properties == Some(Value::Bool(false))
                && schema.pattern_properties.is_empty()
            {
                Some(quote! { #[serde(deny_unknown_fields)] })
            } else {
                None
            };
            if default {
                quote! {
                    #[derive(Clone, PartialEq, Debug, Default, Deserialize, Serialize)]
                    #serde_rename
                    #serde_deny_unknown
                    pub struct #name {
                        #(#fields),*
                    }
                }
            } else {
                quote! {
                    #[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
                    #serde_rename
                    #serde_deny_unknown
                    pub struct #name {
                        #(#fields),*
                    }
                }
            }
        } else if is_enum {
            let mut optional = false;
            let mut repr_i64 = false;
            let variants = if schema.enum_names.as_ref().map_or(false, |e| !e.is_empty()) {
                let values = schema.enum_.as_ref().map_or(&[][..], |v| v);
                let names = schema.enum_names.as_ref().map_or(&[][..], |v| v);
                if names.len() != values.len() {
                    panic!(
                        "enumNames(length {}) and enum(length {}) have different length",
                        names.len(),
                        values.len()
                    )
                }
                names
                    .iter()
                    .enumerate()
                    .map(|(idx, name)| (&values[idx], name))
                    .flat_map(|(value, name)| {
                        let pascal_case_variant = name.to_pascal_case();
                        let variant_name =
                            rename_keyword("", &pascal_case_variant).unwrap_or_else(|| {
                                let v = syn::Ident::new(&pascal_case_variant, Span::call_site());
                                quote!(#v)
                            });
                        match value {
                            Value::String(ref s) => Some(quote! {
                                #[serde(rename = #s)]
                                #variant_name
                            }),
                            Value::Number(ref n) => {
                                repr_i64 = true;
                                let num = syn::LitInt::new(&n.to_string(), Span::call_site());
                                Some(quote! {
                                    #variant_name = #num
                                })
                            }
                            Value::Null => {
                                optional = true;
                                None
                            }
                            _ => panic!("Expected string,bool or number for enum got `{}`", value),
                        }
                    })
                    .collect::<Vec<_>>()
            } else {
                schema
                    .enum_
                    .as_ref()
                    .map_or(&[][..], |v| v)
                    .iter()
                    .flat_map(|v| match *v {
                        Value::String(ref v) => {
                            let pascal_case_variant = v.to_pascal_case();
                            let variant_name = rename_keyword("", &pascal_case_variant)
                                .unwrap_or_else(|| {
                                    let v =
                                        syn::Ident::new(&pascal_case_variant, Span::call_site());
                                    quote!(#v)
                                });
                            Some(if pascal_case_variant == *v {
                                variant_name
                            } else {
                                quote! {
                                    #[serde(rename = #v)]
                                    #variant_name
                                }
                            })
                        }
                        Value::Null => {
                            optional = true;
                            None
                        }
                        _ => panic!("Expected string for enum got `{}`", v),
                    })
                    .collect::<Vec<_>>()
            };
            if optional {
                let enum_name = syn::Ident::new(&format!("{}_", name), Span::call_site());
                if repr_i64 {
                    quote! {
                        pub type #name = Option<#enum_name>;
                        #[derive(Clone, PartialEq, Debug, Serialize_repr, Deserialize_repr)]
                        #serde_rename
                        #[repr(i64)]
                        pub enum #enum_name {
                            #(#variants),*
                        }
                    }
                } else {
                    quote! {
                        pub type #name = Option<#enum_name>;
                        #[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
                        #serde_rename
                        pub enum #enum_name {
                            #(#variants),*
                        }
                    }
                }
            } else if repr_i64 {
                quote! {
                    #[derive(Clone, PartialEq, Debug, Serialize_repr, Deserialize_repr)]
                    #serde_rename
                    #[repr(i64)]
                    pub enum #name {
                        #(#variants),*
                    }
                }
            } else {
                quote! {
                    #[derive(Clone, PartialEq, Debug, Deserialize, Serialize)]
                    #serde_rename
                    pub enum #name {
                        #(#variants),*
                    }
                }
            }
        } else {
            let typ = self.expand_type("", true, schema).typ.parse::<TokenStream>().unwrap();
            // Skip self-referential types, e.g. `struct Schema = Schema`
            if name == typ.to_string() {
                return TokenStream::new();
            }
            return quote! {
                pub type #name = #typ;
            };
        };
        type_decl
    }

    pub fn expand(&mut self, schema: &Schema) -> TokenStream {
        // match self.root_name {
        //     Some(name) => {
        //         let schema = self.expand_schema(name, schema);
        //         self.types.push((name.to_string(), schema));
        //     }
        //     None => self.expand_definitions(schema),
        // }
        let type_decl = self.expand_schema(&schema.title.clone().unwrap(), &schema);
        let definition_tokens = match schema.description {
            Some(ref comment) => {
                let t = make_doc_comment(comment, LINE_LENGTH);
                quote! {
                    #t
                    #type_decl
                }
            }
            None => type_decl,
        };
        self.types.push((schema.title.clone().unwrap(), definition_tokens));

        let types = self.types.iter().map(|t| &t.1);

        quote! {
            #( #types )*
        }
    }

    pub fn expand_root(&mut self) -> TokenStream {
        self.expand(self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expander_type_ref() {
        let json = std::fs::read_to_string("src/schema.json").expect("Read schema JSON file");
        let schema = serde_json::from_str(&json).unwrap_or_else(|err| panic!("{}", err));
        let expander = Expander::new(Some("SchemaName"), "::schemafy_core::", &schema);

        assert_eq!(expander.type_ref("normalField"), "NormalField");
        assert_eq!(expander.type_ref("#"), "SchemaName");
        assert_eq!(expander.type_ref(""), "SchemaName");
        assert_eq!(expander.type_ref("1"), "_1");
        assert_eq!(expander.type_ref("http://example.com/schema.json#"), "SchemaName");
        assert_eq!(
            expander.type_ref("http://example.com/normalField#withFragment"),
            "WithFragment"
        );
        assert_eq!(
            expander.type_ref("http://example.com/normalField#withFragment/and/path"),
            "Path"
        );
        assert_eq!(
            expander.type_ref("http://example.com/normalField?with&params#andFragment/and/path"),
            "Path"
        );
        assert_eq!(expander.type_ref("#/only/Fragment"), "Fragment");

        // Invalid cases, just to verify the behavior
        assert_eq!(expander.type_ref("ref"), "Ref");
        assert_eq!(expander.type_ref("_"), "");
        assert_eq!(expander.type_ref("thieves' tools"), "ThievesTools");
        assert_eq!(
            expander.type_ref("http://example.com/normalField?with&params=1"),
            "NormalFieldWithParams1"
        );
    }

    #[test]
    fn embedded_type_names() {
        use std::collections::HashSet;

        let json = std::fs::read_to_string("tests/multiple-property-types.json")
            .expect("Read schema JSON file");
        let schema = serde_json::from_str(&json).unwrap();
        let mut expander = Expander::new(Some("Root"), "UNUSED", &schema);
        expander.expand(&schema);

        // check that the type names for embedded objects only include their
        // ancestors' type names, and not names from unrelated fields
        let types = expander.types.iter().map(|v| v.0.as_str()).collect::<HashSet<&str>>();
        assert!(types.contains("RootItemAC"));
        assert!(types.contains("RootKM"));
        assert!(types.contains("RootTV"));
    }
}