//! The runtime functions native code calls: their names, their machine signatures and
//! where they are. Each takes and returns only plain numbers.

use cranelift_codegen::ir::types::{F64, I8, I64, Type};
use fennec_interp::embed::{checks, dicts, eval, heap, io, lists, names, objects, strings, temps};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Helper {
    Fail,
    AtCall,
    Enter,
    Leave,
    PushScope,
    PopScope,
    Declare,
    Assign,
    Get,
    Take,
    PutBack,
    DeclareCapability,
    Mark,
    Truncate,
    Keep,
    Int,
    Float,
    Bool,
    None,
    Unit,
    Capability,
    Str,
    ToInt,
    ToFloat,
    ToBool,
    IsErr,
    IsNone,
    Copy,
    Unpack,
    Unwrap,
    Ok,
    Err,
    Passes,
    WithCause,
    Input,
    Expr,
    Stmt,
    Leaving,
    Left,
    MainResult,
    Matches,
    Bind,
    NoMatch,
    PrintInt,
    PrintFloat,
    PrintBool,
    PrintStr,
    PrintValue,
    PrintSpec,
    Items,
    Next,
    Count,
    Item,
    Binary,
    Unary,
    Field,
    Index,
    TypeOf,
    Path,
    PathField,
    PathIndex,
    PathGet,
    Put,
    PutItem,
    Entries,
    MakeInstance,
    MakeVariant,
    CaseOf,
    Alloc,
    Free,
    Unshare,
    BoxToValue,
    BoxFromValue,
    BoxEqual,
    StrFromValue,
    StrConcat,
    StrAppend,
    StrCharAt,
    DictNew,
    DictSet,
    DictMissing,
    StrIndex,
    StrSlice,
    StrCompare,
    StrContains,
    StrPrint,
    Sin,
    Cos,
    IntPower,
    FloatPower,
    ListNew,
    ListGrow,
    ListUnshare,
    ListClear,
    ListRemoveAt,
    ListInsert,
    ListRemove,
    ListContains,
    PopEmpty,
    IntText,
    FloatText,
    ValueText,
    SpecText,
    StrCode,
    StrJoin,
    StrStrip,
    ListSlice,
    SetRemove,
    SetClear,
    CellNew,
    CellRead,
    ListSorted,
    ListSortedBy,
    ListFromValue,
    OutOfRange,
}

pub(crate) const ALL: [Helper; 115] = [
    Helper::Fail,
    Helper::AtCall,
    Helper::Enter,
    Helper::Leave,
    Helper::PushScope,
    Helper::PopScope,
    Helper::Declare,
    Helper::Assign,
    Helper::Get,
    Helper::Take,
    Helper::PutBack,
    Helper::DeclareCapability,
    Helper::Mark,
    Helper::Truncate,
    Helper::Keep,
    Helper::Int,
    Helper::Float,
    Helper::Bool,
    Helper::None,
    Helper::Unit,
    Helper::Capability,
    Helper::Str,
    Helper::ToInt,
    Helper::ToFloat,
    Helper::ToBool,
    Helper::IsErr,
    Helper::IsNone,
    Helper::Copy,
    Helper::Unpack,
    Helper::Unwrap,
    Helper::Ok,
    Helper::Err,
    Helper::Passes,
    Helper::WithCause,
    Helper::Input,
    Helper::Expr,
    Helper::Stmt,
    Helper::Leaving,
    Helper::Left,
    Helper::MainResult,
    Helper::Matches,
    Helper::Bind,
    Helper::NoMatch,
    Helper::PrintInt,
    Helper::PrintFloat,
    Helper::PrintBool,
    Helper::PrintStr,
    Helper::PrintValue,
    Helper::PrintSpec,
    Helper::Items,
    Helper::Next,
    Helper::Count,
    Helper::Item,
    Helper::Binary,
    Helper::Unary,
    Helper::Field,
    Helper::Index,
    Helper::TypeOf,
    Helper::Path,
    Helper::PathField,
    Helper::PathIndex,
    Helper::PathGet,
    Helper::Put,
    Helper::PutItem,
    Helper::Entries,
    Helper::MakeInstance,
    Helper::MakeVariant,
    Helper::CaseOf,
    Helper::Alloc,
    Helper::Free,
    Helper::Unshare,
    Helper::BoxToValue,
    Helper::BoxFromValue,
    Helper::BoxEqual,
    Helper::StrFromValue,
    Helper::StrConcat,
    Helper::StrAppend,
    Helper::StrCharAt,
    Helper::DictNew,
    Helper::DictSet,
    Helper::DictMissing,
    Helper::StrIndex,
    Helper::StrSlice,
    Helper::StrCompare,
    Helper::StrContains,
    Helper::StrPrint,
    Helper::Sin,
    Helper::Cos,
    Helper::IntPower,
    Helper::FloatPower,
    Helper::ListNew,
    Helper::ListGrow,
    Helper::ListUnshare,
    Helper::ListClear,
    Helper::ListRemoveAt,
    Helper::ListInsert,
    Helper::ListRemove,
    Helper::ListContains,
    Helper::PopEmpty,
    Helper::IntText,
    Helper::FloatText,
    Helper::ValueText,
    Helper::SpecText,
    Helper::StrCode,
    Helper::StrJoin,
    Helper::StrStrip,
    Helper::ListSlice,
    Helper::SetRemove,
    Helper::SetClear,
    Helper::CellNew,
    Helper::CellRead,
    Helper::ListSorted,
    Helper::ListSortedBy,
    Helper::ListFromValue,
    Helper::OutOfRange,
];

