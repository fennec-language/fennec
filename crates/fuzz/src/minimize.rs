//! Cutting a failing input down to something a person can read: lines first, then bytes, as
//! long as what's left still fails the same way.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::supervise::{Failure, STUCK, watch};

/// How long to spend cutting one input down.
const MOST_TIME: Duration = Duration::from_secs(60);

/// Whether an input fails the way the original did.
fn fails(fennec_fuzz: &Path, target: &str, input: &[u8], wanted: Failure, file: &Path) -> bool {
    if std::fs::write(file, input).is_err() {
        return false;
    }
    let child = Command::new(fennec_fuzz)
        .args(["one", target])
        .arg(file)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else { return false };
    let stuck = crate::targets::find(target).map_or(STUCK, |target| target.stuck);
    watch(&mut child, stuck, None, || false) == Some(wanted)
}

/// `input` with the chunks `size` long at each offset removed, wherever that still fails.
fn without_chunks(
    mut input: Vec<u8>,
    size: usize,
    deadline: Instant,
    still_fails: &mut impl FnMut(&[u8]) -> bool,
) -> Vec<u8> {
    let mut at = 0;
    while at < input.len() && Instant::now() < deadline {
        let end = (at + size).min(input.len());
        let mut smaller = input.clone();
        smaller.drain(at..end);
        if still_fails(&smaller) {
            input = smaller;
        } else {
            at += size;
        }
    }
    input
}

/// The smallest version of `input` it finds in the time it has.
pub(crate) fn minimize(fennec_fuzz: &Path, target: &str, input: Vec<u8>, wanted: Failure) -> Vec<u8> {
    let folder = std::env::temp_dir().join(format!("fennec-fuzz-small-{}", std::process::id()));
    if std::fs::create_dir_all(&folder).is_err() {
        return input;
    }
    let file = folder.join("input");
    let deadline = Instant::now() + MOST_TIME;
    let mut still_fails = |candidate: &[u8]| fails(fennec_fuzz, target, candidate, wanted, &file);
    // An input that only fails after the ones before it can't be cut down on its own.
    if !still_fails(&input) {
        let _ = std::fs::remove_dir_all(&folder);
        return input;
    }
    let mut small = input;
    let mut size = small.len().div_ceil(2).max(1);
    loop {
        small = without_chunks(small, size, deadline, &mut still_fails);
        if size == 1 || Instant::now() >= deadline {
            break;
        }
        size = size.div_ceil(2);
    }
    let _ = std::fs::remove_dir_all(&folder);
    small
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::without_chunks;

    #[test]
    fn chunks_go_while_what_matters_stays() {
        let later = Instant::now() + Duration::from_secs(60);
        let input = b"keep this x and not that".to_vec();
        let mut has_x = |candidate: &[u8]| candidate.contains(&b'x');
        assert_eq!(without_chunks(input, 1, later, &mut has_x), b"x");
    }

    #[test]
    fn time_running_out_keeps_what_there_is() {
        let past = Instant::now();
        let mut never = |_: &[u8]| panic!("there's no time to try anything");
        assert_eq!(without_chunks(b"abc".to_vec(), 1, past, &mut never), b"abc");
    }
}
