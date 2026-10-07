//! `type_info` at compile time: the tables the C generator emits, built in
//! the interpreter's static memory from the same descriptions. Layouts are
//! Wid's own, which match C's for every type Wid lays out.

use wid_diagnostics::Span;

use super::memory::{Addr, Region};
use super::{Interp, Num, R};
use crate::type_info::describe;
use crate::types::{TyId, TyKind};

impl<'c> Interp<'c> {
    /// `type_info(T)`: the address of `T`'s table, as a `^TypeInfo` (`ptr_ty`).
    pub(super) fn type_info(&mut self, described: TyId, ptr_ty: TyId, span: Span) -> R<Vec<u8>> {
        let TyKind::Pointer(info) = *self.p.types.kind(ptr_ty) else {
            return Err(self.fail_at(span, "`type_info` needs the prelude's `TypeInfo`"));
        };
        let addr = self.type_info_addr(described, info)?;
        Ok(addr.to_le_bytes().to_vec())
    }

    /// The table of `ty`, built the first time. It is registered before it
    /// is filled, so recursive types point back at it.
    fn type_info_addr(&mut self, ty: TyId, info: TyId) -> R<Addr> {
        if let Some(&a) = self.type_infos.get(&ty) {
            return Ok(a);
        }
        let types = self.p.types;
        let (size, align) = types.layout(info);
        let addr = self.alloc_in(Region::Static, size, align)?;
        self.type_infos.insert(ty, addr);
        let d = describe(types, self.p.errors, ty);
        let TyKind::Struct(sid) = *types.kind(info) else { return Ok(addr) };
        for slot in &types.struct_info(sid).fields {
            let at = addr + slot.offset;
            let int = |v: i128| Num::I(v);
            match slot.name.as_str() {
                "name" => {
                    let s = self.string_value(slot.ty, d.name.as_bytes())?;
                    self.wr(at, &s)?;
                }
                "kind" => {
                    let value = match types.kind(slot.ty) {
                        TyKind::Enum(id) => {
                            types.enum_info(*id).members.iter().find(|(n, _)| n.as_str() == d.kind).map_or(0, |m| m.1)
                        }
                        _ => 0,
                    };
                    let bytes = self.encode(slot.ty, int(value));
                    self.wr(at, &bytes)?;
                }
                "size" | "align" => {
                    let (s, a) = d.layout.map_or((0, 0), |l| types.layout(l));
                    let v = if slot.name.as_str() == "size" { s } else { a };
                    let bytes = self.encode(slot.ty, int(i128::from(v)));
                    self.wr(at, &bytes)?;
                }
                "elem" | "key" => {
                    let target = if slot.name.as_str() == "elem" { d.elem } else { d.key };
                    if let Some(t) = target {
                        let p = self.type_info_addr(t, info)?;
                        self.wr_u64(at, p)?;
                    }
                }
                "count" | "columns" => {
                    let v = if slot.name.as_str() == "count" { d.count } else { d.columns };
                    let bytes = self.encode(slot.ty, int(i128::from(v)));
                    self.wr(at, &bytes)?;
                }
                "fields" | "members" | "variants" => {
                    let TyKind::Slice(elem) = *types.kind(slot.ty) else { continue };
                    let len = match slot.name.as_str() {
                        "fields" => d.fields.len(),
                        "members" => d.members.len(),
                        _ => d.variants.len(),
                    };
                    if len == 0 {
                        continue;
                    }
                    let (esize, ealign) = types.layout(elem);
                    let data = self.alloc_in(Region::Static, esize * len as u64, ealign)?;
                    for i in 0..len {
                        let item = data + i as u64 * esize;
                        match slot.name.as_str() {
                            "fields" => {
                                let f = &d.fields[i];
                                let target = self.type_info_addr(f.ty, info)?;
                                self.record(elem, item, |name| match name {
                                    "name" => Some(Value::Str(f.name.clone())),
                                    "type" => Some(Value::Ptr(target)),
                                    "offset" => Some(Value::Int(i128::from(f.offset))),
                                    _ => None,
                                })?;
                            }
                            "members" => {
                                let (name, value) = &d.members[i];
                                self.record(elem, item, |slot| match slot {
                                    "name" => Some(Value::Str(name.clone())),
                                    "value" => Some(Value::Int(i128::from(*value))),
                                    _ => None,
                                })?;
                            }
                            _ => {
                                let target = self.type_info_addr(d.variants[i], info)?;
                                self.wr_u64(item, target)?;
                            }
                        }
                    }
                    let mut bytes = data.to_le_bytes().to_vec();
                    bytes.extend_from_slice(&(len as u64).to_le_bytes());
                    self.wr(at, &bytes)?;
                }
                _ => {}
            }
        }
        Ok(addr)
    }

    /// Fills a prelude record (`TypeInfoField`, `TypeInfoMember`) at `addr`
    /// with each field's value by name.
    fn record(&mut self, ty: TyId, addr: Addr, value: impl Fn(&str) -> Option<Value>) -> R<()> {
        let types = self.p.types;
        let TyKind::Struct(id) = *types.kind(ty) else { return Ok(()) };
        for f in &types.struct_info(id).fields {
            let bytes = match value(f.name.as_str()) {
                Some(Value::Str(s)) => self.string_value(f.ty, s.as_bytes())?,
                Some(Value::Ptr(p)) => p.to_le_bytes().to_vec(),
                Some(Value::Int(v)) => self.encode(f.ty, Num::I(v)),
                None => continue,
            };
            self.wr(addr + f.offset, &bytes)?;
        }
        Ok(())
    }
}

/// A value stored into a table record.
enum Value {
    Str(String),
    Ptr(Addr),
    Int(i128),
}
