//! Starting an unchanged kit from what it was cut down to last time, rather than parsing all of
//! its files to find that out again. So how long `fennec run` takes to start depends on what
//! `main` reaches, not on the size of the kit.
//!
//! It's kept with the size and changed time of every file, which are all that's looked at. A
//! program from here has no source text, so a failure loads the source to point at it.

use std::collections::{HashMap, HashSet};

use fennec_build::{Cache, KitFile, KitState};
use fennec_kit::Program;
use fennec_rt::embed::codec::{Decode, Encode, Reader, Writer};
use fennec_syntax::{Module, Sources};

const BUILD: &str = env!("FENNEC_BUILD");

/// Where a kit's starting point is kept.
fn name_of(path: &str) -> String {
    format!("{}#run", crate::run::absolute(path))
}

fn image_name(path: &str) -> String {
    format!("{}.image", fennec_blake3::hash(name_of(path).as_bytes()).to_hex())
}

/// A kit's program as it was cut down last time, if none of its files have changed since, and a
/// hash of it as it was kept, which says it's the same program another run had.
pub(crate) fn load(path: &str, cache: &Cache) -> Option<(Program, String)> {
    let state = cache.kit_state(&name_of(path), BUILD)?;
    let files = crate::verdict::kit_files(path, Some(&state))?;
    if !crate::verdict::same_files(&state, &files) {
        return None;
    }
    let bytes = cache.read("run-image", &image_name(path))?;
    let module = Module::decode(&mut Reader::new(&bytes))?;
    let kept = fennec_blake3::hash(&bytes).to_hex();
    let program = Program {
        module,
        sources: Sources::new(),
        manifest: None,
        embedded: Vec::new(),
        kit_src: None,
        dependency_files: HashSet::new(),
    };
    Some((program, kept))
}

/// Keeps a kit's program as it was cut down, before anything was checked. `state` is what the kit
/// looked like before it was read, so an edit made since is seen next time.
pub(crate) fn store(path: &str, cache: &Cache, files: &[KitFile], program: &Program) {
    let mut writer = Writer::default();
    program.module.encode(&mut writer);
    // A cache that can't be written only means reading the kit again next time.
    if cache.write("run-image", &image_name(path), &writer.into_bytes()).is_ok() {
        let _ = cache.store_kit_state(&name_of(path), BUILD, &crate::verdict::with_embedded(files, program));
    }
}

/// What a kit's files are now, with what was last noted to save reading them.
pub(crate) fn files_now(path: &str, cache: &Cache) -> Option<Vec<KitFile>> {
    let state: Option<KitState> = cache.kit_state(&name_of(path), BUILD);
    crate::verdict::kit_files(path, state.as_ref())
}

/// The cache's kind for what an edited kit's program is patched from: each file's items that
/// `main` reached last time, before its constants were worked out.
const PARTS: &str = "run-parts";

/// Where a kit's parts are kept, apart from its starting point, under a state of their own.
fn parts_name(path: &str) -> String {
    format!("{}#run-parts", crate::run::absolute(path))
}

fn parts_file(path: &str) -> String {
    format!("{}.parts", fennec_blake3::hash(parts_name(path).as_bytes()).to_hex())
}

/// What resolving a file's names looks at beyond the file and what it imports, which a load of
/// just the changed files and their imports doesn't see: every name the kit defines, since one used
/// from a file that isn't imported is a mistake, and every case name more than one enum has, since
/// a leading-dot case is written with its enum's name only then. A body edit changes neither.
struct Kit {
    names: Vec<String>,
    shared_cases: Vec<String>,
}

impl Kit {
    fn of(module: &Module) -> Kit {
        let names = fennec_build::Defined::of(&module.items).names.into_iter().collect();
        let mut enums_with: HashMap<&str, usize> = HashMap::new();
        for item in &module.items {
            if let fennec_syntax::Item::Enum(def) = item {
                def.cases.iter().for_each(|case| *enums_with.entry(case.name.as_str()).or_default() += 1);
            }
        }
        let mut shared_cases: Vec<String> =
            enums_with.into_iter().filter(|(_, count)| *count > 1).map(|(case, _)| case.to_owned()).collect();
        shared_cases.sort();
        Kit { names, shared_cases }
    }

