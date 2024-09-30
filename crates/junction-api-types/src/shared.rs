use core::fmt;
use kube::core::Duration as KubeDuration;
use once_cell::sync::Lazy;
use regex;
use schemars::JsonSchema;
use serde::de::{self, Visitor};
use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;
use std::time::Duration as StdDuration;
use xds_api::pb::envoy::config::route::v3 as xds_route;

use crate::value_or_default;

#[cfg(feature = "typeinfo")]
use junction_typeinfo::TypeInfo;

/// The fully qualified domain name of a network host. This matches the RFC 1123
/// definition of a hostname with 1 notable exception that numeric IP addresses
/// are not allowed.
pub type PreciseHostname = String;

/// Defines a network port.
pub type PortNumber = u16;

/// A fraction, expressed as a numerator and a denominator.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct Fraction {
    pub numerator: i32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub denominator: Option<i32>,
}

/// A regular expression that will be processed by Rust's standard library
/// (https://docs.rs/regex/latest/regex/)
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

impl FromStr for Regex {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match regex::Regex::try_from(s) {
            Ok(e) => Ok(Self(e)),
            Err(e) => Err(e.to_string()),
        }
    }
}

/// A duration type where parsing and formatting obey the k8s Gateway API
/// GEP-2257 (https://gateway-api.sigs.k8s.io/geps/gep-2257).
///
/// This means when parsing from a string, the string must match
/// `^([0-9]{1,5}(h|m|s|ms)){1,4}$` and is otherwise parsed the same way that
/// Go's `time.ParseDuration` parses durations.
///
/// When formatting as a string, zero-valued durations must always be formatted
/// as `0s`, and non-zero durations must be formatted to with only one instance
/// of each applicable unit, greatest unit first.
#[derive(Copy, Clone, PartialEq, Eq, JsonSchema)]
pub struct Duration(StdDuration);

/// Regex pattern defining valid strings.
const GEP2257_PATTERN: &str = r"^([0-9]{1,5}(h|m|s|ms)){1,4}$";

/// Maximum duration that can be represented in milliseconds.
const MAX_DURATION_MS: u128 = (((99999 * 3600) + (59 * 60) + 59) * 1_000) + 999;

#[cfg(feature = "typeinfo")]
impl TypeInfo for Duration {
    fn kind() -> junction_typeinfo::Kind {
        junction_typeinfo::Kind::Duration
    }
}

/// Checks if a duration is valid. If it's not, return an error result
/// explaining why the duration is not valid.
fn is_valid(duration: StdDuration) -> Result<(), String> {
    // Check nanoseconds to see if we have sub-millisecond precision in this
    // duration.
    if duration.subsec_nanos() % 1_000_000 != 0 {
        return Err("Cannot express sub-millisecond precision in GEP-2257".to_string());
    }

    // Check the duration to see if it's greater than GEP-2257's maximum.
    if duration.as_millis() > MAX_DURATION_MS {
        return Err("Duration exceeds GEP-2257 maximum 99999h59m59s999ms".to_string());
    }

    Ok(())
}

/// Converting from `std::time::Duration` to `gateway_api::Duration` is allowed,
/// but we need to make sure that the incoming duration is valid according to
/// GEP-2257.
///
/// ```rust
/// use junction_api_types::shared::Duration;
/// use std::convert::TryFrom;
/// use std::time::Duration as StdDuration;
///
/// // A one-hour duration is valid according to GEP-2257.
/// let std_duration = StdDuration::from_secs(3600);
/// let duration = Duration::try_from(std_duration);
/// # assert!(duration.as_ref().is_ok());
/// # assert_eq!(format!("{}", duration.as_ref().unwrap()), "1h");
///
/// // This should output "Duration: 1h".
/// match duration {
///    Ok(d) => println!("Duration: {}", d),
///   Err(e) => eprintln!("Error: {}", e),
/// }
///
/// // A 600-nanosecond duration is not valid according to GEP-2257.
/// let std_duration = StdDuration::from_nanos(600);
/// let duration = Duration::try_from(std_duration);
/// # assert!(duration.is_err());
///
/// // This should output "Error: Cannot express sub-millisecond
/// // precision in GEP-2257".
/// match duration {
///    Ok(d) => println!("Duration: {}", d),
///   Err(e) => eprintln!("Error: {}", e),
/// }
/// ```
impl TryFrom<StdDuration> for Duration {
    type Error = String;

