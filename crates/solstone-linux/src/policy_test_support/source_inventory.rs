// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::{
    collections::BTreeSet,
    ffi::OsString,
    fmt, fs,
    path::{Component, Path, PathBuf},
    process::Command,
};
use syn::{
    Attribute, Expr, Item, ItemMod, Lit, Meta,
    visit::{self, Visit},
};

#[derive(Clone, Debug)]
pub(crate) struct CargoCommand {
    pub(crate) program: PathBuf,
    pub(crate) prefix: Vec<OsString>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SourceIdentity {
    pub(crate) package: String,
    pub(crate) target: String,
    pub(crate) target_kind: String,
    pub(crate) module: Vec<String>,
    pub(crate) item: Option<String>,
    pub(crate) cfg_context: Vec<String>,
    pub(crate) test_only: bool,
}

impl fmt::Display for SourceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let module = if self.module.is_empty() {
            "crate".to_owned()
        } else {
            self.module.join("::")
        };
        let item = self.item.as_deref().unwrap_or("<module>");
        let cfg = if self.cfg_context.is_empty() {
            "production".to_owned()
        } else {
            self.cfg_context.join("&")
        };
        write!(
            formatter,
            "{}::{}[{}]::{}::{}{{{}}}",
            self.package, self.target, self.target_kind, module, item, cfg
        )
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SourceNode {
    pub(crate) identity: SourceIdentity,
    pub(crate) path: PathBuf,
    pub(crate) syntax: syn::File,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SourceInventory {
    pub(crate) nodes: Vec<SourceNode>,
    pub(crate) data_inputs: Vec<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ScanErrorCause {
    MetadataExecution,
    MetadataExit,
    MetadataJson,
    MetadataWorkspaceRootMismatch,
    Read,
    NonUtf8,
    RustParse,
    Walk,
    SymlinkFile,
    SymlinkDirectory,
    InvalidPathAttribute,
    UnapprovedInclude,
    UnclassifiableInput,
}

#[derive(Clone, Debug)]
pub(crate) struct ScanError {
    pub(crate) identity: String,
    pub(crate) path: PathBuf,
    pub(crate) cause: ScanErrorCause,
    pub(crate) detail: String,
}

impl ScanError {
    pub(crate) fn new(
        identity: impl Into<String>,
        path: impl Into<PathBuf>,
        cause: ScanErrorCause,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            identity: identity.into(),
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
            "source policy: identity={} path={} rule={:?} detail={}",
            self.identity,
            self.path.display(),
            self.cause,
            self.detail
        )
    }
}

impl std::error::Error for ScanError {}

pub(crate) fn normalize_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut result = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => result.push(value),
            Component::ParentDir => {
                if !result.pop() {
                    return None;
                }
            }
            Component::Prefix(_) => return None,
        }
    }
    Some(result)
}

pub(crate) fn metadata_roots_with_command(
    root: &Path,
    cargo: &CargoCommand,
) -> Result<serde_json::Value, ScanError> {
    let output = Command::new(&cargo.program)
        .args(&cargo.prefix)
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--no-deps",
            "--manifest-path",
        ])
        .arg(root.join("Cargo.toml"))
        .current_dir(root)
        .output()
        .map_err(|error| {
            ScanError::new(
                "workspace::metadata",
                &cargo.program,
                ScanErrorCause::MetadataExecution,
                error.to_string(),
            )
        })?;
    if !output.status.success() {
        return Err(ScanError::new(
            "workspace::metadata",
            root,
            ScanErrorCause::MetadataExit,
            String::from_utf8_lossy(&output.stderr),
        ));
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        ScanError::new(
            "workspace::metadata",
            root,
            ScanErrorCause::MetadataJson,
            error.to_string(),
        )
    })?;
    let reported = metadata["workspace_root"]
        .as_str()
        .and_then(|value| normalize_absolute(Path::new(value)))
        .ok_or_else(|| {
            ScanError::new(
                "workspace::metadata",
                root,
                ScanErrorCause::MetadataWorkspaceRootMismatch,
                "missing absolute workspace_root",
            )
        })?;
    if normalize_absolute(root).as_deref() != Some(reported.as_path()) {
        return Err(ScanError::new(
            "workspace::metadata",
            reported,
            ScanErrorCause::MetadataWorkspaceRootMismatch,
            format!("expected {}", root.display()),
        ));
    }
    Ok(metadata)
}