    fn encode(&self, writer: &mut Writer) {
        self.names.encode(writer);
        self.shared_cases.encode(writer);
    }

    fn decode(reader: &mut Reader<'_>) -> Option<Kit> {
        Some(Kit { names: Vec::decode(reader)?, shared_cases: Vec::decode(reader)? })
    }

    /// Whether the changed files' `items`, as `partial` loaded them, may not be what a whole load
    /// makes of them: they use a name the kit defines that `partial` doesn't, or keep a leading-dot
    /// case another file's enum has too, which a whole load may have written with its enum.
    fn may_differ(&self, items: &[fennec_syntax::Item], partial: &Program) -> bool {
        let loaded = fennec_build::Defined::of(&partial.module.items).names;
        let elsewhere = |name: &String| self.names.binary_search(name).is_ok() && !loaded.contains(name);
        if fennec_build::names_used(items).iter().any(elsewhere) {
            return true;
        }
        let module = Module { items: items.to_vec(), ..Module::default() };
        let mut shared = false;
        fennec_syntax::read::exprs(&module, &mut |expr| {
            if let fennec_syntax::ExprKind::Variant(case) = &expr.kind {
                shared |= self.shared_cases.binary_search(case).is_ok();
            }
        });
        shared
    }
}

/// One file of a kit as the last run left it.
struct Part {
    /// How the file is shown, as the program's sources have it.
    shown: String,
    /// How long its text was, which says where every file after it starts.
    len: u32,
    /// Where its items come among all the program's, by file; `NO_ITEMS` for a file with none.
    rank: u32,
    /// Its items `main` reached, written with spans from the file's start.
    items: Vec<u8>,
    /// What its other items define, which nothing reached then.
    absent: fennec_build::Defined,
}

/// The rank of a file with no items in the program.
const NO_ITEMS: u32 = u32::MAX;

impl Part {
    fn encode(&self, writer: &mut Writer) {
        self.shown.encode(writer);
        self.len.encode(writer);
        self.rank.encode(writer);
        self.items.encode(writer);
        self.absent.names.iter().cloned().collect::<Vec<String>>().encode(writer);
        self.absent.members.iter().cloned().collect::<Vec<String>>().encode(writer);
    }

    fn decode(reader: &mut Reader<'_>) -> Option<Part> {
        Some(Part {
            shown: String::decode(reader)?,
            len: u32::decode(reader)?,
            rank: u32::decode(reader)?,
            items: Vec::decode(reader)?,
            absent: fennec_build::Defined {
                names: Vec::<String>::decode(reader)?.into_iter().collect(),
                members: Vec::<String>::decode(reader)?.into_iter().collect(),
            },
        })
    }
}

/// `items`, written with spans from `base`, read back as they were: or None when a span in them
/// starts before `base`, which reading back from another place would move wrongly.
fn written_from(items: &[fennec_syntax::Item], base: u32) -> Option<Vec<u8>> {
    let mut writer = Writer::with_spans_from(base);
    items.to_vec().encode(&mut writer);
    let bytes = writer.into_bytes();
    let back = Vec::<fennec_syntax::Item>::decode(&mut Reader::with_spans_at(&bytes, base))?;
    (back == items).then_some(bytes)
}

/// Keeps what an edit's program can be patched from: the whole program's files in the order their
/// spans were given, each with the items `main` reaches (`kept`, by where they are in the module)
/// and what its others define. Nothing's kept when an item isn't in one file of its own, or a
/// file's items aren't all together, so a patch couldn't put them back as a whole load does.
pub(crate) fn store_parts(
    path: &str,
    cache: &Cache,
    files: &[KitFile],
    program: &Program,
    kept: &HashSet<usize>,
) {
    let parts = parts_of(program, kept);
    let mut writer = Writer::default();
    if let Some(parts) = &parts {
        Kit::of(&program.module).encode(&mut writer);
        parts.len().encode(&mut writer);
        for part in parts {
            part.encode(&mut writer);
        }
    }
    // An empty record is no record: an older one would be read as this kit's.
    if cache.write(PARTS, &parts_file(path), &writer.into_bytes()).is_ok() && parts.is_some() {
        let _ =
            cache.store_kit_state(&parts_name(path), BUILD, &crate::verdict::with_embedded(files, program));
    }
}