    fn try_from(duration: StdDuration) -> Result<Self, Self::Error> {
        // Check validity, and propagate any error if it's not.
        is_valid(duration)?;

        // It's valid, so we can safely convert it to a gateway_api::Duration.
        Ok(Duration(duration))
    }
}

/// Converting from `k8s::time::Duration` to `gateway_api::Duration` is allowed,
/// but we need to make sure that the incoming duration is valid according to
/// GEP-2257.
///
/// ```rust
/// use junction_api_types::shared::Duration;
/// use std::convert::TryFrom;
/// use std::str::FromStr;
/// use kube::core::Duration as KubeDuration;
///
/// // A one-hour duration is valid according to GEP-2257.
/// let k8s_duration = KubeDuration::from_str("1h").unwrap();
/// let duration = Duration::try_from(k8s_duration);
/// # assert!(duration.as_ref().is_ok());
/// # assert_eq!(format!("{}", duration.as_ref().unwrap()), "1h");
///
/// // This should output "Duration: 1h".
/// match duration {
///    Ok(d) => println!("Duration: {}", d),
///   Err(e) => eprintln!("Error: {}", e),
/// }
///
/// // A 600-nanosecond duration is not valid according to GEP-2257.
/// let k8s_duration = KubeDuration::from_str("600ns").unwrap();
/// let duration = Duration::try_from(k8s_duration);
/// # assert!(duration.as_ref().is_err());
///
/// // This should output "Error: Cannot express sub-millisecond
/// // precision in GEP-2257".
/// match duration {
///    Ok(d) => println!("Duration: {}", d),
///   Err(e) => eprintln!("Error: {}", e),
/// }
///
/// // kube::core::Duration can also express negative durations, which are not
/// // valid according to GEP-2257.
/// let k8s_duration = KubeDuration::from_str("-5s").unwrap();
/// let duration = Duration::try_from(k8s_duration);
/// # assert!(duration.as_ref().is_err());
///
/// // This should output "Error: Cannot express sub-millisecond
/// // precision in GEP-2257".
/// match duration {
///    Ok(d) => println!("Duration: {}", d),
///   Err(e) => eprintln!("Error: {}", e),
/// }
/// ```
impl TryFrom<KubeDuration> for Duration {
    type Error = String;

    fn try_from(duration: KubeDuration) -> Result<Self, Self::Error> {
        // We can't rely on kube::core::Duration to check validity for
        // gateway_api::Duration, so first we need to make sure that our
        // KubeDuration is not negative...
        if duration.is_negative() {
            return Err("Duration cannot be negative".to_string());
        }

        // Once we know it's not negative, we can safely convert it to a
        // std::time::Duration (which will always succeed) and then check it for
        // validity as in TryFrom<StdDuration>.
        let stddur = StdDuration::from(duration);
        is_valid(stddur)?;
        Ok(Duration(stddur))
    }
}

impl Duration {
    /// Create a new `gateway_api::Duration` from seconds and nanoseconds, while
    /// requiring that the resulting duration is valid according to GEP-2257.
    pub fn new(secs: u64, nanos: u32) -> Result<Self, String> {
        let stddur = StdDuration::new(secs, nanos);

        // Propagate errors if not valid, or unwrap the new Duration if all's
        // well.
        is_valid(stddur)?;
        Ok(Self(stddur))
    }

    /// Create a new `gateway_api::Duration` from seconds, while requiring that
    /// the resulting duration is valid according to GEP-2257.
    pub fn from_secs(secs: u64) -> Result<Self, String> {
        Self::new(secs, 0)
    }

    /// Create a new `gateway_api::Duration` from microseconds, while requiring
    /// that the resulting duration is valid according to GEP-2257.
    pub fn from_micros(micros: u64) -> Result<Self, String> {
        let sec = micros / 1_000_000;
        let ns = ((micros % 1_000_000) * 1_000) as u32;

        Self::new(sec, ns)
    }

