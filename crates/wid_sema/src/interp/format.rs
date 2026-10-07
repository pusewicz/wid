//! Text formatting for `puts`, `p`, interpolation and builders, matching the
//! runtime's `wid_w_*` functions byte for byte.

use super::{Interp, R, i64_at, slice, word};
use crate::types::{FloatTy, TyId, TyKind};

impl Interp<'_> {
    /// Writes a value the way `puts` (or `p`, with `inspect`) prints it.
    pub(super) fn format(&self, ty: TyId, v: &[u8], inspect: bool, out: &mut Vec<u8>) -> R<()> {
        let types = self.p.types;
        let base = types.base(ty);
        match types.kind(base) {
            TyKind::String => {
                let bytes = self.string_bytes(v)?;
                if inspect {
                    inspect_bytes(&bytes, b'"', out);
                } else {
                    out.extend_from_slice(&bytes);
                }
            }
            TyKind::CString => {
                let p = Self::ptr(v);
                if p != 0 {
                    out.extend(self.mem.read_cstr(p).map_err(|e| self.mem_fail(e, Default::default()))?);
                }
            }
            TyKind::Bool => out.extend_from_slice(if Self::truthy(v) { b"true" } else { b"false" }),
            TyKind::Rune => {
                let r = i32::from_le_bytes(word(v));
                if inspect {
                    out.push(b'\'');
                    if (0..0x80).contains(&r) {
                        inspect_bytes_inner(&[r as u8], b'\'', out);
                    } else {
                        push_rune(r, out);
                    }
                    out.push(b'\'');
                } else {
                    push_rune(r, out);
                }
            }
            TyKind::Float(f) => {
                let x = match f {
                    FloatTy::F32 => f64::from(f32::from_le_bytes(word(v))),
                    FloatTy::F64 => f64::from_le_bytes(word(v)),
                };
                out.extend_from_slice(ruby_float(x, *f == FloatTy::F32).as_bytes());
            }
            TyKind::Int(i) => {
                let n = self.int(base, v);
                let text = if i.signed() { n.to_string() } else { (n as u128 & u128::from(u64::MAX)).to_string() };
                out.extend_from_slice(text.as_bytes());
            }
            TyKind::Pointer(_) | TyKind::MultiPointer(_) | TyKind::RawPtr => push_ptr(Self::ptr(v), out),
            TyKind::Optional(_) if types.optional_is_pointer(base) => push_ptr(Self::ptr(v), out),
            TyKind::Unknown | TyKind::Void | TyKind::Never | TyKind::Nil | TyKind::TypeValue(_) => {}
            TyKind::Symbol => {
                let name = u32::try_from(u64::from_le_bytes(word(v))).ok().and_then(wid_syntax::Name::from_index);
                if inspect {
                    out.push(b':');
                }
                out.extend_from_slice(name.map_or("?", |n| n.as_str()).as_bytes());
            }
            TyKind::Struct(id) => {
                let info = types.struct_info(*id);
                out.extend_from_slice(info.name.as_bytes());
                out.push(b'(');
                for (i, f) in info.fields.iter().enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    out.extend_from_slice(f.name.as_str().as_bytes());
                    out.extend_from_slice(b": ");
                    self.format(f.ty, &slice(v, f.offset, types.size_of(f.ty)), true, out)?;
                }
                out.push(b')');
            }
            TyKind::Enum(id) => {
                let info = types.enum_info(*id);
                let n = self.int(base, v);
                match info.members.iter().find(|(_, val)| *val == n) {
                    Some((name, _)) => {
                        if inspect {
                            out.push(b':');
                        }
                        out.extend_from_slice(name.as_str().as_bytes());
                    }
                    None => out.extend_from_slice(format!("{}({n})", info.name).as_bytes()),
                }
            }
            TyKind::Array(elem, n) => {
                let s = types.size_of(*elem);
                out.push(b'[');
                for i in 0..*n {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    self.format(*elem, &slice(v, i * s, s), true, out)?;
                }
                out.push(b']');
            }
            TyKind::Matrix(elem, rows, cols) => {
                let s = types.size_of(*elem);
                let (rows, cols) = (u64::from(*rows), u64::from(*cols));
                out.extend_from_slice(b"matrix[");
                for row in 0..rows {
                    if row > 0 {
                        out.extend_from_slice(b", ");
                    }
                    out.push(b'[');
                    for col in 0..cols {
                        if col > 0 {
                            out.extend_from_slice(b", ");
                        }
                        self.format(*elem, &slice(v, (col * rows + row) * s, s), true, out)?;
                    }
                    out.push(b']');
                }
                out.push(b']');
            }
            TyKind::Slice(elem) | TyKind::Dynamic(elem) => {
                let s = types.size_of(*elem);
                let (data, len) = (Self::ptr(v), i64_at(v, 8).max(0) as u64);
                let bytes = self.rd(data, len * s)?;
                out.push(b'[');
                for i in 0..len {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    self.format(*elem, &slice(&bytes, i * s, s), true, out)?;
                }
                out.push(b']');
            }
            TyKind::Map(k, val) => {
                let (ks, vs) = (types.size_of(*k), types.size_of(*val));
                let (ka, va) = (types.align_of(*k).max(1), types.align_of(*val).max(1));
                let align = ka.max(va).max(8);
                let key_off = 16u64.div_ceil(ka) * ka;
                let val_off = (key_off + ks).div_ceil(va) * va;
                let slot_size = (val_off + vs).div_ceil(align) * align;
                let (slots, cap) = (Self::ptr(v), i64_at(v, 16).max(0) as u64);
                out.push(b'{');
                let mut first = true;
                for i in 0..cap {
                    let slot = self.rd(slots + i * slot_size, slot_size)?;
                    if slot[0] != 1 {
                        continue;
                    }
                    if !first {
                        out.extend_from_slice(b", ");
                    }
                    first = false;
                    self.format(*k, &slice(&slot, key_off, ks), true, out)?;
                    out.extend_from_slice(b" => ");
                    self.format(*val, &slice(&slot, val_off, vs), true, out)?;
                }
                out.push(b'}');
            }
            TyKind::Tuple(elems) => {
                let parts: Vec<(u64, u64)> = elems.iter().map(|e| types.layout(*e)).collect();
                out.push(b'(');
                for (i, (e, off)) in elems.iter().zip(crate::types::offsets(&parts)).enumerate() {
                    if i > 0 {
                        out.extend_from_slice(b", ");
                    }
                    self.format(*e, &slice(v, off, types.size_of(*e)), true, out)?;
                }
                out.push(b')');
            }
            TyKind::Union(id) => {
                let tag = u32::from_le_bytes(word(v));
                let variants = &types.union_info(*id).variants;
                match tag.checked_sub(1).and_then(|i| variants.get(i as usize)) {
                    Some(&vt) => {
                        let at = self.union_payload(base);
                        self.format(vt, &slice(v, at, types.size_of(vt)), inspect, out)?;
                    }
                    None => out.extend_from_slice(b"nil"),
                }
            }
            TyKind::Optional(inner) => {
                let s = types.size_of(*inner);
                if v.get(s as usize).copied().unwrap_or(0) == 0 {
                    out.extend_from_slice(b"nil");
                } else {
                    self.format(*inner, &slice(v, 0, s), inspect, out)?;
                }
            }
            TyKind::Error => {
                let n = u32::from_le_bytes(word(v));
                if n == 0 {
                    out.extend_from_slice(b"nil");
                } else if let Some(name) = self.p.errors.get(n as usize - 1) {
                    if inspect {
                        out.push(b':');
                    }
                    out.extend_from_slice(name.as_str().as_bytes());
                } else {
                    out.extend_from_slice(format!("Error({n})").as_bytes());
                }
            }
            TyKind::Type => {
                let t = TyId(u64::from_le_bytes(word(v)) as u32);
                let name = if (t.0 as usize) < types.len() { types.display(t) } else { "?".into() };
                out.extend_from_slice(name.as_bytes());
            }
            _ => out.extend_from_slice(format!("<{}>", types.display(ty)).as_bytes()),
        }
        Ok(())
    }
}

