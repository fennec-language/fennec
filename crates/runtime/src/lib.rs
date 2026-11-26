//! The runtime library compiled executables link against: every function their code
//! calls, exported under the name it calls it by. `fennec build`
//! links an executable's object file with this library and the system's C libraries.
//!
//! The functions are `fennec_rt::embed`'s; this crate gives them names a linker can find,
//! and asks the system for what only it can give: Windows's fine timer, and a process's end.
//! It also has mimalloc call it when it runs out of memory, and on Linux with glibc keeps glibc's
//! own allocator to one arena. Those are the things here Rust calls unsafe.
#![expect(
    unsafe_code,
    reason = "exporting the runtime's functions under fixed names, calling the system to end the process, giving mimalloc its error handler, and keeping glibc's allocator to one arena"
)]

use fennec_rt::embed::guarded::rollback;
use fennec_rt::embed::{
    aot, bytes, capabilities, channels, checks, cleanup, conn_calls, dicts, eval, guarded, heap, io, lists,
    notes, numbers, parallel, regex, serve, strings, temps, text, threads,
};

/// The build of Fennec this library is from, after a marker, which `fennec` reads out of the
/// library's file to tell one built from other source. Kept by `#[used]`, since nothing calls it.
#[used]
static BUILD_STAMP: &str = concat!("fennec-runtime-build:", env!("FENNEC_BUILD"));