/// A helper's symbol, parameters and results.
pub(crate) struct Shape {
    pub(crate) symbol: &'static str,
    pub(crate) params: &'static [Type],
    pub(crate) results: &'static [Type],
}

const fn shape(symbol: &'static str, params: &'static [Type], results: &'static [Type]) -> Shape {
    Shape { symbol, params, results }
}

impl Helper {
    pub(crate) fn shape(self) -> Shape {
        if let Some(shape) = self.data_shape() {
            return shape;
        }
        match self {
            Helper::Fail => shape("fennec_fail", &[I64, I64, I64], &[]),
            Helper::AtCall => shape("fennec_at_call", &[I64, I64], &[]),
            Helper::Enter => shape("fennec_enter", &[], &[]),
            Helper::Leave => shape("fennec_leave", &[], &[]),
            Helper::PushScope => shape("fennec_push_scope", &[], &[]),
            Helper::PopScope => shape("fennec_pop_scope", &[], &[]),
            Helper::Declare => shape("fennec_declare", &[I64, I64, I8], &[]),
            Helper::Assign => shape("fennec_assign", &[I64, I64], &[]),
            Helper::Get => shape("fennec_get", &[I64], &[I64]),
            Helper::Take => shape("fennec_take", &[I64], &[I64]),
            Helper::PutBack => shape("fennec_put_back", &[I64, I64], &[]),
            Helper::DeclareCapability => shape("fennec_declare_capability", &[I64, I64], &[]),
            Helper::Mark => shape("fennec_mark", &[], &[I64]),
            Helper::Truncate => shape("fennec_truncate", &[I64], &[]),
            Helper::Keep => shape("fennec_keep", &[I64, I64], &[I64]),
            Helper::Int => shape("fennec_int", &[I64], &[I64]),
            Helper::Float => shape("fennec_float", &[F64], &[I64]),
            Helper::Bool => shape("fennec_bool", &[I8], &[I64]),
            Helper::None => shape("fennec_none", &[], &[I64]),
            Helper::Unit => shape("fennec_unit", &[], &[I64]),
            Helper::Capability => shape("fennec_capability", &[I64], &[I64]),
            Helper::Str => shape("fennec_str", &[I64], &[I64]),
            Helper::ToInt => shape("fennec_to_int", &[I64, I64, I64], &[I64]),
            Helper::ToFloat => shape("fennec_to_float", &[I64, I64, I64], &[F64]),
            Helper::ToBool => shape("fennec_to_bool", &[I64, I64, I64], &[I8]),
            Helper::IsErr => shape("fennec_is_err", &[I64], &[I8]),
            Helper::IsNone => shape("fennec_is_none", &[I64], &[I8]),
            Helper::Copy => shape("fennec_copy", &[I64], &[I64]),
            Helper::Unpack => shape("fennec_unpack", &[I64, I64, I64, I8, I64, I64], &[I64]),
            Helper::Unwrap => shape("fennec_unwrap", &[I64], &[I64]),
            Helper::Ok => shape("fennec_ok", &[I64], &[I64]),
            Helper::Err => shape("fennec_err", &[I64, I64, I64], &[I64]),
            Helper::Passes => shape("fennec_passes", &[I64], &[I8]),
            Helper::WithCause => shape("fennec_with_cause", &[I64, I64], &[I64]),
            Helper::Input => shape("fennec_input", &[I64], &[]),
            Helper::Expr => shape("fennec_expr", &[I64, I64, I64], &[I64]),
            Helper::Stmt => shape("fennec_stmt", &[I64, I64, I64], &[]),
            Helper::Leaving => shape("fennec_leaving", &[], &[I8]),
            Helper::Left => shape("fennec_left", &[], &[I64]),
            Helper::MainResult => shape("fennec_main_result", &[I64, I64, I64], &[]),
            Helper::Matches => shape("fennec_matches", &[I64, I64, I64, I64], &[I8]),
            Helper::Bind => shape("fennec_bind", &[], &[]),
            Helper::NoMatch => shape("fennec_no_match", &[I64, I64, I64], &[]),
            Helper::PrintInt => shape("fennec_print_int", &[I64], &[]),
            Helper::PrintFloat => shape("fennec_print_float", &[F64], &[]),
            Helper::PrintBool => shape("fennec_print_bool", &[I8], &[]),
            Helper::PrintStr => shape("fennec_print_str", &[I64], &[]),
            Helper::PrintValue => shape("fennec_print_value", &[I64], &[]),
            Helper::PrintSpec => shape("fennec_print_spec", &[I64, I64, I64, I64], &[]),
            Helper::Items => shape("fennec_items", &[I64, I64, I64], &[I64]),
            Helper::Count => shape("fennec_count", &[I64], &[I64]),
            Helper::Item => shape("fennec_item", &[I64, I64], &[I64]),
            Helper::Binary => shape("fennec_binary", &[I64, I64, I64, I64, I64], &[I64]),
            Helper::Unary => shape("fennec_unary", &[I64, I64, I64, I64], &[I64]),
            Helper::Field => shape("fennec_field", &[I64, I64, I8, I64, I64], &[I64]),
            Helper::Index => shape("fennec_index", &[I64, I64, I64, I64], &[I64]),
            Helper::TypeOf => shape("fennec_type_of", &[I64], &[I64]),
            Helper::Path => shape("fennec_path", &[I64], &[]),
            Helper::PathField => shape("fennec_path_field", &[I64], &[]),
            Helper::PathIndex => shape("fennec_path_index", &[I64], &[]),
            Helper::PathGet => shape("fennec_path_get", &[I64, I64], &[I64]),
            Helper::Put => shape("fennec_put", &[I64, I64, I64], &[]),
            Helper::PutItem => shape("fennec_put_item", &[I64, I64, I64, I64], &[]),
            Helper::Entries => shape("fennec_entries", &[I64, I64, I64], &[I64]),
            Helper::MakeInstance => shape("fennec_make_instance", &[I64, I64, I64, I64], &[I64]),
            Helper::Sin => shape("fennec_sin", &[F64], &[F64]),
            Helper::Cos => shape("fennec_cos", &[F64], &[F64]),
            Helper::FloatPower => shape("fennec_float_power", &[F64, F64], &[F64]),
            Helper::IntPower => shape("fennec_int_power", &[I64, I64, I64, I64], &[I64]),
            Helper::ListNew => shape("fennec_list_new", &[I64, I64], &[I64]),
            Helper::ListGrow => shape("fennec_list_grow", &[I64], &[I64]),
            Helper::ListUnshare => shape("fennec_list_unshare", &[I64], &[I64]),
            Helper::ListFromValue => shape("fennec_list_from_value", &[I64, I64, I64, I64], &[I64]),
            Helper::OutOfRange => shape("fennec_out_of_range", &[I64, I64, I64, I64], &[]),
            // The helpers for boxed values and strings, in `data_shape`.
            _ => shape("", &[], &[]),
        }
    }

