//! Checked primitives for the observed cFSD wrapper and bucket layout.
//! The supplied client cFSD bytes use the observed wrapper and bucket format.
//! Reads check arithmetic, extents, record counts and key placement before
//! family decoders consume them. Other layouts and malformed input are
//! rejected rather than interpreted as a general cFSD schema.
use anyhow::{bail, Context, Result};
use std::collections::HashSet;

#[derive(Clone, Copy)]
pub struct Reader<'a> {
    pub data: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(data: &'a [u8]) -> Self { Self { data } }

    pub fn slice(&self, offset: usize, length: usize) -> Result<&'a [u8]> {
        let end = offset.checked_add(length).context("cFSD range overflow")?;
        self.data.get(offset..end).context("cFSD range out of bounds")
    }

    pub fn u8(&self, offset: usize) -> Result<u8> { Ok(self.slice(offset, 1)?[0]) }
    pub fn u32(&self, offset: usize) -> Result<u32> {
        Ok(u32::from_le_bytes(self.slice(offset, 4)?.try_into()?))
    }
    pub fn u64(&self, offset: usize) -> Result<u64> {
        Ok(u64::from_le_bytes(self.slice(offset, 8)?.try_into()?))
    }
    pub fn i32(&self, offset: usize) -> Result<i32> {
        Ok(i32::from_le_bytes(self.slice(offset, 4)?.try_into()?))
    }
    pub fn f32(&self, offset: usize) -> Result<f32> {
        let value = f32::from_bits(self.u32(offset)?);
        if !value.is_finite() { bail!("nonfinite cFSD f32") }
        Ok(value)
    }
    pub fn f64(&self, offset: usize) -> Result<f64> {
        let value = f64::from_bits(self.u64(offset)?);
        if !value.is_finite() { bail!("nonfinite cFSD f64") }
        Ok(value)
    }
    pub fn string(&self, offset: usize) -> Result<&'a str> {
        let prefix = offset.checked_sub(8).context("string pointer has no length prefix")?;
        let length = usize::try_from(self.u64(prefix)?).context("string length overflow")?;
        let bytes = self.slice(offset, length)?;
        let terminator = offset.checked_add(length).context("string terminator overflow")?;
        if self.u8(terminator)? != 0 { bail!("missing cFSD string terminator") }
        std::str::from_utf8(bytes).context("invalid cFSD UTF-8 string")
    }
}

pub fn payload(raw: &[u8]) -> Result<Reader<'_>> {
    let reader = Reader::new(raw);
    reader.slice(0, 32)?;
    let size = usize::try_from(reader.u64(24)?).context("cFSD payload size overflow")?;
    if size != raw.len() - 32 { bail!("cFSD payload length mismatch") }
    Ok(Reader::new(reader.slice(32, size)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BucketRecord {
    pub key: u64,
    pub offset: usize,
}

pub fn bucket_records(payload: &Reader<'_>, stride: usize, key_width: usize) -> Result<Vec<BucketRecord>> {
    if !matches!(key_width, 4 | 8) || stride < key_width { bail!("unsupported cFSD key width or stride") }
    let table = usize::try_from(payload.u64(0)?)?;
    let record_count = usize::try_from(payload.u64(8)?)?;
    let bucket_count = usize::try_from(payload.u64(16)?)?;
    if table != 24 || bucket_count == 0 { bail!("unsupported cFSD bucket header") }
    let table_size = bucket_count.checked_mul(8).context("bucket table overflow")?;
    payload.slice(table, table_size)?;
    let mut last_end = table.checked_add(table_size).context("bucket table overflow")?;
    // Each record occupies at least stride bytes, so the count cannot exceed this bound.
    if record_count > payload.data.len() / stride { bail!("invalid cFSD record count") }
    let mut records = Vec::with_capacity(record_count);
    let mut seen = HashSet::with_capacity(record_count);
    for bucket in 0..bucket_count {
        let entry = table.checked_add(bucket.checked_mul(8).context("bucket offset overflow")?).context("bucket offset overflow")?;
        let start = usize::try_from(payload.u64(entry)?)?;
        let header = start.checked_sub(8).context("bucket has no count header")?;
        if header < last_end { bail!("overlapping or unordered cFSD bucket") }
        let end = if bucket + 1 < bucket_count {
            usize::try_from(payload.u64(entry.checked_add(8).context("bucket offset overflow")?)?)?
                .checked_sub(8).context("bucket has no count header")?
        } else { payload.data.len() };
        if end < start || end > payload.data.len() { bail!("invalid cFSD bucket extent") }
        let count = usize::try_from(payload.u64(header)?)?;
        if count > (end - start) / stride || count > record_count.saturating_sub(records.len()) {
            bail!("cFSD bucket record count exceeds extent or header")
        }
        for index in 0..count {
            let offset = start.checked_add(index.checked_mul(stride).context("record offset overflow")?).context("record offset overflow")?;
            let key = if key_width == 4 { u64::from(payload.u32(offset)?) } else { payload.u64(offset)? };
            payload.slice(offset, stride)?;
            if !seen.insert(key) || key % u64::try_from(bucket_count)? != u64::try_from(bucket)? {
                bail!("duplicate or misbucketed cFSD key {key}")
            }
            records.push(BucketRecord { key, offset });
        }
        last_end = start.checked_add(count.checked_mul(stride).context("record extent overflow")?).context("record extent overflow")?;
    }
    if records.len() != record_count { bail!("cFSD record count mismatch") }
    Ok(records)
}

/// Return the post-record lower bound and exclusive upper bound of a key's bucket.
pub fn bucket_bounds(reader: &Reader<'_>, stride: usize, key: u64) -> Result<(usize, usize)> {
    if stride == 0 { bail!("zero cFSD record stride") }
    let table = usize::try_from(reader.u64(0)?)?;
    let buckets = usize::try_from(reader.u64(16)?)?;
    if table != 24 || buckets == 0 { bail!("unsupported cFSD bucket header") }
    let table_end = table.checked_add(buckets.checked_mul(8).context("bucket table overflow")?).context("bucket table overflow")?;
    reader.slice(table, table_end - table)?;
    let index = usize::try_from(key % u64::try_from(buckets)?)?;
    let entry = table.checked_add(index.checked_mul(8).context("bucket offset overflow")?).context("bucket offset overflow")?;
    let start = usize::try_from(reader.u64(entry)?)?;
    let header = start.checked_sub(8).context("bucket has no count header")?;
    if header < table_end { bail!("cFSD bucket overlaps table") }
    let high = if index + 1 < buckets {
        usize::try_from(reader.u64(entry.checked_add(8).context("bucket offset overflow")?)?)?
            .checked_sub(8).context("bucket has no count header")?
    } else { reader.data.len() };
    if high < start || high > reader.data.len() { bail!("invalid cFSD bucket extent") }
    let count = usize::try_from(reader.u64(header)?)?;
    let low = start.checked_add(count.checked_mul(stride).context("record extent overflow")?).context("record extent overflow")?;
    if low > high { bail!("cFSD bucket record count exceeds extent") }
    Ok((low, high))
}

#[cfg(test)]
mod tests;