/// The exported functions that never fail: none of them sets the flag `fennec_aot_failed_flag`
/// gives the address of. Generated code has to check that flag after calling any other, and may
/// skip the check after one of these. A function missing from here costs only a check. One here
/// that can fail would let a program go on past a failure, so each was read through to what it
/// calls, and in a debug build the export stops the program if one sets the flag anyway.
///
/// Nothing exported unwinds: a panic crossing `extern "C"` aborts the program.
pub const NEVER_FAIL: &[&str] = &[
    // Float math, through `libm` or plain arithmetic.
    "fennec_sin",
    "fennec_cos",
    "fennec_tan",
    "fennec_acos",
    "fennec_sinf",
    "fennec_cosf",
    "fennec_acosf",
    "fennec_exp",
    "fennec_log",
    "fennec_float_power",
    "fennec_round_places",
    // A text's header, made in place, or for a long one in a block the heap gives, which can't
    // fail: running out of memory aborts.
    "fennec_int_header",
    "fennec_u64_header",
    "fennec_f32_header",
    "fennec_float_header",
    "fennec_code_header",
    // A block freed whose type can't hold a generator that closes something when it's let go
    // partway, as the compiler works out: it runs no program code. Any generator it finds others
    // let go of waits for `fennec_free`, which can fail. A string's block holds nothing at all.
    "fennec_free_plain",
    "fennec_free_text",
    // Temporaries pushed, copied and read. Dropping back to a mark lets go of blocks, which only
    // queues the generators among them to be closed: `fennec_free` runs those, so it can fail.
    "fennec_mark",
    "fennec_truncate",
    "fennec_keep",
    "fennec_int",
    "fennec_float",
    "fennec_bool",
    "fennec_none",
    "fennec_unit",
    "fennec_str",
    "fennec_copy",
    "fennec_is_err",
    "fennec_is_none",
    "fennec_passes",
    // The locks this thread notes it holds, a list of words.
    "fennec_lock_note",
    "fennec_lock_forget",
    // The dict hash's seed and step, from keys made once.
    "fennec_dict_hash_seed",
    "fennec_dict_strong_step",
    // A fresh tag, from a counter.
    "fennec_new_tag",
    "fennec_tag_count",
    "fennec_tag_run",
    // A block made, which native code uses at once with no check: each gives back a block or stops
    // the program as a bug in Fennec (`embed::made`), never 0. They run under `with`, which sets no
    // flag, and what they reach makes, copies and lets go of blocks, which can't fail either.
    "fennec_alloc",
    "fennec_unshare",
    "fennec_block_text",
    "fennec_bytes_builder",
    "fennec_bytes_embedded",
    "fennec_bytes_embedded_filled",
    "fennec_bytes_grow",
    "fennec_str_encode",
    "fennec_bytes_decode_problem",
    "fennec_dict_new",
    "fennec_dict_set",
    "fennec_dict_set_header",
    "fennec_dict_add_new_slice",
    "fennec_set_clear",
    "fennec_set_remove",
    "fennec_set_remove_header",
    "fennec_list_new",
    "fennec_list_constant",
    "fennec_list_with_room",
    "fennec_list_grow",
    "fennec_list_reserve",
    "fennec_list_unshare",
    "fennec_list_pop_unshare",
    "fennec_list_extend",
    "fennec_list_truncate",
    "fennec_list_slice",
    "fennec_list_sorted",
    "fennec_list_sorted_by",
    "fennec_list_sort",
    "fennec_list_sort_by",
    "fennec_list_reverse",
    "fennec_str_with_room",
    "fennec_str_concat",
    "fennec_str_append",
    "fennec_str_lower",
    "fennec_str_upper",
    "fennec_str_replace",
    "fennec_str_reversed",
    "fennec_str_strip",
    "fennec_str_join",
    "fennec_str_codes",
    "fennec_str_char_at",
    "fennec_str_repr",
    "fennec_int_text",
    "fennec_u64_text",
    "fennec_f32_text",
    "fennec_float_text",
    "fennec_text_block",
    "fennec_str_from_code_problem",
    "fennec_args_list",
    "fennec_notes_shown",
    "fennec_not_a_number",
    "fennec_regex_problem",
    // Questions about a pattern, answered under `with`, which sets no flag: whether it's one, and
    // how many groups it has, -1 for one that isn't.
    "fennec_regex_is_valid",
    "fennec_regex_groups",
    // A block, or 0 for none, which native code tests for as an answer: a code that isn't a
    // character, bytes that aren't UTF-8, a key that isn't there. None sets the flag.
    "fennec_str_from_code",
    "fennec_bytes_decode",
    "fennec_dict_find_by_value",
    "fennec_dict_find_slice",
    // A text's header, the empty string's for anything that isn't a text.
    "fennec_text_of",
    // A float read from a text's header, or 0 for text that isn't one, which native code tests
    // for as an answer. It runs under `with`, which sets no flag, and only reads the text's bytes
    // and parses them.
    "fennec_parse_float_header",
    "fennec_parse_f32_header",
    // Where a part first is in a slice of a text, read where the text is, or -1 for nowhere, which
    // native code tests for as an answer. It runs under `with`, which sets no flag, and only reads
    // the two texts' bytes.
    "fennec_str_find_range",
    // Only says where a buffer is.
    "fennec_dict_value_words",
    // A value's words taken from that buffer into a dict that holds its values in its entries:
    // `fennec_dict_set` and `fennec_dict_set_header` above for such a dict. Each runs under
    // `with`, which sets no flag, and stops the program as a bug for a dict of one-word values.
    "fennec_dict_set_words",
    "fennec_dict_set_words_header",
    // Tells a lock's log of the value in one entry before it's changed where it is. It runs
    // under `with`, gives nothing back, and saves nothing for a dict no log marked.
    "fennec_lock_save_entry",
    // One atomic add to the count of native blocks made and not freed.
    "fennec_native_live",
    // Bytes made from bytes. Each runs under `with`, which sets no flag, and none has a wrong
    // argument to refuse:
    // bytes written out in hex, with the separator given, if any, between them, give a text always;
    "fennec_bytes_hex",
    // a slice holds both bounds to what's there, as a list's slice does, down to nothing;
    "fennec_bytes_slice",
    // a part read as text holds its places to what's there too, and bytes that aren't UTF-8 give
    // the empty text, which what calls it tests for as an answer;
    "fennec_bytes_text_part",
    // a join only copies the two it's given;
    "fennec_bytes_join",
    // and bytes read as text whatever they hold give a text always, with what isn't UTF-8
    // replaced: `fennec_bytes_decode` above without its one refusal.
    "fennec_bytes_text",
    // Says TLS is wanted before a request, which only sets the three functions that put TLS over a
    // connection, each once.
    "fennec_use_tls",
    // Where the next separator starts from a byte, in a text and a separator given as headers
    // and only read: under `with`, which sets no flag, with nothing to refuse, as a place past
    // the text's end gives its length.
    "fennec_str_split_at_header",
];

/// Which export takes which names, and the function that looks one up: in `fennec-rt`, where the
/// compiler reads them too.
pub use fennec_rt::{MODULE_ENTRIES, module_entry};

/// Whether `symbol` is in [`NEVER_FAIL`], worked out as each export is compiled.
const fn never_fails(symbol: &str) -> bool {
    listed(NEVER_FAIL, symbol.as_bytes())
}

/// Looked through one name at a time, in a loop rather than by recursion: the list outgrew how
/// deep the compiler lets a constant's calls go once it held 100 names.
const fn listed(names: &[&str], symbol: &[u8]) -> bool {
    let mut rest = names;
    while let [first, more @ ..] = rest {
        if same(first.as_bytes(), symbol) {
            return true;
        }
        rest = more;
    }
    false
}

const fn same(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let (mut a, mut b) = (a, b);
    while let ([x, after_a @ ..], [y, after_b @ ..]) = (a, b) {
        if *x != *y {
            return false;
        }
        (a, b) = (after_a, after_b);
    }
    true
}

