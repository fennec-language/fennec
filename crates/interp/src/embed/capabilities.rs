//! Capabilities for native code, as blocks. A plain capability has one block for each kind,
//! made the first time it's asked for and never freed. A capability narrowed with `only` has a
//! block of its own, owned like any other, holding the list of what it allows. Native code
//! carries a capability as its block's address, and the runtime reads from the block what the
//! capability may do, so passing one through native code can't widen it.
//!
//! A kind on its own, 0 to 7, still means that plain capability where one is taken: no
//! block's address is that small.

use std::rc::Rc;

use fennec_syntax::Diagnostic;

use super::heap::{CAPABILITY_SHAPE, IMMORTAL, read_block};
use super::{State, attempt, span, with};
use crate::limits::Limited;
use crate::packed::Element;
use crate::value::{Capability, Value};

/// A capability's block: its owner count, its shape, its kind, and its list of what it
/// allows, which is 0 for a plain one.
const WORDS: usize = 4;

/// The kinds a capability word can be on its own, below any block's address.
const KINDS: i64 = 8;

/// The kind native code numbers a capability by, the inverse of `super::capability`.
fn kind_of(capability: Capability) -> i64 {
    match capability {
        Capability::Stdout => 0,
        Capability::Fs => 1,
        Capability::Clock => 2,
        Capability::Args => 3,
        Capability::Stdin => 4,
        Capability::Process => 5,
    }
}

/// The block of a plain capability of `kind`, made the first time it's asked for.
fn plain(state: &mut State, kind: i64) -> i64 {
    let at = usize::try_from(kind.clamp(0, KINDS - 1)).unwrap_or(0);
    if let Some(address) = state.capabilities.get(at).copied().filter(|address| *address != 0) {
        return address;
    }
    let mut block = state.heap.fresh(WORDS);
    block.copy_from_slice(&[IMMORTAL, CAPABILITY_SHAPE, u64::try_from(at).unwrap_or(0), 0]);
    let address = i64::try_from(block.as_ptr().addr()).unwrap_or(0);
    // It lives for the rest of the run, like a literal, so it isn't counted as allocated.
    state.heap.adopt(&mut vec![block]);
    if state.capabilities.len() <= at {
        state.capabilities.resize(at + 1, 0);
    }
    if let Some(slot) = state.capabilities.get_mut(at) {
        *slot = address;
    }
    address
}

/// `fennec_capability_block(kind)`: the block of the plain capability of a kind, which
/// native code needn't count.
pub extern "C" fn block(kind: i64) -> i64 {
    with(|state| plain(state, kind))
}

/// What the interpreter holds for a capability word: a kind, or a capability's block.
pub(super) fn value_of(state: &State, word: i64) -> Value {
    if (0..KINDS).contains(&word) {
        return Value::Capability(super::capability(word));
    }
    u64::try_from(word).map_or(Value::Unit, |address| read_block(state, address))
}