fn push_ptr(p: u64, out: &mut Vec<u8>) {
    if p == 0 {
        out.extend_from_slice(b"nil");
    } else {
        out.extend_from_slice(format!("{p:#x}").as_bytes());
    }
}

fn push_rune(r: i32, out: &mut Vec<u8>) {
    let c = r as u32;
    let mut buf = [0u8; 4];
    let n = if c < 0x80 {
        buf[0] = c as u8;
        1
    } else if c < 0x800 {
        buf[0] = (0xC0 | (c >> 6)) as u8;
        buf[1] = (0x80 | (c & 0x3F)) as u8;
        2
    } else if c < 0x10000 {
        buf[0] = (0xE0 | (c >> 12)) as u8;
        buf[1] = (0x80 | ((c >> 6) & 0x3F)) as u8;
        buf[2] = (0x80 | (c & 0x3F)) as u8;
        3
    } else {
        buf[0] = (0xF0 | (c >> 18)) as u8;
        buf[1] = (0x80 | ((c >> 12) & 0x3F)) as u8;
        buf[2] = (0x80 | ((c >> 6) & 0x3F)) as u8;
        buf[3] = (0x80 | (c & 0x3F)) as u8;
        4
    };
    out.extend_from_slice(&buf[..n]);
}

/// Writes bytes as a quoted, escaped literal, like `wid_w_str_inspect`.
fn inspect_bytes(bytes: &[u8], quote: u8, out: &mut Vec<u8>) {
    out.push(quote);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' && bytes.get(i + 1) == Some(&b'{') {
            out.extend_from_slice(b"\\#");
            i += 1;
            continue;
        }
        inspect_bytes_inner(&bytes[i..=i], quote, out);
        i += 1;
    }
    out.push(quote);
}