fn parts_of(program: &Program, kept: &HashSet<usize>) -> Option<Vec<Part>> {
    let shown: Vec<&str> = program.sources.files().map(|(shown, _)| shown).collect();
    let index: HashMap<&str, usize> = shown.iter().enumerate().map(|(at, shown)| (*shown, at)).collect();
    let mut items_of: Vec<Vec<usize>> = vec![Vec::new(); shown.len()];
    let mut ranks: Vec<u32> = vec![NO_ITEMS; shown.len()];
    let (mut last, mut next) = (None, 0);
    for (at, item) in program.module.items.iter().enumerate() {
        let (file, ..) = program.sources.file_at(fennec_build::item_span(item))?;
        let file = *index.get(file)?;
        if last != Some(file) {
            let rank = ranks.get_mut(file)?;
            // A file's items come together, so one whose items were seen before is out of order.
            if *rank != NO_ITEMS {
                return None;
            }
            *rank = next;
            next += 1;
            last = Some(file);
        }
        items_of.get_mut(file)?.push(at);
    }
    let mut parts = Vec::with_capacity(shown.len());
    for ((shown, items), rank) in shown.iter().zip(items_of).zip(ranks) {
        let (text, base) = program.sources.by_path(shown)?;
        let (reached, others): (Vec<usize>, Vec<usize>) = items.into_iter().partition(|at| kept.contains(at));
        let reached: Vec<fennec_syntax::Item> =
            reached.iter().filter_map(|at| program.module.items.get(*at)).cloned().collect();
        let absent = fennec_build::Defined::of(others.iter().filter_map(|at| program.module.items.get(*at)));
        let len = u32::try_from(text.len()).ok()?;
        parts.push(Part {
            shown: (*shown).to_owned(),
            len,
            rank,
            items: written_from(&reached, base)?,
            absent,
        });
    }
    Some(parts)
}

/// A file of an edited kit as a patch puts it: where a whole load would start it, its items, and,
/// unless it changed, what its other items define.
struct Placed {
    shown: String,
    base: u32,
    len: u32,
    rank: u32,
    items: Vec<fennec_syntax::Item>,
    absent: Option<fennec_build::Defined>,
}