/// What an executable's program allocates through: its big blocks, and what the runtime
/// allocates for itself while it works. With the `sanitize` feature it's the system's own,
/// for an address sanitizer to watch.
#[cfg(not(feature = "sanitize"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[cfg(not(feature = "sanitize"))]
unsafe extern "C" {
    /// mimalloc's: sets the function it calls on an error, as just before it gives back null for an
    /// allocation it couldn't make, with the error's code and `argument`. Declared by hand from
    /// mimalloc.h's `mi_register_error(mi_error_fun* fun, void* arg)`, so nothing checks it against
    /// the header: read the header again whenever the `mimalloc` pin moves.
    fn mi_register_error(
        handler: Option<extern "C" fn(std::ffi::c_int, *mut std::ffi::c_void)>,
        argument: *mut std::ffi::c_void,
    );
}

/// Has running out of memory said in Fennec's words. Rust's own way, for an allocation that comes
/// back null, is "memory allocation of N bytes failed", a note about `RUST_BACKTRACE`, and an abort
/// that dumps core, with what the program printed lost (runtime exhaustion audit, 2026-11-20).
/// mimalloc calls its error handler just before it gives back null, so the program ends there and
/// nothing is added to the allocations that succeed: a wrapper checking each for null cost
/// `grep_tool` 2% of its instructions.
#[cfg(not(feature = "sanitize"))]
fn say_when_out_of_memory() {
    // SAFETY: `ran_out` is a function of the type mimalloc calls, and reads nothing through its
    // argument, which is null.
    unsafe { mi_register_error(Some(ran_out), std::ptr::null_mut()) }
}

/// mimalloc's error handler. Out of memory, or asked for more than it could ever give, ends the
/// program saying so (`aot::out_of_memory`). Anything else, as freeing what it never gave, is left
/// to mimalloc, which goes on.
#[cfg(not(feature = "sanitize"))]
extern "C" fn ran_out(code: std::ffi::c_int, _: *mut std::ffi::c_void) {
    if code == ENOMEM || code == EOVERFLOW {
        aot::out_of_memory();
    }
}

/// The C library's code for running out of memory, the same on every system Fennec runs on.
#[cfg(not(feature = "sanitize"))]
const ENOMEM: std::ffi::c_int = 12;

/// The C library's code for a request too large to ever give, which differs between systems.
#[cfg(all(not(feature = "sanitize"), target_vendor = "apple"))]
const EOVERFLOW: std::ffi::c_int = 84;
#[cfg(all(not(feature = "sanitize"), windows))]
const EOVERFLOW: std::ffi::c_int = 132;
#[cfg(all(not(feature = "sanitize"), not(target_vendor = "apple"), not(windows)))]
const EOVERFLOW: std::ffi::c_int = 75;

