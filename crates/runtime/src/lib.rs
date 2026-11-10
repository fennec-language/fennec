//! The runtime library compiled executables link against: every function their code
//! calls, exported under the name it calls it by. It has no interpreter in it. `fennec build`
//! links an executable's object file with this library and the system's C libraries.
//!
//! The functions are `fennec_rt::embed`'s; this crate only gives them names a linker
//! can find, which is the one thing here Rust calls unsafe.
#![expect(
    unsafe_code,
    reason = "exporting the runtime's functions under fixed names, for linked executables"
)]

use fennec_rt::embed::guarded::rollback;
use fennec_rt::embed::{
    aot, bytes, capabilities, channels, checks, cleanup, dicts, eval, guarded, heap, io, lists, notes,
    numbers, parallel, regex, serve, strings, temps, text, threads,
};

/// The build of Fennec this library is from, after a marker, which `fennec` reads out of the
/// library's file to tell one built from other source. Kept by `#[used]`, since nothing calls it.
#[used]
static BUILD_STAMP: &str = concat!("fennec-runtime-build:", env!("FENNEC_BUILD"));

/// The exported functions that can fail, setting the flag `fennec_aot_failed_flag` gives the
/// address of, which stays the same for the whole run. Generated code has to check it after
/// calling one of these, and needn't after any other.
///
/// Nothing exported unwinds: a panic crossing `extern "C"` aborts the program.
pub const CAN_FAIL: &[&str] = &[
    "fennec_dict_pop",
    "fennec_dict_pop_header",
    "fennec_stop",
    "fennec_channel_new",
    "fennec_channel_new_every",
    "fennec_bytes_of_list",
    "fennec_assert_sides",
    "fennec_list_sorted_with",
    "fennec_list_sorted_by_keys_with",
    "fennec_regex_split",
    "fennec_regex_find",
    "fennec_regex_find_all",
    "fennec_regex_replace",
    "fennec_box_from_value",
    "fennec_buffer_insert",
    "fennec_buffer_pop_empty",
    "fennec_crypto_call",
    "fennec_native_capability",
    "fennec_net_call",
    "fennec_capability_from_value",
    "fennec_dict_missing",
    "fennec_dict_missing_header",
    "fennec_err",
    "fennec_float_spec_text",
    "fennec_f32_spec_text",
    "fennec_u64_spec_text",
    "fennec_int_power",
    "fennec_int_spec_text",
    "fennec_list_from_value",
    "fennec_list_insert",
    "fennec_list_remove",
    "fennec_main_result",
    "fennec_no_match",
    "fennec_out_of_range",
    "fennec_parallel_for",
    "fennec_parallel_for_waiting",
    "fennec_parallel_map",
    "fennec_parallel_map_waiting",
    "fennec_parallel_reduce",
    "fennec_parallel_reduce_waiting",
    "fennec_channel_send",
    "fennec_channel_receive",
    "fennec_launch",
    "fennec_launch_end",
    "fennec_net_serve",
    "fennec_pop_empty",
    "fennec_print_block",
    "fennec_set_remove",
    "fennec_shared_enter",
    "fennec_shared_in",
    "fennec_shared_leave",
    "fennec_shared_new",
    "fennec_shared_out",
    "fennec_shared_settle",
    "fennec_set_remove_header",
    "fennec_str_code",
    "fennec_str_from_value",
    "fennec_str_index",
    "fennec_str_index_text",
    "fennec_str_print",
    "fennec_str_slice",
    "fennec_str_part",
    "fennec_int_header",
    "fennec_u64_header",
    "fennec_code_header",
    "fennec_float_header",
    "fennec_f32_header",
    "fennec_str_split",
    "fennec_stream_next",
    "fennec_to_bool",
    "fennec_to_float",
    "fennec_to_int",
    "fennec_unpack",
];

