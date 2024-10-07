use std::io::{self, Cursor, Read};
use std::ops::Range;

use byteorder::{BigEndian, ByteOrder};
use bytes::{Buf, BufMut};

use error::Error;
use internal::utils::Buffer;

use crate::error;
use crate::internal;

#[derive(Debug)]
pub(crate) enum MessageReadError {
    Incomplete,
    Parse(String),
}

impl From<MessageReadError> for Error {
    fn from(err: MessageReadError) -> Error {
        match err {
            MessageReadError::Incomplete => Error::deserialize("message incomplete"),
            MessageReadError::Parse(reason) => Error::deserialize(reason),
        }
    }
}

pub(crate) struct Message<B> {
    buf: B,
}

impl<B> Message<B> {
    pub fn new(buf: B) -> Message<B> {
        Message { buf }
    }

    pub fn get_ref(&self) -> &B {
        &self.buf
    }

    pub fn get_mut(&mut self) -> &mut B {
        &mut self.buf
    }

    pub fn into_inner(self) -> B {
        self.buf
    }
}

impl<B: LinearBuf> Message<B> {
    #[inline]
    pub fn read_uint(&mut self) -> Result<u64, MessageReadError> {
        if self.buf.remaining() < 1 {
            return Err(MessageReadError::Incomplete);
        }
        let u7_or_len = self.buf.get_u8();
        if u7_or_len < 128 {
            return Ok(u7_or_len as u64);
        }
        let len = !u7_or_len + 1;
        if self.buf.remaining() < len as usize {
            return Err(MessageReadError::Incomplete);
        }
        Ok(self.buf.fast_get_uint_be(len as usize))
    }

    #[inline]
    pub fn read_int(&mut self) -> Result<i64, MessageReadError> {
        let bits = self.read_uint()?;
        let sign = bits & 1;
        let sint = (bits >> 1) as i64;
        if sign == 0 {
            Ok(sint)
        } else {
            Ok(!sint)
        }
    }

    #[inline]
    pub fn read_float(&mut self) -> Result<f64, MessageReadError> {
        let bits = self.read_uint()?;
        Ok(f64::from_bits(bits.swap_bytes()))
    }

    #[inline]
    pub fn read_bool(&mut self) -> Result<bool, MessageReadError> {
        match self.read_uint()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(MessageReadError::Parse("integer overflow".into())),
        }
    }

    #[inline]
    pub fn read_bytes_len(&mut self) -> Result<usize, MessageReadError> {
        let len = self.read_uint()?;
        if (self.buf.remaining() as u64) < len {
            return Err(MessageReadError::Incomplete);
        }
        Ok(len as usize)
    }
}

impl<B: BufMut> Message<B> {
    #[inline]
    pub fn write_uint(&mut self, n: u64) {
        if n < 128 {
            self.buf.put_u8(n as u8);
        } else {
            let nbytes = 8 - (n.leading_zeros() / 8) as u8;
            self.buf.put_u8(!(nbytes - 1));
            self.buf.put_uint_be(n, nbytes as usize);
        }
    }

    #[inline]
    pub fn write_bool(&mut self, b: bool) {
        match b {
            false => self.write_uint(0),
            true => self.write_uint(1),
        }
    }

    #[inline]
    pub fn write_int(&mut self, n: i64) {
        let u: u64;
        if n < 0 {
            u = (!(n as u64) << 1) | 1;
        } else {
            u = (n as u64) << 1;
        }
        self.write_uint(u);
    }

    #[inline]
    pub fn write_float(&mut self, n: f64) {
        let bits = n.to_bits();
        self.write_uint(bits.swap_bytes());
    }

    #[inline]
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.write_uint(bytes.len() as u64);
        self.buf.put_slice(bytes);
    }
}

pub(crate) struct Stream<Io> {
    inner: Io,
}

impl<Io> Stream<Io> {
    pub fn new(inner: Io) -> Stream<Io> {
        Stream { inner }
    }

    pub fn get_ref(&self) -> &Io {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut Io {
        &mut self.inner
    }

    pub fn into_inner(self) -> Io {
        self.inner
    }
}

#[derive(Clone)]
pub(crate) struct SectionHeader {
    pub(crate) type_id: i64,
    pub(crate) payload_range: Range<usize>,
}

impl<Io: Read> Stream<Io> {
    fn parse_section(&mut self, bytes: &[u8]) -> Result<SectionHeader, MessageReadError> {
        let mut msg = Message::new(Cursor::new(bytes));
        //
        //  <---> message offset
        //        <--------------------> message length
        // [ len | type id | payload... ]
        //  <-------------> payload offset
        //                  <----------> payload length
        //
        let msg_length = msg.read_uint()? as usize;
        let msg_offset = msg.get_ref().position() as usize;
        if bytes.len() < msg_offset + msg_length {
            return Err(MessageReadError::Incomplete);
        }
        let type_id = msg.read_int()?;
        let payload_offset = msg.get_ref().position() as usize;
        let payload_length = msg_length as usize - (payload_offset - msg_offset);
        Ok(SectionHeader {
            type_id,
            payload_range: Range {
                start: payload_offset,
                end: payload_offset + payload_length,
            },
        })
    }

    pub fn read_section(&mut self, buf: &mut Buffer) -> Result<Option<SectionHeader>, Error> {
        if buf.len() == 0 {
            let n = buf.read_from(&mut self.inner)?;
            if n == 0 {
                return Ok(None);
            }
        }
        loop {
            match self.parse_section(buf.bytes()) {
                Ok(header) => {
                    return Ok(Some(header));
                }
                Err(MessageReadError::Incomplete) => {
                    let n = buf.read_from(&mut self.inner)?;
                    if n == 0 {
                        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
                    }
                }
                Err(MessageReadError::Parse(reason)) => {
                    return Err(Error::deserialize(reason));
                }
            }
        }
    }
}

/// A `Buf` backed by linear memory, which can return a slice
/// to _all_ remaining bytes.
pub trait LinearBuf: Buf {
    fn remaining_bytes(&self) -> &[u8];

    fn fast_get_uint_be(&mut self, nbytes: usize) -> u64 {
        let ret = {
            let src = &self.remaining_bytes()[..nbytes];
            BigEndian::read_uint(src, nbytes)
        };
        self.advance(nbytes);
        ret
    }
}

impl<T: AsRef<[u8]>> LinearBuf for io::Cursor<T> {
    fn remaining_bytes(&self) -> &[u8] {
        &self.get_ref().as_ref()[self.position() as usize..]
    }
}
