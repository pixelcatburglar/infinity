//! Reader, mirroring `Marshal::ReadObject` and friends in Marshal.cpp.
//! The decode budget below is a conservative estimate of the decoded
//! stream's size, not a cap on process RSS.

use crate::dbrow::{rle_unpack, DbRowDescriptor};
use crate::{strings, tag, Error, Value};

/// Server codec design: maximum nested objects accepted before rejecting a stream.
const RECURSION_LIMIT: usize = 1000;
/// Server codec design: cap initial dictionary allocation for untrusted stream counts.
const DICT_INITIAL_CAPACITY_LIMIT: usize = 4096;
/// Server codec design: upper bound on the estimated size of one decoded
/// stream. The game listener admits many handshakes at once on a host with
/// limited memory, so the per-stream bound is 32 MiB; a frame is at most
/// 10 MiB, so a frame that is mostly raw payload still fits.
const DECODE_BUDGET_BYTES: usize = 32 * 1024 * 1024;
/// Server codec design: estimated size of one decoded node.
const NODE_COST: usize = std::mem::size_of::<Value>();
/// Server codec design: one shared-object entry: a header map slot plus the
/// shared slot holding the decoded value and its weight.
const SHARED_SLOT_COST: usize = std::mem::size_of::<i32>() + std::mem::size_of::<Option<(Value, usize)>>();