/// The value of a capability's block, which `read_block` gives for one.
pub(super) fn read(state: &State, block: &[u64]) -> Value {
    let kind = block.get(2).and_then(|kind| i64::try_from(*kind).ok()).unwrap_or(0);
    let capability = super::capability(kind);
    let allowed = block.get(3).copied().unwrap_or(0);
    if allowed == 0 {
        return Value::Capability(capability);
    }
    let allowed = match read_block(state, allowed).unpacked() {
        Value::List(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Str(text) => Some(Rc::clone(text)),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    Limited::to(capability, allowed)
}

/// A capability's block, owned once, for a temporary holding a capability: the plain block
/// for a plain one, and a new block for one narrowed with `only`.
pub extern "C" fn from_value(value: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| match state.temp(value) {
        Value::Capability(capability) => Ok(plain(state, kind_of(capability))),
        Value::Limited(limited) => {
            let allowed = limited.allowed.iter().map(|item| Value::Str(Rc::clone(item))).collect();
            let str = Rc::new(Element::Str);
            let list = super::lists::write_list(state, &str, &Value::list(allowed)).unwrap_or(0);
            let mut block = state.heap.fresh(WORDS);
            let kind = u64::try_from(kind_of(limited.capability)).unwrap_or(0);
            block.copy_from_slice(&[1, CAPABILITY_SHAPE, kind, u64::try_from(list).unwrap_or(0)]);
            Ok(state.heap.insert(block))
        }
        other => Err(Diagnostic::new(
            format!("expected a capability here, but this is a {}", other.type_name()),
            span(start, end),
        )),
    })
}

/// `fennec_capability_call(word, method, count, start, end)`: a method of a capability, run
/// as the interpreter runs it, with the last `count` inputs native code handed over as its
/// arguments. `word` is a capability's block or kind, and `method` a string block naming the
/// method. Gives back a temporary holding what the method gave: a result where the method can
/// fail, and a capability where it gives one, as `only` does.
pub extern "C" fn call(word: i64, method: i64, count: i64, start: i64, end: i64) -> i64 {
    attempt(|state: &mut State| {
        let at = span(start, end);
        let receiver = value_of(state, word);
        let name = u64::try_from(method)
            .ok()
            .and_then(|address| state.heap.block(address))
            .filter(|block| super::heap::is_text(state, block))
            .map(super::heap::block_text)
            .ok_or_else(|| Diagnostic::new("the native code lost a method's name", at))?;
        let count = usize::try_from(count).unwrap_or(0).min(state.inputs.len());
        let given = state.inputs.split_off(state.inputs.len() - count);
        let args = given.into_iter().map(|value| crate::call::CallArg::plain(value, at)).collect();
        let result = state.interp.read_method(&receiver, &name, args, at)?;
        Ok(state.push(result))
    })
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use super::super::{Tables, finish, heap, start, with};
    use crate::limits::Limited;
    use crate::packed::Element;
    use crate::value::{Capability, Value};

    fn value(word: i64) -> Value {
        with(|state| Some(super::value_of(state, word))).unwrap()
    }

    #[test]
    fn a_capability_crosses_native_code_as_a_block_that_keeps_what_it_allows() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&fennec_syntax::Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let fs = super::block(1);
        assert_eq!(super::block(1), fs);
        assert!(fs >= super::KINDS);
        assert_eq!(value(fs), Value::Capability(Capability::Fs));
        assert_eq!(value(1), Value::Capability(Capability::Fs));
        // A plain capability is never freed: letting go of it never brings its count to zero.
        with(|state| heap::release(state, u64::try_from(fs).unwrap()));
        assert_eq!(value(fs), Value::Capability(Capability::Fs));
        let live = with(|state| state.heap.live());
        let narrowed = Limited::to(Capability::Process, vec![Rc::from("git"), Rc::from("ls")]);
        let temp = with(|state| Some(state.push(narrowed.clone()))).unwrap();
        let block = super::from_value(temp, 0, 0);
        assert_ne!(block, 0);
        let (Value::Limited(read), Value::Limited(made)) = (value(block), &narrowed) else {
            panic!("narrowed")
        };
        assert_eq!(read, *made);
        let plain = with(|state| Some(state.push(Value::Capability(Capability::Stdout)))).unwrap();
        assert_eq!(super::from_value(plain, 0, 0), super::block(0));
        heap::free(block);
        assert_eq!(with(|state| state.heap.live()), live);
        let not = with(|state| Some(state.push(Value::Int(3)))).unwrap();
        assert_eq!(super::from_value(not, 0, 0), 0);
        let (_, failure, _) = finish();
        assert!(failure.is_some_and(|failure| failure.message.contains("expected a capability")));
    }

    #[test]
    fn a_capability_s_methods_run_from_native_code_as_the_interpreter_runs_them() {
        let tables = Tables { shapes: vec![Rc::new(Element::Str)], ..Tables::default() };
        start(&fennec_syntax::Module { items: Vec::new() }, tables, crate::Inputs::default()).unwrap();
        let string = |text: &str| with(|state| heap::text_address(state, text));
        let input = |value: Value| {
            let temp = with(|state| Some(state.push(value))).unwrap();
            super::super::eval::input(temp);
        };
        // `process.only("echo")` gives a narrowed capability, which crosses as a block.
        input(Value::str("echo"));
        let narrowed = super::call(super::block(5), string("only"), 1, 0, 0);
        let block = super::from_value(narrowed, 0, 0);
        let Value::Limited(limited) = value(block) else { panic!("narrowed") };
        assert_eq!(limited.allowed, vec![Rc::<str>::from("echo")]);
        // Running something else is refused as a `ProcessError`, and nothing is started.
        input(Value::str("rm"));
        input(Value::list(vec![Value::str("-rf"), Value::str("/")]));
        let refused = super::call(block, string("run"), 2, 0, 0);
        let Value::Err(error) = with(|state| Some(state.temp(refused))).unwrap() else { panic!("refused") };
        assert!(format!("{error:?}").contains("can run only `echo`"), "{error:?}");
        heap::free(block);
        finish();
    }
}
