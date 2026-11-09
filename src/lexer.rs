//! A small PDF object lexer and parser. Bounded: nesting depth is limited, and nothing here recurses on
//! untrusted structure except `parse_obj`, which stops at `MAX_NEST`.

/// Deepest allowed nesting of arrays and dictionaries.
const MAX_NEST: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Dict),
    Ref(u32, u16),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Dict(pub Vec<(Vec<u8>, Obj)>);

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
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Lexer { data, pos }
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
                Some(Obj::Name(self.read_name()))
            }
            b'(' => self.read_literal_string().map(Obj::Str),
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    self.read_dict_body(depth).map(Obj::Dict)
                } else {
                    self.read_hex_string().map(Obj::Str)
                }
            }
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    if self.peek()? == b']' {
                        self.pos += 1;
                        break;
                    }
                    items.push(self.parse_nested(depth + 1)?);
                }
                Some(Obj::Array(items))
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
        let mut entries = Vec::new();
        loop {
            self.skip_ws();
            if self.data[self.pos.min(self.data.len())..].starts_with(b">>") {
                self.pos += 2;
                return Some(Dict(entries));
            }
            let Obj::Name(key) = self.parse_nested(depth + 1)? else {
                return None;
            };
            let value = self.parse_nested(depth + 1)?;
            entries.push((key, value));
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
        let rel = self.data[start..]
            .windows(key.len())
            .position(|w| w == key)?;
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
