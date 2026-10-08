//! The registry of stable diagnostic codes.
//!
//! Every code listed here must have `docs/errors/<CODE>.md` and at least one
//! `tests/ui` case that produces it; the test suite enforces both.

/// A stable diagnostic code such as `E0207`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Code(pub &'static str);

impl Code {
    /// Returns the code as text.
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// A registered code with its one-line title.
#[derive(Clone, Copy, Debug)]
pub struct CodeInfo {
    /// The code.
    pub code: Code,
    /// A short title used by `wid explain --list`.
    pub title: &'static str,
}

macro_rules! codes {
    ($($name:ident = $code:literal, $title:literal;)*) => {
        $(
            #[doc = $title]
            pub const $name: Code = Code($code);
        )*
        /// Every registered code, in numeric order.
        pub const ALL: &[CodeInfo] = &[$(CodeInfo { code: Code($code), title: $title }),*];
    };
}

codes! {
    // General.
    UNSUPPORTED = "E0001", "feature not implemented yet";

    // Lexing and parsing.
    UNEXPECTED_CHAR = "E0101", "unexpected character";
    UNTERMINATED_STRING = "E0102", "unterminated string literal";
    INVALID_NUMBER = "E0103", "invalid number literal";
    INVALID_ESCAPE = "E0104", "invalid escape sequence";
    UNEXPECTED_TOKEN = "E0105", "unexpected token";
    MISSING_END = "E0106", "missing `end`";
    INVALID_ASSIGN_TARGET = "E0107", "invalid assignment target";
    TOP_LEVEL_STATEMENT = "E0108", "statement outside a method";
    NESTED_COMMAND_CALL = "E0109", "nested call needs parentheses";
    INVALID_SYMBOL = "E0110", "invalid symbol literal";
    MISPLACED_SPLICE = "E0111", "misplaced splice";
    VARIADIC_PARAM = "E0112", "misused `*` parameter";
    STRUCT_LITERAL = "E0113", "struct literal syntax";

    // Names and scopes.
    UNDEFINED_NAME = "E0201", "undefined name";
    DUPLICATE_DEFINITION = "E0202", "duplicate definition";
    UNUSED_VARIABLE = "E0203", "variable is never read";
    NO_SUCH_MEMBER = "E0204", "no such field or method";
    PRIVATE_ITEM = "E0205", "item is private";
    UNKNOWN_IMPORT = "E0206", "package not found";
    MAYBE_NIL = "E0207", "value may be nil";
    NO_MAIN = "E0208", "missing `def main`";
    SELF_OUTSIDE_METHOD = "E0209", "`self` or `@field` outside a method";
    IMPORT_CYCLE = "E0210", "import cycle";

    // Types.
    TYPE_MISMATCH = "E0301", "mismatched types";
    ARG_COUNT = "E0302", "wrong number of arguments";
    BAD_NAMED_ARG = "E0303", "unknown or duplicate named argument";
    CANNOT_INFER = "E0304", "type cannot be inferred";
    NOT_CALLABLE = "E0305", "value is not callable";
    NON_BOOL_CONDITION = "E0306", "condition is not a `Bool`";
    NO_OPERATOR = "E0307", "operator not defined for these types";
    INVALID_CONVERSION = "E0308", "invalid conversion";
    NOT_ASSIGNABLE = "E0309", "cannot assign to this expression";
    RETURN_MISMATCH = "E0310", "wrong return value";
    CONSTANT_OVERFLOW = "E0311", "constant does not fit in type";
    RECURSIVE_TYPE = "E0312", "type contains itself";
    NON_EXHAUSTIVE = "E0313", "`case` does not cover every variant";
    UNKNOWN_TYPE = "E0314", "unknown type";
    GENERIC_ARGS = "E0315", "wrong generic arguments";
    NO_MATCHING_OVERLOAD = "E0316", "no matching overload";
    IMPLICIT_OVERLOAD = "E0317", "two definitions share a name";
    PROC_CAPTURE = "E0318", "proc captures a local variable";
    BLOCK_MISMATCH = "E0319", "block not expected or missing";
    YIELD_OUTSIDE_BLOCK_METHOD = "E0320", "`yield` in a method without a block parameter";
    BY_REF_NOT_PLACE = "E0321", "cannot bind a temporary by reference";
    NOT_A_TYPE = "E0322", "expected a type";
    NOT_A_VALUE = "E0323", "expected a value";
    MISSING_RETURN = "E0324", "missing return value";
    RECURSIVE_INLINE = "E0325", "yielding method calls itself";
    UNREACHABLE_CODE = "E0326", "unreachable code";
    COMPTIME_ONLY = "E0327", "value is not a compile-time constant";
    UNKNOWN_ATTRIBUTE = "E0328", "unknown or misused attribute";

    // Errors and control flow.
    IGNORED_ERROR = "E0401", "`Error` result ignored";
    GUARD_FALLTHROUGH = "E0402", "`guard` else-branch does not exit";
    GUARD_NOT_FALLIBLE = "E0403", "`guard` on a value that cannot fail";
    LOOP_CONTROL_OUTSIDE_LOOP = "E0404", "`break` or `next` outside a loop";
    EXIT_IN_DEFER = "E0405", "control flow leaves a `defer`";

    // Tools: `wid doc`.
    DOC_UNKNOWN_PACKAGE = "E0601", "package to document not found";
    DOC_UNKNOWN_SYMBOL = "E0602", "symbol to document not found";
    DOC_NO_MEMBER = "E0603", "no member with that name to document";
    DOC_PRIVATE = "E0604", "symbol to document is private";

    // Packages and C interop.
    CIMPORT_FAILED = "E0701", "C header import failed";
    C_COMPILER_FAILED = "E0702", "C compiler failed";
    CIMPORT_OPTION = "E0703", "invalid `cimport` option";
    C_NAME_COLLISION = "E0704", "two C declarations get the same Wid name";
    NOT_IMPORTED = "E0705", "C declaration was not imported";
    C_LAYOUT_MISMATCH = "E0706", "`types:` mapping does not match the C layout";
    OPAQUE_BY_VALUE = "E0707", "opaque type used by value";
    C_VARIADIC = "E0708", "C variadic arguments misused";
    CROSS_TARGET = "E0709", "cannot build for another target";

    // Tests.
    TEST_SIGNATURE = "E0801", "test has the wrong signature";
    TEST_FAILED = "E0802", "test failed";

    // Compile time.
    COMPTIME_FAILED = "E0901", "compile-time evaluation failed";
    MACRO_FAILED = "E0902", "macro expansion failed";
    COMPTIME_LIMIT = "E0903", "compile-time code exceeded a limit";
    COMPTIME_FOREIGN = "E0904", "compile-time code called C";
    COMPTIME_ESCAPE = "E0905", "compile-time value can't be used at run time";
    COMPTIME_AT_RUNTIME = "E0906", "compile-time-only code runs at run time";
    EMBED_FAILED = "E0907", "cannot embed file";
    BAD_DEFINE = "E0908", "invalid `-define:` value";
    COMPTIME_OUTPUT = "E0909", "compile-time code printed output";
    QUOTE_OUTSIDE_MACRO = "E0910", "`quote` outside a `macro def`";
    SPLICE_MISMATCH = "E0911", "spliced value doesn't fit its place";
    MACRO_ARGUMENT = "E0912", "macro argument of the wrong kind";
    MACRO_DECLARATION = "E0913", "declaration a macro can't generate";
    ENUM_MEMBER_MACRO = "E0914", "enum member named like a macro";
}

/// Looks up a registered code by its text, case-insensitively.
pub fn lookup(code: &str) -> Option<CodeInfo> {
    ALL.iter().copied().find(|c| c.code.0.eq_ignore_ascii_case(code))
}
