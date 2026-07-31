// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::{
    policy_test_support::{
        authority_vocabulary::{
            LEGACY_COMMANDS, LEGACY_ENVIRONMENT, LEGACY_EXECUTABLES, LEGACY_OPTIONS, LEGACY_ORIGINS,
        },
        source_inventory::{
            CargoCommand, ScanErrorCause, SourceIdentity, SourceInventory, SourceNode,
            scan_workspace_with_command, walk_member,
        },
    },
    release_rail_tests::{command_path, workspace_root},
};
use proc_macro2::{TokenStream, TokenTree};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fmt, fs,
    path::PathBuf,
};
use syn::{
    Attribute, Block, Expr, ExprCall, ExprMethodCall, ExprPath, ImplItemFn, ItemFn, ItemMod, Lit,
    Macro, Signature, TraitItemFn, UseTree,
    visit::{self, Visit},
};

const RESERVED_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "host",
    "x-solstone-observer",
    "x-solstone-registration",
];

// Deliberate bounded-analysis gap: values assembled at runtime from non-literal
// inputs and deliberately encoded/obfuscated constants are not reconstructed.
// Their eventual use still crosses a locally resolved request, URL, process,
// environment, or header sink, which this policy classifies.

#[derive(Clone, Debug)]
struct PolicyError {
    identity: String,
    path: PathBuf,
    rule: &'static str,
    detail: String,
}

impl fmt::Display for PolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source policy: identity={} path={} rule={} detail={}",
            self.identity,
            self.path.display(),
            self.rule,
            self.detail
        )
    }
}

#[derive(Clone, Debug)]
struct FunctionFact {
    identity: SourceIdentity,
    path: PathBuf,
    name: String,
    test_only: bool,
    direct_sink: Option<(&'static str, String)>,
    calls: BTreeSet<String>,
}

fn attributes_are_test_only(attributes: &[Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        let name = attribute
            .path()
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        name.as_deref() == Some("test")
            || (name.as_deref() == Some("cfg")
                && attribute.meta.require_list().is_ok_and(|list| {
                    let cfg = list.tokens.to_string();
                    cfg == "test"
                        || (cfg.starts_with("all (")
                            && cfg
                                .split(|character: char| {
                                    !(character.is_ascii_alphanumeric() || character == '_')
                                })
                                .any(|token| token == "test"))
                }))
    })
}

fn flatten_use(tree: &UseTree, prefix: Vec<String>, aliases: &mut BTreeMap<String, Vec<String>>) {
    match tree {
        UseTree::Path(path) => {
            let mut prefix = prefix;
            prefix.push(path.ident.to_string());
            flatten_use(&path.tree, prefix, aliases);
        }
        UseTree::Name(name) => {
            let mut canonical = prefix;
            canonical.push(name.ident.to_string());
            aliases.insert(name.ident.to_string(), canonical);
        }
        UseTree::Rename(rename) => {
            let mut canonical = prefix;
            canonical.push(rename.ident.to_string());
            aliases.insert(rename.rename.to_string(), canonical);
        }
        UseTree::Group(group) => {
            for item in &group.items {
                flatten_use(item, prefix.clone(), aliases);
            }
        }
        UseTree::Glob(_) => {}
    }
}

fn canonical_path(path: &syn::Path, aliases: &BTreeMap<String, Vec<String>>) -> Vec<String> {
    let mut segments = path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>();
    if let Some(first) = segments.first()
        && let Some(prefix) = aliases.get(first)
    {
        let mut canonical = prefix.clone();
        canonical.extend(segments.drain(1..));
        return canonical;
    }
    segments
}

fn literal(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(value) => match &value.lit {
            Lit::Str(value) => Some(value.value()),
            Lit::ByteStr(value) => String::from_utf8(value.value()).ok(),
            _ => None,
        },
        Expr::Paren(value) => literal(&value.expr),
        Expr::Group(value) => literal(&value.expr),
        Expr::Binary(value) if matches!(value.op, syn::BinOp::Add(_)) => Some(format!(
            "{}{}",
            literal(&value.left)?,
            literal(&value.right)?
        )),
        Expr::Macro(value)
            if value.mac.path.is_ident("concat") || value.mac.path.is_ident("format") =>
        {
            Some(literal_tokens(value.mac.tokens.clone()))
        }
        _ => None,
    }
}

