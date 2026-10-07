//! Imports `tests/fixtures/kitchen/kitchen.h`, which exercises every item
//! kind and the edge cases the type checker has to reject or special-case.

mod common;

use std::sync::OnceLock;

use common::*;
use wid_cimport::*;

/// The kitchen fixture, imported once for all tests.
fn kitchen() -> &'static CModule {
    static MODULE: OnceLock<CModule> = OnceLock::new();
    MODULE.get_or_init(|| import_fixture("kitchen/kitchen.h"))
}

#[test]
fn module_is_consistent() {
    assert_consistent(kitchen());
}

#[test]
fn root_is_the_header_directory() {
    let module = kitchen();
    let expected = std::fs::canonicalize(fixture("kitchen")).expect("fixture directory exists");
    assert_eq!(module.root, expected);
    assert!(module.header.ends_with("kitchen/kitchen.h"));
    assert!(!module.target.triple.is_empty());
    assert_eq!(module.target.pointer_bits, 64);
}

#[test]
fn headers_in_the_tree_are_imported_and_others_are_not() {
    let module = kitchen();
    assert!(module.find("part_count").is_some(), "sibling header in a subdirectory is part of the import");
    assert_eq!(macro_expr(module, "PART_LIMIT").1, Some(&MacroValue::Int(16)));
    for outside in ["Outside", "outside_id", "outside_function", "OUTSIDE_MACRO", "OUTSIDE_H", "printf", "INT32_MAX"] {
        assert!(module.find(outside).is_none(), "{outside} is outside the header tree");
    }
}

#[test]
fn items_are_in_preprocessing_order() {
    let module = kitchen();
    let position = |name: &str| module.items.iter().position(|item| item.name() == Some(name)).expect("item exists");
    assert!(position("part_count") < position("add"), "the included header comes first");
    assert!(position("add") < position("Vec2"));
    assert!(position("Handle") < position("Later"), "a record is placed where it is first declared");
    assert!(position("Later") < position("Legacy"));
    assert!(position("Callback") < position("global_counter"));
    assert!(position("global_counter") < position("KITCHEN_INT"));
}

#[test]
fn functions() {
    let module = kitchen();
    let add = function(module, "add");
    assert_eq!(add.sig.params.len(), 2);
    assert_eq!(add.sig.params[0].name.as_deref(), Some("a"));
    assert_eq!(add.sig.params[0].ty, int(32, true, "int"));
    assert_eq!(add.sig.ret, int(32, true, "int"));
    assert!(!add.sig.variadic && add.sig.prototyped && !add.is_inline && !add.is_static);
    assert_eq!(item(module, "add").doc.as_deref(), Some("/** Adds two numbers. */"));
    assert_eq!(module.items.iter().filter(|item| item.name() == Some("add")).count(), 1, "redeclarations merge");

    let log = function(module, "log_message");
    assert!(log.sig.variadic);
    let format = &log.sig.params[0];
    assert_eq!(format.ty, pointer(plain_char(module), CONST));
    assert!(format.quals.is_restrict);
    assert_eq!(item(module, "log_message").doc.as_deref(), Some("/// Logs a formatted message."));

    let twice = function(module, "twice");
    assert!(twice.is_inline && twice.is_static);

    let write = function(module, "write_bytes");
    assert_eq!(write.sig.ret, int(64, false, "size_t"));
    assert_eq!(write.sig.params[0].ty, pointer(CType::Opaque("FILE".into()), NONE));
    assert_eq!(write.sig.params[1].ty, pointer(int(8, false, "uint8_t"), CONST));
    assert_eq!(item(module, "write_bytes").doc.as_deref(), Some("// Writes bytes."));

    let unnamed = function(module, "unnamed");
    assert!(unnamed.sig.params.iter().all(|param| param.name.is_none()));
    assert!(function(module, "no_params").sig.params.is_empty());
}

/// Plain `char` as the host target defines it, read from `typedef const char *CString`.
fn plain_char(module: &CModule) -> CType {
    match &typedef(module, "CString").ty {
        CType::Pointer(pointer) if matches!(pointer.pointee, CType::Char { .. }) => pointer.pointee.clone(),
        other => panic!("CString is a pointer to char: {other:?}"),
    }
}

