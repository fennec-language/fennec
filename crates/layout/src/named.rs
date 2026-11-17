//! Layout values with every type by its layout's name and every numbered block written out, so
//! one means the same in any build of a program, whatever numbers that build's layouts gave out.
//! A unit kept to be used again holds these (edit builds, stage 1): what it added to what units
//! share, and what it asked the layouts to number while it was written, which a build using it
//! again asks for again, in the same order, so every number comes out as writing it would give.

use crate::{Block, Cap, Layouts, ListItem, Ty, Word};

/// A [`Ty`], by names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Named {
    Int,
    Float,
    Bool,
    U64,
    F32,
    Unit,
    Cap(Cap),
    Obj,
    Struct(String),
    Boxed(String),
    Str,
    Bytes,
    Gen,
    Dict(NamedWord, NamedWord),
    /// A list of what its item is held as.
    List(Box<Named>),
    /// An optional held in one word, of what its block holds.
    Maybe(Box<Named>),
    /// The kind of block a number names, written out.
    Block(Box<Named>),
}

/// A dict's [`Word`], by names.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum NamedWord {
    Int,
    Float,
    Bool,
    Str,
    Nothing,
    Plain(String),
    Text,
    Block(Box<Named>),
    U64,
    F32,
    /// A plain type held in the entry, by its name.
    InPlace(String),
}

/// What the layouts were asked to number while they noted it (see [`Layouts::note_asked`]).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Asked {
    /// A kind of block, which a list's items or a dict's values go by.
    Block(Named),
    /// A plain type a dict holds in its entries.
    DictValue(String),
}

impl Layouts {
    /// `ty` by names, or `None` if it holds what has no name: a function known while compiling,
    /// which goes by a number of the program's, or a number these layouts never gave.
    #[must_use]
    pub fn named(&self, ty: Ty) -> Option<Named> {
        let name = |id: u32| self.get(id).map(|layout| layout.name.clone());
        Some(match ty {
            Ty::Int => Named::Int,
            Ty::Float => Named::Float,
            Ty::Bool => Named::Bool,
            Ty::U64 => Named::U64,
            Ty::F32 => Named::F32,
            Ty::Unit => Named::Unit,
            Ty::Cap(cap) => Named::Cap(cap),
            Ty::Obj => Named::Obj,
            Ty::Struct(id) => Named::Struct(name(id)?),
            Ty::Boxed(id) => Named::Boxed(name(id)?),
            Ty::Str => Named::Str,
            Ty::Bytes => Named::Bytes,
            Ty::Gen => Named::Gen,
            Ty::Dict(key, value) => Named::Dict(self.named_word(key)?, self.named_word(value)?),
            Ty::List(item) => Named::List(Box::new(self.named(item.ty())?)),
            Ty::Maybe(block) => Named::Maybe(Box::new(self.named(block.ty())?)),
            Ty::Block(number) => Named::Block(Box::new(self.named(self.block(number)?)?)),
            Ty::Known(_) => return None,
        })
    }

    /// A dict's word by names, or `None` as [`Self::named`] says.
    #[must_use]
    pub fn named_word(&self, word: Word) -> Option<NamedWord> {
        Some(match word {
            Word::Int => NamedWord::Int,
            Word::Float => NamedWord::Float,
            Word::Bool => NamedWord::Bool,
            Word::Str => NamedWord::Str,
            Word::Nothing => NamedWord::Nothing,
            Word::Plain(id) => NamedWord::Plain(self.get(id)?.name.clone()),
            Word::Text => NamedWord::Text,
            Word::Block(number) => NamedWord::Block(Box::new(self.named(self.block(number)?)?)),
            Word::U64 => NamedWord::U64,
            Word::F32 => NamedWord::F32,
            Word::InPlace(number) => NamedWord::InPlace(self.get(self.dict_value(number)?)?.name.clone()),
        })
    }

    /// What a named kind is in these layouts, numbering each block it holds that has no number
    /// yet, inner ones first, as writing what used it would have. `None` for a type these layouts
    /// don't have.
    #[must_use]
    pub fn resolved(&self, named: &Named) -> Option<Ty> {
        Some(match named {
            Named::Int => Ty::Int,
            Named::Float => Ty::Float,
            Named::Bool => Ty::Bool,
            Named::U64 => Ty::U64,
            Named::F32 => Ty::F32,
            Named::Unit => Ty::Unit,
            Named::Cap(cap) => Ty::Cap(*cap),
            Named::Obj => Ty::Obj,
            Named::Struct(name) => Ty::Struct(self.id(name)?),
            Named::Boxed(name) => Ty::Boxed(self.id(name)?),
            Named::Str => Ty::Str,
            Named::Bytes => Ty::Bytes,
            Named::Gen => Ty::Gen,
            Named::Dict(key, value) => Ty::Dict(self.resolved_word(key)?, self.resolved_word(value)?),
            Named::List(item) => Ty::List(ListItem::of(self.resolved(item)?)?),
            Named::Maybe(block) => Ty::Maybe(Block::of(self.resolved(block)?)?),
            Named::Block(kind) => Ty::Block(self.block_number(self.resolved(kind)?)?),
        })
    }

    /// What a named word is in these layouts, numbering as [`Self::resolved`] does.
    #[must_use]
    pub fn resolved_word(&self, word: &NamedWord) -> Option<Word> {
        Some(match word {
            NamedWord::Int => Word::Int,
            NamedWord::Float => Word::Float,
            NamedWord::Bool => Word::Bool,
            NamedWord::Str => Word::Str,
            NamedWord::Nothing => Word::Nothing,
            NamedWord::Plain(name) => Word::Plain(self.id(name)?),
            NamedWord::Text => Word::Text,
            NamedWord::Block(kind) => Word::Block(self.block_number(self.resolved(kind)?)?),
            NamedWord::U64 => Word::U64,
            NamedWord::F32 => Word::F32,
            NamedWord::InPlace(name) => Word::InPlace(self.dict_value_number(self.id(name)?)?),
        })
    }

    /// Asks again for every number in `asked`, in order, as a unit written again would: each
    /// kind found, or numbered if it has no number yet. `None` if one names a type these layouts
    /// don't have.
    #[must_use]
    pub fn ask_again(&self, asked: &[Asked]) -> Option<()> {
        for each in asked {
            match each {
                Asked::Block(kind) => {
                    let ty = self.resolved(kind)?;
                    self.block_number(ty)?;
                }
                Asked::DictValue(name) => {
                    self.dict_value_number(self.id(name)?)?;
                }
            }
        }
        Some(())
    }
}