fn literal_tokens(tokens: TokenStream) -> String {
    let mut output = String::new();
    for token in tokens {
        match token {
            TokenTree::Literal(value) => {
                if let Ok(value) = syn::parse_str::<syn::LitStr>(&value.to_string()) {
                    output.push_str(&value.value());
                }
            }
            TokenTree::Group(group) => output.push_str(&literal_tokens(group.stream())),
            _ => {}
        }
    }
    output
}

fn forbidden_literal(value: &str) -> Option<&'static str> {
    let lower = value.to_ascii_lowercase();
    LEGACY_ENVIRONMENT
        .iter()
        .chain(LEGACY_OPTIONS)
        .chain(LEGACY_ORIGINS)
        .chain(LEGACY_COMMANDS)
        .copied()
        .find(|needle| lower.contains(&needle.to_ascii_lowercase()))
}

fn contains_env_call(expr: &Expr, aliases: &BTreeMap<String, Vec<String>>) -> bool {
    match expr {
        Expr::Call(call) => {
            matches!(&*call.func, Expr::Path(path) if {
                let path = canonical_path(&path.path, aliases).join("::");
                path.ends_with("env::var") || path.ends_with("env::var_os")
            }) || call
                .args
                .iter()
                .any(|argument| contains_env_call(argument, aliases))
        }
        Expr::MethodCall(call) => {
            contains_env_call(&call.receiver, aliases)
                || call
                    .args
                    .iter()
                    .any(|argument| contains_env_call(argument, aliases))
        }
        Expr::Paren(expr) => contains_env_call(&expr.expr, aliases),
        Expr::Group(expr) => contains_env_call(&expr.expr, aliases),
        _ => false,
    }
}

struct FunctionScanner<'a> {
    aliases: &'a BTreeMap<String, Vec<String>>,
    direct_sink: Option<(&'static str, String)>,
    calls: BTreeSet<String>,
    pushed_literals: String,
}

impl FunctionScanner<'_> {
    fn sink(&mut self, rule: &'static str, detail: impl Into<String>) {
        if self.direct_sink.is_none() {
            self.direct_sink = Some((rule, detail.into()));
        }
    }

    fn inspect_path_call(&mut self, node: &ExprCall, path: &ExprPath) {
        let canonical = canonical_path(&path.path, self.aliases);
        let joined = canonical.join("::");
        let last = canonical.last().cloned().unwrap_or_default();
        self.calls.insert(last);
        if (joined.ends_with("reqwest::Client::new")
            || joined.ends_with("reqwest::Client::builder")
            || joined.ends_with("reqwest::Request::new")
            || joined.ends_with("reqwest::RequestBuilder::new"))
            && self.direct_sink.is_none()
        {
            self.sink("network-constructor", joined);
        } else if (joined.ends_with("Url::parse") || joined.ends_with("Url::join"))
            && self.direct_sink.is_none()
        {
            self.sink("url-construction", joined);
        } else if joined.ends_with("std::process::Command::new")
            || joined.ends_with("process::Command::new")
            || joined == "Command::new"
        {
            if node
                .args
                .first()
                .and_then(literal)
                .as_deref()
                .is_some_and(|value| LEGACY_EXECUTABLES.contains(&value))
                || node
                    .args
                    .first()
                    .is_some_and(|argument| contains_env_call(argument, self.aliases))
            {
                self.sink("sol-executable", joined);
            }
        } else if (joined.ends_with("std::env::var")
            || joined.ends_with("std::env::var_os")
            || joined.ends_with("env::var")
            || joined.ends_with("env::var_os"))
            && node
                .args
                .first()
                .and_then(literal)
                .and_then(|value| forbidden_literal(&value))
                .is_some()
        {
            self.sink("legacy-environment", joined);
        }
        for argument in &node.args {
            if let Some(value) = literal(argument)
                && let Some(token) = forbidden_literal(&value)
            {
                self.sink("legacy-authority", token);
            }
        }
    }
}

