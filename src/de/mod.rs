//! Deserialization

use std::io::{Cursor, Read};

use bytes::Buf;
use serde::de::Visitor;
use serde::de::value::Error;
use serde::{self, Deserialize};

use internal::gob::{Message, Stream};
use internal::types::{TypeId, Types, WireType};
use internal::utils::{Bow, RingBuf};

use internal::de::FieldValueDeserializer;
use internal::de::ValueDeserializer;

pub struct StreamDeserializer<R> {
    defs: Types,
    stream: Stream<R>,
    buffer: RingBuf,
    prev_len: Option<usize>,
}

impl<R> StreamDeserializer<R> {
    pub fn new(read: R) -> Self {
        StreamDeserializer {
            defs: Types::new(),
            stream: Stream::new(read),
            buffer: RingBuf::new(),
            prev_len: None,
        }
    }

    pub fn deserialize<'de, T>(&'de mut self) -> Result<Option<T>, Error>
    where
        R: Read,
        T: Deserialize<'de>,
    {
        if let Some(deserializer) = self.deserializer()? {
            Ok(Some(T::deserialize(deserializer)?))
        } else {
            Ok(None)
        }
    }

    pub fn deserializer<'de>(&'de mut self) -> Result<Option<Deserializer<'de>>, Error>
    where
        R: Read,
    {
        if let Some(len) = self.prev_len {
            self.buffer.advance(len);
        }
        loop {
            let (type_id, len) = match self.stream.read_section(&mut self.buffer)? {
                Some((type_id, len)) => (type_id, len),
                None => return Ok(None),
            };

            if type_id >= 0 {
                let slice = &self.buffer.bytes()[..len];
                let msg = Message::new(Cursor::new(slice));
                self.prev_len = Some(len);
                return Ok(Some(Deserializer {
                    defs: Bow::Borrowed(&mut self.defs),
                    msg: msg,
                    type_id: Some(TypeId(type_id)),
                }));
            }

            let wire_type = {
                let slice = &self.buffer.bytes()[..len];
                let mut msg = Message::new(Cursor::new(slice));
                let de = FieldValueDeserializer::new(TypeId::WIRE_TYPE, &self.defs, &mut msg);
                WireType::deserialize(de)
            }?;

            if -type_id != wire_type.common().id.0 {
                return Err(serde::de::Error::custom(format!("type id mismatch")));
            }

            self.defs.insert(wire_type);
            self.buffer.advance(len);
        }
    }

    pub fn get_ref(&self) -> &R {
        self.stream.get_ref()
    }

    pub fn get_mut(&mut self) -> &mut R {
        self.stream.get_mut()
    }

    pub fn into_inner(self) -> R {
        self.stream.into_inner()
    }
}

pub struct Deserializer<'de> {
    defs: Bow<'de, Types>,
    msg: Message<Cursor<&'de [u8]>>,
    type_id: Option<TypeId>,
}

impl<'de> Deserializer<'de> {
    pub fn from_slice(input: &'de [u8]) -> Deserializer<'de> {
        Deserializer {
            defs: Bow::Owned(Types::new()),
            msg: Message::new(Cursor::new(input)),
            type_id: None,
        }
    }

    fn value_deserializer<'t>(&'t mut self) -> Result<ValueDeserializer<'t, 'de>, Error> {
        if let Some(type_id) = self.type_id {
            return Ok(ValueDeserializer::new(type_id, &self.defs, &mut self.msg));
        }

        loop {
            let _len = self.msg.read_bytes_len()?;
            let type_id = self.msg.read_int()?;

            if type_id >= 0 {
                return Ok(ValueDeserializer::new(
                    TypeId(type_id),
                    &self.defs,
                    &mut self.msg,
                ));
            }

            let wire_type = {
                let de = FieldValueDeserializer::new(TypeId::WIRE_TYPE, &self.defs, &mut self.msg);
                WireType::deserialize(de)
            }?;

            if -type_id != wire_type.common().id.0 {
                return Err(serde::de::Error::custom(format!("type id mismatch")));
            }

            self.defs.insert(wire_type);
        }
    }
}

impl<'de> serde::Deserializer<'de> for Deserializer<'de> {
    type Error = Error;

    fn deserialize_any<V>(mut self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.value_deserializer()?.deserialize_any(visitor)
    }

    fn deserialize_enum<V>(
        mut self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        self.value_deserializer()?
            .deserialize_enum(name, variants, visitor)
    }

    fn deserialize_char<V>(self, visitor: V) -> Result<V::Value, Self::Error>
    where
        V: Visitor<'de>,
    {
        let int = i64::deserialize(self)?;
        if let Some(c) = ::std::char::from_u32(int as u32) {
            visitor.visit_char(c)
        } else {
            Err(serde::de::Error::custom(format!(
                "invalid char code {}",
                int
            )))
        }
    }

    forward_to_deserialize_any! {
        bool i8 i16 i32 i64 u8 u16 u32 u64 f32 f64 str string bytes
        byte_buf option unit unit_struct newtype_struct seq tuple
        tuple_struct map struct identifier ignored_any
    }
}
