//! Fots
//!
//! Fots is a fuzzing oriented system call description language.
//! A fots file contains four kinds of item: type def, func def, group def and rule def.
//! ``` fots
//! type fd = res<i32>
//! struct stat{...}
//! flag statx_flags { xx = 0x0 }
//! type statx_mask = u64{0x001,0x002}
//!
//! group FileStat{
//!     fn stat(file *In cstr, statbuf *Out stat)
//!     fn lstat(file *In cstr, statbuf *Out stat)
//!     fn fstat(fd Fd, statbuf *Out stat)
//!     fn newfstatat(dfd i32{0}, file *In cstr, statbuf *Out stat, f statx_flags)
//!     fn statx(fd Fd, file *In cstr, flags statx_flags, mask statx_mask, statxbuf *Out statx)
//! }
//!
//! ```

#[macro_use]
extern crate pest_derive;
#[macro_use]
extern crate prettytable;
#[macro_use]
extern crate thiserror;

use pest::iterators::Pairs;
use pest::Parser;

use parse::{GrammarParser, Rule};

pub mod error;
pub mod items;
pub mod num;
pub mod parse;
pub mod types;

/// Parse plain text, return parse tree of text.
///
/// This should be useful if you want to build something like AST or
/// do some analysis.
///
/// ```
/// use fots::parse_grammar;
/// let text = "struct foo { arg1:i8, arg2:*[i8] }";
/// let mut pairs = parse_grammar(text).unwrap();   // pairs is an iterator
/// assert_eq!(pairs.next().unwrap().as_str(),text);
/// ```
pub fn parse_grammar(text: &str) -> Result<Pairs<Rule>, pest::error::Error<Rule>> {
    GrammarParser::parse(Rule::Root, text)
}

/// Parse plain text, return items of text or error.
///
/// ```
/// use fots::parse_items;
/// let text = "struct foo { arg1:i8, arg2:*[i8] }";
/// let mut re = parse_items(text);
/// assert!(re.is_ok());
/// ```
pub fn parse_items(text: &str) -> Result<types::Items, error::Error> {
    items::parse(text)
}
