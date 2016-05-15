extern crate yaml_rust;
mod syntax_definition;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
      use syntax_definition::{SyntaxDefinition};
      let defn : SyntaxDefinition = SyntaxDefinition::load_from_str("name: C\nscope: source.c").unwrap();
      assert_eq!(defn.name, "C");
      assert_eq!(defn.scope, "source.c");
      let exts_empty : Vec<String> = Vec::new();
      assert_eq!(defn.file_extensions, exts_empty);
      assert_eq!(defn.hidden, false);
      assert!(defn.variables.is_empty());
      let defn2 : SyntaxDefinition = SyntaxDefinition::load_from_str("
        name: C
        scope: source.c
        file_extensions: [c, h]
        hidden: true
        variables:
          ident: '[A-Za-z_][A-Za-z_0-9]*'
      ").unwrap();
      assert_eq!(defn2.name, "C");
      assert_eq!(defn2.scope, "source.c");
      let exts : Vec<String> = vec![String::from("c"), String::from("h")];
      assert_eq!(defn2.file_extensions, exts);
      assert_eq!(defn2.hidden, true);
      assert_eq!(defn2.variables.get("ident").unwrap(), "[A-Za-z_][A-Za-z_0-9]*");
    }
}
