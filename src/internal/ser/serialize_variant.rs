use std::borrow::Borrow;

use owning_ref::OwningRef;
use serde::ser::{self, Serialize};
use serde_schema::types::{EnumVariant, Type};

use error::Error;
use internal::types::TypeId;
use schema::{Schema, SchemaType};

use crate::{error, internal, schema};

use super::SerializeStructValue;
use super::{FieldValueSerializer, SerializationCtx, SerializationOk};

pub(crate) struct SerializeVariantValue<S> {
    ctx: SerializationCtx<S>,
    variant: OwningRef<SchemaType, EnumVariant<TypeId>>,
    variant_idx: u32,
}

impl<S: Borrow<Schema>> SerializeVariantValue<S> {
    pub(crate) fn new(
        ctx: SerializationCtx<S>,
        type_id: TypeId,
        variant_idx: u32,
    ) -> Result<Self, Error> {
        let variant;

        if let Some(schema_type) = ctx.schema.borrow().lookup(type_id) {
            variant = OwningRef::new(schema_type).try_map::<_, _, Error>(|typ| {
                if let &Type::Enum(ref enum_type) = typ {
                    if let Some(enum_variant) = enum_type.variant(variant_idx) {
                        Ok(enum_variant)
                    } else {
                        Err(ser::Error::custom("unknown enum variant type"))
                    }
                } else {
                    Err(ser::Error::custom("schema mismatch, not an enum"))
                }
            })?;
        } else {
            return Err(ser::Error::custom("type not found"));
        }

        Ok(SerializeVariantValue {
            ctx,
            variant,
            variant_idx,
        })
    }

    fn write_header(ctx: &mut SerializationCtx<S>, idx: u32) -> Result<(), Error> {
        ctx.value.write_uint(idx as u64 + 1);
        Ok(())
    }

    fn write_footer(ctx: &mut SerializationCtx<S>) -> Result<(), Error> {
        ctx.value.write_uint(0);
        Ok(())
    }

    pub(crate) fn serialize_newtype<T: ?Sized>(
        mut self,
        value: &T,
    ) -> Result<SerializationOk<S>, Error>
    where
        T: Serialize,
    {
        Self::write_header(&mut self.ctx, self.variant_idx)?;

        let type_id = if let Some(newtype_variant) = self.variant.as_newtype_variant() {
            *newtype_variant.inner_type()
        } else {
            return Err(ser::Error::custom(
                "variant type mismatch, expected newtype variant",
            ));
        };

        let de = FieldValueSerializer {
            ctx: self.ctx,
            type_id,
        };
        let mut ok = value.serialize(de)?;

        Self::write_footer(&mut ok.ctx)?;

        Ok(SerializationOk {
            ctx: ok.ctx,
            is_empty: false,
        })
    }

    pub(crate) fn serialize_struct(mut self) -> Result<SerializeStructVariantValue<S>, Error> {
        Self::write_header(&mut self.ctx, self.variant_idx)?;
        let struct_variant = self.variant.clone().try_map::<_, _, Error>(|variant| {
            if let Some(struct_variant) = variant.as_struct_variant() {
                Ok(struct_variant.fields())
            } else {
                Err(ser::Error::custom(
                    "variant type mismatch, expected newtype variant",
                ))
            }
        })?;

        Ok(SerializeStructVariantValue {
            inner: SerializeStructValue::from_parts(self.ctx, struct_variant),
        })
    }
}

pub(crate) struct SerializeStructVariantValue<S> {
    inner: SerializeStructValue<S>,
}

impl<S: Borrow<Schema>> ser::SerializeStructVariant for SerializeStructVariantValue<S> {
    type Ok = SerializationOk<S>;
    type Error = Error;

    fn serialize_field<T: ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> Result<(), Self::Error>
    where
        T: Serialize,
    {
        ser::SerializeStruct::serialize_field(&mut self.inner, key, value)
    }

    fn skip_field(&mut self, key: &'static str) -> Result<(), Self::Error> {
        ser::SerializeStruct::skip_field(&mut self.inner, key)
    }

    fn end(self) -> Result<Self::Ok, Self::Error> {
        let mut ok = ser::SerializeStruct::end(self.inner)?;
        ok.ctx.value.write_uint(0);
        ok.is_empty = false;
        Ok(ok)
    }
}
