//! Targets that run code: the JIT against the interpreter, and the runtime on its own.

use fennec_interp::embed;
use fennec_interp::packed::Element;
use fennec_syntax::Module;

use crate::generate::program;
use crate::mutate::Rng;

/// What a run printed, and the message it failed with, if it did.
type Ran = (Vec<u8>, Option<String>);

/// Whichever of two runs of a program did first, they must do the same: print the same and
/// fail the same way.
pub(crate) fn jit(input: &[u8]) -> Result<(), String> {
    let source = program(input);
    let mut module =
        fennec_syntax::parse(&source).map_err(|err| format!("it wrote bad code: {err:?}\n{source}"))?;
    fennec_check::check_and_resolve(&mut module)
        .map_err(|err| format!("it wrote bad code: {err:?}\n{source}"))?;
    let interpreted = interpreted(&module);
    let Some(native) = native(&module) else { return Ok(()) };
    if interpreted != native {
        return Err(format!(
            "the engines disagree:\n--- interpreter\n{}\n--- JIT\n{}\n--- program\n{source}",
            shown(&interpreted),
            shown(&native)
        ));
    }
    Ok(())
}

fn shown((output, failure): &Ran) -> String {
    format!("{}{failure:?}", String::from_utf8_lossy(output))
}

fn interpreted(module: &Module) -> Ran {
    let mut output = Vec::new();
    let failure = fennec_interp::run(module, &mut output).err().map(|failure| failure.message);
    (output, failure)
}

/// A run as native code, or None for a program the JIT can't compile yet.
fn native(module: &Module) -> Option<Ran> {
    let mut output = Vec::new();
    let failure = match fennec_jit::run(module, &mut output) {
        Ok(()) => None,
        Err(fennec_jit::JitError::Failed(failure)) => Some(failure.message),
        Err(fennec_jit::JitError::Unsupported(_)) => return None,
    };
    Some((output, failure))
}

/// Values worth passing to the runtime when it isn't one it gave out.
const STRANGE: [i64; 9] = [0, 1, 2, 3, -1, 8, 4096, i64::MAX, i64::MIN];

/// The most items any one call is asked for, and how many calls that can double what they're
/// given may run. Past these, an input is only asking for memory, which native code may do.
const MOST_COUNT: i64 = 1 << 12;
const MOST_GROWING: usize = 12;

/// The calls in `CALLS` that can double what they're given.
const GROWING: [usize; 7] = [0, 1, 9, 10, 11, 15, 19];

/// One call the runtime takes from native code, given two or three values.
type Call = fn(i64, i64, i64) -> i64;

const CALLS: [Call; 26] = [
    |a, _, _| embed::lists::grow(a),
    |a, _, _| embed::lists::unshare(a),
    |a, _, _| {
        embed::lists::clear(a);
        0
    },
    |a, b, _| embed::lists::new(a, b.clamp(0, MOST_COUNT)),
    |a, b, _| embed::lists::sorted(a, b),
    |a, b, c| embed::lists::sorted_by(a, b, c),
    |a, b, _| {
        embed::lists::remove_at(a, b);
        0
    },
    |a, b, _| i64::from(embed::lists::contains(a, b)),
    |a, b, c| embed::lists::slice(a, b, c),
    |a, b, _| embed::lists::insert(a, b, 0, 0),
    |a, b, _| embed::strings::concat(a, b),
    |a, b, _| embed::strings::append(a, b),
    |a, b, _| embed::strings::char_at(a, b),
    |a, b, _| embed::strings::compare(a, b),
    |a, b, _| i64::from(embed::strings::contains(a, b)),
    |a, b, _| embed::strings::join(a, b),
    |a, b, c| embed::strings::slice(a, b, c, 0, 0),
    |a, _, _| embed::strings::int_text(a),
    |a, b, _| embed::dicts::new(a, b.clamp(0, MOST_COUNT)),
    |a, b, c| embed::dicts::set(a, b, c),
    |a, _, _| embed::dicts::set_clear(a),
    |a, _, _| embed::heap::alloc(a),
    |a, _, _| {
        embed::heap::free(a);
        0
    },
    |a, _, _| embed::heap::unshare(a),
    |a, b, _| i64::from(embed::heap::equal(a, b)),
    |a, _, _| embed::heap::cell_read(a),
];

/// The runtime never follows an address it didn't hand out: native code calling it with anything
/// at all can make a run fail, but can't make it crash.
#[expect(clippy::unnecessary_wraps, reason = "every target has the same signature")]
pub(crate) fn runtime(input: &[u8]) -> Result<(), String> {
    let mut tables = embed::Tables::default();
    let elements = [Element::Int, Element::Float, Element::Bool, Element::Str, Element::Boxed(0)];
    tables.elements = elements.into_iter().map(std::rc::Rc::new).collect();
    tables.shapes = vec![std::rc::Rc::new(Element::Int), std::rc::Rc::new(Element::Str)];
    tables.str_shape = 2;
    if embed::start(&Module { items: Vec::new() }, tables, fennec_interp::Inputs::default()).is_err() {
        return Ok(());
    }
    let mut values: Vec<i64> = Vec::new();
    let mut rng = Rng::new(0, 0);
    let mut bytes = input.iter().copied();
    let mut growing = 0;
    while let Some(which) = bytes.next() {
        let mut value = |values: &[i64]| {
            let choice = usize::from(bytes.next().unwrap_or(0));
            if choice % 4 == 0 {
                STRANGE.get(choice / 4 % STRANGE.len()).copied().unwrap_or(0)
            } else if choice % 4 == 1 {
                i64::from_le_bytes(rng.next().to_le_bytes())
            } else {
                values.get(choice % values.len().max(1)).copied().unwrap_or(0)
            }
        };
        let (a, b, c) = (value(&values), value(&values), value(&values));
        let at = usize::from(which) % CALLS.len();
        if GROWING.contains(&at) {
            if growing == MOST_GROWING {
                continue;
            }
            growing += 1;
        }
        if let Some(call) = CALLS.get(at) {
            let made = call(a, b, c);
            if made != 0 {
                values.push(made);
            }
        }
        let _ = embed::next_test();
    }
    embed::finish();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{jit, runtime};

    #[test]
    fn an_empty_program_runs_the_same_on_both() {
        assert_eq!(jit(b""), Ok(()));
    }

    #[test]
    fn the_runtime_takes_any_calls() {
        assert_eq!(runtime(b""), Ok(()));
        let every_call: Vec<u8> = (0_u8..=255).collect();
        assert_eq!(runtime(&every_call), Ok(()));
    }
}
