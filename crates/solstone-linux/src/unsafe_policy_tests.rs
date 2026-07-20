// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::release_rail_tests::workspace_root;
use proc_macro2::{TokenStream, TokenTree};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprCall, ExprPath, ForeignItemFn, ImplItemFn, ItemFn, Meta, StaticMutability,
    Stmt, Token, TraitItemFn,
};

#[derive(Debug)]
struct Inventory {
    findings: Vec<Finding>,
    files_inspected: usize,
    nested_src_files: usize,
    build_scripts: usize,
    members: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Finding {
    path: PathBuf,
    kind: FindingKind,
    enclosing_item: Option<String>,
    node: NodeIdentity,
    unsafe_blocks: Vec<UnsafeBlockDetail>,
    attribute_style: Option<AttributePlacement>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FindingKind {
    UnsafeBlock,
    UnsafeFunction,
    UnsafeTrait,
    UnsafeImpl,
    UnsafeExternBlock,
    UnsafeModule,
    StaticMut,
    ForeignStaticMut,
    UnsafeAttribute,
    NoMangleAttribute,
    ExportNameAttribute,
    LinkSectionAttribute,
    UnsafeCodeAllowance,
    UnsafeCodeExpectation,
    UnsafeCodeWarning,
    GlobalAsm,
    NakedAsm,
    UnsafeMacroToken,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NodeIdentity {
    ordinal: usize,
    ancestry: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct UnsafeBlockDetail {
    node: NodeIdentity,
    statement_count: usize,
    calls: Vec<CallIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CallIdentity {
    callee: String,
    arguments: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttributePlacement {
    Inner,
    FunctionOuter,
    OtherOuter,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScanErrorCause {
    RootManifestRead,
    RootManifestUtf8,
    RootManifestToml,
    WorkspaceMembersMissing,
    WorkspaceMemberInvalid,
    MemberManifestMissing,
    Walk,
    Read,
    NonUtf8,
    RustParse,
    SymlinkFile,
    SymlinkDirectory,
    EscapingPathAttribute,
    InvalidPathAttribute,
    UnapprovedInclude,
}

#[derive(Debug)]
struct ScanError {
    path: PathBuf,
    cause: ScanErrorCause,
    detail: String,
}

impl ScanError {
    fn new(path: impl Into<PathBuf>, cause: ScanErrorCause, detail: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            cause,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}: {:?}: {}",
            self.path.display(),
            self.cause,
            self.detail
        )
    }
}

impl std::error::Error for ScanError {}

fn scan_workspace_unsafe(root: &Path) -> Result<Inventory, ScanError> {
    let manifest_path = root.join("Cargo.toml");
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        ScanError::new(
            "Cargo.toml",
            ScanErrorCause::RootManifestRead,
            error.to_string(),
        )
    })?;
    let manifest_text = String::from_utf8(manifest_bytes).map_err(|error| {
        ScanError::new(
            "Cargo.toml",
            ScanErrorCause::RootManifestUtf8,
            error.to_string(),
        )
    })?;
    let manifest: toml::Value = toml::from_str(&manifest_text).map_err(|error| {
        ScanError::new(
            "Cargo.toml",
            ScanErrorCause::RootManifestToml,
            error.to_string(),
        )
    })?;
    let members = manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| {
            ScanError::new(
                "Cargo.toml",
                ScanErrorCause::WorkspaceMembersMissing,
                "workspace.members must be an array",
            )
        })?;

    let mut inventory = Inventory {
        findings: Vec::new(),
        files_inspected: 0,
        nested_src_files: 0,
        build_scripts: 0,
        members: members.len(),
    };
    for member in members {
        let member_name = member.as_str().ok_or_else(|| {
            ScanError::new(
                "Cargo.toml",
                ScanErrorCause::WorkspaceMemberInvalid,
                "workspace member must be a string",
            )
        })?;
        let member_relative = normalize_relative(Path::new(member_name)).ok_or_else(|| {
            ScanError::new(
                "Cargo.toml",
                ScanErrorCause::WorkspaceMemberInvalid,
                format!("workspace member escapes the root: {member_name}"),
            )
        })?;
        let member_root = root.join(&member_relative);
        let member_manifest = member_root.join("Cargo.toml");
        if !member_manifest.is_file() {
            return Err(ScanError::new(
                member_relative.join("Cargo.toml"),
                ScanErrorCause::MemberManifestMissing,
                "member manifest does not exist",
            ));
        }
        walk_member(
            root,
            &member_root,
            &member_relative,
            Path::new(""),
            &mut inventory,
        )?;
    }
    Ok(inventory)
}

fn walk_member(
    workspace_root: &Path,
    member_root: &Path,
    member_relative: &Path,
    relative: &Path,
    inventory: &mut Inventory,
) -> Result<(), ScanError> {
    let directory = member_root.join(relative);
    let entries = fs::read_dir(&directory).map_err(|error| {
        ScanError::new(
            member_relative.join(relative),
            ScanErrorCause::Walk,
            error.to_string(),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            ScanError::new(
                member_relative.join(relative),
                ScanErrorCause::Walk,
                error.to_string(),
            )
        })?;
        let name = entry.file_name();
        let child_relative = relative.join(&name);
        let workspace_relative = member_relative.join(&child_relative);
        let metadata = fs::symlink_metadata(entry.path()).map_err(|error| {
            ScanError::new(&workspace_relative, ScanErrorCause::Walk, error.to_string())
        })?;
        if metadata.file_type().is_symlink() {
            let cause = if entry.path().extension().is_some_and(|value| value == "rs") {
                ScanErrorCause::SymlinkFile
            } else {
                ScanErrorCause::SymlinkDirectory
            };
            return Err(ScanError::new(
                workspace_relative,
                cause,
                "symlinks are not scanned",
            ));
        }
        if metadata.is_dir() {
            if !is_excluded_directory(&child_relative) {
                walk_member(
                    workspace_root,
                    member_root,
                    member_relative,
                    &child_relative,
                    inventory,
                )?;
            }
            continue;
        }
        if metadata.is_file() && entry.path().extension().is_some_and(|value| value == "rs") {
            scan_rust_file(
                workspace_root,
                member_root,
                &workspace_relative,
                &entry.path(),
                inventory,
            )?;
        }
    }
    Ok(())
}

fn is_excluded_directory(relative: &Path) -> bool {
    relative
        .components()
        .any(|component| component.as_os_str() == "target")
}

fn scan_rust_file(
    workspace_root: &Path,
    member_root: &Path,
    workspace_relative: &Path,
    path: &Path,
    inventory: &mut Inventory,
) -> Result<(), ScanError> {
    let bytes = fs::read(path).map_err(|error| {
        ScanError::new(workspace_relative, ScanErrorCause::Read, error.to_string())
    })?;
    let text = String::from_utf8(bytes).map_err(|error| {
        ScanError::new(
            workspace_relative,
            ScanErrorCause::NonUtf8,
            error.to_string(),
        )
    })?;
    let syntax = syn::parse_file(&text).map_err(|error| {
        ScanError::new(
            workspace_relative,
            ScanErrorCause::RustParse,
            error.to_string(),
        )
    })?;
    inventory.files_inspected += 1;
    if path.file_name().is_some_and(|name| name == "build.rs") {
        inventory.build_scripts += 1;
    }
    if nested_under_src(member_root, path) {
        inventory.nested_src_files += 1;
    }
    let mut scanner = AstScanner {
        workspace_root,
        member_root,
        path: workspace_relative,
        source_path: path,
        findings: &mut inventory.findings,
        ancestry: Vec::new(),
        ordinal: 0,
        active_allowance: None,
        function_attributes: false,
        error: None,
    };
    scanner.visit_file(&syntax);
    match scanner.error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

fn nested_under_src(member_root: &Path, path: &Path) -> bool {
    path.strip_prefix(member_root)
        .ok()
        .and_then(|relative| relative.components().next().map(|first| (relative, first)))
        .is_some_and(|(relative, first)| {
            first.as_os_str() == "src" && relative.components().count() > 2
        })
}

fn normalize_relative(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

struct AstScanner<'a> {
    workspace_root: &'a Path,
    member_root: &'a Path,
    path: &'a Path,
    source_path: &'a Path,
    findings: &'a mut Vec<Finding>,
    ancestry: Vec<String>,
    ordinal: usize,
    active_allowance: Option<usize>,
    function_attributes: bool,
    error: Option<ScanError>,
}

impl AstScanner<'_> {
    fn identity(&mut self) -> NodeIdentity {
        let identity = NodeIdentity {
            ordinal: self.ordinal,
            ancestry: self.ancestry.clone(),
        };
        self.ordinal += 1;
        identity
    }

    fn finding(&mut self, kind: FindingKind, placement: Option<AttributePlacement>) -> usize {
        let finding = Finding {
            path: self.path.to_owned(),
            kind,
            enclosing_item: self.ancestry.last().cloned(),
            node: self.identity(),
            unsafe_blocks: Vec::new(),
            attribute_style: placement,
        };
        self.findings.push(finding);
        self.findings.len() - 1
    }

    fn with_item(&mut self, name: String, operation: impl FnOnce(&mut Self)) {
        self.ancestry.push(name);
        operation(self);
        self.ancestry.pop();
    }

    fn visit_function(
        &mut self,
        name: String,
        attrs: &[Attribute],
        signature: &syn::Signature,
        body: Option<&syn::Block>,
    ) {
        self.with_item(name, |scanner| {
            let previous = scanner.active_allowance;
            scanner.function_attributes = true;
            for attribute in attrs {
                let before = scanner.findings.len();
                scanner.inspect_attribute(attribute);
                if scanner.findings.len() > before
                    && scanner.findings.last().is_some_and(|finding| {
                        finding.kind == FindingKind::UnsafeCodeAllowance
                            && finding.attribute_style == Some(AttributePlacement::FunctionOuter)
                    })
                {
                    scanner.active_allowance = Some(scanner.findings.len() - 1);
                }
            }
            scanner.function_attributes = false;
            scanner.visit_signature(signature);
            if let Some(body) = body {
                scanner.visit_block(body);
            }
            scanner.active_allowance = previous;
        });
    }

    fn inspect_attribute(&mut self, attribute: &Attribute) {
        if self.error.is_some() {
            return;
        }
        let placement = match attribute.style {
            syn::AttrStyle::Inner(_) => AttributePlacement::Inner,
            syn::AttrStyle::Outer if self.function_attributes => AttributePlacement::FunctionOuter,
            syn::AttrStyle::Outer => AttributePlacement::OtherOuter,
        };
        if attribute.path().is_ident("path") {
            self.inspect_path_attribute(attribute);
        }
        self.inspect_meta(&attribute.meta, placement, false);
    }

    fn inspect_path_attribute(&mut self, attribute: &Attribute) {
        let Meta::NameValue(name_value) = &attribute.meta else {
            self.set_error(
                ScanErrorCause::InvalidPathAttribute,
                "#[path] must be name-value",
            );
            return;
        };
        let Expr::Lit(expression) = &name_value.value else {
            self.set_error(
                ScanErrorCause::InvalidPathAttribute,
                "#[path] must contain a literal",
            );
            return;
        };
        let syn::Lit::Str(value) = &expression.lit else {
            self.set_error(
                ScanErrorCause::InvalidPathAttribute,
                "#[path] must contain a string",
            );
            return;
        };
        let parent = self.source_path.parent().unwrap_or(self.member_root);
        let Ok(relative_parent) = parent.strip_prefix(self.member_root) else {
            self.set_error(
                ScanErrorCause::EscapingPathAttribute,
                "source is outside member",
            );
            return;
        };
        if normalize_relative(&relative_parent.join(value.value())).is_none() {
            self.set_error(
                ScanErrorCause::EscapingPathAttribute,
                "#[path] escapes member root",
            );
        }
    }

    fn inspect_meta(&mut self, meta: &Meta, placement: AttributePlacement, nested_unsafe: bool) {
        let path = meta.path();
        if nested_unsafe {
            if path.is_ident("no_mangle")
                || path.is_ident("export_name")
                || path.is_ident("link_section")
            {
                self.finding(FindingKind::UnsafeAttribute, Some(placement));
            }
        } else if path.is_ident("no_mangle") {
            self.finding(FindingKind::NoMangleAttribute, Some(placement));
        } else if path.is_ident("export_name") {
            self.finding(FindingKind::ExportNameAttribute, Some(placement));
        } else if path.is_ident("link_section") {
            self.finding(FindingKind::LinkSectionAttribute, Some(placement));
        }

        let Meta::List(list) = meta else {
            return;
        };
        let parser = Punctuated::<Meta, Token![,]>::parse_terminated;
        let Ok(entries) = parser.parse2(list.tokens.clone()) else {
            return;
        };
        if path.is_ident("allow") || path.is_ident("expect") || path.is_ident("warn") {
            let kind = if path.is_ident("allow") {
                FindingKind::UnsafeCodeAllowance
            } else if path.is_ident("expect") {
                FindingKind::UnsafeCodeExpectation
            } else {
                FindingKind::UnsafeCodeWarning
            };
            for entry in &entries {
                if entry.path().is_ident("unsafe_code") {
                    self.finding(kind, Some(placement));
                }
            }
        } else if path.is_ident("cfg_attr") {
            for entry in entries.iter().skip(1) {
                self.inspect_meta(entry, placement, false);
            }
        } else if path.is_ident("unsafe") {
            for entry in &entries {
                self.inspect_meta(entry, placement, true);
            }
        }
    }

    fn set_error(&mut self, cause: ScanErrorCause, detail: &str) {
        if self.error.is_none() {
            self.error = Some(ScanError::new(self.path, cause, detail));
        }
    }

    fn inspect_macro_tokens(&mut self, tokens: TokenStream) {
        // Arbitrary proc-macro expansion is not available to this source scanner; the inherited `deny(unsafe_code)` compiler lint remains the backstop for unsafe code emitted or revealed during expansion.
        let trees = tokens.into_iter().collect::<Vec<_>>();
        for (index, tree) in trees.iter().enumerate() {
            match tree {
                TokenTree::Group(group) => self.inspect_macro_tokens(group.stream()),
                TokenTree::Ident(ident) if ident == "unsafe" => {
                    self.finding(FindingKind::UnsafeMacroToken, None);
                }
                TokenTree::Ident(ident)
                    if (ident == "global_asm" || ident == "naked_asm")
                        && trees.get(index + 1).is_some_and(
                            |next| matches!(next, TokenTree::Punct(punct) if punct.as_char() == '!'),
                        ) =>
                {
                    let kind = if ident == "global_asm" {
                        FindingKind::GlobalAsm
                    } else {
                        FindingKind::NakedAsm
                    };
                    self.finding(kind, None);
                }
                _ => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for AstScanner<'_> {
    fn visit_item_fn(&mut self, node: &'ast ItemFn) {
        self.visit_function(
            node.sig.ident.to_string(),
            &node.attrs,
            &node.sig,
            Some(&node.block),
        );
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.visit_function(
            node.sig.ident.to_string(),
            &node.attrs,
            &node.sig,
            Some(&node.block),
        );
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        self.visit_function(
            node.sig.ident.to_string(),
            &node.attrs,
            &node.sig,
            node.default.as_ref(),
        );
    }

    fn visit_foreign_item_fn(&mut self, node: &'ast ForeignItemFn) {
        self.visit_function(node.sig.ident.to_string(), &node.attrs, &node.sig, None);
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        self.inspect_attribute(node);
        visit::visit_attribute(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        let detail = UnsafeBlockDetail {
            node: self.identity(),
            statement_count: node.block.stmts.len(),
            calls: node.block.stmts.iter().filter_map(statement_call).collect(),
        };
        if let Some(index) = self.active_allowance {
            self.findings[index].unsafe_blocks.push(detail);
        } else {
            let index = self.finding(FindingKind::UnsafeBlock, None);
            self.findings[index].unsafe_blocks.push(detail);
        }
        visit::visit_expr_unsafe(self, node);
    }

    fn visit_signature(&mut self, node: &'ast syn::Signature) {
        if node.unsafety.is_some() {
            self.finding(FindingKind::UnsafeFunction, None);
        }
        visit::visit_signature(self, node);
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if node.unsafety.is_some() {
            self.finding(FindingKind::UnsafeTrait, None);
        }
        self.with_item(node.ident.to_string(), |scanner| {
            visit::visit_item_trait(scanner, node)
        });
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if node.unsafety.is_some() {
            self.finding(FindingKind::UnsafeImpl, None);
        }
        visit::visit_item_impl(self, node);
    }

    fn visit_item_foreign_mod(&mut self, node: &'ast syn::ItemForeignMod) {
        if node.unsafety.is_some() {
            self.finding(FindingKind::UnsafeExternBlock, None);
        }
        visit::visit_item_foreign_mod(self, node);
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if node.unsafety.is_some() {
            self.finding(FindingKind::UnsafeModule, None);
        }
        self.with_item(node.ident.to_string(), |scanner| {
            visit::visit_item_mod(scanner, node)
        });
    }

    fn visit_item_static(&mut self, node: &'ast syn::ItemStatic) {
        if matches!(node.mutability, StaticMutability::Mut(_)) {
            self.finding(FindingKind::StaticMut, None);
        }
        visit::visit_item_static(self, node);
    }

    fn visit_foreign_item_static(&mut self, node: &'ast syn::ForeignItemStatic) {
        if matches!(node.mutability, StaticMutability::Mut(_)) {
            self.finding(FindingKind::ForeignStaticMut, None);
        }
        visit::visit_foreign_item_static(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let path = path_text(&node.path);
        if path == "global_asm" {
            self.finding(FindingKind::GlobalAsm, None);
        } else if path == "naked_asm" {
            self.finding(FindingKind::NakedAsm, None);
        } else if path == "include" && !self.approved_include(node) {
            self.set_error(
                ScanErrorCause::UnapprovedInclude,
                "include! is not approved",
            );
        }
        self.inspect_macro_tokens(node.tokens.clone());
        visit::visit_macro(self, node);
    }
}

impl AstScanner<'_> {
    fn approved_include(&self, node: &syn::Macro) -> bool {
        self.path == Path::new("crates/solstone-linux/src/tray.rs")
            && self.ancestry.last().is_some_and(|name| name == "generated")
            && node.tokens.to_string() == "concat ! (env ! (\"OUT_DIR\") , \"/tray_icons.rs\")"
            && self.workspace_root.join(self.path).is_file()
    }
}

fn statement_call(statement: &Stmt) -> Option<CallIdentity> {
    let expression = match statement {
        Stmt::Expr(expression, _) => expression,
        _ => return None,
    };
    let Expr::Call(call) = expression else {
        return None;
    };
    call_identity(call)
}

fn call_identity(call: &ExprCall) -> Option<CallIdentity> {
    let Expr::Path(ExprPath { path, .. }) = call.func.as_ref() else {
        return None;
    };
    Some(CallIdentity {
        callee: path_text(path),
        arguments: call.args.len(),
    })
}

fn path_text(path: &syn::Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

struct Fixture {
    root: tempfile::TempDir,
    primary_lib: PathBuf,
    nested_source: PathBuf,
}

fn fixture_workspace() -> Fixture {
    let root = tempfile::tempdir().expect("fixture tempdir");
    fs::write(
        root.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/solstone-linux\", \"crates/helper\"]\n",
    )
    .expect("fixture workspace manifest");
    for member in ["crates/solstone-linux", "crates/helper"] {
        let member_root = root.path().join(member);
        fs::create_dir_all(member_root.join("src/nested")).expect("fixture source tree");
        fs::create_dir_all(member_root.join("tests")).expect("fixture tests tree");
        fs::create_dir_all(member_root.join("examples")).expect("fixture examples tree");
        fs::create_dir_all(member_root.join("benches")).expect("fixture benches tree");
        fs::write(
            member_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
                member.rsplit('/').next().expect("member name")
            ),
        )
        .expect("fixture member manifest");
        fs::write(member_root.join("build.rs"), "fn main() {}\n").expect("fixture build script");
        fs::write(member_root.join("src/lib.rs"), "pub fn safe() {}\n").expect("fixture lib");
        fs::write(
            member_root.join("src/nested/mod.rs"),
            "pub fn nested() {}\n",
        )
        .expect("fixture nested source");
        for target in ["tests/check.rs", "examples/demo.rs", "benches/bench.rs"] {
            fs::write(member_root.join(target), "fn safe_target() {}\n")
                .expect("fixture target source");
        }
    }
    Fixture {
        primary_lib: root.path().join("crates/solstone-linux/src/lib.rs"),
        nested_source: root.path().join("crates/helper/src/nested/mod.rs"),
        root,
    }
}

fn scan_fixture(fixture: &Fixture) -> Result<Inventory, ScanError> {
    scan_workspace_unsafe(fixture.root.path())
}

fn kinds(inventory: &Inventory) -> Vec<FindingKind> {
    inventory
        .findings
        .iter()
        .map(|finding| finding.kind)
        .collect()
}

fn reviewed_seams_error(inventory: &Inventory) -> Result<(), String> {
    if inventory.findings.len() != 2 {
        return Err(format!("expected two findings: {:#?}", inventory.findings));
    }
    for (name, callee, arguments) in [
        ("set_session_environment_variable", "env::set_var", 2),
        (
            "session_environment_wrapper_assigns_and_restores",
            "env::remove_var",
            1,
        ),
    ] {
        let finding = inventory
            .findings
            .iter()
            .find(|finding| finding.enclosing_item.as_deref() == Some(name))
            .ok_or_else(|| format!("missing reviewed seam {name}: {:#?}", inventory.findings))?;
        if finding.path != Path::new("crates/solstone-linux/src/cli.rs")
            || finding.kind != FindingKind::UnsafeCodeAllowance
            || finding.attribute_style != Some(AttributePlacement::FunctionOuter)
            || finding.unsafe_blocks.len() != 1
            || finding.unsafe_blocks[0].statement_count != 1
            || finding.unsafe_blocks[0].calls
                != vec![CallIdentity {
                    callee: callee.into(),
                    arguments,
                }]
        {
            return Err(format!("reviewed seam changed: {finding:#?}"));
        }
    }
    Ok(())
}

// AC: the repository has exactly the two reviewed function-local unsafe seams.
#[test]
fn repository_unsafe_inventory_matches_reviewed_seams() {
    let inventory = scan_workspace_unsafe(&workspace_root()).expect("repository must scan");
    assert!(inventory.files_inspected >= 50, "{inventory:#?}");
    assert!(inventory.nested_src_files >= 10, "{inventory:#?}");
    assert!(inventory.build_scripts >= 1, "{inventory:#?}");
    assert_eq!(inventory.members, 1);
    if let Err(error) = reviewed_seams_error(&inventory) {
        panic!("{error}");
    }
}

// AC: recursive enumeration covers both members and every conventional Rust target location.
#[test]
fn fixture_coverage_shape_is_computed_by_the_scanner() {
    let fixture = fixture_workspace();
    let inventory = scan_fixture(&fixture).expect("safe fixture scans");
    assert_eq!(inventory.members, 2);
    assert_eq!(inventory.files_inspected, 12);
    assert_eq!(inventory.nested_src_files, 2);
    assert_eq!(inventory.build_scripts, 2);
    assert!(inventory.findings.is_empty());
}

// AC: build scripts, tests, examples, and benches are scanned through the same member walk.
#[test]
fn detects_unsafe_source_in_every_member_target_location() {
    for relative in [
        "build.rs",
        "tests/check.rs",
        "examples/demo.rs",
        "benches/bench.rs",
    ] {
        let fixture = fixture_workspace();
        let path = fixture.root.path().join("crates/helper").join(relative);
        fs::write(&path, "fn probe() { unsafe { call(); } }\n").expect("mutate target source");
        let inventory = scan_fixture(&fixture).expect("fixture scans");
        assert_eq!(kinds(&inventory), vec![FindingKind::UnsafeBlock]);
        assert_eq!(
            inventory.findings[0].path,
            Path::new("crates/helper").join(relative)
        );
    }
}

fn write_reviewed_fixture_seams(fixture: &Fixture, source: &str) {
    let cli = fixture.root.path().join("crates/solstone-linux/src/cli.rs");
    fs::write(cli, source).expect("write reviewed fixture seams");
}

const REVIEWED_FIXTURE_SEAMS: &str = r#"
use std::env;
#[allow(unsafe_code)]
fn set_session_environment_variable(name: &str, value: &str) {
    unsafe { env::set_var(name, value) };
}
#[allow(unsafe_code)]
fn session_environment_wrapper_assigns_and_restores() {
    const NAME: &str = "NAME";
    unsafe { env::remove_var(NAME) }
}
"#;

// AC: the authorization assertion accepts only the exact reviewed seam structure.
#[test]
fn reviewed_seam_fixture_matches_authorization() {
    let fixture = fixture_workspace();
    write_reviewed_fixture_seams(&fixture, REVIEWED_FIXTURE_SEAMS);
    let inventory = scan_fixture(&fixture).expect("reviewed fixture scans");
    assert_eq!(reviewed_seams_error(&inventory), Ok(()));
}

// AC: statement, duplication, scope, location, callee, arity, and extra-node mutations fail authorization.
#[test]
fn reviewed_seam_mutations_fail_authorization() {
    let mutations = [
        REVIEWED_FIXTURE_SEAMS.replace(
            "unsafe { env::set_var(name, value) };",
            "unsafe { let _extra = 1; env::set_var(name, value) };",
        ),
        REVIEWED_FIXTURE_SEAMS.replace(
            "unsafe { env::set_var(name, value) };",
            "unsafe { env::set_var(name, value) }; unsafe { env::set_var(name, value) };",
        ),
        REVIEWED_FIXTURE_SEAMS.replace(
            "#[allow(unsafe_code)]\nfn set_session_environment_variable",
            "#[allow(unsafe_code)]\nmod widened {}\nfn set_session_environment_variable",
        ),
        REVIEWED_FIXTURE_SEAMS.replace(
            "fn set_session_environment_variable",
            "fn relocated_session_environment_variable",
        ),
        REVIEWED_FIXTURE_SEAMS.replace(
            "env::set_var(name, value)",
            "std::env::set_var(name, value)",
        ),
        REVIEWED_FIXTURE_SEAMS.replace("env::remove_var(NAME)", "env::remove_var(NAME, NAME)"),
        format!("{REVIEWED_FIXTURE_SEAMS}\nunsafe fn third() {{}}\n"),
    ];
    for source in mutations {
        let fixture = fixture_workspace();
        write_reviewed_fixture_seams(&fixture, &source);
        let inventory = scan_fixture(&fixture).expect("mutation remains parseable");
        assert!(
            reviewed_seams_error(&inventory).is_err(),
            "mutation unexpectedly authorized: {source}"
        );
    }
}

// AC: unsafe blocks are syntax-aware across whitespace and nested member source.
#[test]
fn detects_whitespace_unsafe_blocks_in_nested_member_source() {
    for source in [
        "fn probe() { unsafe   { call(); } }\n",
        "fn probe() { unsafe\n{ call(); } }\n",
    ] {
        let fixture = fixture_workspace();
        fs::write(&fixture.nested_source, source).expect("mutate nested source");
        let inventory = scan_fixture(&fixture).expect("fixture scans");
        assert_eq!(kinds(&inventory), vec![FindingKind::UnsafeBlock]);
        assert_eq!(
            inventory.findings[0].path,
            Path::new("crates/helper/src/nested/mod.rs")
        );
    }
}

// AC: every unsafe declaration form has a distinct finding kind.
#[test]
fn detects_unsafe_functions_traits_impls_extern_blocks_and_modules() {
    let cases = [
        ("unsafe fn probe() {}", FindingKind::UnsafeFunction),
        (
            "trait T { unsafe fn probe(); }",
            FindingKind::UnsafeFunction,
        ),
        (
            "struct S; impl S { unsafe fn probe() {} }",
            FindingKind::UnsafeFunction,
        ),
        ("unsafe trait T {}", FindingKind::UnsafeTrait),
        (
            "trait T {} struct S; unsafe impl T for S {}",
            FindingKind::UnsafeImpl,
        ),
        (
            "unsafe extern \"C\" { fn probe(); }",
            FindingKind::UnsafeExternBlock,
        ),
        (
            "extern \"C\" { unsafe fn probe(); }",
            FindingKind::UnsafeFunction,
        ),
        ("unsafe mod nested {}", FindingKind::UnsafeModule),
    ];
    for (source, expected) in cases {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate declaration");
        let inventory = scan_fixture(&fixture).expect("fixture scans");
        assert!(
            kinds(&inventory).contains(&expected),
            "missing {expected:?} for {source}: {inventory:#?}"
        );
    }
}

// AC: mutable ordinary and foreign statics are both rejected.
#[test]
fn detects_static_mut_and_foreign_static_mut() {
    let cases = [
        ("static mut VALUE: u8 = 0;", FindingKind::StaticMut),
        (
            "unsafe extern \"C\" { static mut VALUE: u8; }",
            FindingKind::ForeignStaticMut,
        ),
    ];
    for (source, expected) in cases {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate static");
        let inventory = scan_fixture(&fixture).expect("fixture scans");
        assert!(kinds(&inventory).contains(&expected), "{inventory:#?}");
    }
}

// AC: unsafe and legacy symbol attributes cannot bypass the inventory.
#[test]
fn detects_unsafe_and_legacy_symbol_attributes() {
    let cases = [
        (
            "#[unsafe(no_mangle)] fn probe() {}",
            FindingKind::UnsafeAttribute,
        ),
        ("#[no_mangle] fn probe() {}", FindingKind::NoMangleAttribute),
        (
            "#[export_name = \"probe\"] fn f() {}",
            FindingKind::ExportNameAttribute,
        ),
        (
            "#[link_section = \"probe\"] static VALUE: u8 = 0;",
            FindingKind::LinkSectionAttribute,
        ),
    ];
    for (source, expected) in cases {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate attribute");
        let inventory = scan_fixture(&fixture).expect("fixture scans");
        assert_eq!(
            kinds(&inventory),
            vec![expected],
            "{source}: {inventory:#?}"
        );
    }
}

// AC: unsafe-code lint overrides are found in inner, outer, multi-lint, and nested cfg_attr forms.
#[test]
fn detects_all_unsafe_code_lint_override_forms() {
    let cases = [
        (
            "#[allow(unsafe_code)] fn probe() {}",
            FindingKind::UnsafeCodeAllowance,
        ),
        (
            "#![allow(unsafe_code)]\nfn probe() {}",
            FindingKind::UnsafeCodeAllowance,
        ),
        (
            "#[allow(dead_code, unsafe_code, unused)] fn probe() {}",
            FindingKind::UnsafeCodeAllowance,
        ),
        (
            "#[warn(unsafe_code)] fn probe() {}",
            FindingKind::UnsafeCodeWarning,
        ),
        (
            "#![warn(unsafe_code)]\nfn probe() {}",
            FindingKind::UnsafeCodeWarning,
        ),
        (
            "#[expect(unsafe_code)] fn probe() {}",
            FindingKind::UnsafeCodeExpectation,
        ),
        (
            "#![expect(unsafe_code)]\nfn probe() {}",
            FindingKind::UnsafeCodeExpectation,
        ),
        (
            "#[warn(dead_code, unsafe_code)] fn probe() {}",
            FindingKind::UnsafeCodeWarning,
        ),
        (
            "#[expect(dead_code, unsafe_code)] fn probe() {}",
            FindingKind::UnsafeCodeExpectation,
        ),
        (
            "#[cfg_attr(test, allow(unsafe_code))] fn probe() {}",
            FindingKind::UnsafeCodeAllowance,
        ),
        (
            "#[cfg_attr(test, warn(unsafe_code))] fn probe() {}",
            FindingKind::UnsafeCodeWarning,
        ),
        (
            "#[cfg_attr(test, expect(unsafe_code))] fn probe() {}",
            FindingKind::UnsafeCodeExpectation,
        ),
        (
            "#[cfg_attr(test, cfg_attr(unix, allow(unsafe_code)))] fn probe() {}",
            FindingKind::UnsafeCodeAllowance,
        ),
    ];
    for (source, expected) in cases {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate lint attribute");
        let inventory = scan_fixture(&fixture).expect("fixture scans");
        assert_eq!(
            kinds(&inventory),
            vec![expected],
            "{source}: {inventory:#?}"
        );
    }
}

// AC: assembly macros and unsafe syntax nested in opaque macro tokens are inventoried.
#[test]
fn detects_assembly_macros_and_nested_macro_tokens() {
    let cases = [
        ("global_asm!(\"\");", FindingKind::GlobalAsm),
        ("naked_asm!(\"\");", FindingKind::NakedAsm),
        (
            "wrapper!({ unsafe { probe(); } });",
            FindingKind::UnsafeMacroToken,
        ),
        (
            "wrapper!((((unsafe { probe(); }))));",
            FindingKind::UnsafeMacroToken,
        ),
        ("wrapper!({ global_asm!(\"\"); });", FindingKind::GlobalAsm),
        ("wrapper!({ naked_asm!(\"\"); });", FindingKind::NakedAsm),
    ];
    for (source, expected) in cases {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate macro");
        let inventory = scan_fixture(&fixture).expect("fixture scans");
        assert!(
            kinds(&inventory).contains(&expected),
            "{source}: {inventory:#?}"
        );
    }
}

// AC: a path attribute may not reference source outside its workspace member.
#[test]
fn rejects_escaping_and_nonliteral_path_attributes() {
    for source in [
        "#[path = \"../../../escape.rs\"] mod escape;",
        "#[path(test)] mod escape;",
    ] {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate path attribute");
        let error = scan_fixture(&fixture).expect_err("path attribute must fail");
        assert!(
            matches!(
                error.cause,
                ScanErrorCause::EscapingPathAttribute | ScanErrorCause::InvalidPathAttribute
            ),
            "{error}"
        );
        assert_eq!(error.path, Path::new("crates/solstone-linux/src/lib.rs"));
    }
}

// AC: symlinked Rust files and directories fail closed instead of escaping member traversal.
#[test]
fn rejects_symlinked_rust_files_and_directories() {
    use std::os::unix::fs::symlink;

    for directory in [false, true] {
        let fixture = fixture_workspace();
        let member = fixture.root.path().join("crates/solstone-linux");
        let outside = fixture.root.path().join("outside");
        if directory {
            fs::create_dir(&outside).expect("outside directory");
            symlink(&outside, member.join("linked")).expect("directory symlink");
        } else {
            fs::write(&outside, "fn outside() {}\n").expect("outside source");
            symlink(&outside, member.join("linked.rs")).expect("file symlink");
        }
        let error = scan_fixture(&fixture).expect_err("symlink must fail");
        assert_eq!(
            error.cause,
            if directory {
                ScanErrorCause::SymlinkDirectory
            } else {
                ScanErrorCause::SymlinkFile
            }
        );
    }
}

// AC: source decoding and parsing failures name the offending file.
#[test]
fn reports_non_utf8_and_unparseable_source() {
    for (bytes, expected) in [
        (vec![0xff], ScanErrorCause::NonUtf8),
        (b"fn broken(".to_vec(), ScanErrorCause::RustParse),
    ] {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, bytes).expect("mutate invalid source");
        let error = scan_fixture(&fixture).expect_err("invalid source must fail");
        assert_eq!(error.cause, expected);
        assert_eq!(error.path, Path::new("crates/solstone-linux/src/lib.rs"));
    }
}

// AC: unreadable source is a named scanner failure rather than an ignored file.
#[test]
fn reports_source_read_failure() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = fixture_workspace();
    let original = fs::metadata(&fixture.primary_lib)
        .expect("source metadata")
        .permissions();
    fs::set_permissions(&fixture.primary_lib, fs::Permissions::from_mode(0o000))
        .expect("make source unreadable");
    let result = scan_fixture(&fixture);
    fs::set_permissions(&fixture.primary_lib, original).expect("restore source permissions");
    let error = result.expect_err("unreadable source must fail");
    assert_eq!(error.cause, ScanErrorCause::Read);
    assert_eq!(error.path, Path::new("crates/solstone-linux/src/lib.rs"));
}

// AC: workspace member resolution failures are explicit and path-bearing.
#[test]
fn reports_workspace_member_resolution_failures() {
    let cases = [
        ("[workspace]\n", ScanErrorCause::WorkspaceMembersMissing),
        (
            "[workspace]\nmembers = [1]\n",
            ScanErrorCause::WorkspaceMemberInvalid,
        ),
        (
            "[workspace]\nmembers = [\"missing\"]\n",
            ScanErrorCause::MemberManifestMissing,
        ),
    ];
    for (manifest, expected) in cases {
        let fixture = fixture_workspace();
        fs::write(fixture.root.path().join("Cargo.toml"), manifest).expect("mutate manifest");
        let error = scan_fixture(&fixture).expect_err("member resolution must fail");
        assert_eq!(error.cause, expected, "{error}");
        assert!(!error.path.as_os_str().is_empty());
    }
}

// AC: include source is rejected except for the exact generated tray icon include.
#[test]
fn include_boundary_has_one_exact_exemption() {
    for source in [
        "include!(\"local.rs\");",
        "include!(concat!(env!(\"OUT_DIR\"), \"/other.rs\"));",
    ] {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate include");
        let error = scan_fixture(&fixture).expect_err("unapproved include must fail");
        assert_eq!(error.cause, ScanErrorCause::UnapprovedInclude);
    }

    let fixture = fixture_workspace();
    let tray = fixture
        .root
        .path()
        .join("crates/solstone-linux/src/tray.rs");
    fs::write(
        &tray,
        "mod generated { include!(concat!(env!(\"OUT_DIR\"), \"/tray_icons.rs\")); }\n",
    )
    .expect("write exact tray include");
    scan_fixture(&fixture).expect("exact tray include is approved");

    fs::write(
        &tray,
        "mod generated { include!(concat!(env!(\"OUT_DIR\"), \"/other.rs\")); }\n",
    )
    .expect("write near-match tray include");
    let error = scan_fixture(&fixture).expect_err("near-match tray include must fail");
    assert_eq!(error.cause, ScanErrorCause::UnapprovedInclude);
    assert_eq!(error.path, Path::new("crates/solstone-linux/src/tray.rs"));
}

// AC: comments that resemble unsafe syntax are not Rust findings.
#[test]
fn ignores_comment_containing_unsafe_block_text() {
    let fixture = fixture_workspace();
    fs::write(
        &fixture.primary_lib,
        "// unsafe { probe(); }\nfn safe() {}\n",
    )
    .expect("mutate comment");
    assert!(
        scan_fixture(&fixture)
            .expect("fixture scans")
            .findings
            .is_empty()
    );
}

// AC: string literals containing unsafe source remain literals, not executable syntax.
#[test]
fn ignores_string_literal_containing_unsafe_source() {
    let fixture = fixture_workspace();
    fs::write(
        &fixture.primary_lib,
        "const SOURCE: &str = \"unsafe { probe(); }\";\n",
    )
    .expect("mutate string");
    assert!(
        scan_fixture(&fixture)
            .expect("fixture scans")
            .findings
            .is_empty()
    );
}

// AC: a safe ABI-qualified function is not confused with an unsafe declaration.
#[test]
fn ignores_safe_extern_c_function() {
    let fixture = fixture_workspace();
    fs::write(&fixture.primary_lib, "pub extern \"C\" fn probe() {}\n")
        .expect("mutate safe extern function");
    assert!(
        scan_fixture(&fixture)
            .expect("fixture scans")
            .findings
            .is_empty()
    );
}
