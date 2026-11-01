//! The runtime library compiled executables link against: every function their code
//! calls, exported under the name it calls it by, and the interpreter for the parts of a
//! program that aren't compiled. `fennec build` links an executable's object file with this
//! library and the system's C libraries.
//!
//! The functions are `fennec_interp::embed`'s; this crate only gives them names a linker
//! can find, which is the one thing here Rust calls unsafe.
#![expect(
    unsafe_code,
    reason = "exporting the runtime's functions under fixed names, for linked executables"
)]

use fennec_interp::embed::{
    aot, checks, dicts, eval, heap, io, lists, names, objects, regex, strings, temps,
};

/// The exported functions that can fail, setting the flag `fennec_aot_failed_flag` gives the
/// address of, which stays the same for the whole run. Generated code has to check it after
/// calling one of these, and needn't after any other.
///
/// Nothing exported unwinds: a panic crossing `extern "C"` aborts the program.
pub const CAN_FAIL: &[&str] = &[
    "fennec_list_sorted_with",
    "fennec_regex_split",
    "fennec_regex_find",
    "fennec_regex_find_all",
    "fennec_regex_replace",
    "fennec_binary",
    "fennec_box_from_value",
    "fennec_case_of",
    "fennec_dict_missing",
    "fennec_entries",
    "fennec_err",
    "fennec_expr",
    "fennec_field",
    "fennec_float_spec_text",
    "fennec_index",
    "fennec_int_power",
    "fennec_int_spec_text",
    "fennec_items",
    "fennec_list_from_value",
    "fennec_list_insert",
    "fennec_list_remove",
    "fennec_main_result",
    "fennec_make_instance",
    "fennec_make_variant",
    "fennec_matches",
    "fennec_next",
    "fennec_no_match",
    "fennec_out_of_range",
    "fennec_path_get",
    "fennec_pop_empty",
    "fennec_print_spec",
    "fennec_print_value",
    "fennec_put",
    "fennec_put_item",
    "fennec_set_remove",
    "fennec_spec_text",
    "fennec_stmt",
    "fennec_str_code",
    "fennec_str_from_value",
    "fennec_str_index",
    "fennec_str_print",
    "fennec_str_slice",
    "fennec_to_bool",
    "fennec_to_float",
    "fennec_to_int",
    "fennec_unary",
    "fennec_unpack",
];

/// Exports each function under a symbol: `symbol(params) -> result => function(args);`.
macro_rules! export {
    ($($symbol:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? => $module:ident::$function:ident($($pass:ident),*);)*) => {
        $(
            #[unsafe(no_mangle)]
            pub extern "C" fn $symbol($($arg: $ty),*) $(-> $ret)? {
                $module::$function($($pass),*)
            }
        )*

        /// Every symbol this library exports.
        pub const SYMBOLS: &[&str] = &[$(stringify!($symbol)),*];
    };
}