#[test]
fn parameters_are_adjusted_like_c_does() {
    let module = kitchen();
    let takes = function(module, "takes_array");
    assert_eq!(takes.sig.params[0].ty, pointer(int(32, true, "int"), NONE), "int values[4] is int *");
    let CType::FnPtr(callback) = &takes.sig.params[1].ty else {
        panic!("a function parameter is a function pointer: {:?}", takes.sig.params[1].ty)
    };
    assert_eq!(callback.params[0].name.as_deref(), Some("code"));

    let argv = function(module, "argv_like");
    let char_type = plain_char(module);
    let expected = pointer(pointer(char_type, NONE), CONST);
    assert_eq!(argv.sig.params[0].ty, expected, "char *const * keeps the const on the inner pointer");
    assert_eq!(argv.sig.ret, expected);
}

#[test]
fn types_from_outside_the_tree_are_opaque_or_scalars() {
    let module = kitchen();
    let get = function(module, "get_outside");
    assert_eq!(get.sig.params[0].ty, int(32, false, "outside_id"));
    assert_eq!(get.sig.ret, pointer(CType::Opaque("Outside".into()), NONE));
    assert_eq!(global(module, "va_pointer").ty, pointer(CType::Opaque("va_list".into()), NONE));
    assert_eq!(function(module, "make_part").sig.ret, named(NamedKind::Typedef, "Part"));
}

#[test]
fn records() {
    let module = kitchen();
    let vec2 = record(module, "Vec2");
    assert_eq!(
        (vec2.kind, vec2.tag.as_deref(), vec2.typedef_name.as_deref()),
        (RecordKind::Struct, Some("Vec2"), Some("Vec2"))
    );
    let body = vec2.body.as_ref().expect("Vec2 is complete");
    assert_eq!((body.size, body.align), (8, 4));
    assert_eq!(field(vec2, "y").offset_bits, 32);
    assert_eq!(field(vec2, "x").doc.as_deref(), Some("// Horizontal."));
    assert_eq!(field(vec2, "y").doc.as_deref(), Some("///< Vertical."));
    assert!(module.items.iter().all(|item| !matches!(&item.kind, ItemKind::Typedef(t) if t.name == "Vec2")));

    let size = record(module, "Size");
    assert_eq!((size.tag.as_deref(), size.typedef_name.as_deref()), (None, Some("Size")));

    let handle = record(module, "Handle");
    assert!(handle.is_opaque());
    assert_eq!(handle.tag.as_deref(), Some("Handle"));

    let later = record(module, "Later");
    assert!(!later.is_opaque(), "a forward declaration is completed by its definition");
    assert_eq!(item(module, "Later").location.line, 44);
    assert_eq!(field(later, "next").ty, pointer(named(NamedKind::Struct, "Later"), NONE));

    let legacy = record(module, "Legacy");
    assert_eq!((legacy.tag.as_deref(), legacy.typedef_name.as_deref()), (Some("tagLegacy"), Some("Legacy")));

    let number = record(module, "Number");
    assert_eq!(number.kind, RecordKind::Union);
    assert_eq!(number.body.as_ref().map(|body| body.size), Some(8));
    assert!(field(number, "d").offset_bits == 0 && field(number, "i").offset_bits == 0);
}

#[test]
fn bitfields_and_flexible_arrays() {
    let module = kitchen();
    let flags = record(module, "Flags");
    let fields = &flags.body.as_ref().expect("complete").fields;
    assert_eq!(fields.len(), 3);
    assert_eq!(fields[0].bit_width, Some(1));
    assert_eq!((fields[1].name.as_deref(), fields[1].bit_width), (None, Some(0)), "unnamed zero-width bit-field");
    assert_eq!(fields[2].bit_width, Some(4));
    assert_eq!(fields[2].offset_bits, 32);

    let packet = record(module, "Packet");
    let body = packet.body.as_ref().expect("complete");
    assert!(body.has_flexible_array_member());
    assert_eq!(body.size, 4);
    let CType::Array(data) = &field(packet, "data").ty else { panic!("data is an array") };
    assert_eq!((data.len, &data.element), (None, &int(8, false, "uint8_t")));
}