    /// Create a new `gateway_api::Duration` from milliseconds, while requiring
    /// that the resulting duration is valid according to GEP-2257.
    pub fn from_millis(millis: u64) -> Result<Self, String> {
        let sec = millis / 1_000;
        let ns = ((millis % 1_000) * 1_000_000) as u32;

        Self::new(sec, ns)
    }

    /// The number of whole seconds in the entire duration.
    pub fn as_secs(&self) -> u64 {
        self.0.as_secs()
    }

    /// The number of milliseconds in the whole duration. GEP-2257 doesn't
    /// support sub-millisecond precision, so this is always exact.
    pub fn as_millis(&self) -> u128 {
        self.0.as_millis()
    }

    /// The number of nanoseconds in the whole duration. This is always exact.
    pub fn as_nanos(&self) -> u128 {
        self.0.as_nanos()
    }

    /// The number of nanoseconds in the part of the duration that's not whole
    /// seconds. Since GEP-2257 doesn't support sub-millisecond precision, this
    /// will always be 0 or a multiple of 1,000,000.
    pub fn subsec_nanos(&self) -> u32 {
        self.0.subsec_nanos()
    }

    /// Checks whether the duration is zero.
    pub fn is_zero(&self) -> bool {
        self.0.is_zero()
    }

    pub fn as_secs_f64(&self) -> f64 {
        self.0.as_secs_f64()
    }
}

/// Parsing a `gateway_api::Duration` from a string requires that the input
/// string obey GEP-2257:
///
/// - input strings must match `^([0-9]{1,5}(h|m|s|ms)){1,4}$`
/// - durations are parsed the same way that Go's `time.ParseDuration` does
///
/// If the input string is not valid according to GEP-2257, an error is returned
/// explaining what went wrong.
///
/// ```rust
/// use junction_api_types::shared::Duration;
/// use std::str::FromStr;
///
/// let duration = Duration::from_str("1h");
/// # assert!(duration.as_ref().is_ok());
/// # assert_eq!(format!("{}", duration.as_ref().unwrap()), "1h");
///
/// // This should output "Parsed duration: 1h".
/// match duration {
///    Ok(d) => println!("Parsed duration: {}", d),
///   Err(e) => eprintln!("Error: {}", e),
/// }
///
/// let duration = Duration::from_str("1h30m500ns");
/// # assert!(duration.as_ref().is_err());
///
/// // This should output "Error: Cannot express sub-millisecond
/// // precision in GEP-2257".
/// match duration {
///    Ok(d) => println!("Parsed duration: {}", d),
///   Err(e) => eprintln!("Error: {}", e),
/// }
/// ```
impl FromStr for Duration {
    type Err = String;

    // Parse a GEP-2257-compliant duration string into a
    // `gateway_api::Duration`.
    fn from_str(duration_str: &str) -> Result<Self, Self::Err> {
        // GEP-2257 dictates that string values must match GEP2257_PATTERN and
        // be parsed the same way that Go's time.ParseDuration parses durations.
        //
        // This Lazy Regex::new should never ever fail, given that the regex is
        // a compile-time constant. But just in case.....
        static RE: Lazy<regex::Regex> = Lazy::new(|| {
            regex::Regex::new(GEP2257_PATTERN).unwrap_or_else(|_| {
                panic!(
                    r#"GEP2257 regex "{}" did not compile (this is a bug!)"#,
                    GEP2257_PATTERN
                )
            })
        });

        // If the string doesn't match the regex, it's invalid.
        if !RE.is_match(duration_str) {
            return Err("Invalid duration format".to_string());
        }

        // We use kube::core::Duration to do the heavy lifting of parsing.
        match KubeDuration::from_str(duration_str) {
            // If the parse fails, return an error immediately...
            Err(err) => Err(err.to_string()),

            // ...otherwise, we need to try to turn the KubeDuration into a
            // gateway_api::Duration (which will check validity).
            Ok(kd) => Duration::try_from(kd),
        }
    }
}

