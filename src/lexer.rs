//! A small PDF object lexer and parser. Bounded: nesting depth is limited, and nothing here recurses on
//! untrusted structure except `parse_obj`, which stops at `MAX_NEST`. The items of every array and dictionary
//! are counted while they are alive ([`Items`]), and parsing stops once the call's limit on them is passed.

use std::cell::Cell;

use crate::meter::Meter;
use std::rc::Rc;

/// Deepest allowed nesting of arrays and dictionaries.
const MAX_NEST: usize = 64;

/// With the `force-panic` feature, parsing this name as a value panics.
#[cfg(feature = "force-panic")]
const FORCE_PANIC_NAME: &[u8] = b"PdfProbeForcePanic";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Rc<Vec<u8>>),
    Str(Rc<Vec<u8>>),
    /// Parsed values are immutable and shared, so resolving inherited values only copies pointers.
    Array(Rc<Items<Obj>>),
    Dict(Dict),
    Ref(u32, u16),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Dict(pub Rc<Items<(Vec<u8>, Obj)>>);

// A small compressed stream can inflate to millions of parsed values, so the size of `Obj` bounds peak memory.
// Thin `Rc<..>` keeps it at 16 bytes; `Rc<[..]>` is a fat pointer and makes it 24.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(std::mem::size_of::<Obj>() == 16);

thread_local! {
    /// Items of arrays and dictionaries alive on this thread. Parsed objects hold `Rc`s, so they never leave
    /// the thread, and none outlive the call that parsed them.
    static LIVE: Cell<usize> = const { Cell::new(0) };
}

/// The items of arrays and dictionaries alive on this thread.
pub(crate) fn live_items() -> usize {
    LIVE.with(Cell::get)
}

fn add_live(n: usize) {
    LIVE.with(|live| live.set(live.get().saturating_add(n)));
}

/// Counts one more item alive if that keeps the count since `base` within `max`; `false` otherwise.
pub(crate) fn add_live_within(base: usize, max: usize) -> bool {
    LIVE.with(|live| {
        let n = live.get();
        let ok = n.saturating_sub(base) < max;
        if ok {
            live.set(n + 1);
        }
        ok
    })
}

/// The items of an array or dictionary. They count as alive from when they are added until the container is
/// dropped, wherever that happens, so the limit on parsed values ([`Meter`]) measures memory in use and no
/// caller has to give anything back. Arrays and dictionaries share their items through an `Rc`, so cloning
/// one copies a pointer and counts nothing; a clone of the items themselves counts again.
#[derive(Debug, PartialEq)]
pub(crate) struct Items<T>(Vec<T>);

impl<T> Default for Items<T> {
    fn default() -> Self {
        Items(Vec::new())
    }
}

impl<T> Items<T> {
    /// Adds `item` if the meter allows one more value alive; `false` (and nothing added) otherwise.
    pub fn push(&mut self, item: T, meter: &Meter) -> bool {
        if !meter.take_value() {
            return false;
        }
        self.0.push(item);
        true
    }
}

/// Items built by the library itself rather than parsed: counted, but not checked against the limit.
impl<T> From<Vec<T>> for Items<T> {
    fn from(items: Vec<T>) -> Self {
        add_live(items.len());
        Items(items)
    }
}

impl<T: Clone> Clone for Items<T> {
    fn clone(&self) -> Self {
        Items::from(self.0.clone())
    }
}

impl<T> Drop for Items<T> {
    fn drop(&mut self) {
        let n = self.0.len();
        if n > 0 {
            LIVE.with(|live| live.set(live.get().saturating_sub(n)));
        }
    }
}

impl<T> std::ops::Deref for Items<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}