#[test]
fn nested_declarations() {
    let module = kitchen();
    let shape = record(module, "Shape");
    assert_eq!(field(shape, "kind").ty, named(NamedKind::Enum, "ShapeKind"));
    assert_eq!(enumeration(module, "ShapeKind").constants[1].name, "SHAPE_RECT", "a nested enum is hoisted");
    assert_eq!(field(shape, "origin").ty, named(NamedKind::Struct, "Point"));
    assert!(record(module, "Point").body.is_some(), "a nested tagged struct is hoisted");

    let fields = &shape.body.as_ref().expect("complete").fields;
    let anonymous = fields.iter().find(|field| field.name.is_none()).expect("anonymous member");
    let CType::Record(union) = &anonymous.ty else { panic!("anonymous member is an inline record") };
    assert_eq!((union.kind, union.tag.as_ref(), union.typedef_name.as_ref()), (RecordKind::Union, None, None));
    assert_eq!(field(union, "size").ty, named(NamedKind::Typedef, "Size"));

    let CType::Record(color) = &field(shape, "color").ty else { panic!("an unnamed struct type is inline") };
    assert_eq!(color.body.as_ref().map(|body| body.fields.len()), Some(3));
}

#[test]
fn enums() {
    let module = kitchen();
    let color = enumeration(module, "Color8");
    assert_eq!(color.underlying, int(8, false, "uint8_t"));
    assert_eq!(color.constants.iter().map(|c| c.value).collect::<Vec<_>>(), vec![1, 2, 255]);

    assert_eq!(enumeration(module, "Signed").constants[0].value, -5);
    let big = enumeration(module, "Big");
    assert_eq!(big.constants[0].value, i128::from(u64::MAX), "unsigned 64-bit values do not wrap");
    assert_eq!(big.underlying, int(64, false, "unsigned long long"));

    let anonymous = module
        .items
        .iter()
        .find_map(|item| match &item.kind {
            ItemKind::Enum(e) if e.tag.is_none() && e.typedef_name.is_none() => Some(e),
            _ => None,
        })
        .expect("anonymous enum");
    assert_eq!(
        anonymous.constants.iter().map(|c| (c.name.as_str(), c.value)).collect::<Vec<_>>(),
        vec![("ANON_A", 10), ("ANON_B", 11)]
    );

    let mode = enumeration(module, "Mode");
    assert_eq!((mode.tag.as_deref(), mode.typedef_name.as_deref()), (None, Some("Mode")));
}

#[test]
fn typedefs() {
    let module = kitchen();
    let CType::FnPtr(callback) = &typedef(module, "Callback").ty else { panic!("Callback is a function pointer") };
    let names: Vec<_> = callback.params.iter().map(|param| param.name.as_deref()).collect();
    assert_eq!(names, vec![Some("code"), Some("user_data")]);
    assert_eq!(callback.ret, CType::Void);

    let CType::Function(handler) = &typedef(module, "Handler").ty else { panic!("Handler is a function type") };
    assert_eq!(handler.params[0].name.as_deref(), Some("signal"));

    assert_eq!(typedef(module, "Point2").ty, named(NamedKind::Typedef, "Vec2"));
    let CType::Pointer(cstring) = &typedef(module, "CString").ty else { panic!("CString is a pointer") };
    assert!(cstring.pointee_quals.is_const);

    let CType::Array(rows) = &typedef(module, "Matrix").ty else { panic!("Matrix is an array") };
    let CType::Array(columns) = &rows.element else { panic!("of arrays") };
    assert_eq!((rows.len, columns.len), (Some(4), Some(4)));
}

#[test]
fn globals_and_qualifiers() {
    let module = kitchen();
    assert_eq!(global(module, "global_counter").ty, int(32, true, "int"));
    let version = global(module, "version_string");
    assert!(version.quals.is_const);
    assert!(matches!(&version.ty, CType::Pointer(pointer) if pointer.pointee_quals.is_const));
    assert_eq!(global(module, "atomic_counter").ty, CType::Atomic(Box::new(int(32, true, "int"))));
    assert!(global(module, "hardware_register").quals.is_volatile);
    assert!(global(module, "per_thread").is_thread_local);
    let limit = global(module, "static_limit");
    assert!(limit.is_static && limit.quals.is_const);
    assert_eq!(global(module, "global_hook").ty, named(NamedKind::Typedef, "Callback"));
    let CType::FnPtr(hook) = &global(module, "raw_hook").ty else { panic!("raw_hook is a function pointer") };
    assert_eq!(hook.params[0].name.as_deref(), Some("value"));
}

