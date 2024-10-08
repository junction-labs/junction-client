use std::{borrow::Cow, fmt::Write as _, str::FromStr};

/// An error converting a Junction API type into another type.
///
/// Errors should be treated as opaque, and contain a message about what went
/// wrong and a jsonpath style path to the field that caused problems.
#[derive(Clone, thiserror::Error)]
#[error("{path_str}: {message}", path_str = self.path())]
pub struct Error {
    // an error message
    message: String,

    // the reversed path to the field where the conversion error happened.
    //
    // the leaf of the path is built up at path[0] with the root of the
    // struct at the end. see ErrorContext for how this gets done.
    path: Vec<PathEntry>,
}

impl std::fmt::Debug for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Error")
            .field("message", &self.message)
            .field("path", &self.path())
            .finish()
    }
}

impl Error {
    pub fn path(&self) -> String {
        path_str(None, self.path.iter().rev())
    }

    /// Create a new error with a message.
    pub(crate) fn new(message: String) -> Self {
        Self {
            message,
            path: vec![],
        }
    }

    /// Create a new error with a static message.
    pub(crate) fn new_static(message: &'static str) -> Self {
        Self {
            message: message.to_string(),
            path: vec![],
        }
    }

    pub(crate) fn with_field(mut self, field: &'static str) -> Self {
        self.path.push(PathEntry::from(field));
        self
    }

    pub(crate) fn with_index(mut self, index: usize) -> Self {
        self.path.push(PathEntry::Index(index));
        self
    }
}

/// Join an iterator of PathEntry together into a path string.
///
/// This isn't quite `entries.join('.')` because index fields exist and have to
/// be bracketed.
pub(crate) fn path_str<'a, I, Iter>(prefix: Option<&'static str>, path: I) -> String
where
    I: IntoIterator<IntoIter = Iter>,
    Iter: Iterator<Item = &'a PathEntry> + DoubleEndedIterator,
{
    let path_iter = path.into_iter();
    // this is a random guess based on the fact that we'll often be allocating
    // something, but probably won't ever be allocating much.
    let mut buf = String::with_capacity(16 + prefix.map_or(0, |s| s.len()));

    if let Some(prefix) = prefix {
        let _ = buf.write_fmt(format_args!("{prefix}/"));
    }

    for (i, path_entry) in path_iter.enumerate() {
        if i > 0 && path_entry.is_field() {
            buf.push('.');
        }
        let _ = write!(&mut buf, "{}", path_entry);
    }

    buf
}

/// Parse a path string into a vec of [PathEntry], returning an [Error] with a
/// message (and no path!) if something goes wrong.
pub(crate) fn path_from_str(s: &str) -> Result<Vec<PathEntry>, Error> {
    // this is a small parser, and is probably missing some important edge
    // case testing. sorry in advance.

    let mut entries = vec![];
    let mut s = s;

    // strip a prefix off the front. everything in front of the first slash is
    // the prefix.
    if let Some(idx) = s.find('/') {
        s = &s[(idx + 1)..]
    };

    // parse either field or index entries until the string is empty.
    for _ in 0..10 {
        if s.is_empty() {
            break;
        }
        if s.starts_with('.') {
            s = &s[1..];
        }

        match s.starts_with('[') {
            // index entry
            true => {
                // find the closing `[` or this is malformed.
                let Some(entry_end) = s.find(']') else {
                    return Err(Error::new_static(
                        "invalid index entry: missing closing bracket",
                    ));
                };

                // parse the index as a number
                let idx_str = &s[1..entry_end];
                let idx = idx_str
                    .parse()
                    .map_err(|_| Error::new_static("index entry must be numeric"))?;
                entries.push(PathEntry::Index(idx));

                // start the next field after the closing bracket
                s = &s[(entry_end + 1)..];
            }
            // field entry
            false => {
                // the entry content starts at the beginning of the string and
                // goes until the next `.` or `[` or the end of the string.

                let idx = s.find(['.', '[']).unwrap_or(s.len());
                let field = &s[..idx];
                entries.push(PathEntry::from(field.to_string()));

                // the next entry starts at the delimiter. for field entries,
                // we strip leading dots, for index entries the `[` is required.
                s = &s[idx..];
            }
        }
    }

    Ok(entries)
}

