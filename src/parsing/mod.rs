//! Everything about parsing text into text annotated with scopes.
//! The most important struct here is `SyntaxSet`, check out the docs for that.
pub mod syntax_definition;
#[cfg(feature = "yaml-load")]
mod yaml_load;
mod syntax_set;
mod parser;

pub use self::syntax_definition::SyntaxDefinition;
#[cfg(feature = "yaml-load")]
pub use self::yaml_load::*;
pub use self::syntax_set::*;
pub use self::parser::*;
