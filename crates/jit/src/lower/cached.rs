//! Reusing machine code from the cache. Each function is still lowered every run, which is
//! quick; its machine code, most of the work, is looked up by a key made from its lowered IR,
//! the target and the Fennec version. A function found there skips code generation, and one
//! that isn't is compiled as usual and stored.
//!
//! Stored code refers to other functions and to data by symbol name; loading it maps each name
//! back to this run's declaration of it.

use std::collections::HashMap;

use cranelift_codegen::binemit::Reloc;
use cranelift_codegen::ir::Function as Ir;
use cranelift_codegen::isa::TargetIsa;
use cranelift_module::{FuncId, Module, ModuleReloc, ModuleRelocTarget};
use fennec_build::{Cache, CompiledFunction, FunctionKey, Relocation};

use super::Failure;
use super::codegen::{MachineCode, canonical_ir, compile_all};
use crate::types::Function;

/// Every kind of relocation, numbered by place, which is how a stored relocation records it.
const KINDS: [Reloc; 30] = [
    Reloc::Abs4,
    Reloc::Abs8,
    Reloc::X86PCRel4,
    Reloc::X86CallPCRel4,
    Reloc::X86CallPLTRel4,
    Reloc::X86GOTPCRel4,
    Reloc::X86SecRel,
    Reloc::Arm32Call,
    Reloc::Arm64Call,
    Reloc::S390xPCRel32Dbl,
    Reloc::S390xPLTRel32Dbl,
    Reloc::MachOAarch64TlsAdrPage21,
    Reloc::MachOAarch64TlsAdrPageOff12,
    Reloc::Aarch64TlsDescAdrPage21,
    Reloc::Aarch64TlsDescLd64Lo12,
    Reloc::Aarch64TlsDescAddLo12,
    Reloc::Aarch64TlsDescCall,
    Reloc::Aarch64AdrGotPage21,
    Reloc::Aarch64AdrPrelPgHi21,
    Reloc::Aarch64AddAbsLo12Nc,
    Reloc::Aarch64Ld64GotLo12Nc,
    Reloc::RiscvCallPlt,
    Reloc::RiscvTlsGdHi20,
    Reloc::RiscvPCRelLo12I,
    Reloc::RiscvGotHi20,
    Reloc::RiscvPCRelHi20,
    Reloc::S390xTlsGd64,
    Reloc::S390xTlsGdCall,
    Reloc::PulleyPcRel,
    Reloc::PulleyCallIndirectHost,
];

/// How many functions a pack that's added to may keep, so old versions of them don't pile up.
const MOST_MERGED: usize = 1 << 17;

/// Everything code generation depends on besides the IR: the CPU and every setting.
fn target(isa: &dyn TargetIsa) -> String {
    let features: Vec<String> = isa.isa_flags().iter().map(ToString::to_string).collect();
    format!("{}\n{}\n{}", isa.triple(), isa.flags(), features.join(" "))
}

/// This run's declarations, by name and by number, to turn relocations into names and back.
struct Names {
    by_number: HashMap<(u32, u32), String>,
    by_name: HashMap<String, (u32, u32)>,
}

impl Names {
    fn of(module: &dyn Module) -> Self {
        let declarations = module.declarations();
        let functions = declarations.get_functions().map(|(id, decl)| ((0, id.as_u32()), decl.name.clone()));
        let data = declarations.get_data_objects().map(|(id, decl)| ((1, id.as_u32()), decl.name.clone()));
        let mut names = Names { by_number: HashMap::new(), by_name: HashMap::new() };
        for (number, name) in functions.chain(data) {
            if let Some(name) = name {
                names.by_name.insert(name.clone(), number);
                names.by_number.insert(number, name);
            }
        }
        names
    }

    /// The code as stored, if every relocation refers to something by name.
    fn store(&self, code: &MachineCode) -> Option<CompiledFunction> {
        let relocations = code
            .relocs
            .iter()
            .map(|reloc| {
                let ModuleRelocTarget::User { namespace, index } = reloc.name else { return None };
                Some(Relocation {
                    offset: reloc.offset,
                    kind: u16::try_from(KINDS.iter().position(|kind| *kind == reloc.kind)?).ok()?,
                    symbol: self.by_number.get(&(namespace, index))?.clone(),
                    addend: reloc.addend,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(CompiledFunction { code: code.bytes.clone(), alignment: code.alignment, relocations })
    }

    /// Stored code ready to install, if this run declares everything it refers to.
    fn load(&self, stored: CompiledFunction) -> Option<MachineCode> {
        let relocs = stored
            .relocations
            .into_iter()
            .map(|relocation| {
                let &(namespace, index) = self.by_name.get(&relocation.symbol)?;
                Some(ModuleReloc {
                    offset: relocation.offset,
                    kind: *KINDS.get(usize::from(relocation.kind))?,
                    name: ModuleRelocTarget::User { namespace, index },
                    addend: relocation.addend,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(MachineCode { bytes: stored.code, alignment: stored.alignment, relocs })
    }
}

/// Where a run's machine code is reused from: the cache, and the program it's a run of.
pub(crate) struct CacheUse {
    pub(crate) cache: Cache,
    pub(crate) program: String,
    /// Whether to add to the pack rather than replace it. Several workers running different
    /// tests share one pack, and none of them compiles the whole program.
    pub(crate) merge: bool,
}

impl CacheUse {
    pub(crate) fn of(cache: Option<(&Cache, &str)>, merge: bool) -> Option<Self> {
        cache.map(|(cache, program)| CacheUse { cache: cache.clone(), program: program.to_owned(), merge })
    }
}

/// Generates and installs machine code for every lowered function, reusing what the program's
/// pack in the cache has, then stores this run's functions as its pack. Returns how many
/// functions came from the cache.
pub(super) fn generate(
    module: &mut dyn Module,
    lowered: Vec<(FuncId, Ir, &Function<'_>)>,
    cache: Option<&CacheUse>,
) -> Result<usize, Failure> {
    let Some(CacheUse { cache, program, merge }) = cache else {
        for (id, code) in compile_all(module.isa(), lowered)? {
            code.install(module, id)?;
        }
        return Ok(0);
    };
    let target = target(module.isa());
    let names = Names::of(&*module);
    let mut pack = cache.pack(program);
    let mut kept = Vec::with_capacity(lowered.len());
    let mut keys = HashMap::new();
    let mut misses = Vec::new();
    for (id, ir, function) in lowered {
        let key = FunctionKey::new(&target).ir(&canonical_ir(&*module, &ir)).finish();
        let found = pack.remove(&key).and_then(|stored| Some((names.load(stored.clone())?, stored)));
        if let Some((code, stored)) = found {
            code.install(module, id)?;
            kept.push((key, stored));
        } else {
            keys.insert(id, key);
            misses.push((id, ir, function));
        }
    }
    let found = kept.len();
    for (id, code) in compile_all(module.isa(), misses)? {
        if let (Some(key), Some(stored)) = (keys.get(&id), names.store(&code)) {
            kept.push((*key, stored));
        }
        code.install(module, id)?;
    }
    if found < kept.len() {
        if *merge {
            let have: std::collections::HashSet<_> = kept.iter().map(|(key, _)| *key).collect();
            let others = cache.pack(program).into_iter().filter(|(key, _)| !have.contains(key));
            kept.extend(others.take(MOST_MERGED.saturating_sub(have.len())));
        }
        // A cache that can't be written only means compiling again next time.
        let _ = cache.store_pack(program, &kept);
    }
    Ok(found)
}
