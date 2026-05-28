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
use rosidl_parser::FieldType;
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