/// Formatting a `gateway_api::Duration` for display is defined only for valid
/// durations, and must follow the GEP-2257 rules for formatting:
///
/// - zero-valued durations must always be formatted as `0s`
/// - non-zero durations must be formatted with only one instance of each
///   applicable unit, greatest unit first.
///
/// ```rust
/// use junction_api_types::shared::Duration;
/// use std::fmt::Display;
///
/// // Zero-valued durations are always formatted as "0s".
/// let duration = Duration::from_secs(0);
/// # assert!(duration.as_ref().is_ok());
/// assert_eq!(format!("{}", duration.unwrap()), "0s");
///
/// // Non-zero durations are formatted with only one instance of each
/// // applicable unit, greatest unit first.
/// let duration = Duration::from_secs(3600);
/// # assert!(duration.as_ref().is_ok());
/// assert_eq!(format!("{}", duration.unwrap()), "1h");
///
/// let duration = Duration::from_millis(1500);
/// # assert!(duration.as_ref().is_ok());
/// assert_eq!(format!("{}", duration.unwrap()), "1s500ms");
///
/// let duration = Duration::from_millis(9005500);
/// # assert!(duration.as_ref().is_ok());
/// assert_eq!(format!("{}", duration.unwrap()), "2h30m5s500ms");
/// ```
impl fmt::Display for Duration {
    /// Format a `gateway_api::Duration` for display, following GEP-2257 rules.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Short-circuit if the duration is zero, since "0s" is the special case
        // for a zero-valued duration.
        if self.is_zero() {
            return write!(f, "0s");
        }

        // Unfortunately, we can't rely on kube::core::Duration for formatting,
        // since it can happily hand back things like "5400s" instead of
        // "1h30m".
        //
        // So we'll do the formatting ourselves. Start by grabbing the
        // milliseconds part of the Duration (remember, the constructors make
        // sure that we don't have sub-millisecond precision)...
        let ms = self.subsec_nanos() / 1_000_000;

        // ...then after that, do the usual div & mod tree to take seconds and
        // get hours, minutes, and seconds from it.
        let mut secs = self.as_secs();

        let hours = secs / 3600;

        if hours > 0 {
            secs -= hours * 3600;
            write!(f, "{}h", hours)?;
        }

        let minutes = secs / 60;
        if minutes > 0 {
            secs -= minutes * 60;
            write!(f, "{}m", minutes)?;
        }

        if secs > 0 {
            write!(f, "{}s", secs)?;
        }

        if ms > 0 {
            write!(f, "{}ms", ms)?;
        }

        Ok(())
    }
}

/// Formatting a `gateway_api::Duration` for debug is the same as formatting it
/// for display.
impl fmt::Debug for Duration {
    /// Format a `gateway_api::Duration` for debug, following GEP-2257 rules.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Yes, we format GEP-2257 Durations the same in debug and display.
        fmt::Display::fmt(self, f)
    }
}

impl Serialize for Duration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

struct DurationVisitor;

impl<'de> Visitor<'de> for DurationVisitor {
    type Value = Duration;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("a GEP-2257 Duration as a string")
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match Duration::from_str(value) {
            Ok(s) => Ok(s),
            Err(e) => Err(E::custom(format!("could not parse {}: {}", value, e))),
        }
    }
}

impl<'de> Deserialize<'de> for Duration {
    fn deserialize<D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_string(DurationVisitor)
    }
}

impl TryFrom<xds_api::pb::google::protobuf::Duration> for Duration {
    type Error = String;

    fn try_from(pbduration: xds_api::pb::google::protobuf::Duration) -> Result<Self, Self::Error> {
        let duration = pbduration.try_into().unwrap();
        is_valid(duration)?;
        Ok(Duration(duration))
    }
}

#[cfg(test)]
mod test_duration {
    use super::*;

