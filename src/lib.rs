extern crate yaml_rust;
extern crate onig;
extern crate walkdir;
extern crate regex_syntax;
#[macro_use] extern crate lazy_static;
pub mod syntax_definition;
pub mod yaml_load;
pub mod package_set;
pub mod scope;
pub mod parser;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        use syntax_definition::SyntaxDefinition;
        use scope::*;
        let defn: SyntaxDefinition =
            SyntaxDefinition::load_from_str("name: C\nscope: source.c\ncontexts: {main: []}")
                .unwrap();
        assert_eq!(defn.name, "C");
        assert_eq!(defn.scope, Scope::new("source.c"));
    }
}
