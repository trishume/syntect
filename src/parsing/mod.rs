//! Everything about parsing text into text annotated with scopes.
//! The most important struct here is `SyntaxSet`, check out the docs for that.
pub mod syntax_definition;
mod util;
mod yaml_load;
mod syntax_set;
mod scope;
mod parser;

pub use self::syntax_definition::SyntaxDefinition;
pub use self::yaml_load::*;
pub use self::syntax_set::*;
pub use self::scope::*;
pub use self::parser::*;