    #[test]
    /// Test that the validation logic in `Duration`'s constructor method(s)
    /// correctly handles known-good durations. (The tests are ordered to match
    /// the from_str test cases.)
    fn test_gep2257_from_valid_duration() {
        let test_cases = vec![
            Duration::from_secs(0),                        // 0s / 0h0m0s / 0m0s
            Duration::from_secs(3600),                     // 1h
            Duration::from_secs(1800),                     // 30m
            Duration::from_secs(10),                       // 10s
            Duration::from_millis(500),                    // 500ms
            Duration::from_secs(9000),                     // 2h30m / 150m
            Duration::from_secs(5410),                     // 1h30m10s / 10s30m1h
            Duration::new(7200, 600_000_000),              // 2h600ms
            Duration::new(7200 + 1800, 600_000_000),       // 2h30m600ms
            Duration::new(7200 + 1800 + 10, 600_000_000),  // 2h30m10s600ms
            Duration::from_millis(MAX_DURATION_MS as u64), // 99999h59m59s999ms
        ];

        for (idx, duration) in test_cases.iter().enumerate() {
            assert!(
                duration.is_ok(),
                "{:?}: Duration {:?} should be OK",
                idx,
                duration
            );
        }
    }

    #[test]
    /// Test that the validation logic in `Duration`'s constructor method(s)
    /// correctly handles known-bad durations.
    fn test_gep2257_from_invalid_duration() {
        let test_cases = vec![
            (
                Duration::from_micros(100),
                Err("Cannot express sub-millisecond precision in GEP-2257".to_string()),
            ),
            (
                Duration::from_secs(10000 * 86400),
                Err("Duration exceeds GEP-2257 maximum 99999h59m59s999ms".to_string()),
            ),
            (
                Duration::from_millis((MAX_DURATION_MS + 1) as u64),
                Err("Duration exceeds GEP-2257 maximum 99999h59m59s999ms".to_string()),
            ),
        ];

        for (idx, (duration, expected)) in test_cases.into_iter().enumerate() {
            assert_eq!(
                duration, expected,
                "{:?}: Duration {:?} should be an error",
                idx, duration
            );
        }
    }

    #[test]
    /// Test that the TryFrom implementation for KubeDuration correctly converts
    /// to gateway_api::Duration and validates the result.
    fn test_gep2257_from_valid_k8s_duration() {
        let test_cases = vec![
            (
                KubeDuration::from_str("0s").unwrap(),
                Duration::from_secs(0).unwrap(),
            ),
            (
                KubeDuration::from_str("1h").unwrap(),
                Duration::from_secs(3600).unwrap(),
            ),
            (
                KubeDuration::from_str("500ms").unwrap(),
                Duration::from_millis(500).unwrap(),
            ),
            (
                KubeDuration::from_str("2h600ms").unwrap(),
                Duration::new(7200, 600_000_000).unwrap(),
            ),
        ];

        for (idx, (k8s_duration, expected)) in test_cases.into_iter().enumerate() {
            let duration = Duration::try_from(k8s_duration);

            assert!(
                duration.as_ref().is_ok_and(|d| *d == expected),
                "{:?}: Duration {:?} should be {:?}",
                idx,
                duration,
                expected
            );
        }
    }

    #[test]
    /// Test that the TryFrom implementation for KubeDuration correctly fails
    /// for kube::core::Durations that aren't valid GEP-2257 durations.
    fn test_gep2257_from_invalid_k8s_duration() {
        let test_cases: Vec<(KubeDuration, Result<Duration, String>)> = vec![
            (
                KubeDuration::from_str("100us").unwrap(),
                Err("Cannot express sub-millisecond precision in GEP-2257".to_string()),
            ),
            (
                KubeDuration::from_str("100000h").unwrap(),
                Err("Duration exceeds GEP-2257 maximum 99999h59m59s999ms".to_string()),
            ),
            (
                KubeDuration::from(StdDuration::from_millis((MAX_DURATION_MS + 1) as u64)),
                Err("Duration exceeds GEP-2257 maximum 99999h59m59s999ms".to_string()),
            ),
            (
                KubeDuration::from_str("-5s").unwrap(),
                Err("Duration cannot be negative".to_string()),
            ),
        ];

        for (idx, (k8s_duration, expected)) in test_cases.into_iter().enumerate() {
            assert_eq!(
                Duration::try_from(k8s_duration),
                expected,
                "{:?}: KubeDuration {:?} should be error {:?}",
                idx,
                k8s_duration,
                expected
            );
        }
    }