    /// Whether calling it means the function needs a frame and temporaries.
    pub(crate) fn needs_frame(self) -> bool {
        !matches!(
            self,
            Helper::Fail
                | Helper::Alloc
                | Helper::Free
                | Helper::Unshare
                | Helper::BoxEqual
                | Helper::StrConcat
                | Helper::StrAppend
                | Helper::StrCharAt
                | Helper::FloatPower
                | Helper::IntPower
                | Helper::ListNew
                | Helper::ListGrow
                | Helper::ListUnshare
                | Helper::ListSorted
                | Helper::ListSortedBy
                | Helper::IntText
                | Helper::FloatText
                | Helper::StrCode
                | Helper::StrJoin
                | Helper::StrStrip
                | Helper::ListSlice
                | Helper::SetRemove
                | Helper::SetClear
                | Helper::ListClear
                | Helper::ListRemoveAt
                | Helper::ListInsert
                | Helper::ListRemove
                | Helper::ListContains
                | Helper::PopEmpty
                | Helper::OutOfRange
                | Helper::DictNew
                | Helper::DictSet
                | Helper::DictMissing
                | Helper::StrIndex
                | Helper::StrSlice
                | Helper::StrCompare
                | Helper::StrContains
                | Helper::StrPrint
                | Helper::AtCall
                | Helper::Sin
                | Helper::Cos
                | Helper::PrintInt
                | Helper::PrintFloat
                | Helper::PrintBool
                | Helper::PrintStr
        )
    }