/// Exports each function under a symbol: `symbol(params) -> result => function(args);`.
macro_rules! export {
    ($($(#[$($meta:tt)*])* $symbol:ident($($arg:ident: $ty:ty),*) $(-> $ret:ty)? => $module:ident::$function:ident($($pass:ident),*);)*) => {
        $(
            $(#[$($meta)*])*
            #[unsafe(no_mangle)]
            pub extern "C" fn $symbol($($arg: $ty),*) $(-> $ret)? {
                // In a debug build, one listed as never failing stops the program if it does.
                if cfg!(debug_assertions) && const { never_fails(stringify!($symbol)) } {
                    let before = fennec_rt::embed::failure_flag_set();
                    let result = $module::$function($($pass),*);
                    if !before && fennec_rt::embed::failure_flag_set() {
                        fennec_rt::embed::failed_though_never_fails(stringify!($symbol));
                    }
                    return result;
                }
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
    fennec_main_result_shown(a0: i64, a1: i64, a2: i64, a3: i64) => temps::main_result_shown(a0, a1, a2, a3);
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
    fennec_crash_note(a0: i64, a1: i64) => cleanup::crash_note(a0, a1);
    fennec_channel_send(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => channels::channel_send(a0, a1, a2, a3);
    fennec_channel_receive(a0: i64, a1: i64, a2: &mut i64) -> i64 => channels::channel_receive(a0, a1, a2);
    fennec_channel_send_words(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => channels::channel_send_words(a0, a1, a2, a3, a4);
    fennec_channel_receive_words(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => channels::channel_receive_words(a0, a1, a2, a3);
    fennec_channel_close(a0: i64) => channels::channel_close(a0);
    fennec_channel_free(a0: i64) => channels::channel_free(a0);
    fennec_launch_end(a0: i64, a1: i64, a2: i64, a3: &mut i64) -> i64 => serve::launch_end(a0, a1, a2, a3);
    fennec_task_failed(a0: i64) => serve::task_failed(a0);
    fennec_lock_begin(a0: i64) -> i64 => rollback::begin(a0);
    fennec_lock_save(a0: i64, a1: i64, a2: i64, a3: i64) => rollback::save(a0, a1, a2, a3);
    fennec_lock_save_text(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) => rollback::save_text(a0, a1, a2, a3, a4);
    fennec_lock_save_ref(a0: i64, a1: i64, a2: i64) => rollback::save_ref(a0, a1, a2);
    fennec_lock_save_entry(a0: i64, a1: i64) => dicts::save_entry(a0, a1);
    fennec_lock_to_change(a0: i64) -> i64 => rollback::to_change(a0);
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
    fennec_sinf(a0: f64) -> f64 => checks::sinf(a0);
    fennec_cosf(a0: f64) -> f64 => checks::cosf(a0);
    fennec_acosf(a0: f64) -> f64 => checks::acosf(a0);
    fennec_exp(a0: f64) -> f64 => checks::exp(a0);
    fennec_log(a0: f64) -> f64 => checks::log(a0);
    fennec_float_power(a0: f64, a1: f64) -> f64 => lists::float_power(a0, a1);
    fennec_int_power(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::int_power(a0, a1, a2, a3);
    fennec_list_new(a0: i64, a1: i64) -> i64 => lists::new(a0, a1);
    fennec_list_constant(a0: i64, a1: i64) -> i64 => lists::constant(a0, a1);
    fennec_list_grow(a0: i64) -> i64 => lists::grow(a0);
    fennec_list_pop_unshare(a0: i64) -> i64 => lists::pop_unshare(a0);
    fennec_list_item_unshare(a0: i64, a1: i64) -> i64 => lists::item_unshare(a0, a1);
    fennec_list_reserve(a0: i64, a1: i64) -> i64 => lists::reserve(a0, a1);
    fennec_list_truncate(a0: i64, a1: i64) -> i64 => lists::truncate(a0, a1);
    fennec_list_unshare(a0: i64) -> i64 => lists::unshare(a0);
    fennec_list_clear(a0: i64) => lists::clear(a0);
    fennec_list_remove_at(a0: i64, a1: i64) => lists::remove_at(a0, a1);
    fennec_list_insert(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::insert(a0, a1, a2, a3);
    fennec_buffer_insert(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::insert_buffer(a0, a1, a2, a3);
    fennec_list_extend(a0: i64, a1: i64) -> i64 => lists::extend(a0, a1);
    fennec_list_contains(a0: i64, a1: i64) -> i8 => lists::contains(a0, a1);
    fennec_pop_empty(a0: i64, a1: i64) => lists::pop_empty(a0, a1);
    fennec_buffer_pop_empty(a0: i64, a1: i64) => lists::pop_empty_buffer(a0, a1);
    fennec_parse_int(a0: i64, a1: &mut i64) -> i64 => numbers::parse_int(a0, a1);
    fennec_parse_u64(a0: i64, a1: &mut i64) -> i64 => numbers::parse_u64(a0, a1);
    fennec_parse_float(a0: i64, a1: &mut i64) -> i64 => numbers::parse_float(a0, a1);
    fennec_parse_float_header(a0: u64, a1: u64, a2: u64, a3: &mut i64) -> i64 => numbers::parse_float_header(a0, a1, a2, a3);
    fennec_parse_f32(a0: i64, a1: &mut i64) -> i64 => numbers::parse_f32(a0, a1);
    fennec_parse_f32_header(a0: u64, a1: u64, a2: u64, a3: &mut i64) -> i64 => numbers::parse_f32_header(a0, a1, a2, a3);
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
    fennec_dict_value_words() -> i64 => dicts::value_buffer();
    fennec_dict_set_words(a0: i64, a1: i64) -> i64 => dicts::set_in_place(a0, a1);
    fennec_dict_set_words_header(a0: i64, a1: u64, a2: u64, a3: u64) -> i64 => dicts::set_in_place_header(a0, a1, a2, a3);
    fennec_dict_pop_words(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => dicts::pop_in_place(a0, a1, a2, a3);
    fennec_dict_pop_words_header(a0: i64, a1: u64, a2: u64, a3: u64, a4: i64, a5: i64) -> i64 => dicts::pop_in_place_header(a0, a1, a2, a3, a4, a5);
    fennec_stop(a0: i64, a1: i64, a2: i64) => checks::stop(a0, a1, a2);
    fennec_proof_broken(a0: i64, a1: i64, a2: i64) => checks::proof_broken(a0, a1, a2);
    fennec_bytes_new(a0: i64) -> i64 => bytes::new(a0);
    fennec_bytes_embedded(a0: i64, a1: i64) -> i64 => bytes::embedded(a0, a1);
    fennec_bytes_embedded_filled(a0: i64) => bytes::embedded_filled(a0);
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
    fennec_assert_said(a0: i64, a1: i64, a2: i64) => checks::assert_said(a0, a1, a2);
    fennec_list_sorted_with(a0: i64, a1: extern "C" fn(i64, i64) -> i64) -> i64 => lists::sorted_with(a0, a1);
    fennec_list_sorted_by_keys_with(a0: i64, a1: i64, a2: extern "C" fn(i64, i64) -> i64) -> i64 => lists::sorted_by_keys_with(a0, a1, a2);
    fennec_list_sort_with(a0: i64, a1: extern "C" fn(i64, i64) -> i64) => lists::sort_with(a0, a1);
    fennec_list_sort_by_keys_with(a0: i64, a1: i64, a2: extern "C" fn(i64, i64) -> i64) => lists::sort_by_keys_with(a0, a1, a2);
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
    fennec_set_remove(a0: i64, a1: i64) -> i64 => dicts::set_remove(a0, a1);
    fennec_set_remove_header(a0: i64, a1: u64, a2: u64, a3: u64) -> i64 => dicts::set_remove_header(a0, a1, a2, a3);
    fennec_set_clear(a0: i64) -> i64 => dicts::set_clear(a0);
    fennec_shared_new(a0: i64) -> i64 => guarded::shared_new(a0);
    fennec_replay_lock(a0: i64, a1: i64) => guarded::replay_lock(a0, a1);
    fennec_replay_locked(a0: i64, a1: i64) => guarded::replay_locked(a0, a1);
    fennec_replay_unlocked(a0: i64, a1: i64) => guarded::replay_unlocked(a0, a1);
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
    fennec_list_sort(a0: i64, a1: i64) => lists::sort(a0, a1);
    fennec_list_reverse(a0: i64) => lists::reverse(a0);
    fennec_list_sort_by(a0: i64, a1: i64, a2: i64) => lists::sort_by(a0, a1, a2);
    fennec_list_from_value(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => lists::from_value(a0, a1, a2, a3);
    fennec_out_of_range(a0: i64, a1: i64, a2: i64, a3: i64) => lists::out_of_range(a0, a1, a2, a3);
    fennec_alloc(a0: i64) -> i64 => heap::alloc(a0);
    fennec_new_tag() -> i64 => heap::new_tag();
    fennec_tag_count() -> i64 => heap::tag_count();
    fennec_tag_run() -> i64 => heap::tag_run();
    fennec_free(a0: i64) => heap::free(a0);
    fennec_free_plain(a0: i64) => heap::free_plain(a0);
    fennec_close_waiting() => heap::close_waiting();
    fennec_retain_shared(a0: i64, a1: i64) => threads::retain_shared(a0, a1);
    fennec_release_shared(a0: i64, a1: i64) => threads::release_shared(a0, a1);
    fennec_parallel_for(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for(a0, a1, a2);
    fennec_parallel_failure() -> i64 => parallel::parallel_failure();
    fennec_parallel_map(a0: i64, a1: parallel::MapBody, a2: i64, a3: i64) -> i64 => parallel::parallel_map(a0, a1, a2, a3);
    fennec_parallel_reduce(a0: i64, a1: parallel::ReduceBody, a2: parallel::Combine, a3: i64, a4: &mut i64) -> i64 => parallel::parallel_reduce(a0, a1, a2, a3, a4);
    fennec_parallel_for_heavy(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for_heavy(a0, a1, a2);
    fennec_parallel_map_heavy(a0: i64, a1: parallel::MapBody, a2: i64, a3: i64) -> i64 => parallel::parallel_map_heavy(a0, a1, a2, a3);
    fennec_parallel_for_waiting(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for_waiting(a0, a1, a2);
    fennec_parallel_for_in_place(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for_in_place(a0, a1, a2);
    fennec_parallel_for_in_place_heavy(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for_in_place_heavy(a0, a1, a2);
    fennec_parallel_for_in_place_waiting(a0: i64, a1: parallel::Body, a2: i64) -> i64 => parallel::parallel_for_in_place_waiting(a0, a1, a2);
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
    fennec_str_split_header(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: i64, a7: i64) -> i64 => strings::split_header(a0, a1, a2, a3, a4, a5, a6, a7);
    fennec_str_with_room(a0: i64) -> i64 => strings::with_room(a0);
    fennec_text_block(a0: u64, a1: u64, a2: u64) -> i64 => text::block(a0, a1, a2);
    fennec_text_contains(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i8 => text::contains(a0, a1, a2, a3, a4, a5);
    fennec_str_append_header(a0: i64, a1: u64, a2: u64, a3: u64) -> i64 => text::append_header(a0, a1, a2, a3);
    fennec_str_find_header(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 => text::find_header(a0, a1, a2, a3, a4, a5);
    fennec_str_split_at_header(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64, a6: i64) -> i64 => text::split_at_header(a0, a1, a2, a3, a4, a5, a6);
    fennec_text_starts_with(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i8 => text::starts_with(a0, a1, a2, a3, a4, a5);
    fennec_str_split_range(a0: u64, a1: u64, a2: u64, a3: i64, a4: i64, a5: u64, a6: u64, a7: u64, a8: i64, a9: i64) -> i64 => text::split_range(a0, a1, a2, a3, a4, a5, a6, a7, a8, a9);
    fennec_str_find_range(a0: u64, a1: u64, a2: u64, a3: i64, a4: i64, a5: u64, a6: u64, a7: u64) -> i64 => text::find_range(a0, a1, a2, a3, a4, a5, a6, a7);
    fennec_text_starts_with_range(a0: u64, a1: u64, a2: u64, a3: i64, a4: i64, a5: u64, a6: u64, a7: u64) -> i8 => text::starts_with_range(a0, a1, a2, a3, a4, a5, a6, a7);
    fennec_text_equals_range(a0: u64, a1: u64, a2: u64, a3: i64, a4: i64, a5: u64, a6: u64, a7: u64) -> i8 => text::equals_range(a0, a1, a2, a3, a4, a5, a6, a7);
    fennec_text_ends_with(a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i8 => text::ends_with(a0, a1, a2, a3, a4, a5);
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
    fennec_dict_set_found(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => dicts::set_found(a0, a1, a2, a3);
    fennec_proof_put_in_place(a0: i64, a1: i64) -> i64 => dicts::put_in_place_holds(a0, a1);
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
    fennec_signals_listen() => capabilities::signals_listen();
    fennec_net_call(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::net_call(a0, a1, a2, a3, a4);
    fennec_native_capability(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::native_call(a0, a1, a2, a3, a4);
    fennec_crypto_call(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => capabilities::crypto_call(a0, a1, a2, a3);
    fennec_fs_capability(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::fs_call(a0, a1, a2, a3, a4);
    fennec_sqlite_capability(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::sqlite_call(a0, a1, a2, a3, a4);
    fennec_net_capability(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::net_capability_call(a0, a1, a2, a3, a4);
    fennec_process_capability(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::process_call(a0, a1, a2, a3, a4);
    fennec_io_capability(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::io_call(a0, a1, a2, a3, a4);
    fennec_database_call(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::database_call(a0, a1, a2, a3, a4);
    fennec_file_call(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::file_call(a0, a1, a2, a3, a4);
    fennec_socket_call(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => capabilities::socket_call(a0, a1, a2, a3, a4);
    fennec_conn_write(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => conn_calls::conn_write(a0, a1, a2, a3, a4);
    fennec_conn_reads_within(a0: i64, a1: i64) -> i64 => conn_calls::conn_reads_within(a0, a1);
    fennec_conn_draining(a0: i64) -> i64 => conn_calls::conn_draining(a0);
    fennec_conn_room_for_body(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => conn_calls::conn_room_for_body(a0, a1, a2, a3);
    fennec_conn_body(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64) -> i64 => conn_calls::conn_body(a0, a1, a2, a3, a4);
    fennec_conn_read_head(a0: i64, a1: i64, a2: i64, a3: i64, a4: i64, a5: i64, a6: &mut i64, a7: i64, a8: i64) -> i64 => conn_calls::conn_read_head(a0, a1, a2, a3, a4, a5, a6, a7, a8);
    fennec_compress_call(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => capabilities::compress_call(a0, a1, a2, a3);
    fennec_unicode_call(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => capabilities::unicode_call(a0, a1, a2, a3);
    fennec_crypto_alone_call(a0: i64, a1: i64, a2: i64, a3: i64) -> i64 => capabilities::crypto_alone_call(a0, a1, a2, a3);
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
    fennec_native_live(a0: i64) => aot::native_live(a0);
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
    #[cfg(not(feature = "sanitize"))]
    say_when_out_of_memory();
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    one_glibc_arena();
    let code = aot::run(entry, length, stop);
    #[cfg(windows)]
    end_process(code);
    #[cfg(not(windows))]
    code
}

/// Keeps glibc's allocator to its one main arena, before any worker starts. mimalloc doesn't take
/// glibc's place, so what glibc allocates for itself comes from glibc's allocator: as a new
/// thread's first thread-local value with a destructor registers it, with `calloc`
/// (`__cxa_thread_atexit_impl`). By default glibc gives each new thread an arena of its own, and
/// making one reserves 64 MiB of address space at once. A program short of address space, as one
/// under `ulimit -v` running out of memory in several ranges at once is, then can't make it: glibc
/// aborts with "failed to register TLS destructor: out of memory" and a core dump, before the
/// runtime can say it ran out. That was one run in a thousand of
/// `llvm::ranges_running_out_of_memory_at_once_say_so_once` under load, on Linux Arm in CI and in
/// the VM. The one main arena grows a little at a time, and glibc's own allocations are few and
/// small, so sharing it costs nothing a program would notice.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn one_glibc_arena() {
    unsafe extern "C" {
        /// glibc's, from malloc.h: sets one of its allocator's parameters, giving back 1 when it
        /// did and 0 when it didn't.
        fn mallopt(parameter: std::ffi::c_int, value: std::ffi::c_int) -> std::ffi::c_int;
    }
    /// malloc.h's `M_ARENA_MAX`: the most arenas glibc's allocator makes.
    const M_ARENA_MAX: std::ffi::c_int = -8;
    // SAFETY: `mallopt` takes any parameter and value, and changes only how glibc's allocator
    // chooses arenas for allocations made after it, from any thread. Nothing else runs yet: this is
    // before the runtime starts a thread. If it isn't taken, glibc goes on as it was.
    unsafe { mallopt(M_ARENA_MAX, 1) };
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
    #[cfg(windows)]
    end_process(code);
    #[cfg(not(windows))]
    {
        unsafe extern "C" {
            fn exit(code: i32) -> !;
        }
        // SAFETY: the C library's `exit` takes any code, and this is the last thing the run does.
        unsafe { exit(code) }
    }
}

/// Ends the process at once with `code`, on Windows, once what the run printed has been written:
/// every thread stops where it is, and none of the C runtime's `exit` runs.
///
/// A program used to end by returning from its `main` into that `exit`, which tears the C runtime
/// down, and on Windows loads a library to look up how to, while the program's workers go on. A
/// worker still starting its share of the program then crashed in the allocator, reading the
/// thread's heap as null: 0xC0000005 after the right output, in about one Windows CI run of the
/// replay tests in eight, every crash the same under a debugger (CI run 43932818597). The runtime
/// leaves its heap for the process's end to give back, so nothing that teardown does is needed.
/// Linux and macOS go on ending through `exit`.
#[cfg(windows)]
fn end_process(code: i32) -> ! {
    // Linked by name with no import library, as `fine_timer`'s function is.
    #[link(name = "kernel32", kind = "raw-dylib")]
    unsafe extern "system" {
        fn GetCurrentProcess() -> isize;
        fn TerminateProcess(process: isize, code: u32) -> i32;
    }
    // SAFETY: `GetCurrentProcess` gives a handle to this process that needs no closing, and
    // `TerminateProcess` takes it and any code; ending this process, it doesn't come back.
    unsafe {
        TerminateProcess(GetCurrentProcess(), code.cast_unsigned());
    }
    std::process::abort()
}

#[cfg(test)]
mod tests {
    /// A block of a page, aligned to one, for asking mimalloc for memory with a large alignment.
    #[cfg(not(feature = "sanitize"))]
    #[repr(align(4096))]
    struct Page(#[expect(dead_code, reason = "only its size and alignment are asked for")] [u8; 4096]);

    /// What a child of `running_out_of_memory_is_said_however_mimalloc_comes_to_it` does, as
    /// `RUNTIME_TEST_OUT_OF_MEMORY` says, with mimalloc's error handler set: asks for a petabyte, more than
    /// any machine's address space, as a new block, a zeroed one, a grown one or one aligned to a
    /// page, or says mimalloc refused a request as too large to ever give. Nothing, otherwise.
    #[test]
    #[cfg(not(feature = "sanitize"))]
    fn runs_out_of_memory_when_asked() {
        let Ok(how) = std::env::var("RUNTIME_TEST_OUT_OF_MEMORY") else { return };
        super::say_when_out_of_memory();
        let petabyte = 1_usize << 50;
        let held = match how.as_str() {
            "new" => Vec::<u8>::with_capacity(petabyte).capacity(),
            "zeroed" => vec![0_u8; petabyte].capacity(),
            "grown" => {
                let mut grown = vec![1_u8; 16];
                grown.reserve_exact(petabyte);
                grown.capacity()
            }
            "aligned" => Vec::<Page>::with_capacity(petabyte / 4096).capacity(),
            _ => {
                super::ran_out(super::EOVERFLOW, std::ptr::null_mut());
                0
            }
        };
        std::hint::black_box(held);
    }

    /// Every way mimalloc comes to give back null for a Rust allocation calls its error handler
    /// first, which says the program ran out of memory, once, before Rust's own "memory allocation
    /// of N bytes failed" and abort could: a new block, a zeroed one, a grown one, one aligned to a
    /// page, and a request too large to ever give, which mimalloc calls `EOVERFLOW`. Outside an
    /// executable there's no runtime to end the program with 1, so it aborts after the words.
    #[test]
    #[cfg(not(feature = "sanitize"))]
    fn running_out_of_memory_is_said_however_mimalloc_comes_to_it() -> std::io::Result<()> {
        for how in ["new", "zeroed", "grown", "aligned", "too large"] {
            let ran = std::process::Command::new(std::env::current_exe()?)
                .args(["--exact", "tests::runs_out_of_memory_when_asked", "--test-threads=1"])
                .env("RUNTIME_TEST_OUT_OF_MEMORY", how)
                .output()?;
            let said = String::from_utf8_lossy(&ran.stderr);
            assert!(!ran.status.success(), "{how}: it went on");
            assert_eq!(said.matches("error: out of memory\n").count(), 1, "{how}: {said}");
            assert!(!said.contains("memory allocation of"), "{how}: {said}");
        }
        Ok(())
    }

    /// An error mimalloc has that isn't running out of memory, as a block freed twice, a corrupted
    /// list or a block it never gave, is left to mimalloc, which goes on: the handler gives back.
    #[test]
    #[cfg(not(feature = "sanitize"))]
    fn other_errors_are_left_to_mimalloc() {
        for code in [11, 14, 22] {
            super::ran_out(code, std::ptr::null_mut());
        }
    }

    /// Each function said never to fail is one the library exports, under that name.
    #[test]
    fn every_function_said_never_to_fail_is_exported() {
        for symbol in super::NEVER_FAIL {
            assert!(super::SYMBOLS.contains(symbol), "{symbol} isn't exported");
        }
    }

    /// Each export a name is sent to is one the library has, and no row hides behind an
    /// earlier one that fits the same names.
    #[test]
    fn every_modules_entry_is_exported_and_can_be_reached() {
        for (row, export) in super::MODULE_ENTRIES {
            assert!(super::SYMBOLS.contains(export), "{export} isn't exported");
            assert_eq!(super::module_entry(row), Some(*export), "the row for `{row}` is never reached");
        }
    }

    /// A name goes to its own module's export, and one no row is for goes to none.
    #[test]
    fn a_name_is_sent_to_its_own_modules_export() {
        let sent = super::module_entry;
        assert_eq!(sent("Fs.read_text"), Some("fennec_fs_capability"));
        assert_eq!(sent("Fs.only"), Some("fennec_fs_capability"));
        assert_eq!(sent("Fs._sqlite_open"), Some("fennec_sqlite_capability"));
        assert_eq!(sent("Fs._sqlite_read"), Some("fennec_sqlite_capability"));
        // Only those two names open a database: anything else of an `Fs` is the file system's.
        assert_eq!(sent("Fs._sqlite_opened"), Some("fennec_fs_capability"));
        assert_eq!(sent("Net.get.timeout"), Some("fennec_net_capability"));
        assert_eq!(sent("Process.run.cwd"), Some("fennec_process_capability"));
        assert_eq!(sent("Stdin.read_line"), Some("fennec_io_capability"));
        assert_eq!(sent("Database.query"), Some("fennec_database_call"));
        assert_eq!(sent("Transaction.execute"), Some("fennec_database_call"));
        assert_eq!(sent("File.read_line"), Some("fennec_file_call"));
        assert_eq!(sent("Connection.read_line.timeout"), Some("fennec_socket_call"));
        assert_eq!(sent("compress.gzip"), Some("fennec_compress_call"));
        assert_eq!(sent("crypto.sha256"), Some("fennec_crypto_alone_call"));
        // The runtime does these another way, or not at all.
        assert_eq!(sent("Clock.now"), None);
        assert_eq!(sent("Stdout.print"), None);
        assert_eq!(sent("Fs"), None);
        assert_eq!(sent("fs.read_text"), None);
    }

    /// Whether a symbol is listed is worked out by name, exactly.
    #[test]
    fn a_symbol_is_listed_only_by_its_whole_name() {
        assert!(super::never_fails("fennec_int"));
        assert!(!super::never_fails("fennec_free"));
        assert!(!super::never_fails("fennec_in"));
        assert!(!super::never_fails("fennec_int_"));
    }
}