    #[test]
    fn test_gep2257_from_str() {
        // Test vectors are mostly taken directly from GEP-2257, but there are
        // some extras thrown in and it's not meaningful to test e.g. "0.5m" in
        // Rust.
        let test_cases = vec![
            ("0h", Duration::from_secs(0)),
            ("0s", Duration::from_secs(0)),
            ("0h0m0s", Duration::from_secs(0)),
            ("1h", Duration::from_secs(3600)),
            ("30m", Duration::from_secs(1800)),
            ("10s", Duration::from_secs(10)),
            ("500ms", Duration::from_millis(500)),
            ("2h30m", Duration::from_secs(9000)),
            ("150m", Duration::from_secs(9000)),
            ("7230s", Duration::from_secs(7230)),
            ("1h30m10s", Duration::from_secs(5410)),
            ("10s30m1h", Duration::from_secs(5410)),
            ("100ms200ms300ms", Duration::from_millis(600)),
            ("100ms200ms300ms", Duration::from_millis(600)),
            (
                "99999h59m59s999ms",
                Duration::from_millis(MAX_DURATION_MS as u64),
            ),
            ("1d", Err("Invalid duration format".to_string())),
            ("1", Err("Invalid duration format".to_string())),
            ("1m1", Err("Invalid duration format".to_string())),
            (
                "1h30m10s20ms50h",
                Err("Invalid duration format".to_string()),
            ),
            ("999999h", Err("Invalid duration format".to_string())),
            ("1.5h", Err("Invalid duration format".to_string())),
            ("-15m", Err("Invalid duration format".to_string())),
            (
                "99999h59m59s1000ms",
                Err("Duration exceeds GEP-2257 maximum 99999h59m59s999ms".to_string()),
            ),
        ];

        for (idx, (duration_str, expected)) in test_cases.into_iter().enumerate() {
            assert_eq!(
                Duration::from_str(duration_str),
                expected,
                "{:?}: Duration {:?} should be {:?}",
                idx,
                duration_str,
                expected
            );
        }
    }

    #[test]
    fn test_gep2257_format() {
        // Formatting should always succeed for valid durations, and we've
        // covered invalid durations in the constructor and parse tests.
        let test_cases = vec![
            (Duration::from_secs(0), "0s".to_string()),
            (Duration::from_secs(3600), "1h".to_string()),
            (Duration::from_secs(1800), "30m".to_string()),
            (Duration::from_secs(10), "10s".to_string()),
            (Duration::from_millis(500), "500ms".to_string()),
            (Duration::from_secs(9000), "2h30m".to_string()),
            (Duration::from_secs(5410), "1h30m10s".to_string()),
            (Duration::from_millis(600), "600ms".to_string()),
            (Duration::new(7200, 600_000_000), "2h600ms".to_string()),
            (
                Duration::new(7200 + 1800, 600_000_000),
                "2h30m600ms".to_string(),
            ),
            (
                Duration::new(7200 + 1800 + 10, 600_000_000),
                "2h30m10s600ms".to_string(),
            ),
        ];

        for (idx, (duration, expected)) in test_cases.into_iter().enumerate() {
            assert!(
                duration
                    .as_ref()
                    .is_ok_and(|d| format!("{}", d) == expected),
                "{:?}: Duration {:?} should be {:?}",
                idx,
                duration,
                expected
            );
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct ServiceAttachment {
    ///
    /// The name of the Kubernetes Service
    ///
    pub name: String,

    ///
    /// The namespace of the Kubernetes service.
    /// FIXME(namespace): what should the semantic be when this is not specified:
    /// default, namespace of client, namespace of EZbake?
    ///
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    ///
    /// The port number of the Kubernetes service to target/
    /// attach to.
    ///
    /// When attaching policies, if it is not specified, the
    /// attachment will apply to all connections that don't have a specific
    /// port specified.
    ///
    /// When being used to lookup a backend after a matched rule,
    /// if it is not specified then it will use the same port as the incoming request
    ///
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
    // FIXME(gateway): Section of the gateway API to attach to
    // todo: this is needed eventually to support attaching to gateways
    // in the meantime though it just confuses things.
    //#[serde(default, skip_serializing_if = "Option::is_none")]
    //pub section_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct DNSAttachment {
    ///
    /// The DNS Name to target/attach to
    ///
    pub hostname: PreciseHostname,

    ///
    /// The port number to target/attach to.
    ///
    /// When attaching policies, if it is not specified, the
    /// attachment will apply to all connections that don't have a specific
    /// port specified.
    ///
    /// When being used to lookup a backend after a matched rule,
    /// if it is not specified then it will use the same port as the incoming request
    ///
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, JsonSchema)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum Attachment {
    DNS(DNSAttachment),

    #[serde(untagged)]
    Service(ServiceAttachment),
}

static KUBE_SERVICE_SUFFIX: &str = ".svc.cluster.local";

impl std::fmt::Display for Attachment {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_hostname())?;

        if let Some(port) = self.port() {
            f.write_fmt(format_args!(":{port}"))?;
        }

        Ok(())
    }
}

///
///  FIXME(ports): nothing with ports here will work until we move to xdstp
///
///  FIXME(DNS): For kube service hostnames, forcing the use of the ".svc.cluster.local"
///  suffix is icky,in that it stops the current k8s behavoiur of clients sending a request
///  to http://service_name, and having the namespace resolved based on where
///  the client is running.
///
///  However without it, we are going to have a hard time here saying when an incoming
///  hostname is targeted at DNS, vs when it is targeted at a kube service. That might
///  not be a problem though, in that if we can process XDS NACKs, we can use something
///  at a higher level to work out what it is.
///
///  So punting thinking more about this until we support DNS properly.
///
impl Attachment {
    pub fn as_hostname(&self) -> String {
        match self {
            Attachment::DNS(c) => c.hostname.clone(),
            Attachment::Service(c) => match &c.namespace {
                Some(x) => format!("{}.{}{}", c.name, x, KUBE_SERVICE_SUFFIX),
                None => format!("{}{}", c.name, KUBE_SERVICE_SUFFIX), //as per above discusstion, this is not right
            },
        }
    }

