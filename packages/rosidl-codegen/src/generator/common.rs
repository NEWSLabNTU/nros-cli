use crate::{
    templates::{CField, CppFfiField, CppField, FieldKind, NrosField, SequenceStructDef},
    types::{
        C_DEFAULT_SEQUENCE_CAPACITY, CPP_DEFAULT_SEQUENCE_CAPACITY, CPP_DEFAULT_STRING_CAPACITY,
        NrosCodegenMode, c_array_suffix_for_field, c_cdr_read_method, c_cdr_write_method,
        c_type_for_field, cpp_array_suffix_for_field, cpp_type_for_field, escape_keyword,
        nros_type_for_field_with_mode, repr_c_type_for_field, to_c_package_name,
    },
    utils::to_snake_case,
};
use rosidl_parser::{FieldType, PrimitiveType};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum GeneratorError {
    #[error("Template rendering failed: {0}")]
    TemplateError(#[from] askama::Error),

    #[error("Invalid message structure: {0}")]
    InvalidMessage(String),
}

/// Determine the exhaustive FieldKind enum variant for a given ROS 2 field type
/// This function provides compile-time guarantees that all field type combinations are handled
pub(crate) fn determine_field_kind(field_type: &FieldType) -> FieldKind {
    match field_type {
        // Scalar types
        FieldType::Primitive(_) => FieldKind::Primitive,

        FieldType::String => FieldKind::UnboundedString,
        FieldType::BoundedString(_) => FieldKind::BoundedString,

        FieldType::WString => FieldKind::UnboundedWString,
        FieldType::BoundedWString(_) => FieldKind::BoundedWString,

        FieldType::NamespacedType { .. } => FieldKind::NestedMessage,

        // Array types
        FieldType::Array { element_type, size } => {
            // Arrays > 32 elements don't impl Copy/Clone in Rust
            if *size > 32 {
                return FieldKind::LargeArray;
            }

            match element_type.as_ref() {
                FieldType::Primitive(_) => FieldKind::PrimitiveArray,

                FieldType::String => FieldKind::UnboundedStringArray,
                FieldType::BoundedString(_) => FieldKind::BoundedStringArray,

                FieldType::WString => FieldKind::UnboundedWStringArray,
                FieldType::BoundedWString(_) => FieldKind::BoundedWStringArray,

                _ => FieldKind::NestedMessageArray,
            }
        }

        // Bounded sequences (T[<=N])
        FieldType::BoundedSequence { element_type, .. } => match element_type.as_ref() {
            FieldType::Primitive(_) => FieldKind::BoundedPrimitiveSequence,

            FieldType::String => FieldKind::BoundedUnboundedStringSequence,
            FieldType::BoundedString(_) => FieldKind::BoundedBoundedStringSequence,

            FieldType::WString => FieldKind::BoundedUnboundedWStringSequence,
            FieldType::BoundedWString(_) => FieldKind::BoundedBoundedWStringSequence,

            _ => FieldKind::BoundedNestedMessageSequence,
        },

        // Unbounded sequences (T[])
        FieldType::Sequence { element_type } => match element_type.as_ref() {
            FieldType::Primitive(_) => FieldKind::UnboundedPrimitiveSequence,

            FieldType::String => FieldKind::UnboundedUnboundedStringSequence,
            FieldType::BoundedString(_) => FieldKind::UnboundedBoundedStringSequence,

            FieldType::WString => FieldKind::UnboundedUnboundedWStringSequence,
            FieldType::BoundedWString(_) => FieldKind::UnboundedBoundedWStringSequence,

            _ => FieldKind::UnboundedNestedMessageSequence,
        },
    }
}

/// Get the CDR primitive method name for a primitive type
pub(super) fn primitive_to_cdr_method(prim: &rosidl_parser::PrimitiveType) -> String {
    use rosidl_parser::PrimitiveType;
    match prim {
        PrimitiveType::Bool => "bool".to_string(),
        PrimitiveType::Byte => "u8".to_string(),
        PrimitiveType::Char => "u8".to_string(),
        PrimitiveType::Int8 => "i8".to_string(),
        PrimitiveType::UInt8 => "u8".to_string(),
        PrimitiveType::Int16 => "i16".to_string(),
        PrimitiveType::UInt16 => "u16".to_string(),
        PrimitiveType::Int32 => "i32".to_string(),
        PrimitiveType::UInt32 => "u32".to_string(),
        PrimitiveType::Int64 => "i64".to_string(),
        PrimitiveType::UInt64 => "u64".to_string(),
        PrimitiveType::Float32 => "f32".to_string(),
        PrimitiveType::Float64 => "f64".to_string(),
    }
}

/// Convert a Message field to NrosField with explicit codegen mode
pub(super) fn field_to_nros_field_with_mode(
    field: &rosidl_parser::Field,
    package_name: &str,
    mode: NrosCodegenMode,
) -> NrosField {
    let name = escape_keyword(&field.name);
    let rust_type = nros_type_for_field_with_mode(&field.field_type, Some(package_name), mode);

    // Determine field properties
    let (is_primitive, primitive_method) = match &field.field_type {
        FieldType::Primitive(prim) => (true, primitive_to_cdr_method(prim)),
        _ => (false, String::new()),
    };

    let is_string = matches!(
        &field.field_type,
        FieldType::String
            | FieldType::BoundedString(_)
            | FieldType::WString
            | FieldType::BoundedWString(_)
    );

    let (is_array, array_size) = match &field.field_type {
        FieldType::Array { size, .. } => (true, *size),
        _ => (false, 0),
    };

    let is_sequence = matches!(
        &field.field_type,
        FieldType::Sequence { .. } | FieldType::BoundedSequence { .. }
    );

    let is_nested = matches!(&field.field_type, FieldType::NamespacedType { .. });

    // Element type info for arrays and sequences
    let (is_primitive_element, is_string_element, element_primitive_method) =
        match &field.field_type {
            FieldType::Array { element_type, .. }
            | FieldType::Sequence { element_type }
            | FieldType::BoundedSequence { element_type, .. } => match element_type.as_ref() {
                FieldType::Primitive(prim) => (true, false, primitive_to_cdr_method(prim)),
                FieldType::String
                | FieldType::BoundedString(_)
                | FieldType::WString
                | FieldType::BoundedWString(_) => (false, true, String::new()),
                _ => (false, false, String::new()),
            },
            _ => (false, false, String::new()),
        };

    NrosField {
        name,
        rust_type,
        primitive_method,
        element_primitive_method,
        array_size,
        is_primitive,
        is_string,
        is_array,
        is_sequence,
        is_nested,
        is_primitive_element,
        is_string_element,
        is_large_array: array_size > 32,
    }
}

/// Convert a Message field to NrosField
pub(super) fn field_to_nros_field(field: &rosidl_parser::Field, package_name: &str) -> NrosField {
    field_to_nros_field_with_mode(field, package_name, NrosCodegenMode::Crate)
}

/// Build a CField from a field type
pub(super) fn build_c_field(
    name: &str,
    field_type: &FieldType,
    current_package: Option<&str>,
) -> CField {
    let escaped_name = escape_keyword(name);
    let c_type = c_type_for_field(field_type, current_package);
    let array_suffix = c_array_suffix_for_field(field_type);

    // Determine type characteristics
    let (is_primitive, primitive_type) = match field_type {
        FieldType::Primitive(prim) => (true, Some(prim)),
        _ => (false, None),
    };

    let is_string = matches!(
        field_type,
        FieldType::String
            | FieldType::BoundedString(_)
            | FieldType::WString
            | FieldType::BoundedWString(_)
    );

    let is_array = matches!(field_type, FieldType::Array { .. });
    let is_sequence = matches!(
        field_type,
        FieldType::Sequence { .. } | FieldType::BoundedSequence { .. }
    );
    let is_nested = matches!(field_type, FieldType::NamespacedType { .. });

    // Get array/sequence info
    let (array_size, sequence_capacity) = match field_type {
        FieldType::Array { size, .. } => (*size, 0),
        FieldType::Sequence { .. } => (0, C_DEFAULT_SEQUENCE_CAPACITY),
        FieldType::BoundedSequence { max_size, .. } => (0, *max_size),
        _ => (0, 0),
    };

    // Get element info for arrays/sequences
    let (is_primitive_element, is_string_element, element_type) = match field_type {
        FieldType::Array { element_type, .. }
        | FieldType::Sequence { element_type }
        | FieldType::BoundedSequence { element_type, .. } => {
            let is_prim = matches!(element_type.as_ref(), FieldType::Primitive(_));
            let is_str = matches!(
                element_type.as_ref(),
                FieldType::String
                    | FieldType::BoundedString(_)
                    | FieldType::WString
                    | FieldType::BoundedWString(_)
            );
            (is_prim, is_str, Some(element_type.as_ref()))
        }
        _ => (false, false, None),
    };

    // Get CDR methods
    let (cdr_write_method, cdr_read_method) = if let Some(prim) = primitive_type {
        (
            c_cdr_write_method(prim).to_string(),
            c_cdr_read_method(prim).to_string(),
        )
    } else {
        (String::new(), String::new())
    };

    let (element_cdr_write_method, element_cdr_read_method) =
        if let Some(FieldType::Primitive(prim)) = element_type {
            (
                c_cdr_write_method(prim).to_string(),
                c_cdr_read_method(prim).to_string(),
            )
        } else {
            (String::new(), String::new())
        };

    // Get nested struct names (use current_package for intra-package references)
    let nested_struct_name = if let FieldType::NamespacedType { package, name } = field_type {
        let pkg = package.as_deref().or(current_package).unwrap_or("");
        format!("{}_msg_{}", to_c_package_name(pkg), to_snake_case(name))
    } else {
        String::new()
    };

    let element_struct_name =
        if let Some(FieldType::NamespacedType { package, name }) = element_type {
            let pkg = package.as_deref().or(current_package).unwrap_or("");
            format!("{}_msg_{}", to_c_package_name(pkg), to_snake_case(name))
        } else {
            String::new()
        };

    CField {
        name: escaped_name,
        c_type,
        array_suffix,
        cdr_write_method,
        cdr_read_method,
        element_cdr_write_method,
        element_cdr_read_method,
        array_size,
        sequence_capacity,
        nested_struct_name,
        element_struct_name,
        is_primitive,
        is_string,
        is_array,
        is_sequence,
        is_nested,
        is_primitive_element,
        is_string_element,
    }
}

/// Build a CppField for C++ header generation
pub(super) fn build_cpp_field(
    name: &str,
    field_type: &FieldType,
    current_package: Option<&str>,
) -> CppField {
    let escaped_name = escape_keyword(name);
    let cpp_type = cpp_type_for_field(field_type, current_package);
    let array_suffix = cpp_array_suffix_for_field(field_type);

    // For arrays, the cpp_type already contains the base type, and array_suffix has [N]
    // For FixedString/FixedSequence, cpp_type is the full type, no suffix needed
    // But for fixed-size arrays of primitives, cpp_type is "int32_t[3]" — split it
    let (final_type, final_suffix) = if !array_suffix.is_empty() {
        // Array field: base type is without the [N] suffix
        let base = match field_type {
            FieldType::Array { element_type, .. } => {
                cpp_type_for_field(element_type, current_package)
            }
            _ => cpp_type,
        };
        (base, array_suffix)
    } else {
        (cpp_type, String::new())
    };

    CppField {
        name: escaped_name,
        cpp_type: final_type,
        array_suffix: final_suffix,
    }
}

/// Build a CppFfiField and optional SequenceStructDef for Rust FFI glue generation
pub(super) fn build_cpp_ffi_field(
    name: &str,
    field_type: &FieldType,
    struct_name: &str,
    current_package: Option<&str>,
) -> (CppFfiField, Option<SequenceStructDef>) {
    let escaped_name = escape_keyword(name);

    // Determine type characteristics
    let (is_primitive, primitive_type) = match field_type {
        FieldType::Primitive(prim) => (true, Some(prim)),
        _ => (false, None),
    };

    let is_string = matches!(
        field_type,
        FieldType::String
            | FieldType::BoundedString(_)
            | FieldType::WString
            | FieldType::BoundedWString(_)
    );

    let is_array = matches!(field_type, FieldType::Array { .. });
    let is_sequence = matches!(
        field_type,
        FieldType::Sequence { .. } | FieldType::BoundedSequence { .. }
    );
    let is_nested = matches!(field_type, FieldType::NamespacedType { .. });

    // Array/sequence size info
    let (array_size, sequence_capacity) = match field_type {
        FieldType::Array { size, .. } => (*size, 0),
        FieldType::Sequence { .. } => (0, CPP_DEFAULT_SEQUENCE_CAPACITY),
        FieldType::BoundedSequence { max_size, .. } => (0, *max_size),
        _ => (0, 0),
    };

    // Element type info
    let (is_primitive_element, is_string_element, element_type) = match field_type {
        FieldType::Array { element_type, .. }
        | FieldType::Sequence { element_type }
        | FieldType::BoundedSequence { element_type, .. } => {
            let is_prim = matches!(element_type.as_ref(), FieldType::Primitive(_));
            let is_str = matches!(
                element_type.as_ref(),
                FieldType::String
                    | FieldType::BoundedString(_)
                    | FieldType::WString
                    | FieldType::BoundedWString(_)
            );
            (is_prim, is_str, Some(element_type.as_ref()))
        }
        _ => (false, false, None),
    };

    // CDR methods for primitives
    let (cdr_write_method, cdr_read_method) = if let Some(prim) = primitive_type {
        (
            c_cdr_write_method(prim).to_string(),
            c_cdr_read_method(prim).to_string(),
        )
    } else {
        (String::new(), String::new())
    };

    let (element_cdr_write_method, element_cdr_read_method) =
        if let Some(FieldType::Primitive(prim)) = element_type {
            (
                c_cdr_write_method(prim).to_string(),
                c_cdr_read_method(prim).to_string(),
            )
        } else {
            (String::new(), String::new())
        };

    // Nested function names
    let nested_serialize_fn = if let FieldType::NamespacedType { package, name: n } = field_type {
        let pkg = package.as_deref().or(current_package).unwrap_or("unknown");
        format!(
            "serialize_{}_msg_{}_fields",
            to_c_package_name(pkg),
            to_snake_case(n)
        )
    } else {
        String::new()
    };

    let nested_deserialize_fn = if let FieldType::NamespacedType { package, name: n } = field_type {
        let pkg = package.as_deref().or(current_package).unwrap_or("unknown");
        format!(
            "deserialize_{}_msg_{}_fields",
            to_c_package_name(pkg),
            to_snake_case(n)
        )
    } else {
        String::new()
    };

    // Element nested function names (for arrays/sequences of nested types)
    let (elem_nested_ser, elem_nested_deser) =
        if let Some(FieldType::NamespacedType { package, name: n }) = element_type {
            let pkg = package.as_deref().or(current_package).unwrap_or("unknown");
            (
                format!(
                    "serialize_{}_msg_{}_fields",
                    to_c_package_name(pkg),
                    to_snake_case(n)
                ),
                format!(
                    "deserialize_{}_msg_{}_fields",
                    to_c_package_name(pkg),
                    to_snake_case(n)
                ),
            )
        } else {
            (String::new(), String::new())
        };

    // Compute repr(C) type
    let repr_c_type = if is_sequence {
        // Sequence uses named struct
        let seq_struct_name = format!("{}_{}_seq_t", struct_name, to_snake_case(name));
        seq_struct_name
    } else {
        repr_c_type_for_field(field_type, current_package)
    };

    // Build sequence struct def if needed
    let seq_struct = if is_sequence {
        let elem_repr_c = match element_type {
            Some(FieldType::Primitive(prim)) => {
                use crate::types::repr_c_type_for_field;
                repr_c_type_for_field(&FieldType::Primitive(*prim), current_package)
            }
            Some(FieldType::String) => format!("[u8; {}]", CPP_DEFAULT_STRING_CAPACITY),
            Some(FieldType::BoundedString(sz)) => format!("[u8; {}]", sz),
            Some(FieldType::WString) => format!("[u8; {}]", CPP_DEFAULT_STRING_CAPACITY),
            Some(FieldType::BoundedWString(sz)) => format!("[u8; {}]", sz),
            Some(FieldType::NamespacedType { package, name: n }) => {
                // When package is None the element type is from the current package
                let pkg = package.as_deref().or(current_package).unwrap_or("unknown");
                format!("{}_msg_{}_t", to_c_package_name(pkg), to_snake_case(n))
            }
            _ => "u8".to_string(),
        };
        Some(SequenceStructDef {
            struct_name: format!("{}_{}_seq_t", struct_name, to_snake_case(name)),
            element_type: elem_repr_c,
            capacity: sequence_capacity,
        })
    } else {
        None
    };

    // Use element nested functions for array/sequence elements
    let final_nested_ser = if is_nested {
        nested_serialize_fn
    } else {
        elem_nested_ser
    };
    let final_nested_deser = if is_nested {
        nested_deserialize_fn
    } else {
        elem_nested_deser
    };

    // String capacity for deserialization
    let string_capacity = match field_type {
        FieldType::String | FieldType::WString => CPP_DEFAULT_STRING_CAPACITY,
        FieldType::BoundedString(sz) | FieldType::BoundedWString(sz) => *sz,
        _ => 0,
    };

    let element_string_capacity = match element_type {
        Some(FieldType::String) | Some(FieldType::WString) => CPP_DEFAULT_STRING_CAPACITY,
        Some(FieldType::BoundedString(sz)) | Some(FieldType::BoundedWString(sz)) => *sz,
        _ => 0,
    };

    let field = CppFfiField {
        name: escaped_name,
        repr_c_type,
        cdr_write_method,
        cdr_read_method,
        element_cdr_write_method,
        element_cdr_read_method,
        array_size,
        sequence_capacity,
        nested_serialize_fn: final_nested_ser,
        nested_deserialize_fn: final_nested_deser,
        string_capacity,
        element_string_capacity,
        is_primitive,
        is_string,
        is_array,
        is_sequence,
        is_nested,
        is_primitive_element,
        is_string_element,
    };

    (field, seq_struct)
}

// ============================================================================
// nros-serdes::Message schema builder
// ============================================================================
//
// Emits the `impl ::nros_serdes::Message for <Msg>` block + any helper
// `pub const` items (NestedType + element FieldType statics) so backends
// like `nros-rmw-cyclonedds` (Phase 212.K.7.4-6) can walk the static
// field schema at runtime via `<M as Message>::FIELDS` / `TYPE_NAME`.
//
// Per-field expressions reference helper consts (`FT_<name>`, `NESTED_<name>`)
// rather than inlining `&FieldType::...` literals — `&FieldType::Foo` doesn't
// yield a `&'static FieldType` because the temporary is dropped at end of
// expression. Top-level `pub const` items live for `'static` and provide
// the stable address the recursive variants need.

/// Schema artefacts attached to a generated nros message struct.
///
/// `nros_type_name` is the package-qualified ROS type name (e.g.
/// `"std_msgs/msg/Header"`) used for `Message::TYPE_NAME`.
///
/// `helper_consts` is a (possibly empty) block of `pub const` items that
/// must be emitted in the same module as the message struct so the
/// recursive `FieldType::Array(_, &FT_FOO)` / `FieldType::Nested(&NESTED_FOO)`
/// references resolve to `'static` addresses.
///
/// `fields_block` is the body of the `Message::FIELDS` slice — one
/// `::nros_serdes::Field { … },` per IDL field, in declaration order.
#[derive(Debug, Clone, Default)]
pub struct NrosMessageSchema {
    pub nros_type_name: String,
    pub helper_consts: String,
    pub fields_block: String,
}

/// Build the [`NrosMessageSchema`] for a parsed `.msg` body.
pub fn build_nros_message_schema(
    package_name: &str,
    message_name: &str,
    fields: &[rosidl_parser::Field],
) -> NrosMessageSchema {
    let nros_type_name = format!("{}/msg/{}", package_name, message_name);

    let mut helper_consts = String::new();
    let mut fields_block = String::new();

    for field in fields {
        // Use the *raw* IDL field name for the schema (matches the .msg
        // source); the rendered struct field still goes through
        // `escape_keyword` to dodge Rust reserved words.
        let raw_name = &field.name;
        let access_name = escape_keyword(raw_name);
        let ty_expr = render_field_type_expr(
            raw_name,
            &field.field_type,
            package_name,
            &mut helper_consts,
        );
        fields_block.push_str(&format!(
            "        ::nros_serdes::Field {{\n            \
             name: \"{name}\",\n            \
             ty: {ty_expr},\n            \
             offset: ::core::mem::offset_of!({msg}, {access}),\n        }},\n",
            name = raw_name,
            ty_expr = ty_expr,
            msg = message_name,
            access = access_name,
        ));
    }

    NrosMessageSchema {
        nros_type_name,
        helper_consts,
        fields_block,
    }
}

/// Emit the FieldType expression for a single field. Recursive variants
/// hoist their inner FieldType / NestedType into a module-scoped
/// `pub const`, appended to `helper_consts`, and reference it by name.
fn render_field_type_expr(
    field_name: &str,
    field_type: &FieldType,
    package_name: &str,
    helper_consts: &mut String,
) -> String {
    match field_type {
        FieldType::Primitive(prim) => primitive_field_type_expr(prim).to_string(),
        FieldType::String => "::nros_serdes::FieldType::String".to_string(),
        FieldType::WString => "::nros_serdes::FieldType::WString".to_string(),
        FieldType::BoundedString(n) => {
            format!("::nros_serdes::FieldType::BoundedString({})", n)
        }
        FieldType::BoundedWString(n) => {
            format!("::nros_serdes::FieldType::BoundedWString({})", n)
        }
        FieldType::NamespacedType { package, name } => {
            // Emit a NestedType helper const, sourcing TYPE_NAME + FIELDS
            // from the nested type's own Message impl so we never duplicate
            // the package/type-name string.
            let nested_const = format!("NESTED_{}", upper_ident(field_name));
            let nested_path = nested_type_path(package.as_deref(), name, package_name);
            helper_consts.push_str(&format!(
                "#[allow(non_upper_case_globals)]\n\
                 pub const {nested_const}: ::nros_serdes::NestedType = ::nros_serdes::NestedType {{\n    \
                 type_name: <{nested_path} as ::nros_serdes::Message>::TYPE_NAME,\n    \
                 fields: <{nested_path} as ::nros_serdes::Message>::FIELDS,\n}};\n",
            ));
            format!("::nros_serdes::FieldType::Nested(&{})", nested_const)
        }
        FieldType::Array { element_type, size } => {
            let elem_const = format!("FT_{}_ELEM", upper_ident(field_name));
            emit_element_const(
                &elem_const,
                field_name,
                element_type,
                package_name,
                helper_consts,
            );
            format!("::nros_serdes::FieldType::Array({}, &{})", size, elem_const)
        }
        FieldType::Sequence { element_type } => {
            let elem_const = format!("FT_{}_ELEM", upper_ident(field_name));
            emit_element_const(
                &elem_const,
                field_name,
                element_type,
                package_name,
                helper_consts,
            );
            format!("::nros_serdes::FieldType::Sequence(&{})", elem_const)
        }
        FieldType::BoundedSequence {
            element_type,
            max_size,
        } => {
            let elem_const = format!("FT_{}_ELEM", upper_ident(field_name));
            emit_element_const(
                &elem_const,
                field_name,
                element_type,
                package_name,
                helper_consts,
            );
            format!(
                "::nros_serdes::FieldType::BoundedSequence({}, &{})",
                max_size, elem_const
            )
        }
    }
}

/// Emit a `pub const <ident>: FieldType = <expr>;` for the recursive
/// element of an Array / Sequence / BoundedSequence field.
fn emit_element_const(
    const_ident: &str,
    field_name: &str,
    element_type: &FieldType,
    package_name: &str,
    helper_consts: &mut String,
) {
    // The inner expression is rendered with the *parent* field name so any
    // further-nested helpers stay scoped under the same FT_<FIELD>_ prefix.
    let inner_expr = render_field_type_expr(field_name, element_type, package_name, helper_consts);
    helper_consts.push_str(&format!(
        "#[allow(non_upper_case_globals)]\n\
         pub const {ident}: ::nros_serdes::FieldType = {inner};\n",
        ident = const_ident,
        inner = inner_expr,
    ));
}

/// Map an IDL primitive to its `::nros_serdes::FieldType::*` variant.
fn primitive_field_type_expr(prim: &PrimitiveType) -> &'static str {
    match prim {
        PrimitiveType::Bool => "::nros_serdes::FieldType::Bool",
        // ROS IDL `octet` / `byte` / `char` and `uint8` all map to Uint8 on
        // the wire (same single-byte CDR encoding).
        PrimitiveType::Byte | PrimitiveType::Char | PrimitiveType::UInt8 => {
            "::nros_serdes::FieldType::Uint8"
        }
        PrimitiveType::Int8 => "::nros_serdes::FieldType::Int8",
        PrimitiveType::UInt16 => "::nros_serdes::FieldType::Uint16",
        PrimitiveType::Int16 => "::nros_serdes::FieldType::Int16",
        PrimitiveType::UInt32 => "::nros_serdes::FieldType::Uint32",
        PrimitiveType::Int32 => "::nros_serdes::FieldType::Int32",
        PrimitiveType::UInt64 => "::nros_serdes::FieldType::Uint64",
        PrimitiveType::Int64 => "::nros_serdes::FieldType::Int64",
        PrimitiveType::Float32 => "::nros_serdes::FieldType::Float32",
        PrimitiveType::Float64 => "::nros_serdes::FieldType::Float64",
    }
}