impl<'ast> Visit<'ast> for FunctionScanner<'_> {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let Expr::Path(path) = &*node.func {
            self.inspect_path_call(node, path);
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_expr_method_call(&mut self, node: &'ast ExprMethodCall) {
        let method = node.method.to_string();
        if method == "bearer_auth" {
            self.sink("caller-owned-auth", method.clone());
        }
        if method == "header"
            && node.args.first().and_then(literal).is_some_and(|header| {
                RESERVED_HEADERS
                    .iter()
                    .any(|reserved| header.eq_ignore_ascii_case(reserved))
            })
        {
            self.sink("reserved-header", method.clone());
        }
        if matches!(method.as_str(), "headers_mut" | "query")
            && self
                .pushed_literals
                .to_ascii_lowercase()
                .contains("authorization")
        {
            self.sink("generic-request-escape", method.clone());
        }
        if method == "push_str"
            && let Some(value) = node.args.first().and_then(literal)
        {
            self.pushed_literals.push_str(&value);
            if let Some(token) = forbidden_literal(&self.pushed_literals) {
                self.sink("split-literal-authority", token);
            }
        }
        for argument in &node.args {
            if let Some(value) = literal(argument)
                && let Some(token) = forbidden_literal(&value)
            {
                self.sink("legacy-authority", token);
            }
        }
        visit::visit_expr_method_call(self, node);
    }

    fn visit_macro(&mut self, node: &'ast Macro) {
        let value = literal_tokens(node.tokens.clone());
        if let Some(token) = forbidden_literal(&value) {
            self.sink("split-literal-authority", token);
        }
        visit::visit_macro(self, node);
    }
}

struct FileScanner<'a> {
    node: &'a SourceNode,
    aliases: BTreeMap<String, Vec<String>>,
    module_test: Vec<bool>,
    facts: Vec<FunctionFact>,
}

impl<'a> FileScanner<'a> {
    fn new(node: &'a SourceNode) -> Self {
        let mut aliases = BTreeMap::new();
        for item in &node.syntax.items {
            if let syn::Item::Use(item) = item {
                flatten_use(&item.tree, Vec::new(), &mut aliases);
            }
        }
        Self {
            node,
            aliases,
            module_test: vec![node.identity.test_only],
            facts: Vec::new(),
        }
    }

    fn record_function(&mut self, signature: &Signature, attributes: &[Attribute], block: &Block) {
        let test_only = *self.module_test.last().unwrap() || attributes_are_test_only(attributes);
        let mut scanner = FunctionScanner {
            aliases: &self.aliases,
            direct_sink: None,
            calls: BTreeSet::new(),
            pushed_literals: String::new(),
        };
        scanner.visit_block(block);
        let mut identity = self.node.identity.clone();
        identity.item = Some(signature.ident.to_string());
        identity.test_only = test_only;
        self.facts.push(FunctionFact {
            identity,
            path: self.node.path.clone(),
            name: signature.ident.to_string(),
            test_only,
            direct_sink: scanner.direct_sink,
            calls: scanner.calls,
        });
    }
}

impl<'ast> Visit<'ast> for FileScanner<'_> {
    fn visit_item_mod(&mut self, node: &'ast ItemMod) {
        let inherited = *self.module_test.last().unwrap();
        self.module_test
            .push(inherited || attributes_are_test_only(&node.attrs));
        visit::visit_item_mod(self, node);
        self.module_test.pop();
    }

    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.record_function(&node.sig, &node.attrs, &node.block);
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.record_function(&node.sig, &node.attrs, &node.block);
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        if let Some(block) = &node.default {
            self.record_function(&node.sig, &node.attrs, block);
        }
    }
}

fn permitted_capability(fact: &FunctionFact) -> bool {
    fact.path
        .ends_with("crates/solstone-linux/src/private_link.rs")
        && matches!(
            fact.name.as_str(),
            "start_private_link_session_with_options"
                | "start_private_link_session_inner"
                | "confine_path"
                | "ensure_registered_inner"
                | "register"
                | "send"
                | "request"
        )
}