fn inspect_bytes_inner(bytes: &[u8], quote: u8, out: &mut Vec<u8>) {
    for &c in bytes {
        match c {
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x1B => out.extend_from_slice(b"\\e"),
            0 => out.extend_from_slice(b"\\0"),
            c if c == quote => {
                out.push(b'\\');
                out.push(c);
            }
            c if c < 0x20 || c == 0x7F => out.extend_from_slice(format!("\\x{c:02X}").as_bytes()),
            c => out.push(c),
        }
    }
}

/// `%.*e` the way C prints it: an exponent with a sign and at least two digits.
fn c_exp(v: f64, prec: usize) -> String {
    let s = format!("{v:.prec$e}");
    let Some((mantissa, exp)) = s.split_once('e') else { return s };
    let (sign, digits) = match exp.strip_prefix('-') {
        Some(d) => ('-', d),
        None => ('+', exp),
    };
    format!("{mantissa}e{sign}{digits:0>2}")
}

/// Formats a float the way Ruby prints it, like the runtime's
/// `wid_format_float_`: the shortest digits that round-trip, in fixed
/// notation for exponents from -4 to 14 and scientific notation otherwise,
/// always with a decimal point.
pub(super) fn ruby_float(v: f64, single: bool) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    let max = if single { 9 } else { 17 };
    let mut prec = 0;
    let mut sci = c_exp(v, 0);
    while prec < max {
        sci = c_exp(v, prec);
        let back: f64 = sci.parse().unwrap_or(f64::NAN);
        let ok = if single { back as f32 == v as f32 } else { back == v };
        if ok {
            break;
        }
        prec += 1;
    }
    let exp: i32 = sci.split_once('e').and_then(|(_, e)| e.parse().ok()).unwrap_or(0);
    if (-4..15).contains(&exp) {
        let decimals = (prec as i32 - exp).max(1) as usize;
        return format!("{v:.decimals$}");
    }
    if prec == 0
        && let Some((m, e)) = sci.split_once('e')
    {
        return format!("{m}.0e{e}");
    }
    sci
}

/// `%.*g` the way C prints it.
fn c_general(v: f64, prec: usize) -> String {
    let p = prec.max(1);
    let sci = c_exp(v, p - 1);
    let exp: i32 = sci.split_once('e').and_then(|(_, e)| e.parse().ok()).unwrap_or(0);
    let text = if exp >= -4 && exp < p as i32 {
        let decimals = (p as i32 - 1 - exp).max(0) as usize;
        format!("{v:.decimals$}")
    } else {
        sci
    };
    let (mantissa, exp_part) = match text.split_once('e') {
        Some((m, e)) => (m.to_string(), format!("e{e}")),
        None => (text.clone(), String::new()),
    };
    let mantissa = if mantissa.contains('.') {
        mantissa.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        mantissa
    };
    format!("{mantissa}{exp_part}")
}

/// `wid_fmt_float`: a float with a precision and a style (`f`, `e`, `g`).
pub(super) fn fmt_float(v: f64, precision: i64, style: u8) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v < 0.0 { "-Infinity".into() } else { "Infinity".into() };
    }
    if precision < 0 && style != b'e' {
        return ruby_float(v, false);
    }
    let p = if precision < 0 { 16 } else { precision.min(100) as usize };
    match style {
        b'e' => c_exp(v, p),
        b'g' => c_general(v, p),
        _ => format!("{v:.p$}"),
    }
}

/// `wid_parse_float`: the whole text as a float, or `None`.
pub(super) fn parse_float(s: &[u8]) -> Option<f64> {
    if s.is_empty() || s.len() >= 128 || matches!(s[0], b' ' | b'\t' | b'\n') {
        return None;
    }
    let text = std::str::from_utf8(s).ok()?;
    let v: f64 = text.parse().ok()?;
    let lower = text.to_ascii_lowercase();
    if v.is_infinite() && !lower.contains("inf") {
        return None;
    }
    Some(v)
}