#[test]
fn constant_macros() {
    let module = kitchen();
    let check = |name: &str, ty: CType, value: Option<MacroValue>| {
        let (actual_ty, actual_value) = macro_expr(module, name);
        assert_eq!((actual_ty, actual_value), (&ty, value.as_ref()), "{name}");
    };
    let char_type = plain_char(module);
    check("KITCHEN_INT", int(32, true, "int"), Some(MacroValue::Int(42)));
    check("KITCHEN_NEG", int(32, true, "int"), Some(MacroValue::Int(-7)));
    check("KITCHEN_HEX", int(32, false, "unsigned int"), Some(MacroValue::Int(255)));
    check("KITCHEN_SHIFT", int(64, false, "unsigned long long"), Some(MacroValue::Int(1 << 40)));
    check(
        "KITCHEN_FLOAT",
        CType::Float(FloatType { bits: 32, spelling: "float".into() }),
        Some(MacroValue::Float(1.5)),
    );
    check(
        "KITCHEN_DOUBLE",
        CType::Float(FloatType { bits: 64, spelling: "double".into() }),
        Some(MacroValue::Float(2.25)),
    );
    check("KITCHEN_STR", pointer(char_type.clone(), NONE), Some(MacroValue::Str(b"kitchen".to_vec())));
    check("KITCHEN_PSTR", pointer(char_type, NONE), Some(MacroValue::Str(b"parenA".to_vec())));
    check("KITCHEN_CHAR", int(32, true, "int"), Some(MacroValue::Char(i128::from(b'k'))));
    check("KITCHEN_BOOL", CType::Bool, Some(MacroValue::Bool(true)));
    // C23 gives the constants of an enum with a fixed underlying type the
    // enum's type; clang before 22 gives them the underlying type.
    let (ty, value) = macro_expr(module, "KITCHEN_ENUM");
    assert!(*ty == named(NamedKind::Enum, "Color8") || *ty == int(8, false, "uint8_t"), "{ty:?}");
    assert_eq!(value, Some(&MacroValue::Int(2)));
    check("KITCHEN_CAST", int(16, false, "uint16_t"), Some(MacroValue::Int(300)));
}

#[test]
fn typed_macros_without_a_value() {
    let module = kitchen();
    assert_eq!(macro_expr(module, "KITCHEN_VEC"), (&named(NamedKind::Typedef, "Vec2"), None), "compound literal");
    assert_eq!(macro_expr(module, "KITCHEN_NULL"), (&pointer(CType::Void, NONE), None));
    let (alias, value) = macro_expr(module, "KITCHEN_ALIAS");
    assert!(matches!(alias, CType::FnPtr(sig) if sig.params.len() == 2) && value.is_none(), "function alias");
}

#[test]
fn macros_that_are_not_expressions() {
    let module = kitchen();
    for name in ["KITCHEN_API", "KITCHEN_TYPE", "KITCHEN_COMMA", "KITCHEN_UNBALANCED", "KITCHEN_UNDECLARED"] {
        assert_eq!(macro_def(module, name).kind, MacroKind::Other, "{name}");
    }
    assert_eq!(macro_def(module, "KITCHEN_COMMA").body, "1, 2");
    assert!(module.find("KITCHEN_EMPTY").is_none(), "empty macros are skipped");
    assert!(module.find("KITCHEN_H").is_none(), "include guards are skipped");
}

#[test]
fn function_like_macros() {
    let module = kitchen();
    let max = macro_def(module, "KITCHEN_MAX");
    assert_eq!(max.kind, MacroKind::FunctionLike { params: vec!["a".into(), "b".into()], variadic: false });
    assert_eq!(max.body, "((a) > (b) ? (a) : (b))");
    let log = macro_def(module, "KITCHEN_LOG");
    assert_eq!(log.kind, MacroKind::FunctionLike { params: vec!["fmt".into()], variadic: true });
}

#[test]
fn macro_docs() {
    let module = kitchen();
    assert_eq!(item(module, "KITCHEN_ANSWER").doc.as_deref(), Some("/** The answer. */"));
    assert_eq!(item(module, "KITCHEN_TRAILING").doc.as_deref(), Some("// Three."));
    assert_eq!(item(module, "KITCHEN_INT").doc, None);
}

#[test]
fn locations_point_at_names() {
    let module = kitchen();
    let add = item(module, "add");
    assert!(add.location.file.ends_with("kitchen/kitchen.h"));
    assert_eq!((add.location.line, add.location.column), (14, 5));
    let constant = &enumeration(module, "Signed").constants[0];
    assert_eq!((constant.location.line, constant.location.column), (81, 15));
}
