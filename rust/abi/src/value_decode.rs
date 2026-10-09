//! Bounded recursive decoding before allocating/deserializing a value tree.
//! The existing enum order, field order and postcard bytes are unchanged.
use crate::{Field, Handle, Value};
use alloc::{boxed::Box, string::String, vec::Vec};
use core::{cell::Cell, fmt};
use serde::{Deserialize, Deserializer, de::{self, DeserializeSeed, EnumAccess, MapAccess, SeqAccess, VariantAccess, Visitor}};

pub(crate) const MAX_DEPTH: usize = 64;
pub(crate) const MAX_NODES: usize = 4096;

#[derive(Deserialize)]
#[serde(field_identifier)]
enum Tag {Null,Bool,Signed,Unsigned,String,Bytes,List,Record,Variant,Handle}
struct Seed<'a> {depth:usize,left:&'a Cell<usize>}
impl<'de> Deserialize<'de> for Value {
    fn deserialize<D:Deserializer<'de>>(deserializer:D)->Result<Self,D::Error> {
        Seed{depth:0,left:&Cell::new(MAX_NODES)}.deserialize(deserializer)
    }
}
impl<'de> DeserializeSeed<'de> for Seed<'_> {
    type Value=Value;
    fn deserialize<D:Deserializer<'de>>(self,deserializer:D)->Result<Value,D::Error> {
        if self.depth>MAX_DEPTH || self.left.get()==0 {return Err(de::Error::custom("ABI value exceeds structural bounds"));}
        self.left.set(self.left.get()-1);
        deserializer.deserialize_enum("Value",&["Null","Bool","Signed","Unsigned","String","Bytes","List","Record","Variant","Handle"],self)
    }
}
impl<'de> Visitor<'de> for Seed<'_> {
    type Value=Value;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result {f.write_str("a bounded ABI value")}
    fn visit_enum<A:EnumAccess<'de>>(self,access:A)->Result<Value,A::Error> {
        let (tag,variant)=access.variant::<Tag>()?;
        let child=Seed{depth:self.depth+1,left:self.left};
        Ok(match tag {
            Tag::Null=>{variant.unit_variant()?;Value::Null},
            Tag::Bool=>Value::Bool(variant.newtype_variant()?),
            Tag::Signed=>Value::Signed(variant.newtype_variant()?),
            Tag::Unsigned=>Value::Unsigned(variant.newtype_variant()?),
            Tag::String=>Value::String(variant.newtype_variant()?),
            Tag::Bytes=>Value::Bytes(variant.newtype_variant()?),
            Tag::List=>Value::List(variant.newtype_variant_seed(Values(child))?),
            Tag::Record=>Value::Record(variant.newtype_variant_seed(Fields(child))?),
            Tag::Variant=>variant.struct_variant(&["case","value"],Variant(child))?,
            Tag::Handle=>Value::Handle(variant.newtype_variant::<Handle>()?),
        })
    }
}
struct Values<'a>(Seed<'a>);
impl<'de> DeserializeSeed<'de> for Values<'_> {
    type Value=Vec<Value>;
    fn deserialize<D:Deserializer<'de>>(self,d:D)->Result<Self::Value,D::Error> {d.deserialize_seq(self)}
}
impl<'de> Visitor<'de> for Values<'_> {
    type Value=Vec<Value>;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result {f.write_str("a bounded ABI list")}
    fn visit_seq<A:SeqAccess<'de>>(self,mut access:A)->Result<Self::Value,A::Error> {
        // Never reserve an attacker-provided length hint.
        let mut values=Vec::new();
        while let Some(value)=access.next_element_seed(Seed{depth:self.0.depth,left:self.0.left})? {values.push(value);}
        Ok(values)
    }
}
struct Fields<'a>(Seed<'a>);
impl<'de> DeserializeSeed<'de> for Fields<'_> {
    type Value=Vec<Field>;
    fn deserialize<D:Deserializer<'de>>(self,d:D)->Result<Self::Value,D::Error> {d.deserialize_seq(self)}
}
impl<'de> Visitor<'de> for Fields<'_> {
    type Value=Vec<Field>;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result {f.write_str("bounded ABI fields")}
    fn visit_seq<A:SeqAccess<'de>>(self,mut access:A)->Result<Self::Value,A::Error> {
        let mut fields=Vec::new();
        while let Some(field)=access.next_element_seed(FieldSeed(Seed{depth:self.0.depth,left:self.0.left}))? {fields.push(field);}
        Ok(fields)
    }
}
#[derive(Deserialize)]
#[serde(field_identifier)]
enum FieldKey {#[serde(rename="name")] Name,#[serde(rename="value")] Value}
struct FieldSeed<'a>(Seed<'a>);
impl<'de> DeserializeSeed<'de> for FieldSeed<'_> {
    type Value=Field;
    fn deserialize<D:Deserializer<'de>>(self,d:D)->Result<Field,D::Error> {d.deserialize_struct("Field",&["name","value"],self)}
}
impl<'de> Visitor<'de> for FieldSeed<'_> {
    type Value=Field;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result {f.write_str("an ABI field")}
    fn visit_seq<A:SeqAccess<'de>>(self,mut access:A)->Result<Field,A::Error> {
        let name=access.next_element()?.ok_or_else(||de::Error::missing_field("name"))?;
        let value=access.next_element_seed(self.0)?.ok_or_else(||de::Error::missing_field("value"))?;
        Ok(Field{name,value})
    }
    fn visit_map<A:MapAccess<'de>>(self,mut access:A)->Result<Field,A::Error> {
        let(mut name,mut value)=(None,None);
        while let Some(key)=access.next_key()? {match key {
            FieldKey::Name=>{if name.is_some(){return Err(de::Error::duplicate_field("name"));}name=Some(access.next_value::<String>()?);},
            FieldKey::Value=>{if value.is_some(){return Err(de::Error::duplicate_field("value"));}value=Some(access.next_value_seed(Seed{depth:self.0.depth,left:self.0.left})?);},
        }}
        Ok(Field{name:name.ok_or_else(||de::Error::missing_field("name"))?,value:value.ok_or_else(||de::Error::missing_field("value"))?})
    }
}
struct Optional<'a>(Seed<'a>);
impl<'de> DeserializeSeed<'de> for Optional<'_> {
    type Value=Option<Box<Value>>;
    fn deserialize<D:Deserializer<'de>>(self,d:D)->Result<Self::Value,D::Error>{d.deserialize_option(self)}
}
impl<'de> Visitor<'de> for Optional<'_> {
    type Value=Option<Box<Value>>;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result {f.write_str("an optional ABI payload")}
    fn visit_none<E:de::Error>(self)->Result<Self::Value,E>{Ok(None)}
    fn visit_unit<E:de::Error>(self)->Result<Self::Value,E>{Ok(None)}
    fn visit_some<D:Deserializer<'de>>(self,d:D)->Result<Self::Value,D::Error>{self.0.deserialize(d).map(|v|Some(Box::new(v)))}
}
#[derive(Deserialize)]
#[serde(field_identifier)]
enum VariantKey {#[serde(rename="case")] Case,#[serde(rename="value")] Value}
struct Variant<'a>(Seed<'a>);
impl<'de> Visitor<'de> for Variant<'_> {
    type Value=Value;
    fn expecting(&self,f:&mut fmt::Formatter)->fmt::Result {f.write_str("an ABI variant")}
    fn visit_seq<A:SeqAccess<'de>>(self,mut access:A)->Result<Value,A::Error>{
        let case=access.next_element()?.ok_or_else(||de::Error::missing_field("case"))?;
        let value=access.next_element_seed(Optional(self.0))?.ok_or_else(||de::Error::missing_field("value"))?;
        Ok(Value::Variant{case,value})
    }
    fn visit_map<A:MapAccess<'de>>(self,mut access:A)->Result<Value,A::Error>{
        let(mut case,mut value)=(None,None);
        while let Some(key)=access.next_key()? {match key {
            VariantKey::Case=>{if case.is_some(){return Err(de::Error::duplicate_field("case"));}case=Some(access.next_value::<String>()?);},
            VariantKey::Value=>{if value.is_some(){return Err(de::Error::duplicate_field("value"));}value=Some(access.next_value_seed(Optional(Seed{depth:self.0.depth,left:self.0.left}))?);},
        }}
        Ok(Value::Variant{case:case.ok_or_else(||de::Error::missing_field("case"))?,value:value.unwrap_or(None)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode,encode,CodecLimits,ProtocolError};
    use alloc::vec;
    #[test]
    fn deeply_nested_and_wide_postcard_values_fail_before_building_trees(){
        // List tag (6), length (1), repeated without constructing a deep Rust
        // value. This byte vector previously entered an unbounded derive.
        let mut bytes=Vec::new();for _ in 0..10000 {bytes.extend([6,1]);}bytes.push(0);
        assert_eq!(decode::<Value>(&bytes,CodecLimits::default()),Err(ProtocolError::Malformed));
        let value=Value::List(vec![Value::Null;MAX_NODES]);
        let bytes=encode(&value,CodecLimits::default()).unwrap();
        assert_eq!(decode::<Value>(&bytes,CodecLimits::default()),Err(ProtocolError::Malformed));
        let value=Value::List(vec![Value::Null;MAX_NODES-1]);
        let bytes=encode(&value,CodecLimits::default()).unwrap();assert_eq!(decode::<Value>(&bytes,CodecLimits::default()).unwrap(),value);
    }
    #[test]
    fn json_variants_records_and_postcard_keep_existing_value_shapes(){
        for value in [Value::Null,Value::Variant{case:"none".into(),value:None},Value::Variant{case:"some".into(),value:Some(Box::new(Value::Record(vec![Field{name:"synthetic".into(),value:Value::Signed(7)}])))},Value::Bytes(vec![0,255])] {
            let json=serde_json::to_value(&value).unwrap();assert_eq!(serde_json::from_value::<Value>(json).unwrap(),value);
            let bytes=encode(&value,CodecLimits::default()).unwrap();assert_eq!(decode::<Value>(&bytes,CodecLimits::default()).unwrap(),value);
        }
    }
}