    /// Whether it gives back a temporary, which the statement must drop when it's done.
    /// Others give back plain numbers: a count, an address, an unboxed int.
    pub(crate) fn makes_temporary(self) -> bool {
        self.shape().results == [I64]
            && !matches!(
                self,
                Helper::Mark
                    | Helper::Count
                    | Helper::TypeOf
                    | Helper::ToInt
                    | Helper::IntPower
                    | Helper::ListNew
                    | Helper::ListGrow
                    | Helper::ListUnshare
                    | Helper::ListSorted
                    | Helper::ListSortedBy
                    | Helper::ListInsert
                    | Helper::CellNew
                    | Helper::SetRemove
                    | Helper::SetClear
                    | Helper::IntText
                    | Helper::FloatText
                    | Helper::ValueText
                    | Helper::SpecText
                    | Helper::StrCode
                    | Helper::StrJoin
                    | Helper::ListSlice
                    | Helper::ListFromValue
                    | Helper::CaseOf
                    | Helper::Alloc
                    | Helper::Unshare
                    | Helper::BoxFromValue
                    | Helper::StrFromValue
                    | Helper::StrConcat
                    | Helper::StrAppend
                    | Helper::StrCharAt
                    | Helper::DictNew
                    | Helper::DictSet
                    | Helper::StrIndex
                    | Helper::StrSlice
                    | Helper::StrCompare
            )
    }