/// What `main` reaches in an edited kit, patched from what the last run kept rather than loaded
/// whole: each unchanged file's items as they were, moved to where its text now starts, and each
/// changed file's from `partial`, a program holding just the changed files and what they import.
/// None, for a whole load instead, whenever anything isn't as a patch needs it: a file added, gone
/// or not in `partial` as it is now, a kit.toml or embedded file changed, nothing kept, or what
/// `main` reaches using something the last run didn't keep. Otherwise the patched program is what
/// a whole load cut down to what `main` reaches would be, and what's kept is brought up to date.
pub(crate) fn patched(path: &str, cache: &Cache, files: &[KitFile], partial: &Program) -> Option<Module> {
    let state = cache.kit_state(&parts_name(path), BUILD)?;
    if state.files.len() != files.len() {
        return None;
    }
    let mut changed: HashSet<&str> = HashSet::new();
    for file in files {
        if state.files.get(&file.module)?.text != file.text {
            changed.insert(file.module.as_str());
        }
    }
    let special = |module: &&str| *module == "kit.toml" || module.starts_with(fennec_kit::EMBEDDED);
    if changed.is_empty() || changed.iter().any(special) {
        return None;
    }
    let bytes = cache.read(PARTS, &parts_file(path))?;
    let mut reader = Reader::new(&bytes);
    let kit = Kit::decode(&mut reader)?;
    let parts: Vec<Part> =
        (0..usize::decode(&mut reader)?).map(|_| Part::decode(&mut reader)).collect::<Option<_>>()?;
    let placed = placed(parts, files, &changed, partial)?;
    let edited: Vec<fennec_syntax::Item> = placed
        .iter()
        .filter(|file| file.absent.is_none())
        .flat_map(|file| file.items.iter().cloned())
        .collect();
    if kit.may_differ(&edited, partial) {
        return None;
    }
    let mut order: Vec<usize> = (0..placed.len()).collect();
    order.sort_by_key(|at| placed.get(*at).map_or(NO_ITEMS, |file| file.rank));
    let mut module = Module::default();
    let mut file_of: Vec<usize> = Vec::new();
    for at in order {
        let file = placed.get(at)?;
        module.items.extend(file.items.iter().cloned());
        file_of.extend(std::iter::repeat_n(at, file.items.len()));
    }
    let mut absent = fennec_build::Defined::default();
    placed.iter().filter_map(|file| file.absent.clone()).for_each(|defined| absent.extend(defined));
    if fennec_build::reaches_into(&module, &absent) {
        return None;
    }
    let kept = fennec_build::reached_by_main(&module);
    let mut writer = Writer::default();
    kit.encode(&mut writer);
    placed.len().encode(&mut writer);
    for (at, file) in placed.into_iter().enumerate() {
        let mine = module.items.iter().enumerate().filter(|(item, _)| file_of.get(*item) == Some(&at));
        let (reached, others): (Vec<_>, Vec<_>) = mine.partition(|(item, _)| kept.contains(item));
        let reached: Vec<fennec_syntax::Item> = reached.into_iter().map(|(_, item)| item.clone()).collect();
        let mut defined = file.absent.unwrap_or_default();
        defined.extend(fennec_build::Defined::of(others.into_iter().map(|(_, item)| item)));
        let items = written_from(&reached, file.base)?;
        Part { shown: file.shown, len: file.len, rank: file.rank, items, absent: defined }
            .encode(&mut writer);
    }
    // A cache that can't be written only means a whole load next time.
    if cache.write(PARTS, &parts_file(path), &writer.into_bytes()).is_ok() {
        let _ = cache.store_kit_state(&parts_name(path), BUILD, files);
    }
    let items =
        module.items.iter().enumerate().filter(|(at, _)| kept.contains(at)).map(|(_, item)| item.clone());
    Some(Module { items: items.collect(), std_rewritten: module.std_rewritten.clone() })
}

