use serde_json::{Map, Number, Value};
use steel::{
    HashMap, SteelVal,
    gc::Gc,
    rerrs::{ErrorKind, SteelErr},
    rvals::{FromSteelVal, IntoSteelVal, Result},
};

pub(crate) struct Json(pub Value);

impl IntoSteelVal for Json {
    fn into_steelval(self) -> Result<SteelVal> {
        match self.0 {
            Value::Null => Ok(SteelVal::Void),
            Value::Bool(value) => value.into_steelval(),
            Value::String(value) => value.into_steelval(),
            Value::Number(value) => {
                if let Some(value) = value.as_i64() {
                    return Ok(value.into());
                }
                if let Some(value) = value.as_u64() {
                    return Ok(u128::from(value).into());
                }
                match value.as_f64() {
                    Some(value) if value.is_finite() => Ok(SteelVal::NumV(value)),
                    _ => Err(conversion_error("JSON number is not finite")),
                }
            }
            Value::Array(values) => Ok(SteelVal::ListV(
                values
                    .into_iter()
                    .map(|value| Json(value).into_steelval())
                    .collect::<Result<_>>()?,
            )),
            Value::Object(values) => {
                let mut map = HashMap::new();
                for (key, value) in values {
                    map.insert(SteelVal::SymbolV(key.into()), Json(value).into_steelval()?);
                }
                Ok(SteelVal::HashMapV(Gc::new(map).into()))
            }
        }
    }
}

impl FromSteelVal for Json {
    fn from_steelval(value: &SteelVal) -> Result<Self> {
        let value = match value {
            SteelVal::Void => Value::Null,
            SteelVal::BoolV(value) => Value::Bool(*value),
            SteelVal::IntV(value) => Value::Number((*value).into()),
            SteelVal::BigNum(value) => {
                if let Ok(value) = i64::try_from(value.as_ref()) {
                    Value::Number(value.into())
                } else if let Ok(value) = u64::try_from(value.as_ref()) {
                    Value::Number(value.into())
                } else {
                    return Err(conversion_error("integer exceeds the JSON integer range"));
                }
            }
            SteelVal::NumV(value) => {
                Value::Number(Number::from_f64(*value).ok_or_else(|| {
                    conversion_error("non-finite number is not JSON serializable")
                })?)
            }
            SteelVal::CharV(value) => Value::String(value.to_string()),
            SteelVal::StringV(value) | SteelVal::SymbolV(value) => Value::String(value.to_string()),
            SteelVal::ListV(values) => Value::Array(
                values
                    .iter()
                    .map(|value| Self::from_steelval(value).map(|value| value.0))
                    .collect::<Result<_>>()?,
            ),
            SteelVal::VectorV(values) => Value::Array(
                values
                    .iter()
                    .map(|value| Self::from_steelval(value).map(|value| value.0))
                    .collect::<Result<_>>()?,
            ),
            SteelVal::HashSetV(values) => Value::Array(
                values
                    .iter()
                    .map(|value| Self::from_steelval(value).map(|value| value.0))
                    .collect::<Result<_>>()?,
            ),
            SteelVal::HashMapV(values) => {
                let mut map = Map::new();
                for (key, value) in values.iter() {
                    map.insert(String::from_steelval(key)?, Self::from_steelval(value)?.0);
                }
                Value::Object(map)
            }
            _ => return Err(conversion_error("type is not JSON serializable")),
        };
        Ok(Self(value))
    }
}

fn conversion_error(message: &str) -> SteelErr {
    SteelErr::new(ErrorKind::ConversionError, message.into())
}