/// Render the Rust path to a nested message type. Mirrors the
/// crate-mode rules in `nros_type_for_field_with_mode` for
/// `NamespacedType` so we can hand the type as `<Path as Message>`.
fn nested_type_path(pkg: Option<&str>, name: &str, current_package: &str) -> String {
    match pkg {
        Some(p) if p == current_package => format!("crate::msg::{}", name),
        Some(p) => format!("{}::msg::{}", p, name),
        None => format!("crate::msg::{}", name),
    }
}

/// Turn a field name into an UPPER_SNAKE_CASE identifier fragment for
/// use inside helper-const names (`NESTED_<X>`, `FT_<X>_ELEM`).
fn upper_ident(s: &str) -> String {
    // Strip a trailing `_` first — `escape_keyword` adds one for reserved
    // words, but it's stable to recompute via the raw IDL name. We keep
    // ASCII-safe transforms only; IDL field names are already
    // `[a-z][a-z0-9_]*`.
    s.trim_end_matches('_').to_ascii_uppercase()
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use rosidl_parser::{Field, PrimitiveType};

    fn prim_field(name: &str, prim: PrimitiveType) -> Field {
        Field {
            name: name.to_string(),
            field_type: FieldType::Primitive(prim),
            default_value: None,
        }
    }

    fn nested_field(name: &str, pkg: &str, ty: &str) -> Field {
        Field {
            name: name.to_string(),
            field_type: FieldType::NamespacedType {
                package: Some(pkg.to_string()),
                name: ty.to_string(),
            },
            default_value: None,
        }
    }

    #[test]
    fn primitive_only_emits_inline_field_type() {
        let schema = build_nros_message_schema(
            "std_msgs",
            "Int32",
            &[prim_field("data", PrimitiveType::Int32)],
        );
        assert_eq!(schema.nros_type_name, "std_msgs/msg/Int32");
        assert_eq!(schema.helper_consts, "");
        assert!(schema.fields_block.contains("name: \"data\","));
        assert!(
            schema
                .fields_block
                .contains("ty: ::nros_serdes::FieldType::Int32,")
        );
        assert!(
            schema
                .fields_block
                .contains("offset: ::core::mem::offset_of!(Int32, data)")
        );
    }

    #[test]
    fn nested_field_emits_nested_helper_const() {
        let schema = build_nros_message_schema(
            "std_msgs",
            "Header",
            &[
                nested_field("stamp", "builtin_interfaces", "Time"),
                Field {
                    name: "frame_id".to_string(),
                    field_type: FieldType::String,
                    default_value: None,
                },
            ],
        );
        assert!(
            schema
                .helper_consts
                .contains("pub const NESTED_STAMP: ::nros_serdes::NestedType")
        );
        assert!(
            schema
                .helper_consts
                .contains("<builtin_interfaces::msg::Time as ::nros_serdes::Message>::TYPE_NAME")
        );
        assert!(
            schema
                .fields_block
                .contains("ty: ::nros_serdes::FieldType::Nested(&NESTED_STAMP),")
        );
        assert!(
            schema
                .fields_block
                .contains("ty: ::nros_serdes::FieldType::String,")
        );
    }

    #[test]
    fn bounded_sequence_emits_element_const() {
        let schema = build_nros_message_schema(
            "test_msgs",
            "Bounded",
            &[Field {
                name: "items".to_string(),
                field_type: FieldType::BoundedSequence {
                    element_type: Box::new(FieldType::Primitive(PrimitiveType::UInt8)),
                    max_size: 16,
                },
                default_value: None,
            }],
        );
        assert!(
            schema
                .helper_consts
                .contains("pub const FT_ITEMS_ELEM: ::nros_serdes::FieldType")
        );
        assert!(
            schema
                .helper_consts
                .contains("= ::nros_serdes::FieldType::Uint8;")
        );
        assert!(
            schema
                .fields_block
                .contains("ty: ::nros_serdes::FieldType::BoundedSequence(16, &FT_ITEMS_ELEM),")
        );
    }

    #[test]
    fn bounded_string_inlines_capacity() {
        let schema = build_nros_message_schema(
            "test_msgs",
            "Strs",
            &[Field {
                name: "label".to_string(),
                field_type: FieldType::BoundedString(32),
                default_value: None,
            }],
        );
        assert!(schema.helper_consts.is_empty());
        assert!(
            schema
                .fields_block
                .contains("ty: ::nros_serdes::FieldType::BoundedString(32),")
        );
    }

    #[test]
    fn array_of_nested_emits_chained_consts() {
        let schema = build_nros_message_schema(
            "test_msgs",
            "Mixed",
            &[Field {
                name: "points".to_string(),
                field_type: FieldType::Array {
                    element_type: Box::new(FieldType::NamespacedType {
                        package: Some("geometry_msgs".to_string()),
                        name: "Point".to_string(),
                    }),
                    size: 3,
                },
                default_value: None,
            }],
        );
        // Array hoists FT_POINTS_ELEM; the nested type hoists NESTED_POINTS
        // (named after the parent field, since we scope inner consts under
        // the parent field's name).
        assert!(
            schema
                .helper_consts
                .contains("pub const NESTED_POINTS: ::nros_serdes::NestedType")
        );
        assert!(
            schema
                .helper_consts
                .contains("pub const FT_POINTS_ELEM: ::nros_serdes::FieldType = ::nros_serdes::FieldType::Nested(&NESTED_POINTS);")
        );
        assert!(
            schema
                .fields_block
                .contains("ty: ::nros_serdes::FieldType::Array(3, &FT_POINTS_ELEM),")
        );
    }

    #[test]
    fn self_package_nested_uses_crate_path() {
        let schema = build_nros_message_schema(
            "local_msgs",
            "Outer",
            &[nested_field("inner", "local_msgs", "Inner")],
        );
        assert!(
            schema
                .helper_consts
                .contains("<crate::msg::Inner as ::nros_serdes::Message>::TYPE_NAME")
        );
    }

    #[test]
    fn keyword_field_name_escapes_for_offset() {
        // `type` is a Rust keyword and gets a trailing underscore in the
        // host struct field — schema name stays raw, but offset_of!
        // must reference the escaped Rust field.
        let schema = build_nros_message_schema(
            "test_msgs",
            "Sample",
            &[prim_field("type", PrimitiveType::Int32)],
        );
        assert!(schema.fields_block.contains("name: \"type\","));
        assert!(schema.fields_block.contains("offset_of!(Sample, type_)"));
    }
}