    /// Whether it can fail, so native code must check the failure flag after calling it.
    pub(crate) fn can_fail(self) -> bool {
        matches!(
            self,
            Helper::Next
                | Helper::ToInt
                | Helper::Binary
                | Helper::Unary
                | Helper::Field
                | Helper::Index
                | Helper::Put
                | Helper::PathGet
                | Helper::PutItem
                | Helper::Entries
                | Helper::MakeInstance
                | Helper::MakeVariant
                | Helper::CaseOf
                | Helper::BoxFromValue
                | Helper::StrFromValue
                | Helper::StrIndex
                | Helper::StrSlice
                | Helper::StrPrint
                | Helper::DictMissing
                | Helper::IntPower
                | Helper::ListFromValue
                | Helper::SpecText
                | Helper::SetRemove
                | Helper::StrCode
                | Helper::ListInsert
                | Helper::ListRemove
                | Helper::PopEmpty
                | Helper::OutOfRange
                | Helper::MainResult
                | Helper::Unpack
                | Helper::ToFloat
                | Helper::ToBool
                | Helper::Err
                | Helper::Expr
                | Helper::Stmt
                | Helper::Matches
                | Helper::NoMatch
                | Helper::PrintSpec
                | Helper::PrintValue
                | Helper::Items
        )
    }

    pub(crate) fn address(self) -> *const u8 {
        if let Some(address) = self.data_address() {
            return address;
        }
        match self {
            Helper::Fail => checks::fail as *const u8,
            Helper::AtCall => eval::at_call as *const u8,
            Helper::Enter => names::enter as *const u8,
            Helper::Leave => names::leave as *const u8,
            Helper::PushScope => names::push_scope as *const u8,
            Helper::PopScope => names::pop_scope as *const u8,
            Helper::Declare => names::declare as *const u8,
            Helper::Assign => names::assign as *const u8,
            Helper::Get => names::get as *const u8,
            Helper::Take => names::take as *const u8,
            Helper::PutBack => names::put_back as *const u8,
            Helper::DeclareCapability => names::declare_capability as *const u8,
            Helper::Mark => temps::mark as *const u8,
            Helper::Truncate => temps::truncate as *const u8,
            Helper::Keep => temps::keep as *const u8,
            Helper::Int => temps::int as *const u8,
            Helper::Float => temps::float as *const u8,
            Helper::Bool => temps::bool as *const u8,
            Helper::None => temps::none as *const u8,
            Helper::Unit => temps::unit as *const u8,
            Helper::Capability => temps::capability as *const u8,
            Helper::Str => temps::str as *const u8,
            Helper::ToInt => temps::to_int as *const u8,
            Helper::ToFloat => temps::to_float as *const u8,
            Helper::ToBool => temps::to_bool as *const u8,
            Helper::IsErr => temps::is_err as *const u8,
            Helper::IsNone => temps::is_none as *const u8,
            Helper::Copy => temps::copy as *const u8,
            Helper::Unpack => temps::unpack as *const u8,
            Helper::Unwrap => temps::unwrap as *const u8,
            Helper::Ok => temps::ok as *const u8,
            Helper::Err => temps::err as *const u8,
            Helper::Passes => temps::passes as *const u8,
            Helper::WithCause => temps::with_cause as *const u8,
            Helper::Input => eval::input as *const u8,
            Helper::Expr => eval::expr as *const u8,
            Helper::Stmt => eval::stmt as *const u8,
            Helper::Leaving => eval::leaving as *const u8,
            Helper::Left => eval::left as *const u8,
            Helper::MainResult => temps::main_result as *const u8,
            Helper::Matches => eval::matches as *const u8,
            Helper::Bind => eval::bind as *const u8,
            Helper::NoMatch => eval::no_match as *const u8,
            Helper::PrintInt => io::print_int as *const u8,
            Helper::PrintFloat => io::print_float as *const u8,
            Helper::PrintBool => io::print_bool as *const u8,
            Helper::PrintStr => io::print_str as *const u8,
            Helper::PrintValue => io::print_value as *const u8,
            Helper::PrintSpec => io::print_spec as *const u8,
            Helper::Items => io::items as *const u8,
            Helper::Count => io::count as *const u8,
            Helper::Item => io::item as *const u8,
            Helper::Binary => objects::binary as *const u8,
            Helper::Unary => objects::unary as *const u8,
            Helper::Field => objects::field as *const u8,
            Helper::Index => objects::index as *const u8,
            Helper::TypeOf => objects::type_of as *const u8,
            Helper::Path => objects::path as *const u8,
            Helper::PathField => objects::path_field as *const u8,
            Helper::PathIndex => objects::path_index as *const u8,
            Helper::PathGet => objects::path_get as *const u8,
            Helper::Put => objects::put as *const u8,
            Helper::PutItem => objects::put_item as *const u8,
            Helper::Entries => objects::entries as *const u8,
            Helper::MakeInstance => objects::make_instance as *const u8,
            Helper::Sin => checks::sin as *const u8,
            Helper::Cos => checks::cos as *const u8,
            Helper::FloatPower => lists::float_power as *const u8,
            Helper::IntPower => lists::int_power as *const u8,
            Helper::ListNew => lists::new as *const u8,
            Helper::ListGrow => lists::grow as *const u8,
            Helper::ListUnshare => lists::unshare as *const u8,
            Helper::ListFromValue => lists::from_value as *const u8,
            Helper::OutOfRange => lists::out_of_range as *const u8,
            // The helpers for boxed values and strings, in `data_address`.
            _ => std::ptr::null(),
        }
    }

