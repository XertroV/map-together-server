use crate::{check_len_lt_er, StreamErr};

pub trait MTEncode {
    fn encode(&self) -> Vec<u8>;
}

pub trait MTDecode
where
    Self: Sized,
{
    fn decode(buf: &[u8]) -> Result<Self, StreamErr>;
}

/// Bounds-checked reader over a spec buffer. The `MTDecode` interface is
/// still `decode(&[u8])`; implementations use this so a short tail is
/// `StreamErr`, never a panic.
pub struct DecodeCursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> DecodeCursor<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    pub fn rest(&self) -> &'a [u8] {
        self.buf.get(self.pos..).unwrap_or(&[])
    }

    pub fn require(&self, n: usize, what: &str) -> Result<(), StreamErr> {
        if self.remaining() < n {
            return Err(StreamErr::InvalidData(format!("truncated {what}")));
        }
        Ok(())
    }

    pub fn skip(&mut self, n: usize, what: &str) -> Result<(), StreamErr> {
        self.require(n, what)?;
        self.pos += n;
        Ok(())
    }

    fn take<const N: usize>(&mut self, what: &str) -> Result<[u8; N], StreamErr> {
        self.require(N, what)?;
        let mut arr = [0u8; N];
        arr.copy_from_slice(&self.buf[self.pos..self.pos + N]);
        self.pos += N;
        Ok(arr)
    }

    pub fn u8(&mut self, what: &str) -> Result<u8, StreamErr> {
        Ok(self.take::<1>(what)?[0])
    }

    pub fn i8(&mut self, what: &str) -> Result<i8, StreamErr> {
        Ok(self.u8(what)? as i8)
    }

    pub fn u16_le(&mut self, what: &str) -> Result<u16, StreamErr> {
        Ok(u16::from_le_bytes(self.take(what)?))
    }

    pub fn u32_le(&mut self, what: &str) -> Result<u32, StreamErr> {
        Ok(u32::from_le_bytes(self.take(what)?))
    }

    pub fn i32_le(&mut self, what: &str) -> Result<i32, StreamErr> {
        Ok(i32::from_le_bytes(self.take(what)?))
    }

    pub fn f32_le(&mut self, what: &str) -> Result<f32, StreamErr> {
        Ok(f32::from_le_bytes(self.take(what)?))
    }

    pub fn u32x3(&mut self, what: &str) -> Result<[u32; 3], StreamErr> {
        Ok([self.u32_le(what)?, self.u32_le(what)?, self.u32_le(what)?])
    }

    pub fn f32x3(&mut self, what: &str) -> Result<[f32; 3], StreamErr> {
        Ok([self.f32_le(what)?, self.f32_le(what)?, self.f32_le(what)?])
    }

    pub fn f32x9(&mut self, what: &str) -> Result<[f32; 9], StreamErr> {
        Ok([
            self.f32_le(what)?,
            self.f32_le(what)?,
            self.f32_le(what)?,
            self.f32_le(what)?,
            self.f32_le(what)?,
            self.f32_le(what)?,
            self.f32_le(what)?,
            self.f32_le(what)?,
            self.f32_le(what)?,
        ])
    }

    pub fn magic(&mut self, expected: &[u8; 4], what: &str) -> Result<(), StreamErr> {
        let got = self.take::<4>(what)?;
        if &got != expected {
            return Err(StreamErr::InvalidData(format!(
                "expected magic bytes for {what}"
            )));
        }
        Ok(())
    }

    pub fn lp_string(&mut self, what: &str) -> Result<String, StreamErr> {
        let len = self.u16_le(what)? as usize;
        check_len_lt_er(len as u16)?;
        self.require(len, what)?;
        let s = String::from_utf8_lossy(&self.buf[self.pos..self.pos + len]).into_owned();
        self.pos += len;
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_reads_are_invalid_without_panic() {
        let mut cur = DecodeCursor::new(&[]);
        assert!(matches!(cur.u32_le("x"), Err(StreamErr::InvalidData(_))));
        let mut cur = DecodeCursor::new(&[1, 2, 3]);
        assert!(matches!(
            cur.magic(b"BLKs", "blocks"),
            Err(StreamErr::InvalidData(_))
        ));
    }

    #[test]
    fn lp_string_roundtrip_and_truncation() {
        let mut cur = DecodeCursor::new(&[3, 0, b'a', b'b', b'c', 9]);
        assert_eq!(cur.lp_string("s").unwrap(), "abc");
        assert_eq!(cur.remaining(), 1);
        let mut cur = DecodeCursor::new(&[5, 0, b'a']);
        assert!(matches!(cur.lp_string("s"), Err(StreamErr::InvalidData(_))));
    }

    #[test]
    fn lp_string_rejects_overlong() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&10001u16.to_le_bytes());
        buf.extend_from_slice(&vec![b'x'; 8]);
        let mut cur = DecodeCursor::new(&buf);
        assert!(matches!(
            cur.lp_string("s"),
            Err(StreamErr::InvalidData(_))
        ));
    }
}