pub(crate) fn scan_workspace_with_command(
    root: &Path,
    cargo: &CargoCommand,
) -> Result<SourceInventory, ScanError> {
    let metadata = metadata_roots_with_command(root, cargo)?;
    let mut inventory = SourceInventory::default();
    let mut visited = BTreeSet::new();
    for package in metadata["packages"].as_array().ok_or_else(|| {
        ScanError::new(
            "workspace::metadata",
            root,
            ScanErrorCause::MetadataJson,
            "packages is not an array",
        )
    })? {
        let package_name = package["name"].as_str().ok_or_else(|| {
            ScanError::new(
                "workspace::metadata",
                root,
                ScanErrorCause::MetadataJson,
                "package name is missing",
            )
        })?;
        for target in package["targets"].as_array().ok_or_else(|| {
            ScanError::new(
                package_name,
                root,
                ScanErrorCause::MetadataJson,
                "targets is not an array",
            )
        })? {
            let target_name = target["name"].as_str().unwrap_or("<unnamed>");
            let kinds = target["kind"]
                .as_array()
                .ok_or_else(|| {
                    ScanError::new(
                        package_name,
                        root,
                        ScanErrorCause::MetadataJson,
                        "target kind is missing",
                    )
                })?
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>();
            let target_kind = kinds.join("+");
            let test_only = kinds
                .iter()
                .all(|kind| matches!(*kind, "test" | "bench" | "example"));
            let source = PathBuf::from(target["src_path"].as_str().ok_or_else(|| {
                ScanError::new(
                    package_name,
                    root,
                    ScanErrorCause::MetadataJson,
                    "target src_path is missing",
                )
            })?);
            let identity = SourceIdentity {
                package: package_name.to_owned(),
                target: target_name.to_owned(),
                target_kind,
                module: Vec::new(),
                item: None,
                cfg_context: Vec::new(),
                test_only,
            };
            scan_rust_file(root, &source, identity, &mut inventory, &mut visited)?;
        }
    }
    inventory
        .nodes
        .sort_by_key(|node| node.identity.to_string());
    Ok(inventory)
}

pub(crate) fn walk_member(
    root: &Path,
    member: &Path,
    identity: SourceIdentity,
    inventory: &mut SourceInventory,
) -> Result<(), ScanError> {
    let metadata = fs::symlink_metadata(member).map_err(|error| {
        ScanError::new(
            identity.to_string(),
            member,
            ScanErrorCause::Walk,
            error.to_string(),
        )
    })?;
    if metadata.file_type().is_symlink() {
        let directory = fs::metadata(member).is_ok_and(|target| target.is_dir());
        return Err(ScanError::new(
            identity.to_string(),
            member,
            if directory {
                ScanErrorCause::SymlinkDirectory
            } else {
                ScanErrorCause::SymlinkFile
            },
            "source inputs may not be symlinks",
        ));
    }
    let mut visited = BTreeSet::new();
    scan_rust_file(root, member, identity, inventory, &mut visited)
}

pub(crate) fn scan_rust_file(
    root: &Path,
    path: &Path,
    identity: SourceIdentity,
    inventory: &mut SourceInventory,
    visited: &mut BTreeSet<(PathBuf, String)>,
) -> Result<(), ScanError> {
    let path = normalize_absolute(path).ok_or_else(|| {
        ScanError::new(
            identity.to_string(),
            path,
            ScanErrorCause::Walk,
            "path is not lexically valid",
        )
    })?;
    if !visited.insert((path.clone(), identity.to_string())) {
        return Ok(());
    }
    let metadata = fs::symlink_metadata(&path).map_err(|error| {
        ScanError::new(
            identity.to_string(),
            &path,
            ScanErrorCause::Read,
            error.to_string(),
        )
    })?;
    if metadata.file_type().is_symlink() {
        return Err(ScanError::new(
            identity.to_string(),
            &path,
            ScanErrorCause::SymlinkFile,
            "Rust source is a symlink",
        ));
    }
    let bytes = fs::read(&path).map_err(|error| {
        ScanError::new(
            identity.to_string(),
            &path,
            ScanErrorCause::Read,
            error.to_string(),
        )
    })?;
    let source = String::from_utf8(bytes).map_err(|error| {
        ScanError::new(
            identity.to_string(),
            &path,
            ScanErrorCause::NonUtf8,
            error.to_string(),
        )
    })?;
    let syntax = syn::parse_file(&source).map_err(|error| {
        ScanError::new(
            identity.to_string(),
            &path,
            ScanErrorCause::RustParse,
            error.to_string(),
        )
    })?;
    inventory.nodes.push(SourceNode {
        identity: identity.clone(),
        path: path.clone(),
        syntax: syntax.clone(),
    });
    scan_items(root, &path, &syntax.items, identity, inventory, visited)
}

