//! A display wrapper for report lines.
//!
//! The Observatory's default renderer spells a value with its `Debug` form.
//! [`Line`] exists only so a report line is drawn as text, not as a quoted
//! `String`; it is data, not a renderer.

use std::fmt;

/// One report line, drawn without quotes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct Line(pub String);

impl fmt::Debug for Line {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