/// Each file of the kit at its place after an edit, in the order their spans are given.
fn placed(
    parts: Vec<Part>,
    files: &[KitFile],
    changed: &HashSet<&str>,
    partial: &Program,
) -> Option<Vec<Placed>> {
    let modules = crate::verdict::Modules::of(files);
    let mut placed = Vec::with_capacity(parts.len());
    let (mut base, mut found) = (0_u32, 0);
    for part in parts {
        let file = modules
            .of_file(&part.shown)
            .and_then(|module| files.iter().find(|file| file.module == module))
            .filter(|file| changed.contains(file.module.as_str()));
        let (len, items, absent) = if let Some(file) = file {
            found += 1;
            let (text, from) = partial.sources.by_path(&part.shown)?;
            // What was loaded is what the file holds now, or a later edit would be kept as this.
            if partial.sources.hashed_text_at(from)?.1 != file.text {
                return None;
            }
            let in_file = |item: &&fennec_syntax::Item| {
                partial
                    .sources
                    .file_at(fennec_build::item_span(item))
                    .is_some_and(|(shown, ..)| shown == part.shown)
            };
            let items: Vec<fennec_syntax::Item> =
                partial.module.items.iter().filter(in_file).cloned().collect();
            // A file that had no items has no place among the others for new ones.
            if part.rank == NO_ITEMS && !items.is_empty() {
                return None;
            }
            let moved = Vec::decode(&mut Reader::with_spans_at(&written_from(&items, from)?, base))?;
            (u32::try_from(text.len()).ok()?, moved, None)
        } else {
            (part.len, Vec::decode(&mut Reader::with_spans_at(&part.items, base))?, Some(part.absent))
        };
        placed.push(Placed { shown: part.shown, base, len, rank: part.rank, items, absent });
        base = base.checked_add(len)?.checked_add(1)?;
    }
    (found == changed.len()).then_some(placed)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use fennec_build::{Cache, reached_by_main, what_main_reaches};
    use fennec_syntax::Module;

    /// Writes a kit of `files`, by module, into `dir`.
    fn write_kit(dir: &Path, files: &[(&str, &str)]) {
        std::fs::create_dir_all(dir.join("src")).unwrap();
        let manifest =
            format!("name = \"patched\"\nversion = \"0.1.0\"\nfennec = \"{}\"\n", env!("CARGO_PKG_VERSION"));
        std::fs::write(dir.join("kit.toml"), manifest).unwrap();
        for (module, text) in files {
            std::fs::write(dir.join(format!("src/{module}.fen")), text).unwrap();
        }
    }

    /// Loads the kit at `dir` whole, as a run does.
    fn whole(dir: &Path) -> fennec_kit::Program {
        let path = dir.to_str().unwrap();
        fennec_kit::load(dir, path, fennec_kit::Entry::Main).unwrap()
    }

    /// Keeps what the kit at `dir` can be patched from, as a whole run does.
    fn keep(dir: &Path, cache: &Cache) {
        let path = dir.to_str().unwrap();
        let files = crate::verdict::kit_files(path, None).unwrap();
        let program = whole(dir);
        let kept = reached_by_main(&program.module);
        super::store_parts(path, cache, &files, &program, &kept);
    }

    /// The kit at `dir` patched after its `changed` modules were edited, as a run does.
    fn patch(dir: &Path, cache: &Cache, changed: &[&str]) -> Option<Module> {
        let path = dir.to_str()?;
        let files = crate::verdict::kit_files(path, None)?;
        let changed: Vec<String> = changed.iter().map(|module| (*module).to_owned()).collect();
        let partial = fennec_kit::load_modules_with(dir, path, &changed, &fennec_kit::Overlay::new()).ok()?;
        super::patched(path, cache, &files, &partial)
    }

    /// What a whole load of the kit at `dir`, cut down to what `main` reaches, is.
    fn cut(dir: &Path) -> Module {
        what_main_reaches(&whole(dir).module)
    }

    fn scratch(name: &str) -> (fennec_temp::Temp, Cache) {
        let dir = fennec_temp::Temp::at(
            std::env::temp_dir().join(format!("fennec-patch-{name}-{}", std::process::id())),
        );
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("home")).unwrap();
        let cache = Cache::new(&dir.join("home"));
        (dir, cache)
    }

    const FIRST: &str = "def grow(x: int) -> int:\n    x + 1\n\ndef unused() -> int:\n    7\n\ndef opened() -> with int:\n    print(\"open\")\n    lend 41\n    print(\"closed\")\n";
    const SECOND: &str = "from first import grow, opened\n\ndef twice(x: int) -> int:\n    grow(grow(x))\n\ndef later() -> int:\n    assert 1 > 2\n    0\n\ndef used() -> int:\n    with opened() as n:\n        return n + 1\n\ndef spare() -> int:\n    3\n";
    const MAIN: &str = "from second import twice, later, used\n\ndef main():\n    print(twice(1) + used())\n    print(later())\n";

    fn three_files(dir: &Path) {
        write_kit(&dir.join("kit"), &[("first", FIRST), ("second", SECOND), ("main", MAIN)]);
    }

    /// An edit that makes a body longer moves every file after it: each item a patch puts back is
    /// where a whole load puts it, a `with` source used from another file among them.
    #[test]
    fn a_patched_program_is_the_whole_program_cut_down_when_a_body_grows() {
        let (dir, cache) = scratch("grows");
        three_files(&dir);
        let kit = dir.join("kit");
        keep(&kit, &cache);
        let longer = FIRST.replace("    x + 1\n", "    y = x * 1\n    z = y + 0\n    z + 1\n");
        std::fs::write(kit.join("src/first.fen"), &longer).unwrap();
        let patched = patch(&kit, &cache, &["first"]).expect("a body edit is patched");
        assert_eq!(patched, cut(&kit));
        // And again, from what the patch kept, with the next file edited too.
        let shorter = SECOND.replace("    grow(grow(x))\n", "    grow(x) + 1\n");
        std::fs::write(kit.join("src/second.fen"), &shorter).unwrap();
        let patched = patch(&kit, &cache, &["second"]).expect("a second edit is patched too");
        assert_eq!(patched, cut(&kit));
    }

    /// A body may now call a function of its own file that nothing reached: it's there, since every
    /// item of a changed file is.
    #[test]
    fn a_body_calling_another_function_of_its_file_is_patched() {
        let (dir, cache) = scratch("own");
        three_files(&dir);
        let kit = dir.join("kit");
        keep(&kit, &cache);
        let edited = SECOND.replace("    grow(grow(x))\n", "    grow(grow(x)) + spare()\n");
        std::fs::write(kit.join("src/second.fen"), edited).unwrap();
        let patched = patch(&kit, &cache, &["second"]).expect("patched");
        assert_eq!(patched, cut(&kit));
        assert!(format!("{patched:?}").contains("spare"));
    }

    /// A body that now calls what the last run didn't keep, in another file, isn't patched: the
    /// whole program is loaded instead.
    #[test]
    fn a_body_calling_what_was_cut_from_another_file_loads_it_whole() {
        let (dir, cache) = scratch("cut");
        three_files(&dir);
        let kit = dir.join("kit");
        keep(&kit, &cache);
        let edited = SECOND
            .replace("from first import grow, opened", "from first import grow, opened, unused")
            .replace("    grow(grow(x))\n", "    grow(grow(x)) + unused()\n");
        std::fs::write(kit.join("src/second.fen"), edited).unwrap();
        // The import changed, which a run's check sends to a whole load too: the patch says so itself.
        assert_eq!(patch(&kit, &cache, &["second"]), None);
    }

    /// Each example kit, with a line put at the top of each of its files in turn, which moves
    /// every span after it, is patched to just what a whole load cut down gives.
    #[test]
    fn every_example_kit_is_patched_to_what_a_whole_load_gives() {
        let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/projects");
        let mut patched_any = 0;
        for entry in std::fs::read_dir(examples).unwrap().flatten() {
            let source = entry.path();
            if !source.join("kit.toml").is_file() {
                continue;
            }
            let name = source.file_name().unwrap().to_string_lossy().into_owned();
            let files = crate::verdict::kit_files(source.to_str().unwrap(), None).unwrap();
            for file in files.iter().filter(|file| file.module != "kit.toml") {
                let (dir, cache) = scratch(&format!("{name}-{}", file.module.replace('.', "-")));
                let kit = dir.join("kit");
                copy(&source, &kit);
                if whole_fails(&kit) {
                    break;
                }
                keep(&kit, &cache);
                let shown = kit.join("src").join(format!("{}.fen", file.module.replace('.', "/")));
                let Ok(text) = std::fs::read_to_string(&shown) else { continue };
                std::fs::write(&shown, format!("# moved down a line\n{text}")).unwrap();
                if let Some(patched) = patch(&kit, &cache, &[file.module.as_str()]) {
                    assert_eq!(patched, cut(&kit), "{name}, {} moved", file.module);
                    patched_any += 1;
                }
            }
        }
        assert!(patched_any > 10, "only {patched_any} edits were patched");
    }

    fn whole_fails(dir: &Path) -> bool {
        fennec_kit::load(dir, dir.to_str().unwrap(), fennec_kit::Entry::Main).is_err()
    }

    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                copy(&path, &to.join(entry.file_name()));
            } else {
                std::fs::copy(&path, to.join(entry.file_name())).unwrap();
            }
        }
    }
}