/// What an executable's program allocates through: its big blocks, and what the runtime
/// allocates for itself while it works. With the `sanitize` feature it's the system's own,
/// for an address sanitizer to watch.
#[cfg(not(feature = "sanitize"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Exports each function under a symbol: `symbol(params) -> result => function(args);`.
macro_rules! export {
    ($($(#[$($meta:tt)*])* $symbol:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? => $module:ident::$function:ident($($pass:ident),*);)*) => {
        $(
            $(#[$($meta)*])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $symbol($($arg: $ty),*) $(-> $ret)? {
                $module::$function($($pass),*)
            }
        )*

        /// Every symbol this library exports.
        pub const SYMBOLS: &[&str] = &[$($(#[$($meta)*])* stringify!($symbol)),*];
    };
}

export! {
    fennec_fail(a0: i64, a1: i64, a2: i64) => checks::fail(a0, a1, a2);
    fennec_at_call(a0: i64, a1: i64) => eval::at_call(a0, a1);
    fennec_trace_frame(a0: i64, a1: i64) => eval::trace_frame(a0, a1);
    fennec_native_made(a0: i64) => heap::native_made(a0);
    fennec_native_freed(a0: i64) => heap::native_freed(a0);
    fennec_notes_frame(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => notes::frame(a0, a1, a2, a3);
    fennec_notes_traced(a0: i64) => notes::traced(a0);
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
    fennec_error_of(a0: i64) -> i64 => temps::error_of(a0);
    fennec_ok(a0: i64) -> i64 => temps::ok(a0);
    fennec_err(a0: i64, a1: i64, a2: i64) -> i64 => temps::err(a0, a1, a2);
    fennec_passes(a0: i64) -> i8 => temps::passes(a0);
    fennec_with_cause(a0: i64, a1: i64) -> i64 => temps::with_cause(a0, a1);
    fennec_input(a0: i64) => eval::input(a0);
    fennec_main_result(a0: i64, a1: i64, a2: i64) => temps::main_result(a0, a1, a2);
    fennec_main_exit(a0: i64) => temps::main_exit(a0);
    fennec_no_match(a0: i64, a1: i64, a2: i64) => eval::no_match(a0, a1, a2);
    fennec_lock_note(a0: u64) -> i64 => threads::lock_note(a0);
    fennec_lock_forget(a0: u64) => threads::lock_forget(a0);
    fennec_launch(a0: serve::TaskBody, a1: i64) -> i64 => serve::launch(a0, a1);
    fennec_channel_new(a0: i64) -> i64 => channels::channel_new(a0);
    fennec_channel_new_every(a0: i64) -> i64 => channels::channel_new_every(a0);
    fennec_channel_listen(a0: i64, a1: i64) -> i64 => channels::channel_listen(a0, a1);
    fennec_crash_hold() -> i64 => cleanup::crash_hold();
    fennec_crash_resume(a0: i64) => cleanup::crash_resume(a0);
    fennec_channel_send(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => channels::channel_send(a0, a1, a2, a3);
    fennec_channel_receive(a0: i64, a1: i64, a2: &mut i64) -> i64 => channels::channel_receive(a0, a1, a2);
    fennec_channel_close(a0: i64) => channels::channel_close(a0);
    fennec_channel_free(a0: i64) => channels::channel_free(a0);
    fennec_launch_end(a0: i64, a1: i64, a2: i64, a3: &mut i64) -> i64 => serve::launch_end(a0, a1, a2, a3);
    fennec_task_failed(a0: i64) => serve::task_failed(a0);
    fennec_lock_begin(a0: i64) => rollback::begin(a0);
    fennec_lock_save(a0: i64, a1: i64, a2: i64, a3: i64) => rollback::save(a0, a1, a2, a3);
    fennec_lock_save_text(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) => rollback::save_text(a0, a1, a2, a3, a4);
    fennec_lock_save_ref(a0: i64, a1: i64, a2: i64) => rollback::save_ref(a0, a1, a2);
    fennec_lock_kept() => rollback::kept();
    fennec_lock_undo() => rollback::undo();
    fennec_lock_undo_word(a0: i64, a1: i64, a2: i64) -> i64 => rollback::undo_word(a0, a1, a2);
    fennec_print_int(a0: i64) => io::print_int(a0);
    fennec_print_float(a0: f64) => io::print_float(a0);
    fennec_print_u64(a0: i64) => io::print_u64(a0);
    fennec_print_f32(a0: f64) => io::print_f32(a0);
    fennec_print_bool(a0: i8) => io::print_bool(a0);
    fennec_print_str(a0: i64) => io::print_str(a0);
    fennec_print_block(a0: i64) => io::print_block(a0);
    fennec_count(a0: i64) -> i64 => io::count(a0);
    fennec_item(a0: i64, a1: i64) -> i64 => io::item(a0, a1);
    fennec_sin(a0: f64) -> f64 => checks::sin(a0);
    fennec_cos(a0: f64) -> f64 => checks::cos(a0);
    fennec_tan(a0: f64) -> f64 => checks::tan(a0);
    fennec_acos(a0: f64) -> f64 => checks::acos(a0);
    fennec_exp(a0: f64) -> f64 => checks::exp(a0);
    fennec_log(a0: f64) -> f64 => checks::log(a0);
    fennec_float_power(a0: f64, a1: f64) -> f64 => lists::float_power(a0, a1);
    fennec_int_power(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::int_power(a0, a1, a2, a3);
    fennec_list_new(a0: i64, a1: i64) -> i64 => lists::new(a0, a1);
    fennec_list_grow(a0: i64) -> i64 => lists::grow(a0);
    fennec_list_pop_unshare(a0: i64) -> i64 => lists::pop_unshare(a0);
    fennec_list_reserve(a0: i64, a1: i64) -> i64 => lists::reserve(a0, a1);
    fennec_list_truncate(a0: i64, a1: i64) -> i64 => lists::truncate(a0, a1);
    fennec_list_unshare(a0: i64) -> i64 => lists::unshare(a0);
    fennec_list_clear(a0: i64) => lists::clear(a0);
    fennec_list_remove_at(a0: i64, a1: i64) => lists::remove_at(a0, a1);
    fennec_list_insert(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::insert(a0, a1, a2, a3);
    fennec_buffer_insert(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::insert_buffer(a0, a1, a2, a3);
    fennec_list_extend(a0: i64, a1: i64) -> i64 => lists::extend(a0, a1);
    fennec_list_remove(a0: i64, a1: i64, a2: i64, a3: i64) => lists::remove(a0, a1, a2, a3);
    fennec_list_contains(a0: i64, a1: i64) -> i8 => lists::contains(a0, a1);
    fennec_pop_empty(a0: i64, a1: i64) => lists::pop_empty(a0, a1);
    fennec_buffer_pop_empty(a0: i64, a1: i64) => lists::pop_empty_buffer(a0, a1);
    fennec_parse_int(a0: i64, a1: &mut i64) -> i64 => numbers::parse_int(a0, a1);
    fennec_parse_u64(a0: i64, a1: &mut i64) -> i64 => numbers::parse_u64(a0, a1);
    fennec_parse_float(a0: i64, a1: &mut i64) -> i64 => numbers::parse_float(a0, a1);
    fennec_not_a_number(a0: i64, a1: i64) -> i64 => numbers::not_a_number(a0, a1);
    fennec_int_text(a0: i64) -> i64 => strings::int_text(a0);
    fennec_u64_text(a0: i64) -> i64 => strings::u64_text(a0);
    fennec_f32_text(a0: f64) -> i64 => strings::f32_text(a0);
    fennec_u64_spec_text(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => strings::u64_spec_text(a0, a1, a2, a3);
    fennec_f32_spec_text(a0: f64, a1: i64, a2: i64, a3: i64) -> i64 => strings::f32_spec_text(a0, a1, a2, a3);
    fennec_str_repr(a0: i64) -> i64 => strings::str_repr(a0);
    fennec_str_codes(a0: i64, a1: i64) -> i64 => strings::codes(a0, a1);
    fennec_str_from_code(a0: i64) -> i64 => strings::from_code(a0);
    fennec_str_from_code_problem(a0: i64) -> i64 => strings::from_code_problem(a0);
    fennec_str_reversed(a0: i64) -> i64 => strings::reversed(a0);
    fennec_dict_find_by_value(a0: i64, a1: i64) -> i64 => dicts::find_by_value(a0, a1);
    fennec_round_places(a0: f64, a1: i64) -> f64 => checks::round_places(a0, a1);
    fennec_float_text(a0: f64) -> i64 => strings::float_text(a0);
    fennec_block_text(a0: i64) -> i64 => strings::shown_text(a0);
    fennec_own_text(a0: i64, a1: extern "C" fn(i64)) => heap::own_text(a0, a1);
    fennec_own_text_written(a0: u64, a1: u64, a2: u64) => heap::own_text_written(a0, a1, a2);
    fennec_str_code(a0: i64, a1: i64, a2: i64) -> i64 => strings::code(a0, a1, a2);
    fennec_dict_find_slice(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => dicts::find_slice(a0, a1, a2, a3);
    fennec_dict_add_slice(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => dicts::add_slice(a0, a1, a2, a3, a4);
    fennec_dict_add_new_slice(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => dicts::add_new_slice(a0, a1, a2, a3, a4);
    fennec_dict_pop(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => dicts::pop(a0, a1, a2, a3);
    fennec_dict_popped() -> i64 => dicts::popped();
    fennec_dict_find_header(a0: i64, a1: u64, a2: u64, a3: u64) -> i64 => dicts::find_header(a0, a1, a2, a3);
    fennec_dict_set_header(a0: i64, a1: u64, a2: u64, a3: u64, a4: i64) -> i64 => dicts::set_header(a0, a1, a2, a3, a4);
    fennec_dict_pop_header(a0: i64, a1: u64, a2: u64, a3: u64, a4: i64, a5: i64) -> i64 => dicts::pop_header(a0, a1, a2, a3, a4, a5);
    fennec_stop(a0: i64, a1: i64, a2: i64) => checks::stop(a0, a1, a2);
    fennec_proof_broken(a0: i64, a1: i64, a2: i64) => checks::proof_broken(a0, a1, a2);
    fennec_bytes_new(a0: i64) -> i64 => bytes::new(a0);
    fennec_bytes_constant(a0: i64) -> i64 => bytes::constant(a0);
    fennec_bytes_builder() -> i64 => bytes::builder();
    fennec_bytes_grow(a0: i64, a1: i64) -> i64 => bytes::grow(a0, a1);
    fennec_bytes_of_list(a0: i64, a1: i64, a2: i64) -> i64 => bytes::of_list(a0, a1, a2);
    fennec_bytes_join(a0: i64, a1: i64) -> i64 => bytes::join(a0, a1);
    fennec_bytes_slice(a0: i64, a1: i64, a2: i64) -> i64 => bytes::slice(a0, a1, a2);
    fennec_bytes_text_part(a0: i64, a1: i64, a2: i64) -> i64 => bytes::text_part(a0, a1, a2);
    fennec_bytes_order(a0: i64, a1: i64) -> i64 => bytes::order(a0, a1);
    fennec_bytes_contains(a0: i64, a1: i64) -> i8 => bytes::contains(a0, a1);
    fennec_bytes_hex(a0: i64, a1: i64) -> i64 => bytes::hex_text(a0, a1);
    fennec_bytes_decode(a0: i64) -> i64 => bytes::decode(a0);
    fennec_bytes_text(a0: i64) -> i64 => bytes::text(a0);
    fennec_bytes_decode_problem(a0: i64) -> i64 => bytes::decode_problem(a0);
    fennec_bytes_from_hex(a0: i64) -> i64 => bytes::from_hex_text(a0);
    fennec_bytes_from_hex_problem(a0: i64) -> i64 => bytes::from_hex_problem(a0);
    fennec_str_encode(a0: i64) -> i64 => bytes::encode(a0);
    fennec_assert_sides(a0: i64, a1: i64, a2: i64, a3: i64) => checks::assert_sides(a0, a1, a2, a3);
    fennec_list_sorted_with(a0: i64, a1: extern "C" fn(i64, i64) -> i64) -> i64 => lists::sorted_with(a0, a1);
    fennec_list_sorted_by_keys_with(a0: i64, a1: i64, a2: extern "C" fn(i64, i64) -> i64) -> i64 => lists::sorted_by_keys_with(a0, a1, a2);
    fennec_regex_problem(a0: i64) -> i64 => regex::problem(a0);
    fennec_regex_is_valid(a0: i64) -> i8 => regex::is_valid(a0);
    fennec_regex_groups(a0: i64) -> i64 => regex::groups(a0);
    fennec_regex_replace(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => regex::replace(a0, a1, a2, a3, a4);
    fennec_regex_find_all(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => regex::find_all(a0, a1, a2, a3, a4);
    fennec_regex_find(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64) -> i64 => regex::find(a0, a1, a2, a3, a4, a5);
    fennec_regex_split(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => regex::split(a0, a1, a2, a3, a4);
    fennec_list_with_room(a0: i64, a1: i64) -> i64 => lists::with_room(a0, a1);
    fennec_float_spec_text(a0: f64, a1: i64, a2: i64, a3: i64) -> i64 => strings::float_spec_text(a0, a1, a2, a3);
    fennec_int_spec_text(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => strings::int_spec_text(a0, a1, a2, a3);
    fennec_str_strip(a0: i64, a1: i64) -> i64 => strings::strip(a0, a1);
    fennec_str_join(a0: i64, a1: i64) -> i64 => strings::join(a0, a1);
    fennec_notes_add(a0: i64, a1: i64) -> i64 => notes::add(a0, a1);
    fennec_notes_list(a0: i64) -> i64 => notes::list(a0);
    fennec_notes_shown(a0: i64) -> i64 => notes::shown(a0);
    fennec_list_slice(a0: i64, a1: i64, a2: i64) -> i64 => lists::slice(a0, a1, a2);
    fennec_set_remove(a0: i64, a1: i64, a2: i8, a3: i64, a4: i64) -> i64 => dicts::set_remove(a0, a1, a2, a3, a4);
    fennec_set_remove_header(a0: i64, a1: u64, a2: u64, a3: u64, a4: i8, a5: i64, a6: i64) -> i64 => dicts::set_remove_header(a0, a1, a2, a3, a4, a5, a6);
    fennec_set_clear(a0: i64) -> i64 => dicts::set_clear(a0);
    fennec_shared_new(a0: i64) -> i64 => guarded::shared_new(a0);
    fennec_replay_lock(a0: i64, a1: i64) => guarded::replay_lock(a0, a1);
    fennec_replay_locked(a0: i64, a1: i64) => guarded::replay_locked(a0, a1);
    fennec_shared_enter(a0: i64, a1: i64) -> i64 => guarded::shared_enter(a0, a1);
    fennec_shared_leave(a0: i64, a1: i64) -> i64 => guarded::shared_leave(a0, a1);
    fennec_shared_in(a0: i64) -> i64 => guarded::shared_in(a0);
    fennec_shared_out(a0: i64, a1: i64) -> i64 => guarded::shared_out(a0, a1);
    fennec_shared_settle(a0: i64) => guarded::shared_settle(a0);
    fennec_shared_wait() => guarded::shared_wait();
    fennec_cell_new(a0: i64) -> i64 => heap::cell_new(a0);
    fennec_cell_read(a0: i64) -> i64 => heap::cell_read(a0);
    fennec_list_sorted(a0: i64, a1: i64) -> i64 => lists::sorted(a0, a1);
    fennec_list_sorted_by(a0: i64, a1: i64, a2: i64) -> i64 => lists::sorted_by(a0, a1, a2);
    fennec_list_from_value(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::from_value(a0, a1, a2, a3);
    fennec_out_of_range(a0: i64, a1: i64, a2: i64, a3: i64) => lists::out_of_range(a0, a1, a2, a3);
    fennec_alloc(a0: i64) -> i64 => heap::alloc(a0);
    fennec_new_tag() -> i64 => heap::new_tag();
    fennec_free(a0: i64) => heap::free(a0);
    fennec_retain_shared(a0: i64, a1: i64) => threads::retain_shared(a0, a1);
    fennec_release_shared(a0: i64, a1: i64) => threads::release_shared(a0, a1);
    fennec_parallel_for(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for(a0, a1, a2);
    fennec_parallel_failure() -> i64 => parallel::parallel_failure();
    fennec_parallel_map(a0: i64, a1: parallel::MapBody, a2: i64, a3: i64) -> i64 => parallel::parallel_map(a0, a1, a2, a3);
    fennec_parallel_reduce(a0: i64, a1: parallel::ReduceBody, a2: parallel::Combine, a3: i64, a4: &mut i64) -> i64 => parallel::parallel_reduce(a0, a1, a2, a3, a4);
    fennec_parallel_for_waiting(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for_waiting(a0, a1, a2);
    fennec_parallel_map_waiting(a0: i64, a1: parallel::MapBody, a2: i64, a3: i64) -> i64 => parallel::parallel_map_waiting(a0, a1, a2, a3);
    fennec_parallel_reduce_waiting(a0: i64, a1: parallel::ReduceBody, a2: parallel::Combine, a3: i64, a4: &mut i64) -> i64 => parallel::parallel_reduce_waiting(a0, a1, a2, a3, a4);
    fennec_parallel_stream(a0: i64, a1: parallel::MapBody, a2: i64, a3: i64) -> i64 => parallel::parallel_stream(a0, a1, a2, a3);
    fennec_parallel_stream_waiting(a0: i64, a1: parallel::MapBody, a2: i64, a3: i64) -> i64 => parallel::parallel_stream_waiting(a0, a1, a2, a3);
    fennec_stream_next(a0: i64, a1: &mut i64) -> i64 => parallel::stream_next(a0, a1);
    fennec_stream_end(a0: i64) => parallel::stream_end(a0);
    fennec_net_serve(a0: i64, a1: serve::ConnBody, a2: i64) -> i64 => serve::net_serve(a0, a1, a2);
    fennec_net_serve_failure() -> i64 => serve::net_serve_failure();
    fennec_task_tick() -> i64 => serve::task_tick();
    fennec_free_text(a0: i64) => heap::free_text(a0);
    fennec_set_abandon(a0: i64, a1: extern "C" fn(i64)) => heap::set_abandon(a0, a1);
    fennec_unshare(a0: i64) -> i64 => heap::unshare(a0);
    fennec_box_to_value(a0: i64) -> i64 => heap::to_value(a0);
    fennec_box_from_value(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => heap::from_value(a0, a1, a2, a3);
    fennec_box_equal(a0: i64, a1: i64) -> i8 => heap::equal(a0, a1);
    fennec_str_from_value(a0: i64, a1: i64, a2: i64) -> i64 => strings::from_value(a0, a1, a2);
    fennec_str_concat(a0: i64, a1: i64) -> i64 => strings::concat(a0, a1);
    fennec_str_append(a0: i64, a1: i64) -> i64 => strings::append(a0, a1);
    fennec_str_upper(a0: i64) -> i64 => strings::upper(a0);
    fennec_str_lower(a0: i64) -> i64 => strings::lower(a0);
    fennec_str_replace(a0: i64, a1: i64, a2: i64) -> i64 => strings::replace(a0, a1, a2);
    fennec_str_find(a0: i64, a1: i64) -> i64 => strings::find(a0, a1);
    fennec_str_split(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => strings::split(a0, a1, a2, a3);
    fennec_str_with_room(a0: i64) -> i64 => strings::with_room(a0);
    fennec_text_block(a0: u64, a1: u64, a2: u64) -> i64 => text::block(a0, a1, a2);
    fennec_text_contains(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i8 => text::contains(a0, a1, a2, a3, a4, a5);
    fennec_text_of(a0: i64) -> text::Text => text::of(a0);
    fennec_int_header(a0: i64) -> text::Text => text::int_header(a0);
    fennec_code_header(a0: i64) -> text::Text => text::code_header(a0);
    fennec_float_header(a0: f64) -> text::Text => text::float_header(a0);
    fennec_u64_header(a0: i64) -> text::Text => text::u64_header(a0);
    fennec_f32_header(a0: f64) -> text::Text => text::f32_header(a0);
    fennec_str_char_at(a0: i64, a1: i64) -> i64 => strings::char_at(a0, a1);
    fennec_dict_hash_seed() -> u64 => dicts::hash_seed();
    fennec_dict_strong_step(a0: u64, a1: u64) -> u64 => dicts::strong_step(a0, a1);
    fennec_dict_new(a0: i64, a1: i64) -> i64 => dicts::new(a0, a1);
    fennec_dict_set(a0: i64, a1: i64, a2: i64) -> i64 => dicts::set(a0, a1, a2);
    fennec_dict_missing(a0: i64, a1: i64, a2: i8, a3: i64, a4: i64) => dicts::missing(a0, a1, a2, a3, a4);
    fennec_dict_missing_header(a0: i64, a1: u64, a2: u64, a3: u64, a4: i8, a5: i64, a6: i64) => dicts::missing_header(a0, a1, a2, a3, a4, a5, a6);
    fennec_str_index(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => strings::index(a0, a1, a2, a3);
    fennec_str_index_text(a0: i64, a1: i64, a2: i64, a3: i64) -> text::Text => strings::index_text(a0, a1, a2, a3);
    fennec_str_slice(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => strings::slice(a0, a1, a2, a3, a4);
    fennec_str_part(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => strings::part(a0, a1, a2, a3, a4);
    fennec_str_compare(a0: i64, a1: i64) -> i64 => strings::compare(a0, a1);
    fennec_str_contains(a0: i64, a1: i64) -> i8 => strings::contains(a0, a1);
    fennec_str_print(a0: i64) => strings::print(a0);
    fennec_capability(a0: i64) -> i64 => temps::capability(a0);
    fennec_capability_block(a0: i64) -> i64 => capabilities::block(a0);
    fennec_net_call(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::net_call(a0, a1, a2, a3, a4);
    fennec_native_capability(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::native_call(a0, a1, a2, a3, a4);
    fennec_crypto_call(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => capabilities::crypto_call(a0, a1, a2, a3);
    fennec_use_tls() => capabilities::use_tls();
    fennec_constant_kept(a0: i64) => heap::constant_kept(a0);
    fennec_on_task_resume(a0: extern "C" fn(i64)) => parallel::on_task_resume(a0);
    fennec_clock_now() -> i64 => capabilities::clock_now();
    fennec_clock_time() -> i64 => capabilities::clock_time();
    fennec_clock_sleep(a0: i64) => capabilities::clock_sleep(a0);
    fennec_args_len() -> i64 => capabilities::args_len();
    fennec_args_program() -> i64 => capabilities::args_program();
    fennec_args_item(a0: i64, a1: i64, a2: i64) -> i64 => capabilities::args_item(a0, a1, a2);
    fennec_args_list(a0: i64) -> i64 => capabilities::args_list(a0);
    fennec_capability_from_value(a0: i64, a1: i64, a2: i64) -> i64 => capabilities::from_value(a0, a1, a2);
    fennec_aot_image(a0: i64) => aot::image(a0);
    fennec_aot_failed_flag() -> i64 => aot::failed_flag();
    fennec_aot_stack_limit() -> i64 => aot::stack_limit();
    fennec_aot_ascii() -> i64 => aot::ascii();
    fennec_aot_literal(a0: i64) -> i64 => aot::literal(a0);
    fennec_aot_tests_start() => aot::tests_start();
    fennec_aot_next_test() -> i64 => aot::next_test_number();
    fennec_aot_test_ran(a0: i64) => aot::test_ran(a0);
    fennec_aot_test_done(a0: i64) => aot::test_done(a0);
    fennec_trace_at(a0: i64) => aot::trace_at(a0);
    fennec_trace_allocated(a0: i64) => aot::trace_allocated(a0);
    fennec_counting() -> i64 => aot::counting();
    fennec_aot_test_world(a0: i64) => aot::test_world(a0);
    fennec_aot_attempt_failed() => aot::attempt_failed();
    fennec_aot_attempt_passed() -> i8 => aot::attempt_passed();
    fennec_aot_failed_for(a0: i64) => aot::failed_for(a0);
}

/// Runs a compiled executable's program: its `main` calls this.
#[unsafe(no_mangle)]
pub extern "C" fn fennec_aot_run(entry: extern "C" fn(i64), length: i64) -> i32 {
    #[cfg(windows)]
    fine_timer();
    aot::run(entry, length, stop)
}

/// Asks Windows to tick its timer every millisecond while the program runs, as Go's runtime does.
/// A task's wait gives up when the poll it parks on times out, which Windows ends only on a tick,
/// every 15.6 ms unless asked: without this, a task's `clock.sleep(1ms)` takes about 15 ms. Windows
/// puts the timer back as the process ends, so nothing has to ask it to.
#[cfg(windows)]
fn fine_timer() {
    // Linked by name with no import library, which the libraries Fennec links programs with lack.
    #[link(name = "winmm", kind = "raw-dylib")]
    unsafe extern "system" {
        fn timeBeginPeriod(period: u32) -> u32;
    }
    // SAFETY: `timeBeginPeriod` takes any period, and only fails, harmlessly, for one the system
    // can't give.
    unsafe { timeBeginPeriod(1) };
}

/// Ends the process now, for a failure in cleanup that nothing can pass on.
fn stop(code: i32) {
    unsafe extern "C" {
        fn exit(code: i32) -> !;
    }
    // SAFETY: the C library's `exit` takes any code, and this is the last thing the run does.
    unsafe { exit(code) }
}