fn analyze_inventory(inventory: &SourceInventory) -> Result<(), PolicyError> {
    let mut facts = Vec::new();
    for node in &inventory.nodes {
        if node
            .syntax
            .items
            .iter()
            .any(|item| matches!(item, syn::Item::Mod(module) if module.ident == "chat_bridge"))
        {
            return Err(PolicyError {
                identity: node.identity.to_string(),
                path: node.path.clone(),
                rule: "deleted-module",
                detail: "chat_bridge".to_owned(),
            });
        }
        let mut scanner = FileScanner::new(node);
        scanner.visit_file(&node.syntax);
        facts.extend(scanner.facts);
    }
    let key = |fact: &FunctionFact| (fact.path.clone(), fact.name.clone(), fact.test_only);
    let mut sink_keys = facts
        .iter()
        .filter(|fact| fact.direct_sink.is_some() && !fact.test_only && !permitted_capability(fact))
        .map(key)
        .collect::<BTreeSet<(PathBuf, String, bool)>>();
    loop {
        let newly = facts
            .iter()
            .filter(|fact| {
                !fact.test_only
                    && !permitted_capability(fact)
                    && fact.calls.iter().any(|callee| {
                        let local = (fact.path.clone(), callee.clone(), fact.test_only);
                        if sink_keys.contains(&local) {
                            return true;
                        }
                        let matches = facts
                            .iter()
                            .filter(|candidate| {
                                candidate.name == *callee
                                    && (!fact.test_only || candidate.test_only == fact.test_only)
                            })
                            .collect::<Vec<_>>();
                        matches.len() == 1 && sink_keys.contains(&key(matches[0]))
                    })
            })
            .map(key)
            .collect::<BTreeSet<_>>();
        let before = sink_keys.len();
        sink_keys.extend(newly);
        if sink_keys.len() == before {
            break;
        }
    }
    if let Some(fact) = facts.iter().find(|fact| {
        !fact.test_only && !permitted_capability(fact) && sink_keys.contains(&key(fact))
    }) {
        let (rule, detail) = fact
            .direct_sink
            .clone()
            .unwrap_or(("sink-call-graph", fact.name.clone()));
        return Err(PolicyError {
            identity: fact.identity.to_string(),
            path: fact.path.clone(),
            rule,
            detail,
        });
    }
    Ok(())
}

fn current_inventory() -> SourceInventory {
    let mut inventory = scan_workspace_with_command(
        &workspace_root(),
        &CargoCommand {
            program: command_path("cargo"),
            prefix: Vec::new(),
        },
    )
    .unwrap();
    inventory
        .nodes
        .retain(|node| node.identity.package == "solstone-linux");
    inventory
}

#[test]
fn linked_authority_is_confined_to_the_typed_capability() {
    analyze_inventory(&current_inventory()).unwrap();
}

fn mutation(source: &str, test_only: bool) -> SourceInventory {
    let syntax = syn::parse_file(source).unwrap();
    SourceInventory {
        nodes: vec![SourceNode {
            identity: SourceIdentity {
                package: "fixture".to_owned(),
                target: "fixture".to_owned(),
                target_kind: "bin".to_owned(),
                module: Vec::new(),
                item: None,
                cfg_context: Vec::new(),
                test_only,
            },
            path: PathBuf::from("src/main.rs"),
            syntax,
        }],
        data_inputs: Vec::new(),
    }
}

fn fixture_identity() -> SourceIdentity {
    SourceIdentity {
        package: "fixture".to_owned(),
        target: "fixture".to_owned(),
        target_kind: "bin".to_owned(),
        module: Vec::new(),
        item: None,
        cfg_context: Vec::new(),
        test_only: false,
    }
}

fn assert_mutation(source: &str, rule: &str) {
    let error = analyze_inventory(&mutation(source, false)).unwrap_err();
    assert_eq!(error.rule, rule);
    assert!(error.identity.contains("fixture::fixture[bin]"));
    assert!(error.to_string().contains("identity=fixture::fixture[bin]"));
}