fn scan_items(
    root: &Path,
    source_path: &Path,
    items: &[Item],
    identity: SourceIdentity,
    inventory: &mut SourceInventory,
    visited: &mut BTreeSet<(PathBuf, String)>,
) -> Result<(), ScanError> {
    for item in items {
        if let Item::Mod(module) = item {
            let mut child = identity.clone();
            child.module.push(module.ident.to_string());
            inherit_context(&module.attrs, &mut child);
            if let Some((_, items)) = &module.content {
                scan_items(root, source_path, items, child, inventory, visited)?;
            } else {
                let path = module_path(source_path, module, &child)?;
                scan_rust_file(root, &path, child, inventory, visited)?;
            }
        }
    }
    let mut includes = IncludeVisitor::default();
    includes.visit_file(&syn::File {
        shebang: None,
        attrs: Vec::new(),
        items: items.to_vec(),
    });
    for include in includes.inputs {
        match include {
            IncludeInput::Data(relative) => {
                let path = source_path.parent().unwrap_or(root).join(relative);
                inventory.data_inputs.push(path);
            }
            IncludeInput::Rust(relative) => {
                let path = source_path.parent().unwrap_or(root).join(relative);
                scan_rust_file(root, &path, identity.clone(), inventory, visited)?;
            }
            IncludeInput::GeneratedTray => {
                if !approved_include(source_path, &identity) {
                    return Err(ScanError::new(
                        identity.to_string(),
                        source_path,
                        ScanErrorCause::UnapprovedInclude,
                        "generated include is not the tray icon input",
                    ));
                }
            }
            IncludeInput::Unclassifiable(detail) => {
                return Err(ScanError::new(
                    identity.to_string(),
                    source_path,
                    ScanErrorCause::UnclassifiableInput,
                    detail,
                ));
            }
        }
    }
    Ok(())
}

fn inherit_context(attributes: &[Attribute], identity: &mut SourceIdentity) {
    for attribute in attributes {
        let rendered = quote_attribute(attribute);
        if rendered.contains("cfg") {
            identity.cfg_context.push(rendered.clone());
        }
        if rendered.contains("test") {
            identity.test_only = true;
        }
    }
}

fn quote_attribute(attribute: &Attribute) -> String {
    match &attribute.meta {
        Meta::Path(path) => path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::"),
        Meta::List(list) => format!(
            "{}({})",
            list.path.segments.last().unwrap().ident,
            list.tokens
        ),
        Meta::NameValue(value) => value.path.segments.last().unwrap().ident.to_string(),
    }
}

pub(crate) fn module_identity(module: &ItemMod) -> String {
    let form = if module.content.is_some() {
        "inline"
    } else {
        "external"
    };
    format!("{}[{form}]", module.ident)
}

fn module_path(
    source_path: &Path,
    module: &ItemMod,
    identity: &SourceIdentity,
) -> Result<PathBuf, ScanError> {
    for attribute in &module.attrs {
        if attribute.path().is_ident("path") {
            let Meta::NameValue(value) = &attribute.meta else {
                return Err(ScanError::new(
                    identity.to_string(),
                    source_path,
                    ScanErrorCause::InvalidPathAttribute,
                    "#[path] must be a literal name-value attribute",
                ));
            };
            let Expr::Lit(literal) = &value.value else {
                return Err(ScanError::new(
                    identity.to_string(),
                    source_path,
                    ScanErrorCause::InvalidPathAttribute,
                    "#[path] value must be literal",
                ));
            };
            let Lit::Str(path) = &literal.lit else {
                return Err(ScanError::new(
                    identity.to_string(),
                    source_path,
                    ScanErrorCause::InvalidPathAttribute,
                    "#[path] value must be a string",
                ));
            };
            return Ok(source_path.parent().unwrap().join(path.value()));
        }
    }
    let parent = source_path.parent().unwrap();
    let stem = source_path.file_stem().and_then(|value| value.to_str());
    let module_root = if matches!(stem, Some("lib" | "main" | "mod")) {
        parent.to_owned()
    } else {
        parent.join(stem.unwrap_or_default())
    };
    let file = module_root.join(format!("{}.rs", module.ident));
    let directory = module_root.join(module.ident.to_string()).join("mod.rs");
    match (file.is_file(), directory.is_file()) {
        (true, false) => Ok(file),
        (false, true) => Ok(directory),
        _ => Err(ScanError::new(
            identity.to_string(),
            source_path,
            ScanErrorCause::UnclassifiableInput,
            format!("module {} has missing or ambiguous source", module.ident),
        )),
    }
}

pub(crate) fn nested_under_src(member_root: &Path, path: &Path) -> bool {
    path.strip_prefix(member_root).is_ok_and(|relative| {
        relative.components().next().is_some_and(|component| {
            component.as_os_str() == "src" && relative.components().count() > 2
        })
    })
}

