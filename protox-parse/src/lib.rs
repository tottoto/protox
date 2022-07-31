use logos::Span;
use miette::Diagnostic;
use prost_types::FileDescriptorProto;
use thiserror::Error;

mod ast;
mod case;
mod generate;
mod lex;
mod parse;
mod tag;
#[cfg(test)]
mod tests;

const MAX_MESSAGE_FIELD_NUMBER: i32 = 536_870_911;

#[derive(Error, Debug, Diagnostic, PartialEq)]
#[error("{}", kind)]
#[diagnostic(forward(kind))]
pub struct ParseError {
    kind: ParseErrorKind,
    #[related]
    related: Vec<ParseErrorKind>,
    #[source_code]
    source_code: String,
}

#[derive(Error, Debug, Diagnostic, PartialEq)]
pub(crate) enum ParseErrorKind {
    #[error("invalid token")]
    InvalidToken {
        #[label("found here")]
        span: Span,
    },
    #[error("integer is too large")]
    IntegerOutOfRange {
        #[label("integer defined here")]
        span: Span,
    },
    #[error("invalid string character")]
    InvalidStringCharacters {
        #[label("invalid characters")]
        span: Span,
    },
    #[error("unterminated string")]
    UnterminatedString {
        #[label("string starts here")]
        span: Span,
    },
    #[error("invalid string escape")]
    InvalidStringEscape {
        #[label("defined here")]
        span: Span,
    },
    #[error("string is not valid utf-8")]
    InvalidUtf8String {
        #[label("defined here")]
        span: Span,
    },
    #[error("nested block comments are not supported")]
    NestedBlockComment {
        #[label("defined here")]
        span: Span,
    },
    #[error("unknown syntax '{syntax}'")]
    #[diagnostic(help("possible values are 'proto2' and 'proto3'"))]
    UnknownSyntax {
        syntax: String,
        #[label("defined here")]
        span: Span,
    },
    #[error("invalid identifier")]
    #[diagnostic(help("identifiers must consist of letters, numbers and underscores, and may not start with a number"))]
    InvalidIdentifier {
        #[label("defined here")]
        span: Span,
    },
    #[error("invalid group name")]
    #[diagnostic(help(
        "group names must consist of a capital letter followed by letters, numbers and underscores"
    ))]
    InvalidGroupName {
        #[label("defined here")]
        span: Span,
    },
    #[error("invalid group name")]
    #[diagnostic(help(
        "imports may not contain backslashes, repeated forward slashes, '.' or '..' components"
    ))]
    InvalidImport {
        #[label("defined here")]
        span: Span,
    },
    #[error("multiple package names specified")]
    DuplicatePackage {
        #[label("defined here…")]
        first: Span,
        #[label("…and again here")]
        second: Span,
    },
    #[error("whitespace is required between an integer literal and an identifier")]
    NoSpaceBetweenIntAndIdent {
        #[label("found here")]
        span: Span,
    },
    #[error("'#' comments are not allowed here")]
    HashCommentOutsideTextFormat {
        #[label("found here")]
        span: Span,
    },
    #[error("'f' suffix for float literals is not allowed")]
    FloatSuffixOutsideTextFormat {
        #[label("found here")]
        span: Span,
    },
    #[error("a colon is required between a field name and scalar value")]
    MissingColonForScalarTextFormatField {
        #[label("expected ':' after field name here")]
        field_name: Span,
    },
    #[error("expected {expected}, but found '{found}'")]
    UnexpectedToken {
        expected: String,
        found: String,
        #[label("found here")]
        span: Span,
    },
    #[error("expected {expected}, but reached end of file")]
    UnexpectedEof { expected: String },
    #[error("identifiers may not be negative")]
    NegativeIdentOutsideDefault {
        #[label("found here")]
        span: Span,
    },
    #[error("message numbers must be between 1 and {}", MAX_MESSAGE_FIELD_NUMBER)]
    InvalidMessageNumber {
        #[label("defined here")]
        span: Span,
    },
    #[error("enum numbers must be between {} and {}", i32::MIN, i32::MAX)]
    InvalidEnumNumber {
        #[label("defined here")]
        span: Span,
    },
    #[error("{kind} fields may not have default values")]
    InvalidDefault {
        kind: &'static str,
        #[label("defined here")]
        span: Span,
    },
    #[error("default values are not allowed in proto3")]
    Proto3DefaultValue {
        #[label("defined here")]
        span: Span,
    },
    #[error("{kind} fields are not allowed in extensions")]
    InvalidExtendFieldKind {
        kind: &'static str,
        #[label("defined here")]
        span: Span,
    },
    #[error("extension fields may not be required")]
    RequiredExtendField {
        #[label("defined here")]
        span: Span,
    },
    #[error("map fields cannot have labels")]
    MapFieldWithLabel {
        #[label("defined here")]
        span: Span,
    },
    #[error("oneof fields cannot have labels")]
    OneofFieldWithLabel {
        #[label("defined here")]
        span: Span,
    },
    #[error("fields must have a label with proto2 syntax (expected one of 'optional', 'repeated' or 'required')")]
    Proto2FieldMissingLabel {
        #[label("field defined here")]
        span: Span,
    },
    #[error("groups are not allowed in proto3 syntax")]
    Proto3GroupField {
        #[label("defined here")]
        span: Span,
    },
    #[error("required fields are not allowed in proto3 syntax")]
    Proto3RequiredField {
        #[label("defined here")]
        span: Span,
    },
    #[error("{kind} fields are not allowed in a oneof")]
    InvalidOneofFieldKind {
        kind: &'static str,
        #[label("defined here")]
        span: Span,
    },
    #[error("a map field key type must be an integer, boolean or string")]
    InvalidMapFieldKeyType {
        #[label("defined here")]
        span: Span,
    },
    #[error("expected value to be {expected}, but found '{actual}'")]
    ValueInvalidType {
        expected: String,
        actual: String,
        #[label("defined here")]
        span: Span,
    },
    #[error("expected value to be {expected}, but the value is out of range")]
    #[diagnostic(help("the value must be between {min} and {max} inclusive"))]
    IntegerValueOutOfRange {
        expected: String,
        actual: String,
        min: String,
        max: String,
        #[label("defined here")]
        span: Span,
    },
    #[error("expected a string, but the value is not valid utf-8")]
    StringValueInvalidUtf8 {
        #[label("defined here")]
        span: Span,
    },
    #[error("a oneof must have at least one field")]
    EmptyOneof {
        #[label("defined here")]
        span: Span,
    },
    #[error("file is too large")]
    #[diagnostic(help("the maximum file length is 2,147,483,647 bytes"))]
    FileTooLarge,
}

pub fn parse(source: &str) -> Result<FileDescriptorProto, ParseError> {
    if source.len() > MAX_FILE_LEN {
        return Err(ParseError::new(vec![ParseErrorKind::FileTooLarge], source));
    }

    let ast = parse::parse_file(source).map_err(|errors| ParseError::new(errors, source))?;

    generate::generate_file(ast, source).map_err(|errors| ParseError::new(errors, source))
}

const MAX_FILE_LEN: usize = i32::MAX as usize;

fn index_to_i32(index: usize) -> i32 {
    // We enforce that all files parsed are at most i32::MAX bytes long. Therefore the indices of any
    // definitions in a single file must fit into an i32.
    index.try_into().unwrap()
}

fn join_span(start: Span, end: Span) -> Span {
    start.start..end.end
}

impl ParseError {
    fn new(mut related: Vec<ParseErrorKind>, source: impl Into<String>) -> Self {
        debug_assert!(!related.is_empty());
        let kind = related.remove(0);
        ParseError {
            kind,
            related,
            source_code: source.into(),
        }
    }
}