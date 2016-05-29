extern crate yaml_rust;
extern crate onig;
extern crate walkdir;
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
        let mut repo = ScopeRepository::new();
        let defn: SyntaxDefinition =
            SyntaxDefinition::load_from_str("name: C\nscope: source.c\ncontexts: {main: []}", &mut repo)
                .unwrap();
        assert_eq!(defn.name, "C");
        assert_eq!(defn.scope, repo.build("source.c"));
    }
}