pub(crate) fn approved_include(path: &Path, identity: &SourceIdentity) -> bool {
    path.ends_with("crates/solstone-linux/src/tray.rs")
        && identity
            .module
            .last()
            .is_some_and(|module| module == "generated")
}

#[derive(Default)]
struct IncludeVisitor {
    inputs: Vec<IncludeInput>,
}

enum IncludeInput {
    Rust(String),
    Data(String),
    GeneratedTray,
    Unclassifiable(String),
}

impl<'ast> Visit<'ast> for IncludeVisitor {
    fn visit_item_mod(&mut self, _node: &'ast ItemMod) {}

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        let name = node
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string());
        match name.as_deref() {
            Some("include") => {
                if let Ok(path) = syn::parse2::<syn::LitStr>(node.tokens.clone()) {
                    self.inputs.push(IncludeInput::Rust(path.value()));
                } else if node.tokens.to_string()
                    == "concat ! (env ! (\"OUT_DIR\") , \"/tray_icons.rs\")"
                {
                    self.inputs.push(IncludeInput::GeneratedTray);
                } else {
                    self.inputs.push(IncludeInput::Unclassifiable(
                        "include! source is not a literal".to_owned(),
                    ));
                }
            }
            Some("include_str" | "include_bytes") => {
                if let Ok(path) = syn::parse2::<syn::LitStr>(node.tokens.clone()) {
                    self.inputs.push(IncludeInput::Data(path.value()));
                } else {
                    self.inputs.push(IncludeInput::Unclassifiable(
                        "include data source is not a literal".to_owned(),
                    ));
                }
            }
            _ => {}
        }
        visit::visit_macro(self, node);
    }
}

pub(crate) struct ItemVisitor<F> {
    callback: F,
}

impl<F> ItemVisitor<F> {
    pub(crate) fn new(callback: F) -> Self {
        Self { callback }
    }
}

impl<'ast, F: FnMut(&'ast Item)> Visit<'ast> for ItemVisitor<F> {
    fn visit_item(&mut self, node: &'ast Item) {
        (self.callback)(node);
        visit::visit_item(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        visit::visit_macro(self, node);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_rail_tests::{command_path, workspace_root};
    use syn::visit::Visit;

    #[test]
    fn current_workspace_inventory_is_closed_and_names_test_context() {
        let root = workspace_root();
        let inventory = scan_workspace_with_command(
            &root,
            &CargoCommand {
                program: command_path("cargo"),
                prefix: Vec::new(),
            },
        )
        .unwrap();
        assert!(inventory.nodes.iter().any(|node| {
            node.path.ends_with("private_link_test_peer.rs") && node.identity.test_only
        }));
        assert!(
            inventory
                .nodes
                .iter()
                .any(|node| { node.path.ends_with("private_link.rs") && !node.identity.test_only })
        );
        assert!(
            inventory
                .data_inputs
                .iter()
                .any(|path| path.ends_with("cli.rs"))
        );
    }

    #[test]
    fn diagnostics_name_identity_and_path() {
        let error = ScanError::new(
            "pkg::target[bin]::crate::item{production}",
            "src/main.rs",
            ScanErrorCause::UnclassifiableInput,
            "dynamic include",
        );
        assert_eq!(
            error.to_string(),
            "source policy: identity=pkg::target[bin]::crate::item{production} path=src/main.rs rule=UnclassifiableInput detail=dynamic include"
        );
    }

    #[test]
    fn exported_walk_module_and_item_helpers_preserve_identity() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("src");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("nested")).unwrap();
        let path = source.join("nested/deep.rs");
        fs::write(&path, "mod inline { fn item() {} }\n").unwrap();
        let identity = SourceIdentity {
            package: "fixture".to_owned(),
            target: "fixture".to_owned(),
            target_kind: "lib".to_owned(),
            module: Vec::new(),
            item: None,
            cfg_context: Vec::new(),
            test_only: false,
        };
        let mut inventory = SourceInventory::default();
        walk_member(root.path(), &path, identity, &mut inventory).unwrap();
        assert!(nested_under_src(root.path(), &path));
        let module = inventory.nodes[0]
            .syntax
            .items
            .iter()
            .find_map(|item| match item {
                Item::Mod(module) => Some(module),
                _ => None,
            })
            .unwrap();
        assert_eq!(module_identity(module), "inline[inline]");
        let mut count = 0;
        ItemVisitor::new(|_: &Item| count += 1).visit_file(&inventory.nodes[0].syntax);
        assert!(count >= 2);
    }
}
