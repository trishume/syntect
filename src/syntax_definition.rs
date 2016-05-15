use yaml_rust::{YamlLoader, Yaml};
use std::collections::{HashMap};

type Regex = String;
type Scope = String;
type CaptureMapping = HashMap<usize, Scope>;

#[derive(Debug)]
struct SyntaxDefinition {
    name: String,
    file_extensions: Vec<String>,
    default_scope: Scope,
    first_line_match: Regex,
    hidden: bool,

    variables: HashMap<String, String>,
    contexts: HashMap<String, Context>
}

#[derive(Debug)]
struct Context {
    meta_scope: Scope,
    meta_content_scope: Scope,
    meta_include_prototype: bool,

    includes: Vec<ContextReference>,
    patterns: Vec<MatchPattern>
}

#[derive(Debug)]
struct MatchPattern {
    regex: Regex,
    scope: Option<Scope>,
    captures: Option<CaptureMapping>,
    operation: MatchOperation
}

#[derive(Debug)]
enum ContextReference {
  Named(String),
  Scope {name: String, sub_context: Option<String>},
  File(String),
  Inline(Box<Context>)
}

#[derive(Debug)]
enum MatchOperation {
    Push(Vec<ContextReference>),
    Set(Vec<ContextReference>),
    Pop,
    None
}