    /// The machine signature of a helper for enums, boxed values or strings.
    fn data_shape(self) -> Option<Shape> {
        Some(match self {
            Helper::Next => shape("fennec_next", &[I64, I64, I64, I64, I64], &[I64]),
            Helper::MakeVariant => shape("fennec_make_variant", &[I64, I64, I64, I64, I64], &[I64]),
            Helper::CaseOf => shape("fennec_case_of", &[I64, I64, I64, I64], &[I64]),
            Helper::ListClear => shape("fennec_list_clear", &[I64], &[]),
            Helper::ListRemoveAt => shape("fennec_list_remove_at", &[I64, I64], &[]),
            Helper::ListInsert => shape("fennec_list_insert", &[I64, I64, I64, I64], &[I64]),
            Helper::ListRemove => shape("fennec_list_remove", &[I64, I64, I64, I64], &[]),
            Helper::ListContains => shape("fennec_list_contains", &[I64, I64], &[I8]),
            Helper::PopEmpty => shape("fennec_pop_empty", &[I64, I64], &[]),
            Helper::IntText => shape("fennec_int_text", &[I64], &[I64]),
            Helper::FloatText => shape("fennec_float_text", &[F64], &[I64]),
            Helper::ValueText => shape("fennec_value_text", &[I64], &[I64]),
            Helper::SpecText => shape("fennec_spec_text", &[I64, I64, I64, I64], &[I64]),
            Helper::StrCode => shape("fennec_str_code", &[I64, I64, I64], &[I64]),
            Helper::StrStrip => shape("fennec_str_strip", &[I64, I64], &[I64]),
            Helper::StrJoin => shape("fennec_str_join", &[I64, I64], &[I64]),
            Helper::ListSlice => shape("fennec_list_slice", &[I64, I64, I64], &[I64]),
            Helper::SetRemove => shape("fennec_set_remove", &[I64, I64, I8, I64, I64], &[I64]),
            Helper::SetClear => shape("fennec_set_clear", &[I64], &[I64]),
            Helper::CellNew => shape("fennec_cell_new", &[I64], &[I64]),
            Helper::CellRead => shape("fennec_cell_read", &[I64], &[I64]),
            Helper::ListSorted => shape("fennec_list_sorted", &[I64, I64], &[I64]),
            Helper::ListSortedBy => shape("fennec_list_sorted_by", &[I64, I64, I64], &[I64]),
            Helper::Alloc => shape("fennec_alloc", &[I64], &[I64]),
            Helper::Free => shape("fennec_free", &[I64], &[]),
            Helper::Unshare => shape("fennec_unshare", &[I64], &[I64]),
            Helper::BoxToValue => shape("fennec_box_to_value", &[I64], &[I64]),
            Helper::BoxFromValue => shape("fennec_box_from_value", &[I64, I64, I64, I64], &[I64]),
            Helper::BoxEqual => shape("fennec_box_equal", &[I64, I64], &[I8]),
            Helper::StrFromValue => shape("fennec_str_from_value", &[I64, I64, I64], &[I64]),
            Helper::StrConcat => shape("fennec_str_concat", &[I64, I64], &[I64]),
            Helper::StrAppend => shape("fennec_str_append", &[I64, I64], &[I64]),
            Helper::StrCharAt => shape("fennec_str_char_at", &[I64, I64], &[I64]),
            Helper::DictNew => shape("fennec_dict_new", &[I64, I64], &[I64]),
            Helper::DictSet => shape("fennec_dict_set", &[I64, I64, I64], &[I64]),
            Helper::DictMissing => shape("fennec_dict_missing", &[I64, I64, I8, I64, I64], &[]),
            Helper::StrIndex => shape("fennec_str_index", &[I64, I64, I64, I64], &[I64]),
            Helper::StrSlice => shape("fennec_str_slice", &[I64, I64, I64, I64, I64], &[I64]),
            Helper::StrCompare => shape("fennec_str_compare", &[I64, I64], &[I64]),
            Helper::StrContains => shape("fennec_str_contains", &[I64, I64], &[I8]),
            Helper::StrPrint => shape("fennec_str_print", &[I64], &[]),
            _ => return None,
        })
    }

