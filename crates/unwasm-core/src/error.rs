//! Failure, said out loud.
//!
//! There is no `Unsupported => skip` in this crate. A decompiler that drops an
//! instruction it does not know emits code that runs and is wrong, which is the
//! one outcome worse than emitting nothing: the reader has no way to tell.
//! Every gap comes back as an [`Error::Unsupported`] naming the construct and
//! where it was found.

use std::fmt;

/// What went wrong.
#[derive(Debug)]
pub enum Error {
    /// The bytes are not a valid wasm module. Comes straight from the decoder.
    Decode(wasmparser::BinaryReaderError),
    /// A construct this version does not model. Never a silent skip.
    Unsupported {
        /// The construct, named the way wasm names it.
        construct: String,
        /// Where it turned up — a function, a section, a segment.
        location: String,
    },
    /// The module is well-formed but says something impossible for the code we
    /// would have to emit — an index out of range, a stack that underflows.
    Invalid {
        /// What did not add up.
        problem: String,
        /// Where it turned up.
        location: String,
    },
}

impl Error {
    /// Reports an unsupported construct.
    pub fn unsupported(construct: impl Into<String>, location: impl Into<String>) -> Self {
        Self::Unsupported {
            construct: construct.into(),
            location: location.into(),
        }
    }

    /// Reports a module that cannot mean what it says.
    pub fn invalid(problem: impl Into<String>, location: impl Into<String>) -> Self {
        Self::Invalid {
            problem: problem.into(),
            location: location.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(inner) => write!(f, "decoding the module failed: {inner}"),
            Self::Unsupported {
                construct,
                location,
            } => write!(
                f,
                "unsupported: {construct} (in {location}). \
                 This is a gap in unwasm, not a problem with the module; \
                 nothing was emitted for it rather than emitting a guess."
            ),
            Self::Invalid { problem, location } => {
                write!(f, "invalid module: {problem} (in {location})")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(inner) => Some(inner),
            _ => None,
        }
    }
}

impl From<wasmparser::BinaryReaderError> for Error {
    fn from(inner: wasmparser::BinaryReaderError) -> Self {
        Self::Decode(inner)
    }
}

/// The crate's result type.
pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unsupported_construct_names_itself_and_its_location() {
        let error = Error::unsupported("v128.load", "function #12");
        let text = error.to_string();
        assert!(text.contains("v128.load"), "{text}");
        assert!(text.contains("function #12"), "{text}");
        // The message has to say that nothing was emitted, because the reader's
        // next question is always "so what did it do instead?".
        assert!(text.contains("rather than emitting a guess"), "{text}");
    }

    #[test]
    fn an_invalid_module_says_what_did_not_add_up() {
        let text = Error::invalid("stack underflow", "function #3").to_string();
        assert!(text.contains("stack underflow"), "{text}");
        assert!(text.contains("function #3"), "{text}");
    }

    #[test]
    fn only_a_decode_failure_has_an_underlying_cause() {
        // The other two are this crate's own judgements; there is nothing
        // underneath them to point at.
        let unsupported = Error::unsupported("v128", "somewhere");
        assert!(std::error::Error::source(&unsupported).is_none());
        let invalid = Error::invalid("nonsense", "somewhere");
        assert!(std::error::Error::source(&invalid).is_none());
    }

    #[test]
    fn a_decode_failure_keeps_the_decoder_error_as_its_source() {
        let decoded = crate::Module::parse(b"not wasm at all");
        let error = decoded.expect_err("random bytes are not a module");
        assert!(matches!(error, Error::Decode(_)));
        assert!(std::error::Error::source(&error).is_some());
        assert!(error.to_string().contains("decoding the module failed"));
    }
}
