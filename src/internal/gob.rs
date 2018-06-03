use std::io::{self, Cursor, Read, Write};

use bytes::{Buf, BufMut};

use error::Error;
use internal::utils::RingBuf;

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

impl<B: Buf> Message<B> {
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
        Ok(self.buf.get_uint_be(len as usize))
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

    pub fn borrow_mut(&mut self) -> Stream<&mut Io> {
        Stream::new(self.get_mut())
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

impl<Io: Write> Stream<Io> {
    fn write_buf(&mut self, buf: &[u8]) -> Result<(), Error> {
        self.inner.write_all(buf)?;
        Ok(())
    }

    pub fn write_section(&mut self, type_id: i64, buf: &[u8]) -> Result<(), Error> {
        let mut type_id_msg = Message::new(Cursor::new([0u8; 9]));
        type_id_msg.write_int(type_id);
        let type_id_pos = type_id_msg.get_ref().position() as usize;
        let mut len_msg = Message::new(Cursor::new([0u8; 9]));
        len_msg.write_uint((buf.len() + type_id_pos) as u64);
        let len_pos = len_msg.get_ref().position() as usize;
        self.write_buf(&len_msg.get_ref().get_ref()[..len_pos])?;
        self.write_buf(&type_id_msg.get_ref().get_ref()[..type_id_pos])?;
        self.write_buf(buf)
    }
}

impl<Io: Read> Stream<Io> {
    fn read_section_len(&mut self, buf: &mut RingBuf) -> Result<Option<u64>, Error> {
        if buf.len() == 0 {
            let n = buf.read_from(&mut self.inner)?;
            if n == 0 {
                return Ok(None);
            }
        }
        loop {
            let (result, position) = {
                let mut cursor = Cursor::new(buf.bytes());
                let result = {
                    let mut msg = Message::new(&mut cursor);
                    msg.read_uint()
                };
                (result, cursor.position() as usize)
            };
            match result {
                Ok(len) => {
                    buf.advance(position);
                    return Ok(Some(len));
                }
                Err(MessageReadError::Incomplete) => {
                    let n = buf.read_from(&mut self.inner)?;
                    if n > 0 {
                        continue;
                    } else {
                        return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
                    }
                }
                Err(MessageReadError::Parse(reason)) => {
                    return Err(Error::deserialize(reason));
                }
            }
        }
    }

    pub fn read_section(&mut self, buf: &mut RingBuf) -> Result<Option<(i64, usize)>, Error> {
        let msg_len = match self.read_section_len(buf)? {
            Some(len) => len as usize,
            None => return Ok(None),
        };
        let buf_len = buf.len();
        if buf_len < msg_len {
            buf.read_from_exact(&mut self.inner, msg_len - buf_len)?;
        }
        let (type_id, position) = {
            let mut cursor = Cursor::new(buf.bytes());
            let type_id = Message::new(&mut cursor)
                .read_int()
                .map_err(|err| Error::deserialize(format!("failed to read type id: {:?}", err)))?;
            (type_id, cursor.position() as usize)
        };
        buf.advance(position);
        Ok(Some((type_id, msg_len - position)))
    }
}