    /// Where a helper for enums, boxed values or strings is.
    fn data_address(self) -> Option<*const u8> {
        Some(match self {
            Helper::Next => io::next as *const u8,
            Helper::MakeVariant => objects::make_variant as *const u8,
            Helper::CaseOf => objects::case_of as *const u8,
            Helper::ListClear => lists::clear as *const u8,
            Helper::ListRemoveAt => lists::remove_at as *const u8,
            Helper::ListInsert => lists::insert as *const u8,
            Helper::ListRemove => lists::remove as *const u8,
            Helper::ListContains => lists::contains as *const u8,
            Helper::PopEmpty => lists::pop_empty as *const u8,
            Helper::IntText => strings::int_text as *const u8,
            Helper::FloatText => strings::float_text as *const u8,
            Helper::ValueText => strings::value_text as *const u8,
            Helper::SpecText => strings::spec_text as *const u8,
            Helper::StrCode => strings::code as *const u8,
            Helper::StrJoin => strings::join as *const u8,
            Helper::StrStrip => strings::strip as *const u8,
            Helper::ListSlice => lists::slice as *const u8,
            Helper::SetRemove => dicts::set_remove as *const u8,
            Helper::SetClear => dicts::set_clear as *const u8,
            Helper::CellNew => heap::cell_new as *const u8,
            Helper::CellRead => heap::cell_read as *const u8,
            Helper::ListSorted => lists::sorted as *const u8,
            Helper::ListSortedBy => lists::sorted_by as *const u8,
            Helper::Alloc => heap::alloc as *const u8,
            Helper::Free => heap::free as *const u8,
            Helper::Unshare => heap::unshare as *const u8,
            Helper::BoxToValue => heap::to_value as *const u8,
            Helper::BoxFromValue => heap::from_value as *const u8,
            Helper::BoxEqual => heap::equal as *const u8,
            Helper::StrFromValue => strings::from_value as *const u8,
            Helper::StrConcat => strings::concat as *const u8,
            Helper::StrAppend => strings::append as *const u8,
            Helper::StrCharAt => strings::char_at as *const u8,
            Helper::DictNew => dicts::new as *const u8,
            Helper::DictSet => dicts::set as *const u8,
            Helper::DictMissing => dicts::missing as *const u8,
            Helper::StrIndex => strings::index as *const u8,
            Helper::StrSlice => strings::slice as *const u8,
            Helper::StrCompare => strings::compare as *const u8,
            Helper::StrContains => strings::contains as *const u8,
            Helper::StrPrint => strings::print as *const u8,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_runtime_library_exports_every_helper() {
        for helper in super::ALL {
            let symbol = helper.shape().symbol;
            assert!(fennec_runtime::SYMBOLS.contains(&symbol), "the runtime library doesn't export {symbol}");
        }
    }
}
