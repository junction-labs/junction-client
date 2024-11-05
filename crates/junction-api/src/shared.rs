//! Shared configuration.

use core::fmt;
use serde::de::{self, Visitor};
use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;
use std::time::Duration as StdDuration;

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

/// A fraction, expressed as a numerator and a denominator.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Fraction {
    pub numerator: i32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<i32>,
}

/// A regular expression.
///
/// `Regex` has same syntax and semantics as Rust's [`regex` crate](https://docs.rs/regex/latest/regex/).
#[derive(Clone)]
pub struct Regex(regex::Regex);

impl std::fmt::Debug for Regex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        f.write_str(self.0.as_str())
    }
}

impl std::ops::Deref for Regex {
    type Target = regex::Regex;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl AsRef<regex::Regex> for Regex {
    fn as_ref(&self) -> &regex::Regex {
        &self.0
    }
}

impl FromStr for Regex {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match regex::Regex::try_from(s) {
            Ok(e) => Ok(Self(e)),
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(feature = "typeinfo")]
impl TypeInfo for Regex {
    fn kind() -> junction_typeinfo::Kind {
        junction_typeinfo::Kind::String
    }
}

impl serde::Serialize for Regex {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.0.as_str())
    }
}

struct RegexVisitor;

impl<'de> Visitor<'de> for RegexVisitor {
    type Value = Regex;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a Regex")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match Regex::from_str(value) {
            Ok(s) => Ok(s),
            Err(e) => Err(E::custom(format!("could not parse {}: {}", value, e))),
        }
    }
}

impl<'de> Deserialize<'de> for Regex {
    fn deserialize<D>(deserializer: D) -> Result<Regex, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(RegexVisitor)
    }
}

impl PartialEq for Regex {
    fn eq(&self, other: &Self) -> bool {
        self.0.as_str() == other.0.as_str()
    }
}

/// A wrapper around [std::time::Duration] that serializes to and from a f64
/// number of seconds.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct Duration(StdDuration);

impl Duration {
    pub const fn new(secs: u64, nanos: u32) -> Duration {
        Duration(StdDuration::new(secs, nanos))
    }
}

impl AsRef<StdDuration> for Duration {
    fn as_ref(&self) -> &StdDuration {
        &self.0
    }
}

impl std::ops::Deref for Duration {
    type Target = StdDuration;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::fmt::Debug for Duration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::fmt::Display for Duration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_secs_f64())
    }
}

#[cfg(feature = "typeinfo")]
impl TypeInfo for Duration {
    fn kind() -> junction_typeinfo::Kind {
        junction_typeinfo::Kind::Duration
    }
}

impl From<Duration> for StdDuration {
    fn from(val: Duration) -> Self {
        val.0
    }
}

impl From<StdDuration> for Duration {
    fn from(duration: StdDuration) -> Self {
        Duration(duration)
    }
}

macro_rules! duration_from {
    ($($(#[$attr:meta])* $method:ident: $arg:ty),* $(,)*) => {
        impl Duration {
            $(
            $(#[$attr])*
            pub fn $method(val: $arg) -> Self {
                Duration(StdDuration::$method(val))
            }
            )*
        }
    };
}

duration_from! {
    /// Create a new `Duration` from a whole number of seconds. See
    /// [Duration::from_secs][std::time::Duration::from_secs].
    from_secs: u64,

    /// Create a new `Duration` from a whole number of milliseconds. See
    /// [Duration::from_millis][std::time::Duration::from_millis].
    from_millis: u64,

    /// Create a new `Duration` from a whole number of microseconds. See
    /// [Duration::from_micros][std::time::Duration::from_micros].
    from_micros: u64,

    /// Create a new `Duration` from a floating point number of seconds. See
    /// [Duration::from_secs_f32][std::time::Duration::from_secs_f32].
    from_secs_f32: f32,


    /// Create a new `Duration` from a floating point number of seconds. See
    /// [Duration::from_secs_f64][std::time::Duration::from_secs_f64].
    from_secs_f64: f64,
}

impl Serialize for Duration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_f64(self.as_secs_f64())
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        // deserialize as a number of seconds, may be any int or float
        //
        // https://serde.rs/string-or-struct.html
        struct DurationVisitor;

        impl<'de> Visitor<'de> for DurationVisitor {
            type Value = Duration;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a Duration expressed as a number of seconds")
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Duration::from(StdDuration::from_secs_f64(v)))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(Duration::from(StdDuration::from_secs(v)))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let v: u64 = v
                    .try_into()
                    .map_err(|_| E::custom("Duration cannot be negative"))?;

                Ok(Duration::from(StdDuration::from_secs(v)))
            }
        }

        deserializer.deserialize_any(DurationVisitor)
    }
}

#[cfg(test)]
mod test_duration {
    use super::*;

    #[test]
    /// Duration should deserialize from strings, an int number of seconds, or a
    /// float number of seconds.
    fn test_duration_deserialize() {
        #[derive(Debug, Deserialize, PartialEq, Eq)]
        struct SomeValue {
            float_duration: Duration,
            int_duration: Duration,
        }

        let value = serde_json::json!({
            "string_duration": "1h23m4s",
            "float_duration": 1.234,
            "int_duration": 1234,
        });

        assert_eq!(
            serde_json::from_value::<SomeValue>(value).unwrap(),
            SomeValue {
                float_duration: Duration::from_millis(1234),
                int_duration: Duration::from_secs(1234),
            }
        );
    }

    #[test]
    /// Duration should always serialize to the string representation.
    fn test_duration_serialize() {
        #[derive(Debug, Serialize, PartialEq, Eq)]
        struct SomeValue {
            duration: Duration,
        }

        assert_eq!(
            serde_json::json!({
                "duration": 123.456,
            }),
            serde_json::to_value(SomeValue {
                duration: Duration::from_secs_f64(123.456),
            })
            .unwrap(),
        );
    }
}
