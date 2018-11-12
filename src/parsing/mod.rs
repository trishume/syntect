//! Everything about parsing text into text annotated with scopes.
//! The most important struct here is `SyntaxSet`, check out the docs for that.
#[cfg(feature = "parsing")]
pub mod syntax_definition;
#[cfg(all( feature = "parsing", feature = "yaml-load"))]
mod yaml_load;
#[cfg(feature = "parsing")]
mod syntax_set;
#[cfg(feature = "parsing")]
mod parser;
#[cfg(feature = "metadata")]
pub mod metadata;

mod scope;

#[cfg(feature = "parsing")]
pub use self::syntax_definition::SyntaxDefinition;
#[cfg(all( feature = "parsing", feature = "yaml-load"))]
pub use self::yaml_load::*;
#[cfg(feature = "parsing")]
pub use self::syntax_set::*;
#[cfg(feature = "parsing")]
pub use self::parser::*;
#[cfg(feature = "metadata")]
pub use self::metadata::*;

pub use self::scope::*;
