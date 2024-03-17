use crate::StreamErr;

pub trait MTEncode {
    fn encode(&self) -> Vec<u8>;
}

pub trait MTDecode
where
    Self: Sized,
{
    fn decode(buf: &[u8]) -> Result<Self, StreamErr>;
}