impl Dict {
    /// The last entry wins when a key is repeated, as in pdf.js.
    pub fn get(&self, key: &[u8]) -> Option<&Obj> {
        self.0.iter().rev().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

impl Obj {
    /// An integer, accepting a real with no fractional part (pdf.js does: `2.0` is `2` in JavaScript).
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Obj::Int(i) => Some(*i),
            Obj::Real(r) if r.fract() == 0.0 && r.abs() < 9.0e15 => Some(*r as i64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Obj::Int(i) => Some(*i as f64),
            Obj::Real(r) if r.is_finite() => Some(*r),
            _ => None,
        }
    }
}

fn is_ws(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

/// An indirect object, with the location of its stream data if it has one.
pub(crate) struct Indirect {
    pub num: u32,
    pub gen_: u16,
    pub obj: Obj,
    pub stream: Option<std::ops::Range<usize>>,
}

pub(crate) struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
    /// Checks the limit on items alive for every item added, so that parsing stops before a huge array or
    /// dictionary exhausts memory.
    meter: &'a Meter,
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], pos: usize, meter: &'a Meter) -> Self {
        Lexer { data, pos, meter }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    pub fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    if c == 10 || c == 13 {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// Consumes `kw` if it is next (after whitespace) and is followed by a delimiter, whitespace or the end.
    pub fn eat_keyword(&mut self, kw: &[u8]) -> bool {
        self.skip_ws();
        if !self.data[self.pos.min(self.data.len())..].starts_with(kw) {
            return false;
        }
        match self.data.get(self.pos + kw.len()) {
            None => {}
            Some(&b) if is_ws(b) || is_delim(b) => {}
            Some(_) => return false,
        }
        self.pos += kw.len();
        true
    }

    /// An unsigned decimal integer token; `None` on anything else or on overflow.
    pub fn read_uint(&mut self) -> Option<u64> {
        self.skip_ws();
        let start = self.pos;
        let mut n: u64 = 0;
        while let Some(b @ b'0'..=b'9') = self.peek() {
            n = n.checked_mul(10)?.checked_add(u64::from(b - b'0'))?;
            self.pos += 1;
        }
        (self.pos > start).then_some(n)
    }

    pub fn parse_obj(&mut self) -> Option<Obj> {
        self.parse_nested(0)
    }

    fn parse_nested(&mut self, depth: usize) -> Option<Obj> {
        if depth > MAX_NEST {
            return None;
        }
        self.skip_ws();
        match self.peek()? {
            b'/' => {
                self.pos += 1;
                let name = self.read_name();
                // Test builds only (see the feature in Cargo.toml): a real panic in the middle of parsing,
                // with the items parsed so far still alive, so the hosts' panic handling can be tested.
                #[cfg(feature = "force-panic")]
                if name == FORCE_PANIC_NAME {
                    panic!("pdf-probe: forced panic (the force-panic feature is on)");
                }
                Some(Obj::Name(Rc::new(name)))
            }
            b'(' => self.read_literal_string().map(|s| Obj::Str(Rc::new(s))),
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    self.read_dict_body(depth).map(Obj::Dict)
                } else {
                    self.read_hex_string().map(|s| Obj::Str(Rc::new(s)))
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Items::default();
                loop {
                    self.skip_ws();
                    if self.peek()? == b']' {
                        self.pos += 1;
                        break;
                    }
                    let item = self.parse_nested(depth + 1)?;
                    if !items.push(item, self.meter) {
                        return None;
                    }
                }
                Some(Obj::Array(Rc::new(items)))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => self.read_number_or_ref(),
            _ => {
                if self.eat_keyword(b"true") {
                    Some(Obj::Bool(true))
                } else if self.eat_keyword(b"false") {
                    Some(Obj::Bool(false))
                } else if self.eat_keyword(b"null") {
                    Some(Obj::Null)
                } else {
                    None
                }
            }
        }
    }

    fn read_dict_body(&mut self, depth: usize) -> Option<Dict> {
        let mut entries = Items::default();
        loop {
            self.skip_ws();
            if self.data[self.pos.min(self.data.len())..].starts_with(b">>") {
                self.pos += 2;
                return Some(Dict(Rc::new(entries)));
            }
            // Keys are owned by the dictionary and never resolved, so they need no Rc allocation.
            if depth >= MAX_NEST || self.peek()? != b'/' {
                return None;
            }
            self.pos += 1;
            let key = self.read_name();
            let value = self.parse_nested(depth + 1)?;
            if !entries.push((key, value), self.meter) {
                return None;
            }
        }
    }

    fn read_name(&mut self) -> Vec<u8> {
        let mut name = Vec::new();
        while let Some(b) = self.peek() {
            if is_ws(b) || is_delim(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let hi = self.peek().and_then(hex_val);
                let lo = self.data.get(self.pos + 1).copied().and_then(hex_val);
                if let (Some(h), Some(l)) = (hi, lo) {
                    name.push(h * 16 + l);
                    self.pos += 2;
                    continue;
                }
            }
            name.push(b);
        }
        name
    }

    fn read_literal_string(&mut self) -> Option<Vec<u8>> {
        self.pos += 1; // (
        let mut out = Vec::new();
        let mut depth = 1usize;
        loop {
            let b = self.peek()?;
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(out);
                    }
                    out.push(b);
                }
                b'\\' => {
                    let e = self.peek()?;
                    self.pos += 1;
                    match e {
                        b'n' => out.push(10),
                        b'r' => out.push(13),
                        b't' => out.push(9),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        10 => {}
                        13 => {
                            if self.peek() == Some(10) {
                                self.pos += 1;
                            }
                        }
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
    }

    fn read_hex_string(&mut self) -> Option<Vec<u8>> {
        self.pos += 1; // <
        let mut out = Vec::new();
        let mut half: Option<u8> = None;
        loop {
            let b = self.peek()?;
            self.pos += 1;
            if b == b'>' {
                if let Some(h) = half {
                    out.push(h * 16);
                }
                return Some(out);
            }
            if is_ws(b) {
                continue;
            }
            let v = hex_val(b)?;
            match half.take() {
                Some(h) => out.push(h * 16 + v),
                None => half = Some(v),
            }
        }
    }

    fn read_number_or_ref(&mut self) -> Option<Obj> {
        let start = self.pos;
        while let Some(b) = self.peek() {
            if matches!(b, b'+' | b'-' | b'.' | b'0'..=b'9') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let token = std::str::from_utf8(&self.data[start..self.pos]).ok()?;
        if token.contains('.') {
            return token.parse::<f64>().ok().map(Obj::Real);
        }
        let Ok(n) = token.parse::<i64>() else {
            return token.parse::<f64>().ok().map(Obj::Real);
        };
        // An unsigned integer may start an indirect reference: `12 0 R`.
        if let (Ok(num), false) = (u32::try_from(n), token.starts_with(['+', '-'])) {
            let save = self.pos;
            self.skip_ws();
            if let Some(gen_) = self.read_uint() {
                self.skip_ws();
                let followed = self.data.get(self.pos + 1).copied();
                if self.peek() == Some(b'R')
                    && followed.is_none_or(|b| is_ws(b) || is_delim(b))
                    && let Ok(g) = u16::try_from(gen_)
                {
                    self.pos += 1;
                    return Some(Obj::Ref(num, g));
                }
            }
            self.pos = save;
        }
        Some(Obj::Int(n))
    }

    /// Like [`parse_indirect`](Self::parse_indirect), but also locates the data of a stream object.
    pub fn parse_indirect_full(&mut self) -> Option<Indirect> {
        let (num, gen_, obj) = self.parse_indirect()?;
        let mut stream = None;
        if let Obj::Dict(d) = &obj
            && self.eat_keyword(b"stream")
        {
            stream = Some(self.stream_range(d)?);
        }
        Some(Indirect {
            num,
            gen_,
            obj,
            stream,
        })
    }

    /// The byte range of the stream data that starts after the `stream` keyword. The declared `/Length` is
    /// used when `endstream` follows it; otherwise the data runs up to the next `endstream`.
    fn stream_range(&mut self, dict: &Dict) -> Option<std::ops::Range<usize>> {
        let mut p = self.pos;
        while self.data.get(p) == Some(&b' ') {
            p += 1;
        }
        match self.data.get(p) {
            Some(b'\r') => {
                p += 1;
                if self.data.get(p) == Some(&b'\n') {
                    p += 1;
                }
            }
            Some(b'\n') => p += 1,
            _ => p = self.pos,
        }
        let start = p;
        let declared = dict
            .get(b"Length")
            .and_then(Obj::as_int)
            .and_then(|n| usize::try_from(n).ok());
        if let Some(end) = declared.and_then(|n| start.checked_add(n))
            && end <= self.data.len()
            && self.endstream_at(end)
        {
            return Some(start..end);
        }
        let key = b"endstream";
        let tail = &self.data[start..];
        let mut search = 0;
        let rel = loop {
            let Some(i) = tail[search..].iter().position(|&b| b == b'e') else {
                self.pos = self.data.len();
                return None;
            };
            search += i;
            if tail[search..].starts_with(key) {
                break search;
            }
            search += 1;
        };
        let mut end = start + rel;
        if end > start && self.data[end - 1] == b'\n' {
            end -= 1;
        }
        if end > start && self.data[end - 1] == b'\r' {
            end -= 1;
        }
        Some(start..end)
    }

    fn endstream_at(&self, mut at: usize) -> bool {
        while self.data.get(at).is_some_and(|&b| is_ws(b)) {
            at += 1;
        }
        self.data[at.min(self.data.len())..].starts_with(b"endstream")
    }

    /// `N G obj <object>`: the object number and the object. Any stream that follows is ignored.
    pub fn parse_indirect(&mut self) -> Option<(u32, u16, Obj)> {
        let num = u32::try_from(self.read_uint()?).ok()?;
        let gen_ = u16::try_from(self.read_uint()?).ok()?;
        if !self.eat_keyword(b"obj") {
            return None;
        }
        Some((num, gen_, self.parse_obj()?))
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_stops_as_soon_as_the_value_limit_is_passed() {
        let meter = Meter::new(u64::MAX).with_max_values(5);
        assert!(Lexer::new(b"[0 0 0 0 0]", 0, &meter).parse_obj().is_some());
        let meter = Meter::new(u64::MAX).with_max_values(4);
        let mut lx = Lexer::new(b"[0 0 0 0 0 0 0 0 0 0]", 0, &meter);
        assert!(lx.parse_obj().is_none());
        assert!(meter.values_exceeded());
        // It stopped at the fifth item, which passed the limit, well before the end of the array.
        assert_eq!(lx.pos(), 10);
    }

    #[test]
    fn values_count_while_their_object_is_alive() {
        let base = live_items();
        let meter = Meter::new(u64::MAX).with_max_values(8);
        let first = Lexer::new(b"<< /A [1 2 3] >>", 0, &meter)
            .parse_obj()
            .unwrap();
        assert_eq!(live_items() - base, 4); // one entry, three items
        let copy = first.clone();
        assert_eq!(
            live_items() - base,
            4,
            "dictionaries and arrays are shared, not copied"
        );
        drop((first, copy));
        assert_eq!(live_items(), base);
        // With the first object dropped, the same budget fits it again, twice over.
        let a = Lexer::new(b"<< /A [1 2 3] >>", 0, &meter).parse_obj();
        let b = Lexer::new(b"<< /A [1 2 3] >>", 0, &meter).parse_obj();
        assert!(a.is_some() && b.is_some());
        // A third alive at once does not.
        assert!(
            Lexer::new(b"<< /A [1 2 3] >>", 0, &meter)
                .parse_obj()
                .is_none()
        );
    }

    #[test]
    fn resolving_clones_share_dictionaries_names_and_strings() {
        let meter = Meter::new(u64::MAX);
        let obj = Lexer::new(
            b"<< /Text (shared) /Name /Shared /Nested << /A 1 >> >>",
            0,
            &meter,
        )
        .parse_obj()
        .unwrap();
        let Obj::Dict(original) = &obj else {
            panic!("dictionary")
        };
        let Obj::Dict(copy) = obj.clone() else {
            panic!("dictionary")
        };
        assert!(Rc::ptr_eq(&original.0, &copy.0));
        for key in [b"Text".as_slice(), b"Name"] {
            let value = original.get(key).unwrap();
            match (value, value.clone()) {
                (Obj::Str(a), Obj::Str(b)) | (Obj::Name(a), Obj::Name(b)) => {
                    assert!(Rc::ptr_eq(a, &b));
                }
                _ => panic!("shared bytes"),
            }
        }
    }

    #[test]
    fn consecutive_names_and_strings_decode_independently() {
        let meter = Meter::new(u64::MAX);
        let Some(Obj::Array(items)) =
            Lexer::new(b"[/Long#20Name (a\\(b\\)) /C <414> () /]", 0, &meter).parse_obj()
        else {
            panic!("array")
        };
        let bytes: Vec<&[u8]> = items
            .iter()
            .map(|o| match o {
                Obj::Name(b) | Obj::Str(b) => &b[..],
                _ => panic!("bytes"),
            })
            .collect();
        assert_eq!(
            bytes,
            [b"Long Name".as_slice(), b"a(b)", b"C", b"A@", b"", b""]
        );
    }

    #[test]
    fn dictionary_keys_decode_escapes_and_keep_the_last_duplicate() {
        let meter = Meter::new(u64::MAX);
        let Obj::Dict(dict) =
            Lexer::new(b"<< /A 1 /#41 2 /Nested << /#42 (value) >> >>", 0, &meter)
                .parse_obj()
                .unwrap()
        else {
            panic!("dictionary")
        };
        assert_eq!(dict.get(b"A"), Some(&Obj::Int(2)));
        let Some(Obj::Dict(nested)) = dict.get(b"Nested") else {
            panic!("nested dictionary")
        };
        assert!(matches!(nested.get(b"B"), Some(Obj::Str(s)) if s.as_slice() == b"value"));
    }
}
