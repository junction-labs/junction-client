use core::fmt;
use kube::core::Duration as KubeDuration;
use once_cell::sync::Lazy;
use regex;
use schemars::JsonSchema;
use serde::de::{self, Visitor};
use serde::{self, Deserialize, Deserializer, Serialize, Serializer};
use std::str::FromStr;
use std::time::Duration as StdDuration;

/// The fully qualified domain name of a network host. This matches the RFC 1123
/// definition of a hostname with 1 notable exception that numeric IP addresses
/// are not allowed.
pub type PreciseHostname = String;

/// Defines a network port.
pub type PortNumber = u16;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub enum BackendKind {
    Service,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type")]
pub enum StringMatch {
    #[serde(alias = "regex", alias = "Regex")]
    RegularExpression { value: Regex },
    #[serde(untagged)]
    Exact { value: String },
}

impl StringMatch {
    pub fn is_match(&self, s: &str) -> bool {
        match self {
            StringMatch::Exact { value } => value == s,
            StringMatch::RegularExpression { value } => value.is_match(s),
        }
    }
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

    fn visit_string<E>(self, value: std::string::String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match Regex::from_str(&value) {
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

#[cfg(test)]
mod test_string_matcher {
    use super::*;

    #[test]
    fn test_from_json() {
        let literal_match = serde_json::json!({
            "value": "literally this"
        });

        assert_eq!(
            serde_json::from_value::<StringMatch>(literal_match).unwrap(),
            StringMatch::Exact {
                value: "literally this".to_string()
            }
        );

        let regex_match = serde_json::json!({
            "type": "regex",
            "value": "foo.*bar",

        });

        assert_eq!(
            serde_json::from_value::<StringMatch>(regex_match).unwrap(),
            StringMatch::RegularExpression {
                value: Regex::from_str("foo.*bar").unwrap(),
            }
        );
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
            regex::Regex::new(GEP2257_PATTERN).expect(
                format!(
                    r#"GEP2257 regex "{}" did not compile (this is a bug!)"#,
                    GEP2257_PATTERN
                )
                .as_str(),
            )
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

    fn visit_string<E>(self, value: std::string::String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        match Duration::from_str(&value) {
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

///
/// Everything below is preserved from gateway API types, and may be completely
/// unnecessary if we dont use its "ref" concept in Junction's API.
///

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ParentRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,

    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section_name: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct BackendRef {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<u16>,

    pub kind: BackendKind,

    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    // Port specifies the destination port number to use for this resource.
    /// Port is required when the referent is a Kubernetes Service or DNS name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
}

///
/// this is what the gateway api uses for a mirror filter.
///
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MirrorBackendRef {
    pub kind: BackendKind,

    pub name: String,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,

    // Port specifies the destination port number to use for this resource.
    /// Port is required when the referent is a Kubernetes Service or DNS name
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<PortNumber>,
}