/// Add field-path context to an error by appending an entry to its path. Because
/// Context is added at the callsite this means a function can add its own fields
/// and the path ends up in the appropriate order.
///
/// This trait isn't meant to be implemented, but it's not explicitly sealed
/// because it's only `pub(crate)`. Don't implement it!
pub(crate) trait ErrorContext<T>: Sized {
    fn with_field(self, field: &'static str) -> Result<T, Error>;
    fn with_index(self, index: usize) -> Result<T, Error>;

    /// Shorthand for `with_field(b).with_field(a)` but in a more intuitive
    /// order.
    fn with_fields(self, a: &'static str, b: &'static str) -> Result<T, Error> {
        self.with_field(b).with_field(a)
    }

    /// Shorthand for `with_index(idx).with_field(name)`, but in a slightly more
    /// inutitive order.
    fn with_field_index(self, field: &'static str, index: usize) -> Result<T, Error> {
        self.with_index(index).with_field(field)
    }
}

/// A JSON-path style path entry. An entry is either a field name or an index
/// into a sequence.
#[derive(Debug, PartialEq, Eq, Clone)]
pub(crate) enum PathEntry {
    Field(Cow<'static, str>),
    Index(usize),
}

impl PathEntry {
    fn is_field(&self) -> bool {
        matches!(self, PathEntry::Field(_))
    }
}

impl std::fmt::Display for PathEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PathEntry::Field(field) => f.write_str(field),
            PathEntry::Index(idx) => f.write_fmt(format_args!("[{idx}]")),
        }
    }
}

impl FromStr for PathEntry {
    type Err = &'static str;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // an index is always at least 3 chars, starts with [ and ends with ]
        if s.starts_with('[') {
            if s.len() <= 2 || !s.ends_with(']') {
                return Err("invalid field index: missing closing bracket");
            }

            // safety: we know the first and last chars are [] so it's safe to
            // slice single bytes off the front and back.
            let idx_str = &s[1..s.len() - 1];
            let idx = idx_str
                .parse()
                .map_err(|_| "invalid field index: field index must be a number")?;

            return Ok(PathEntry::Index(idx));
        }

        // parse anything that's not an index as a field name
        Ok(PathEntry::from(s.to_string()))
    }
}

impl From<String> for PathEntry {
    fn from(value: String) -> Self {
        PathEntry::Field(Cow::Owned(value))
    }
}

impl From<&'static str> for PathEntry {
    fn from(value: &'static str) -> Self {
        PathEntry::Field(Cow::Borrowed(value))
    }
}

impl<T> ErrorContext<T> for Result<T, Error> {
    fn with_field(self, field: &'static str) -> Result<T, Error> {
        match self {
            Ok(v) => Ok(v),
            Err(err) => Err(err.with_field(field)),
        }
    }

    fn with_index(self, index: usize) -> Result<T, Error> {
        match self {
            Ok(v) => Ok(v),
            Err(err) => Err(err.with_index(index)),
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_error_message() {
        fn baz() -> Result<(), Error> {
            Err(Error::new_static("it broke"))
        }

        fn bar() -> Result<(), Error> {
            baz().with_field_index("baz", 2)
        }

        fn foo() -> Result<(), Error> {
            bar().with_field("bar")
        }

        assert_eq!(foo().unwrap_err().to_string(), "bar.baz[2]: it broke",)
    }

    #[test]
    fn test_path_strings() {
        let path = &[
            PathEntry::Index(0),
            PathEntry::from("hi"),
            PathEntry::from("dr"),
            PathEntry::Index(2),
            PathEntry::from("nick"),
        ];
        let string = "[0].hi.dr[2].nick";
        assert_eq!(path_str(None, path), string);
        assert_eq!(path_from_str(string).unwrap(), path);

        let path = &[
            PathEntry::from("hi"),
            PathEntry::from("dr"),
            PathEntry::from("nick"),
        ];
        let string = "prefix/hi.dr.nick";
        assert_eq!(path_str(Some("prefix"), path), string);
        assert_eq!(path_from_str(string).unwrap(), path);
    }
}