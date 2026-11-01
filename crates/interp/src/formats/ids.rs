//! Checking the `Id`s read from text against the `IdMap` they belong to. A program can write
//! an `Id` the map never handed out, by editing a save or by mistake; it's reported as the
//! text not fitting, rather than becoming a link that looks real. An `Id` of an item that's
//! since been removed is fine, as it is in a running program, since `get` finds nothing for it.

use fennec_syntax::TypeExpr;

use super::decode::{given_types, substituted, written};
use crate::interp::Interp;
use crate::value::{Id, IdMap, Value};

/// The `Id`s and `IdMap`s found, each with where it is and the type of item it's for.
#[derive(Default)]
struct Found {
    ids: Vec<(String, String, Id)>,
    maps: Vec<(String, String, IdMap)>,
}

impl Interp<'_> {
    /// Checks every `Id[T]` in `value` against the one `IdMap[T]` it holds. With no map of
    /// that kind, or more than one, there's nothing to say which an `Id` belongs to.
    pub(crate) fn check_ids(&self, value: &Value, ty: &TypeExpr) -> Result<(), String> {
        let mut found = Found::default();
        self.find(value, ty, "", &mut found, &mut Vec::new());
        for (at, item, id) in &found.ids {
            let mut maps = found.maps.iter().filter(|(_, kind, _)| kind == item);
            let (Some((map_at, _, map)), None) = (maps.next(), maps.next()) else { continue };
            let issued = map.slots.get(id.slot).is_some_and(|slot| id.generation <= slot.generation);
            if !issued {
                let map = if map_at.is_empty() { "the IdMap".to_owned() } else { format!("`{map_at}`") };
                return Err(format!(
                    "`{at}` is an Id that {map} never handed out: slot {}, generation {}",
                    id.slot, id.generation
                ));
            }
        }
        Ok(())
    }

    /// Collects the `Id`s and `IdMap`s inside `value`, which was read as a `ty`. `open` holds the
    /// structs being looked inside, so one that holds itself is looked at once at a time.
    fn find(&self, value: &Value, ty: &TypeExpr, at: &str, found: &mut Found, open: &mut Vec<String>) {
        let arg = |n: usize| ty.args.get(n);
        match (ty.name.as_str(), value) {
            ("Id", Value::Id(id)) => {
                let item = arg(0).map(written).unwrap_or_default();
                found.ids.push((name(at), item, *id));
            }
            ("IdMap", Value::IdMap(map)) => {
                let Some(item) = arg(0) else { return };
                for (n, slot) in map.slots.iter().enumerate() {
                    if let Some(held) = &slot.value {
                        self.find(held, item, &format!("{at}.slots[{n}].value"), found, open);
                    }
                }
                found.maps.push((name(at), written(item), map.as_ref().clone()));
            }
            ("list", Value::List(items)) => {
                let Some(item) = arg(0) else { return };
                for (n, held) in items.iter().enumerate() {
                    self.find(held, item, &format!("{at}[{n}]"), found, open);
                }
            }
            ("dict", Value::Dict(pairs)) => {
                let Some(item) = arg(1) else { return };
                for (key, held) in pairs.iter() {
                    self.find(held, item, &format!("{at}[{}]", crate::format::repr(key)), found, open);
                }
            }
            ("tuple", Value::Tuple(items)) => {
                for (n, (held, item)) in items.iter().zip(&ty.args).enumerate() {
                    self.find(held, item, &format!("{at}[{n}]"), found, open);
                }
            }
            (type_name, Value::Instance(instance)) if !open.iter().any(|seen| *seen == written(ty)) => {
                let Some(info) = self.type_info(type_name) else { return };
                let fields = match &instance.case {
                    Some(case) => {
                        info.def.cases.iter().find(|each| *each.name == **case).map(|each| &each.fields)
                    }
                    None => Some(&info.def.fields),
                };
                let given = given_types(&info.def.generics, ty);
                open.push(written(ty));
                for field in fields.into_iter().flatten() {
                    let (Some(field_ty), Some(held)) = (&field.ty, instance.field(&field.name)) else {
                        continue;
                    };
                    let inner = TypeExpr { optional: false, ..substituted(field_ty, &given) };
                    self.find(held, &inner, &format!("{at}.{}", field.name), found, open);
                }
                open.pop();
            }
            _ => {}
        }
    }
}

/// Where something is, without the leading dot of a field at the top.
fn name(at: &str) -> String {
    at.strip_prefix('.').unwrap_or(at).to_owned()
}
