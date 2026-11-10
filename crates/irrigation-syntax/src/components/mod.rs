//! The pipeline components.
//!
//! Each component owns one View and reads only the delta of the View before it:
//! text → tokens → AST → metadata. There is no stage ordering beyond the units
//! themselves, and no component looks at the whole document unless it is
//! starting from scratch.

pub mod lexer;
pub mod parser;
pub mod text;

pub use lexer::LexerComponent;
pub use parser::{AstConsumer, ParserComponent};
pub use text::TextComponent;