/// Decode one marshal stream (`0x7E` / `0x7D` header).
pub fn loads(buf: &[u8]) -> Result<Value, Error> {
    let mut r = Reader::new(buf)?;
    r.object()
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    /// End of object data; the version-0 shared map follows it.
    end: usize,
    version: u8,
    /// Version 0: 1-based slot for the n-th shared object in stream order.
    map: Vec<i32>,
    map_used: usize,
    shared: Vec<Option<(Value, usize)>>,
    depth: usize,
    /// Estimated decoded size charged against `DECODE_BUDGET_BYTES`.
    spent: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Result<Self, Error> {
        let mut r = Reader { buf, pos: 0, end: buf.len(), version: 0, map: vec![], map_used: 0, shared: vec![], depth: 0, spent: 0 };
        match r.u8()? {
            tag::SIGNATURE => {
                let count = r.i32()?;
                if count < 0 || (count as usize) * 4 > buf.len().saturating_sub(r.pos) {
                    return Err(Error::BadSharedMap(format!("count {count}")));
                }
                let count = count as usize;
                // The header map and `shared` slots are allocated up front;
                // charge their estimated size before collecting them.
                r.charge(count.saturating_mul(SHARED_SLOT_COST))?;
                r.end = buf.len() - count * 4;
                r.map = buf[r.end..].chunks_exact(4).map(|c| i32::from_le_bytes(c.try_into().unwrap())).collect();
                if let Some(bad) = r.map.iter().find(|&&m| m < 1 || m as usize > count) {
                    return Err(Error::BadSharedMap(format!("slot {bad} out of 1..={count}")));
                }
                r.shared = vec![None; count];
            }
            tag::SIGNATURE2 => r.version = r.u8()?,
            other => return Err(Error::BadSignature(other)),
        }
        Ok(r)
    }

    /// Charge `n` bytes of estimated decoded size against the stream budget.
    fn charge(&mut self, n: usize) -> Result<(), Error> {
        self.spent = self.spent.saturating_add(n);
        if self.spent > DECODE_BUDGET_BYTES {
            return Err(Error::TooLarge);
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if n > self.end.saturating_sub(self.pos) {
            return Err(Error::Truncated { at: self.pos, wanted: n });
        }
        self.charge(n)?;
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }
    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// `ReadStream::ReadInteger`: one byte, or 0xFF followed by an i32.
    fn size(&mut self) -> Result<usize, Error> {
        let b = self.u8()?;
        let n = if b == 0xFF { self.i32()? } else { b as i32 };
        usize::try_from(n).map_err(|_| Error::Truncated { at: self.pos, wanted: 0 })
    }
    fn sized(&mut self) -> Result<&'a [u8], Error> {
        let n = self.size()?;
        self.take(n)
    }
    fn utf8(&mut self, bytes: &[u8]) -> Result<String, Error> {
        String::from_utf8(bytes.to_vec()).map_err(|_| Error::BadUtf8 { at: self.pos })
    }

    /// Reserve the next shared slot (pre-order, before children are read).
    fn reserve_shared(&mut self) -> Result<usize, Error> {
        if self.version == 0 {
            let slot = *self
                .map
                .get(self.map_used)
                .ok_or_else(|| Error::BadSharedMap("more shared objects than map entries".into()))?;
            self.map_used += 1;
            Ok(slot as usize - 1)
        } else {
            self.charge(SHARED_SLOT_COST)?;
            self.shared.push(None);
            Ok(self.shared.len() - 1)
        }
    }

    fn peek_mark(&mut self) -> Result<bool, Error> {
        if self.pos >= self.end {
            return Err(Error::Truncated { at: self.pos, wanted: 1 });
        }
        if self.buf[self.pos] == tag::MARK {
            self.take(1)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn object(&mut self) -> Result<Value, Error> {
        self.depth += 1;
        if self.depth > RECURSION_LIMIT {
            return Err(Error::TooDeep);
        }
        let at = self.pos;
        let start = self.spent;
        self.charge(NODE_COST)?;
        let raw = self.u8()?;
        let slot = if raw & tag::SHARED_FLAG != 0 { Some(self.reserve_shared()?) } else { None };
        let v = self.body(raw & tag::TYPE_MASK, at)?;
        if let Some(slot) = slot {
            // A shared object's weight is charged again for the slot copy;
            // nested shared stores count toward the parent's weight.
            let weight = self.spent - start;
            self.charge(weight)?;
            self.shared[slot] = Some((v.clone(), weight));
        }
        self.depth -= 1;
        Ok(v)
    }

    fn body(&mut self, t: u8, at: usize) -> Result<Value, Error> {
        Ok(match t {
            tag::NONE => Value::None,
            tag::GLOBAL => {
                let b = self.sized()?;
                Value::Global(self.utf8(b)?)
            }
            tag::INT64 => Value::Int(i64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            tag::INT32 => Value::Int(self.i32()? as i64),
            tag::INT16 => Value::Int(i16::from_le_bytes(self.take(2)?.try_into().unwrap()) as i64),
            tag::INT8 => Value::Int(self.u8()? as i8 as i64),
            tag::INT_N1 => Value::Int(-1),
            tag::INT_0 => Value::Int(0),
            tag::INT_1 => Value::Int(1),
            tag::FLOAT => Value::Float(f64::from_le_bytes(self.take(8)?.try_into().unwrap())),
            tag::FLOAT_0 => Value::Float(0.0),
            tag::STR | tag::BUFFER => Value::Bytes(self.sized()?.to_vec()),
            tag::STR_EMPTY => Value::Bytes(vec![]),
            tag::STR_CHAR => Value::Bytes(self.take(1)?.to_vec()),
            tag::STR_SHORT => {
                let n = self.u8()? as usize;
                Value::Bytes(self.take(n)?.to_vec())
            }
            tag::STR_TABLE => {
                let i = self.u8()?;
                let s = strings::lookup(i).ok_or(Error::BadStringIndex(i))?;
                self.charge(s.len())?;
                Value::Bytes(s.as_bytes().to_vec())
            }
            tag::UNICODE | tag::UTF8 => {
                let b = self.sized()?;
                Value::Unicode(self.utf8(b)?)
            }
            tag::UNICODE_0 => Value::Unicode(String::new()),
            tag::UNICODE_1 => {
                let b = self.take(1)?;
                let c = b[0] as char;
                self.charge(c.len_utf8() - 1)?;
                Value::Unicode(c.to_string())
            }
            tag::TUPLE | tag::LIST => {
                let n = self.size()?;
                let items = (0..n).map(|_| self.object()).collect::<Result<Vec<_>, _>>()?;
                if t == tag::TUPLE { Value::Tuple(items) } else { Value::List(items) }
            }
            tag::TUPLE0 => Value::Tuple(vec![]),
            tag::TUPLE1 => Value::Tuple(vec![self.object()?]),
            tag::TUPLE2 => Value::Tuple(vec![self.object()?, self.object()?]),
            tag::LIST0 => Value::List(vec![]),
            tag::LIST1 => Value::List(vec![self.object()?]),
            tag::DICT => {
                let n = self.size()?;
                let mut pairs = Vec::with_capacity(n.min(DICT_INITIAL_CAPACITY_LIMIT));
                for _ in 0..n {
                    // Value is written before key.
                    let v = self.object()?;
                    let k = self.object()?;
                    pairs.push((k, v));
                }
                Value::Dict(pairs)
            }
            tag::INSTANCE => {
                let class = self.object()?;
                let state = self.object()?;
                Value::Instance(Box::new(class), Box::new(state))
            }
            tag::REDUCE | tag::NEWOBJ => {
                let data = Box::new(self.object()?);
                let mut list = vec![];
                while !self.peek_mark()? {
                    list.push(self.object()?);
                }
                let mut dict = vec![];
                while !self.peek_mark()? {
                    let k = self.object()?;
                    let v = self.object()?;
                    dict.push((k, v));
                }
                if t == tag::REDUCE { Value::Reduce { data, list, dict } } else { Value::NewObj { data, list, dict } }
            }
            tag::REFERENCE => {
                let n = self.size()? as i32;
                let idx = if self.version == 0 { n - 1 } else { n };
                let i = usize::try_from(idx).map_err(|_| Error::BadReference(n))?;
                let weight = match self.shared.get(i) {
                    Some(Some((_, weight))) => *weight,
                    _ => return Err(Error::BadReference(n)),
                };
                self.charge(weight)?;
                self.shared[i].as_ref().unwrap().0.clone()
            }
            tag::CRC_CHECK => {
                let _crc = self.i32()?;
                return self.object();
            }
            tag::TRUE => Value::Bool(true),
            tag::FALSE => Value::Bool(false),
            tag::LONG => {
                let b = self.sized()?;
                long_from_le(b)
            }
            tag::DBROW => {
                let descriptor = self.object()?;
                let desc = DbRowDescriptor::from_value(&descriptor)
                    .ok_or(Error::Unsupported("DBRow with unrecognised descriptor"))?;
                let layout = desc.layout();
                let packed = self.sized()?;
                self.charge(layout.data_len)?;
                let data = rle_unpack(packed, layout.data_len)?;
                let objects = (0..layout.n_objects).map(|_| self.object()).collect::<Result<Vec<_>, _>>()?;
                Value::DbRow { descriptor: Box::new(descriptor), data, objects }
            }
            tag::WSTREAM => Value::SubStream(self.sized()?.to_vec()),
            tag::CALLBACK => Value::Callback(Box::new(self.object()?)),
            tag::PICKLE | tag::PICKLER => return Err(Error::Unsupported("pickle fallback")),
            tag::COMPLEX => return Err(Error::Unsupported("complex")),
            _ => return Err(Error::BadTag { tag: t, at }),
        })
    }
}

/// `_PyLong_FromByteArray(little_endian=1, signed=1)`.
pub(crate) fn long_from_le(b: &[u8]) -> Value {
    if b.is_empty() {
        return Value::Int(0);
    }
    // WriteLong emits nbits/8+1 bytes, so i64::MAX arrives as 9 bytes; drop redundant sign bytes.
    let mut b = b;
    while b.len() > 8 {
        let (last, prev) = (b[b.len() - 1], b[b.len() - 2]);
        if crate::redundant_sign_byte(last, prev) {
            b = &b[..b.len() - 1];
        } else {
            break;
        }
    }
    if b.len() <= 8 {
        let fill = if b[b.len() - 1] & 0x80 != 0 { 0xFF } else { 0 };
        let mut w = [fill; 8];
        w[..b.len()].copy_from_slice(b);
        return Value::Int(i64::from_le_bytes(w));
    }
    Value::BigLong(b.to_vec())
}
