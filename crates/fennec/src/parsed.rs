//! Parses kept in the cache, so loading a kit parses only the files whose text changed. A parse is
//! kept by a hash of the text, the build of Fennec that parsed it, and what else changes what a
//! parse holds, so a new `fennec` never reads an old one's. Its spans are kept as from the start of
//! its file, and put back at wherever the file is among the program's when it's read.

use fennec_build::Cache;
use fennec_rt::embed::codec::{Decode, Encode, Reader, Writer};
use fennec_syntax::Module;

/// Where parses are kept in the cache.
const PARSES: &str = "parses";

/// The parses kept in a cache.
pub(crate) struct Kept {
    cache: Cache,
    build: &'static str,
}

impl Kept {
    pub(crate) fn new(cache: Cache) -> Self {
        Kept { cache, build: env!("FENNEC_BUILD") }
    }

    /// What a parse is kept as: the text, and the build that parsed it, since another build may
    /// parse the same text differently. A file's doc examples are part of its parse when they're
    /// wanted, and their tests are named for the file.
    fn name(&self, text: &fennec_blake3::Hash, shown: &str, doc_examples: bool) -> String {
        let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec parses 2026-11-10");
        hasher.update(self.build.as_bytes());
        hasher.update(&[0, u8::from(doc_examples)]);
        if doc_examples {
            hasher.update(shown.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(text.as_bytes());
        hasher.finalize().to_hex()
    }
}

impl fennec_kit::Parses for Kept {
    fn kept(&self, text: &fennec_blake3::Hash, shown: &str, doc_examples: bool, base: u32) -> Option<Module> {
        let bytes = self.cache.read(PARSES, &self.name(text, shown, doc_examples))?;
        Module::decode(&mut Reader::with_spans_at(&bytes, base))
    }

    fn keep(&self, text: &fennec_blake3::Hash, shown: &str, doc_examples: bool, base: u32, module: &Module) {
        let mut writer = Writer::with_spans_from(base);
        module.encode(&mut writer);
        // A cache that can't be written only means parsing the file again next time.
        let parse = self.cache.write(PARSES, &self.name(text, shown, doc_examples), &writer.into_bytes());
        fennec_build::kept("keep a file's parse", parse);
    }
}

/// Where the standard library's parse is kept in the cache: one for each build of Fennec, whose
/// name is a hash of all its source, the standard library's text included, so a kept parse is
/// always of the text this build holds.
const PRELUDE: &str = "prelude";

impl Kept {
    fn prelude_name(&self) -> String {
        let mut hasher = fennec_blake3::Hasher::new_derive_key("fennec prelude parse 2026-11-10");
        hasher.update(self.build.as_bytes());
        hasher.finalize().to_hex()
    }
}

impl fennec_syntax::KeptPrelude for Kept {
    fn prelude(&self) -> Option<Vec<Module>> {
        let bytes = self.cache.read(PRELUDE, &self.prelude_name())?;
        Vec::<Module>::decode(&mut Reader::new(&bytes))
    }

    fn keep_prelude(&self, modules: &[Module]) {
        let mut writer = Writer::default();
        modules.encode(&mut writer);
        // A cache that can't be written only means parsing the standard library again next time.
        let prelude = self.cache.write(PRELUDE, &self.prelude_name(), &writer.into_bytes());
        fennec_build::kept("keep the prelude's parse", prelude);
    }
}

#[cfg(test)]
mod tests {
    use fennec_kit::Parses as _;

    use super::Kept;

    fn hashed(text: &str) -> fennec_blake3::Hash {
        fennec_blake3::hash(text.as_bytes())
    }

    fn kept(dir: &std::path::Path, build: &'static str) -> Kept {
        Kept { cache: fennec_build::Cache::new(dir), build }
    }

    /// A parse kept at one offset is read back at another as parsing there would give it, and an
    /// edit to the file, or another build of Fennec, finds nothing kept.
    #[test]
    fn a_kept_parse_is_found_only_for_the_same_text_and_build() {
        let dir =
            fennec_temp::Temp::at(std::env::temp_dir().join(format!("fennec-parses-{}", std::process::id())));
        let _ = std::fs::remove_dir_all(&dir);
        let text = "## Twice the number.\nexport def twice(number: int) -> int:\n    return number * 2\n";
        let parses = kept(&dir, "build one");
        assert!(parses.kept(&hashed(text), "src/maths.fen", false, 0).is_none(), "nothing's kept yet");
        parses.keep(&hashed(text), "src/maths.fen", false, 40, &fennec_syntax::parse_at(text, 40).unwrap());
        for base in [0, 40, 1234] {
            let found = parses.kept(&hashed(text), "src/maths.fen", false, base);
            assert_eq!(found, Some(fennec_syntax::parse_at(text, base).unwrap()), "at {base}");
        }
        let edited = text.replace("number * 2", "number * 3");
        assert!(
            parses.kept(&hashed(&edited), "src/maths.fen", false, 40).is_none(),
            "an edited file is parsed again"
        );
        assert!(
            parses.kept(&hashed(text), "src/maths.fen", true, 40).is_none(),
            "doc examples are another parse"
        );
        assert!(
            kept(&dir, "build two").kept(&hashed(text), "src/maths.fen", false, 40).is_none(),
            "another build parses again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn fen_files(dir: &std::path::Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                fen_files(&path, found);
            } else if path.extension().is_some_and(|ext| ext == "fen") {
                found.push(path);
            }
        }
    }

    /// Every example's parse, kept where it was parsed at one offset and read back at another, is
    /// what parsing it at the other gives, with its doc examples and without: nothing in a parse
    /// depends on where its file is but its spans.
    #[test]
    fn every_examples_parse_reads_back_as_parsing_it_elsewhere_gives() {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-parses-examples-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        let parses = kept(&dir, "build");
        let mut files = Vec::new();
        fen_files(&std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples"), &mut files);
        assert!(files.len() > 100, "the examples are where they were");
        let parse = |text: &str, base: u32, doc_examples: bool| {
            let mut module = fennec_syntax::parse_at(text, base).ok()?;
            if doc_examples {
                module.items.extend(fennec_syntax::doc_examples(text, base, "src/example.fen").ok()?);
            }
            Some(module)
        };
        let mut compared = 0;
        for file in &files {
            let text = std::fs::read_to_string(file).unwrap();
            for doc_examples in [false, true] {
                let Some(module) = parse(&text, 3, doc_examples) else { continue };
                parses.keep(&hashed(&text), "src/example.fen", doc_examples, 3, &module);
                let read = parses.kept(&hashed(&text), "src/example.fen", doc_examples, 90_001);
                assert_eq!(read, parse(&text, 90_001, doc_examples), "{}", file.display());
                compared += 1;
            }
        }
        assert!(compared > 100, "only {compared} examples parsed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The standard library's parse, kept and read back, is what parsing it gives, and is kept for
    /// one build of Fennec only.
    #[test]
    fn the_standard_librarys_parse_reads_back_as_parsing_it_gives() {
        use fennec_syntax::KeptPrelude as _;
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-prelude-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        let library = fennec_syntax::prelude_modules().unwrap();
        let cache = kept(&dir, "build one");
        assert!(cache.prelude().is_none(), "nothing's kept yet");
        cache.keep_prelude(library);
        assert_eq!(cache.prelude().as_deref(), Some(library));
        assert!(kept(&dir, "build two").prelude().is_none(), "another build parses again");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
