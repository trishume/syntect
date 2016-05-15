extern crate yaml_rust;
mod syntax_definition;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
      use syntax_definition::{SyntaxDefinition};
      let defn : SyntaxDefinition = SyntaxDefinition::load_from_str("name: C\nscope: source.c\ncontexts: {}").unwrap();
      assert_eq!(defn.name, "C");
      assert_eq!(defn.scope, "source.c");
    }
}