#[test]
fn linked_authority_mutation_aliases_are_semantic() {
    assert_mutation(
        "use reqwest::Client as Renamed; fn moved() { let _ = Renamed::builder(); }",
        "network-constructor",
    );
    assert_mutation(
        "use std::process::Command as Renamed; fn moved() { let _ = Renamed::new(\"sol\"); }",
        "sol-executable",
    );
    assert_mutation(
        "fn moved(r: reqwest::RequestBuilder) { let _ = r.header(\"authorization\", \"x\"); }",
        "reserved-header",
    );
}

#[test]
fn linked_authority_mutation_split_literal_is_folded() {
    assert_mutation(
        "fn moved() { let _ = concat!(\"local\", \"host:5015\"); }",
        "split-literal-authority",
    );
}

#[test]
fn linked_authority_mutation_env_derived_executable_is_caught() {
    assert_mutation(
        "fn moved() { let _ = std::process::Command::new(std::env::var(\"TOOL\").unwrap()); }",
        "sol-executable",
    );
}

#[test]
fn linked_authority_mutation_renamed_helper_reaches_fixpoint() {
    assert_mutation(
        "fn renamed() { let _ = reqwest::Client::builder(); } fn caller() { renamed(); }",
        "network-constructor",
    );
}

#[test]
fn linked_authority_mutation_extra_target_and_feature_are_production() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
    let source = root.path().join("src/direct.rs");
    fs::write(
        &source,
        "#[cfg(feature=\"direct\")] fn feature_binary() { let _ = reqwest::Client::new(); }",
    )
    .unwrap();
    let metadata = serde_json::json!({
        "workspace_root": root.path(),
        "target_directory": root.path().join("target"),
        "build_directory": root.path().join("target"),
        "packages": [{
            "name": "fixture",
            "targets": [{
                "name": "direct",
                "kind": ["bin"],
                "src_path": source,
            }]
        }]
    });
    let script = root.path().join("metadata.sh");
    fs::write(&script, format!("printf '%s' '{}'\n", metadata)).unwrap();
    let inventory = scan_workspace_with_command(
        root.path(),
        &CargoCommand {
            program: command_path("bash"),
            prefix: vec![OsString::from(script)],
        },
    )
    .unwrap();
    let error = analyze_inventory(&inventory).unwrap_err();
    assert_eq!(error.rule, "network-constructor");
    assert!(error.identity.contains("fixture::direct[bin]"));
}

#[test]
fn linked_authority_mutation_path_include_and_generated_inputs_name_identity() {
    for declaration in [
        "#[path=\"direct.rs\"] mod renamed;",
        "include!(\"direct.rs\");",
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        let main = root.path().join("src/main.rs");
        fs::write(&main, declaration).unwrap();
        fs::write(
            root.path().join("src/direct.rs"),
            "fn renamed_helper() { let _ = reqwest::Client::new(); }",
        )
        .unwrap();
        let mut inventory = SourceInventory::default();
        walk_member(root.path(), &main, fixture_identity(), &mut inventory).unwrap();
        let error = analyze_inventory(&inventory).unwrap_err();
        assert_eq!(error.rule, "network-constructor");
        assert!(error.identity.contains("fixture::fixture[bin]"));
    }

    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    let main = root.path().join("src/main.rs");
    fs::write(
        &main,
        "include!(concat!(env!(\"OUT_DIR\"), \"/direct.rs\"));",
    )
    .unwrap();
    let mut inventory = SourceInventory::default();
    let error = walk_member(root.path(), &main, fixture_identity(), &mut inventory).unwrap_err();
    assert_eq!(error.cause, ScanErrorCause::UnclassifiableInput);
    assert!(error.identity.contains("fixture::fixture[bin]"));
}

#[test]
fn linked_authority_test_context_is_ancestry_based() {
    analyze_inventory(&mutation(
        "#[cfg(test)] mod peer { fn request() { let _ = reqwest::Client::new(); } }",
        false,
    ))
    .unwrap();
    analyze_inventory(&mutation(
        "#[test] fn request() { let _ = reqwest::Client::new(); }",
        false,
    ))
    .unwrap();
    analyze_inventory(&mutation(
        "fn request() { let _ = reqwest::Client::new(); }",
        true,
    ))
    .unwrap();
}
