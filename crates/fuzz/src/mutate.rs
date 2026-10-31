//! Making inputs: real programs broken in small ways, the kinds of mistakes a parser has to
//! survive. Every input comes from a seed and its number, so a run can be repeated exactly.

/// `SplitMix64`: small, good enough, and the same on every machine.
pub(crate) struct Rng(u64);

impl Rng {
    pub(crate) fn new(seed: u64, number: u64) -> Self {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ number.wrapping_mul(0xBF58_476D_1CE4_E5B9))
    }

    pub(crate) fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A number below `limit`, or 0 when there's nothing to choose from.
    pub(crate) fn below(&mut self, limit: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(limit.max(1)).unwrap_or(1)).unwrap_or(0)
    }

    pub(crate) fn pick<'a>(&mut self, from: &[&'a str]) -> &'a str {
        from.get(self.below(from.len())).copied().unwrap_or("")
    }
}

/// How many times the deepest nesting is repeated: far past what real code does.
const DEEP: [usize; 3] = [64, 1_000, 20_000];

/// Changes `input` once. `tokens` is what a target's inputs are made of, and `other` is another
/// input to take pieces from.
fn mutate_once(input: &mut Vec<u8>, other: &[u8], tokens: &[&str], rng: &mut Rng) {
    let at = rng.below(input.len() + 1);
    let end = (at + 1 + rng.below(32)).min(input.len());
    match rng.below(8) {
        0 if at < input.len() => {
            if let Some(byte) = input.get_mut(at) {
                *byte ^= 1 << rng.below(8);
            }
        }
        1 if at < end => {
            input.drain(at..end);
        }
        2 if at < end => {
            let piece: Vec<u8> = input.get(at..end).unwrap_or_default().to_vec();
            input.splice(at..at, piece);
        }
        3 => {
            let from = rng.below(other.len());
            let to = (from + 1 + rng.below(200)).min(other.len());
            let piece = other.get(from..to).unwrap_or_default().to_vec();
            input.splice(at..at, piece);
        }
        4 => input.truncate(at),
        5 => {
            let token = rng.pick(tokens);
            let times = DEEP.get(rng.below(DEEP.len())).copied().unwrap_or(1);
            input.splice(at..at, token.repeat(times).into_bytes());
        }
        _ => {
            let token = rng.pick(tokens).as_bytes().to_vec();
            input.splice(at..at, token);
        }
    }
}

/// The input numbered `number` of a run: one of `corpus`, changed a few times.
pub(crate) fn input(corpus: &[Vec<u8>], tokens: &[&str], seed: u64, number: u64) -> Vec<u8> {
    let mut rng = Rng::new(seed, number);
    let empty = Vec::new();
    let mut made = corpus.get(rng.below(corpus.len())).unwrap_or(&empty).clone();
    for _ in 0..=rng.below(4) {
        let other = corpus.get(rng.below(corpus.len())).unwrap_or(&empty);
        mutate_once(&mut made, other, tokens, &mut rng);
    }
    // Big inputs are slow, not interesting: what they'd find, small ones find sooner.
    made.truncate(1 << 17);
    made
}

#[cfg(test)]
mod tests {
    use super::{Rng, input};

    #[test]
    fn a_seed_and_a_number_always_give_the_same_input() {
        let corpus = [b"def main():\n    pass\n".to_vec(), b"x = 1\n".to_vec()];
        for number in 0..64 {
            let first = input(&corpus, &["(", "\""], 7, number);
            assert_eq!(first, input(&corpus, &["(", "\""], 7, number));
        }
        assert_ne!(Rng::new(1, 0).next(), Rng::new(2, 0).next());
        assert_ne!(Rng::new(1, 0).next(), Rng::new(1, 1).next());
    }

    #[test]
    fn nothing_to_choose_from_is_fine() {
        assert_eq!(Rng::new(1, 1).below(0), 0);
        assert_eq!(Rng::new(1, 1).pick(&[]), "");
        assert_eq!(input(&[], &[], 1, 1).len(), 0);
    }
}
