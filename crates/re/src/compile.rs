//! A syntax tree to a program for the Pike VM. Repeats are spelled out: `a{2,3}` becomes
//! `aa` followed by an optional `a`.

use crate::parse::{Class, Look, Node};

#[derive(Clone, Debug)]
pub(crate) enum Inst {
    Char(char),
    Any,
    Class(Class),
    Look(Look),
    /// Try both; the first is preferred.
    Split(usize, usize),
    Jump(usize),
    /// Note the current position in capture slot `n`.
    Save(usize),
    Match,
}

#[derive(Clone, Debug)]
pub(crate) struct Program {
    pub(crate) insts: Vec<Inst>,
    /// Groups that capture, not counting the whole match.
    pub(crate) groups: usize,
}

struct Compiler {
    insts: Vec<Inst>,
    limit: usize,
}

/// Compiles `node`, or gives back `None` if the program would pass `limit` instructions.
pub(crate) fn compile(node: &Node, groups: usize, limit: usize) -> Option<Program> {
    let mut compiler = Compiler { insts: Vec::new(), limit };
    compiler.push(Inst::Save(0))?;
    compiler.node(node)?;
    compiler.push(Inst::Save(1))?;
    compiler.push(Inst::Match)?;
    Some(Program { insts: compiler.insts, groups })
}

impl Compiler {
    fn push(&mut self, inst: Inst) -> Option<usize> {
        if self.insts.len() >= self.limit {
            return None;
        }
        self.insts.push(inst);
        Some(self.insts.len() - 1)
    }

    fn here(&self) -> usize {
        self.insts.len()
    }

    /// Points the jump or split at `at` to `to`; for a split, its second choice.
    fn patch(&mut self, at: usize, to: usize) {
        if let Some(Inst::Jump(target) | Inst::Split(_, target)) = self.insts.get_mut(at) {
            *target = to;
        }
    }

    fn node(&mut self, node: &Node) -> Option<()> {
        match node {
            Node::Empty => {}
            Node::Char(c) => drop(self.push(Inst::Char(*c))?),
            Node::Any => drop(self.push(Inst::Any)?),
            Node::Class(class) => drop(self.push(Inst::Class(class.clone()))?),
            Node::Look(look) => drop(self.push(Inst::Look(*look))?),
            Node::Group(number, inner) => match number {
                Some(number) => {
                    self.push(Inst::Save(number * 2))?;
                    self.node(inner)?;
                    self.push(Inst::Save(number * 2 + 1))?;
                }
                None => self.node(inner)?,
            },
            Node::Concat(parts) => {
                for part in parts {
                    self.node(part)?;
                }
            }
            Node::Alt(options) => self.alternation(options)?,
            Node::Repeat { node, min, max, greedy } => self.repeat(node, *min, *max, *greedy)?,
        }
        Some(())
    }

    /// `a|b|c`: a split before each option but the last, and a jump to the end after each.
    fn alternation(&mut self, options: &[Node]) -> Option<()> {
        let mut jumps = Vec::new();
        let count = options.len();
        for (at, option) in options.iter().enumerate() {
            if at + 1 < count {
                let split = self.push(Inst::Split(0, 0))?;
                self.set_first(split, split + 1);
                self.node(option)?;
                jumps.push(self.push(Inst::Jump(0))?);
                let next = self.here();
                self.patch(split, next);
            } else {
                self.node(option)?;
            }
        }
        let end = self.here();
        for jump in jumps {
            self.patch(jump, end);
        }
        Some(())
    }

    fn set_first(&mut self, at: usize, to: usize) {
        if let Some(Inst::Split(first, _)) = self.insts.get_mut(at) {
            *first = to;
        }
    }

    /// Aims a repeat's split: into the repeated part first when greedy, past it first when
    /// lazy.
    fn aim(&mut self, split: usize, body: usize, past: usize, greedy: bool) {
        if let Some(Inst::Split(first, second)) = self.insts.get_mut(split) {
            (*first, *second) = if greedy { (body, past) } else { (past, body) };
        }
    }

    fn repeat(&mut self, node: &Node, min: u32, max: Option<u32>, greedy: bool) -> Option<()> {
        for _ in 0..min {
            self.node(node)?;
        }
        match max {
            None => {
                let split = self.push(Inst::Split(0, 0))?;
                self.node(node)?;
                self.push(Inst::Jump(split))?;
                let past = self.here();
                self.aim(split, split + 1, past, greedy);
            }
            Some(max) => {
                // Each optional copy can be skipped, which skips the rest too.
                let mut splits = Vec::new();
                for _ in min..max {
                    let split = self.push(Inst::Split(0, 0))?;
                    splits.push(split);
                    self.node(node)?;
                }
                let past = self.here();
                for split in splits {
                    self.aim(split, split + 1, past, greedy);
                }
            }
        }
        Some(())
    }
}