export! {
    fennec_fail(a0: i64, a1: i64, a2: i64) => checks::fail(a0, a1, a2);
    fennec_at_call(a0: i64, a1: i64) => eval::at_call(a0, a1);
    fennec_enter() => names::enter();
    fennec_leave() => names::leave();
    fennec_push_scope() => names::push_scope();
    fennec_pop_scope() => names::pop_scope();
    fennec_declare(a0: i64, a1: i64, a2: i8) => names::declare(a0, a1, a2);
    fennec_assign(a0: i64, a1: i64) => names::assign(a0, a1);
    fennec_get(a0: i64) -> i64 => names::get(a0);
    fennec_take(a0: i64) -> i64 => names::take(a0);
    fennec_put_back(a0: i64, a1: i64) => names::put_back(a0, a1);
    fennec_declare_capability(a0: i64, a1: i64) => names::declare_capability(a0, a1);
    fennec_mark() -> i64 => temps::mark();
    fennec_truncate(a0: i64) => temps::truncate(a0);
    fennec_keep(a0: i64, a1: i64) -> i64 => temps::keep(a0, a1);
    fennec_int(a0: i64) -> i64 => temps::int(a0);
    fennec_float(a0: f64) -> i64 => temps::float(a0);
    fennec_bool(a0: i8) -> i64 => temps::bool(a0);
    fennec_none() -> i64 => temps::none();
    fennec_unit() -> i64 => temps::unit();
    fennec_str(a0: i64) -> i64 => temps::str(a0);
    fennec_to_int(a0: i64, a1: i64, a2: i64) -> i64 => temps::to_int(a0, a1, a2);
    fennec_to_float(a0: i64, a1: i64, a2: i64) -> f64 => temps::to_float(a0, a1, a2);
    fennec_to_bool(a0: i64, a1: i64, a2: i64) -> i8 => temps::to_bool(a0, a1, a2);
    fennec_is_err(a0: i64) -> i8 => temps::is_err(a0);
    fennec_is_none(a0: i64) -> i8 => temps::is_none(a0);
    fennec_copy(a0: i64) -> i64 => temps::copy(a0);
    fennec_unpack(a0: i64, a1: i64, a2: i64, a3: i8, a4: i64, a5: i64) -> i64 => temps::unpack(a0, a1, a2, a3, a4, a5);
    fennec_unwrap(a0: i64) -> i64 => temps::unwrap(a0);
    fennec_ok(a0: i64) -> i64 => temps::ok(a0);
    fennec_err(a0: i64, a1: i64, a2: i64) -> i64 => temps::err(a0, a1, a2);
    fennec_passes(a0: i64) -> i8 => temps::passes(a0);
    fennec_with_cause(a0: i64, a1: i64) -> i64 => temps::with_cause(a0, a1);
    fennec_input(a0: i64) => eval::input(a0);
    fennec_expr(a0: i64, a1: i64, a2: i64) -> i64 => eval::expr(a0, a1, a2);
    fennec_stmt(a0: i64, a1: i64, a2: i64) => eval::stmt(a0, a1, a2);
    fennec_leaving() -> i8 => eval::leaving();
    fennec_left() -> i64 => eval::left();
    fennec_main_result(a0: i64, a1: i64, a2: i64) => temps::main_result(a0, a1, a2);
    fennec_matches(a0: i64, a1: i64, a2: i64, a3: i64) -> i8 => eval::matches(a0, a1, a2, a3);
    fennec_bind() => eval::bind();
    fennec_no_match(a0: i64, a1: i64, a2: i64) => eval::no_match(a0, a1, a2);
    fennec_print_int(a0: i64) => io::print_int(a0);
    fennec_print_float(a0: f64) => io::print_float(a0);
    fennec_print_bool(a0: i8) => io::print_bool(a0);
    fennec_print_str(a0: i64) => io::print_str(a0);
    fennec_print_value(a0: i64) => io::print_value(a0);
    fennec_print_spec(a0: i64, a1: i64, a2: i64, a3: i64) => io::print_spec(a0, a1, a2, a3);
    fennec_items(a0: i64, a1: i64, a2: i64) -> i64 => io::items(a0, a1, a2);
    fennec_next(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => io::next(a0, a1, a2, a3, a4);
    fennec_count(a0: i64) -> i64 => io::count(a0);
    fennec_item(a0: i64, a1: i64) -> i64 => io::item(a0, a1);
    fennec_binary(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => objects::binary(a0, a1, a2, a3, a4);
    fennec_unary(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => objects::unary(a0, a1, a2, a3);
    fennec_field(a0: i64, a1: i64, a2: i8, a3: i64, a4: i64) -> i64 => objects::field(a0, a1, a2, a3, a4);
    fennec_index(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => objects::index(a0, a1, a2, a3);
    fennec_type_of(a0: i64) -> i64 => objects::type_of(a0);
    fennec_path(a0: i64) => objects::path(a0);
    fennec_path_field(a0: i64) => objects::path_field(a0);
    fennec_path_index(a0: i64) => objects::path_index(a0);
    fennec_path_get(a0: i64, a1: i64) -> i64 => objects::path_get(a0, a1);
    fennec_put(a0: i64, a1: i64, a2: i64) => objects::put(a0, a1, a2);
    fennec_put_item(a0: i64, a1: i64, a2: i64, a3: i64) => objects::put_item(a0, a1, a2, a3);
    fennec_entries(a0: i64, a1: i64, a2: i64) -> i64 => objects::entries(a0, a1, a2);
    fennec_make_instance(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => objects::make_instance(a0, a1, a2, a3);
    fennec_sin(a0: f64) -> f64 => checks::sin(a0);
    fennec_cos(a0: f64) -> f64 => checks::cos(a0);
    fennec_float_power(a0: f64, a1: f64) -> f64 => lists::float_power(a0, a1);
    fennec_int_power(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::int_power(a0, a1, a2, a3);
    fennec_list_new(a0: i64, a1: i64) -> i64 => lists::new(a0, a1);
    fennec_list_grow(a0: i64) -> i64 => lists::grow(a0);
    fennec_list_unshare(a0: i64) -> i64 => lists::unshare(a0);
    fennec_list_clear(a0: i64) => lists::clear(a0);
    fennec_list_remove_at(a0: i64, a1: i64) => lists::remove_at(a0, a1);
    fennec_list_insert(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::insert(a0, a1, a2, a3);
    fennec_list_remove(a0: i64, a1: i64, a2: i64, a3: i64) => lists::remove(a0, a1, a2, a3);
    fennec_list_contains(a0: i64, a1: i64) -> i8 => lists::contains(a0, a1);
    fennec_pop_empty(a0: i64, a1: i64) => lists::pop_empty(a0, a1);
    fennec_int_text(a0: i64) -> i64 => strings::int_text(a0);
    fennec_float_text(a0: f64) -> i64 => strings::float_text(a0);
    fennec_value_text(a0: i64) -> i64 => strings::value_text(a0);
    fennec_spec_text(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => strings::spec_text(a0, a1, a2, a3);
    fennec_str_code(a0: i64, a1: i64, a2: i64) -> i64 => strings::code(a0, a1, a2);
    fennec_list_sorted_with(a0: i64, a1: extern "C" fn(i64, i64) -> i64) -> i64 => lists::sorted_with(a0, a1);
    fennec_regex_is_valid(a0: i64) -> i8 => regex::is_valid(a0);
    fennec_regex_replace(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => regex::replace(a0, a1, a2, a3, a4);
    fennec_regex_find_all(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => regex::find_all(a0, a1, a2, a3, a4);
    fennec_regex_find(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64) -> i64 => regex::find(a0, a1, a2, a3, a4, a5);
    fennec_regex_split(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => regex::split(a0, a1, a2, a3, a4);
    fennec_list_with_room(a0: i64, a1: i64) -> i64 => lists::with_room(a0, a1);
    fennec_float_spec_text(a0: f64, a1: i64, a2: i64, a3: i64) -> i64 => strings::float_spec_text(a0, a1, a2, a3);
    fennec_int_spec_text(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => strings::int_spec_text(a0, a1, a2, a3);
    fennec_str_strip(a0: i64, a1: i64) -> i64 => strings::strip(a0, a1);
    fennec_str_join(a0: i64, a1: i64) -> i64 => strings::join(a0, a1);
    fennec_list_slice(a0: i64, a1: i64, a2: i64) -> i64 => lists::slice(a0, a1, a2);
    fennec_set_remove(a0: i64, a1: i64, a2: i8, a3: i64, a4: i64) -> i64 => dicts::set_remove(a0, a1, a2, a3, a4);
    fennec_set_clear(a0: i64) -> i64 => dicts::set_clear(a0);
    fennec_cell_new(a0: i64) -> i64 => heap::cell_new(a0);
    fennec_cell_read(a0: i64) -> i64 => heap::cell_read(a0);
    fennec_list_sorted(a0: i64, a1: i64) -> i64 => lists::sorted(a0, a1);
    fennec_list_sorted_by(a0: i64, a1: i64, a2: i64) -> i64 => lists::sorted_by(a0, a1, a2);
    fennec_list_from_value(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::from_value(a0, a1, a2, a3);
    fennec_out_of_range(a0: i64, a1: i64, a2: i64, a3: i64) => lists::out_of_range(a0, a1, a2, a3);
    fennec_make_variant(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => objects::make_variant(a0, a1, a2, a3, a4);
    fennec_case_of(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => objects::case_of(a0, a1, a2, a3);
    fennec_alloc(a0: i64) -> i64 => heap::alloc(a0);
    fennec_free(a0: i64) => heap::free(a0);
    fennec_unshare(a0: i64) -> i64 => heap::unshare(a0);
    fennec_box_to_value(a0: i64) -> i64 => heap::to_value(a0);
    fennec_box_from_value(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => heap::from_value(a0, a1, a2, a3);
    fennec_box_equal(a0: i64, a1: i64) -> i8 => heap::equal(a0, a1);
    fennec_str_from_value(a0: i64, a1: i64, a2: i64) -> i64 => strings::from_value(a0, a1, a2);
    fennec_str_concat(a0: i64, a1: i64) -> i64 => strings::concat(a0, a1);
    fennec_str_append(a0: i64, a1: i64) -> i64 => strings::append(a0, a1);
    fennec_str_char_at(a0: i64, a1: i64) -> i64 => strings::char_at(a0, a1);
    fennec_dict_new(a0: i64, a1: i64) -> i64 => dicts::new(a0, a1);
    fennec_dict_set(a0: i64, a1: i64, a2: i64) -> i64 => dicts::set(a0, a1, a2);
    fennec_dict_missing(a0: i64, a1: i64, a2: i8, a3: i64, a4: i64) => dicts::missing(a0, a1, a2, a3, a4);
    fennec_str_index(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => strings::index(a0, a1, a2, a3);
    fennec_str_slice(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => strings::slice(a0, a1, a2, a3, a4);
    fennec_str_compare(a0: i64, a1: i64) -> i64 => strings::compare(a0, a1);
    fennec_str_contains(a0: i64, a1: i64) -> i8 => strings::contains(a0, a1);
    fennec_str_print(a0: i64) => strings::print(a0);
    fennec_capability(a0: i64) -> i64 => temps::capability(a0);
    fennec_aot_image(a0: i64) => aot::image(a0);
    fennec_aot_failed_flag() -> i64 => aot::failed_flag();
    fennec_aot_ascii() -> i64 => aot::ascii();
    fennec_aot_literal(a0: i64) -> i64 => aot::literal(a0);
}

/// Runs a compiled executable's program: its `main` calls this.
#[unsafe(no_mangle)]
pub extern "C" fn fennec_aot_run(entry: extern "C" fn(i64), length: i64) -> i32 {
    aot::run(entry, length)
}