    pub fn from_hostname(hostname: &str, port: Option<u16>) -> Self {
        if hostname.ends_with(KUBE_SERVICE_SUFFIX) {
            let mut parts = hostname.split('.');
            let name = parts.next().unwrap().to_string();
            let namespace = parts.next().map(|x| x.to_string());
            Attachment::Service(ServiceAttachment {
                name,
                namespace,
                port,
            })
        } else {
            Attachment::DNS(DNSAttachment {
                hostname: hostname.to_string(),
                port,
            })
        }
    }

    // FIXME(DNS): no reason we cannot support DNS here, but likely it should
    // be determined by whats in the cluster config so passed as a extra parameter
    pub fn from_cluster_xds_name(name: &str) -> Result<Self, crate::xds::Error> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() == 3 && parts[2].eq("cluster") {
            Ok(Attachment::Service(ServiceAttachment {
                name: parts[1].to_string(),
                namespace: Some(parts[0].to_string()),
                port: None,
            }))
        } else {
            Err(crate::xds::Error::InvalidXds {
                resource_type: "Cluster",
                resource_name: name.to_string(),
                message: "Unable to parse name".to_string(),
            })
        }
    }

    pub fn as_cluster_xds_name(&self) -> String {
        match self {
            Attachment::DNS(c) => c.hostname.clone(),
            Attachment::Service(c) => match &c.namespace {
                Some(x) => format!("{}/{}/cluster", x, c.name),
                None => format!("{}/cluster", c.name),
            },
        }
    }

    pub fn from_listener_xds_name(name: &str) -> Option<Self> {
        //for now, the listener name is just the hostname
        Some(Self::from_hostname(name, None))
    }

    pub fn as_listener_xds_name(&self) -> String {
        //FIXME(ports): for now this is just the hostname, with no support for port
        self.as_hostname()
    }

    pub fn port(&self) -> Option<u16> {
        match self {
            Attachment::DNS(c) => c.port,
            //FIXME(namespace): work out what to do if namespace is optional
            Attachment::Service(c) => c.port,
        }
    }

    pub fn with_port(&self, port: u16) -> Self {
        match self {
            Attachment::DNS(c) => Attachment::DNS(DNSAttachment {
                port: Some(port),
                hostname: c.hostname.clone(),
            }),

            Attachment::Service(c) => Attachment::Service(ServiceAttachment {
                port: Some(port),
                name: c.name.clone(),
                namespace: c.namespace.clone(),
            }),
        }
    }
}

const WEIGHT_DEFAULT: u32 = 1;
fn weight_default() -> u32 {
    WEIGHT_DEFAULT
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct WeightedBackend {
    #[serde(default = "weight_default")]
    pub weight: u32,

    #[serde(flatten)]
    pub attachment: Attachment,
    //Todo: gateway API also allows filters here under an extended support condition
    // we need to decide whether this is one where its simpler just to drop it.
}

impl WeightedBackend {
    pub(crate) fn from_xds(
        xds: Option<&xds_route::route_action::ClusterSpecifier>,
    ) -> Result<Vec<Self>, crate::xds::Error> {
        match xds {
            Some(xds_route::route_action::ClusterSpecifier::Cluster(name)) => Ok(vec![Self {
                attachment: Attachment::from_cluster_xds_name(name)?,
                weight: 1,
            }]),
            Some(xds_route::route_action::ClusterSpecifier::WeightedClusters(weighted_cluster)) => {
                let mut ret: Vec<_> = vec![];
                for w in &weighted_cluster.clusters {
                    ret.push(Self {
                        attachment: Attachment::from_cluster_xds_name(&w.name)?,
                        weight: value_or_default!(w.weight, 1),
                    })
                }
                Ok(ret)
            }
            _ => {
                Err(crate::xds::Error::InvalidXds {
                    resource_type: "Cluster",
                    resource_name: "".to_string(), //ideally the xds enum would support a to_string here
                    message: "Unable to parse specifier".to_string(),
                })
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[serde(tag = "type")]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub enum SessionAffinityHashParamType {
    /// Hash the value of a header. If the header has multiple values, they will
    /// all be used as hash input.
    #[serde(alias = "header")]
    Header {
        /// The name of the header to use as hash input.
        name: String,
    },
}

// FIXME: Ben votes to skip the extra "affinity" naming here as its redundant
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, JsonSchema)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct SessionAffinityHashParam {
    /// Whether to stop immediately after hashing this value.
    ///
    /// This is useful if you want to try to hash a value, and then fall back
    /// to another as a default if it wasn't set.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub terminal: bool,

    #[serde(flatten)]
    pub matcher: SessionAffinityHashParamType,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default, JsonSchema)]
#[cfg_attr(feature = "typeinfo", derive(TypeInfo))]
pub struct SessionAffinityPolicy {
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        alias = "hashParams",
        alias = "HashParams"
    )]
    pub hash_params: Vec<SessionAffinityHashParam>,
}

impl SessionAffinityHashParam {
    //only returns session affinity
    pub fn from_xds(hash_policy: &xds_route::route_action::HashPolicy) -> Option<Self> {
        use xds_route::route_action::hash_policy::PolicySpecifier;

        match hash_policy.policy_specifier.as_ref() {
            Some(PolicySpecifier::Header(h)) => Some(SessionAffinityHashParam {
                terminal: hash_policy.terminal,
                matcher: SessionAffinityHashParamType::Header {
                    name: h.header_name.clone(),
                },
            }),
            _ => {
                //FIXME; thrown away config
                None
            }
        }
    }
}

impl SessionAffinityPolicy {
    //only returns session affinity
    pub fn from_xds(hash_policy: &[xds_route::route_action::HashPolicy]) -> Option<Self> {
        let hash_params: Vec<_> = hash_policy
            .iter()
            .filter_map(SessionAffinityHashParam::from_xds)
            .collect();

        if hash_params.is_empty() {
            None
        } else {
            Some(SessionAffinityPolicy { hash_params })
        }
    }
}

#[cfg(test)]
mod test_session_affinity {
    use crate::shared::SessionAffinityPolicy;
    use serde_json::json;

    #[test]
    fn parses_session_affinity_policy() {
        let test_json = json!({
            "hash_params": [
                { "type": "Header", "name": "FOO",  "terminal": true },
                { "type": "Header", "name": "FOO"}
            ]
        });
        let obj: SessionAffinityPolicy = serde_json::from_value(test_json.clone()).unwrap();
        let output_json = serde_json::to_value(obj).unwrap();
        assert_eq!(test_json, output_json);
    }
}
