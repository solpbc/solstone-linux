// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use crate::release_rail_tests::{command_path, workspace_root};
use proc_macro2::{TokenStream, TokenTree};
use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use syn::parse::Parser;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};
use syn::{
    Attribute, Expr, ExprPath, ForeignItemFn, ImplItemFn, ItemFn, Meta, StaticMutability, Stmt,
    Token, TraitItemFn,
};

#[derive(Debug)]
struct Inventory {
    findings: Vec<Finding>,
    files: Vec<PathBuf>,
    files_inspected: usize,
    nested_src_files: usize,
    build_scripts: usize,
    members: usize,
}

#[derive(Clone, Debug)]
struct Finding {
    path: PathBuf,
    kind: FindingKind,
    function: Option<FunctionIdentity>,
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
    ancestry: Vec<AncestryNode>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum AncestryNode {
    Module {
        name: String,
        outer: Vec<AttributeIdentity>,
    },
    Trait(String),
    Impl {
        trait_path: Option<Vec<String>>,
    },
    Function(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AttributeIdentity {
    CfgTest,
    Test,
    Ignore,
    AllowUnsafeCode,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FunctionIdentity {
    visibility: VisibilityIdentity,
    is_const: bool,
    is_async: bool,
    is_unsafe: bool,
    abi: Option<String>,
    has_generics: bool,
    has_where_clause: bool,
    parameters: Vec<ParameterIdentity>,
    returns_default: bool,
    outer: Vec<AttributeIdentity>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VisibilityIdentity {
    Inherited,
    Public,
    Restricted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ParameterIdentity {
    Receiver,
    SharedStr(String),
    Other,
}

#[derive(Clone, Debug)]
struct UnsafeBlockDetail {
    node: NodeIdentity,
    statement_count: usize,
    expressions: Vec<ExpressionIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ExpressionIdentity {
    GlobalCall {
        segments: Vec<String>,
        arguments: Vec<String>,
        semicolon: bool,
    },
    Other,
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
    NestedMetaParse,
    SymlinkFile,
    SymlinkDirectory,
    SymlinkTarget,
    InvalidPathAttribute,
    UnapprovedInclude,
    MetadataExecution,
    MetadataExit,
    MetadataStdoutUtf8,
    MetadataJson,
    MetadataTargetDirectory,
    MetadataBuildDirectory,
    MetadataWorkspaceRootMismatch,
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

fn normalize_absolute(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::Normal(value) => normalized.push(value),
            Component::ParentDir => {
                if normalized == Path::new("/") {
                    return None;
                }
                normalized.pop();
            }
            Component::Prefix(_) => return None,
        }
    }
    Some(normalized)
}

fn metadata_roots(root: &Path) -> Result<Vec<PathBuf>, ScanError> {
    metadata_roots_with_command(root, &command_path("cargo"))
}

fn metadata_roots_with_command(root: &Path, cargo: &Path) -> Result<Vec<PathBuf>, ScanError> {
    let output = Command::new(cargo)
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--no-deps",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| {
            ScanError::new(cargo, ScanErrorCause::MetadataExecution, error.to_string())
        })?;
    if !output.status.success() {
        return Err(ScanError::new(
            root,
            ScanErrorCause::MetadataExit,
            format!(
                "cargo metadata exited {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        ));
    }
    let stdout = String::from_utf8(output.stdout).map_err(|error| {
        ScanError::new(root, ScanErrorCause::MetadataStdoutUtf8, error.to_string())
    })?;
    let metadata: serde_json::Value = serde_json::from_str(&stdout)
        .map_err(|error| ScanError::new(root, ScanErrorCause::MetadataJson, error.to_string()))?;
    let scan_root = normalize_absolute(root).ok_or_else(|| {
        ScanError::new(
            root,
            ScanErrorCause::MetadataWorkspaceRootMismatch,
            "scan root is not absolute",
        )
    })?;
    let metadata_workspace = metadata
        .get("workspace_root")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| normalize_absolute(Path::new(value)))
        .ok_or_else(|| {
            ScanError::new(
                root,
                ScanErrorCause::MetadataWorkspaceRootMismatch,
                "workspace_root is missing, non-string, or non-absolute",
            )
        })?;
    if metadata_workspace != scan_root {
        return Err(ScanError::new(
            metadata_workspace,
            ScanErrorCause::MetadataWorkspaceRootMismatch,
            format!("expected workspace root {}", scan_root.display()),
        ));
    }
    let mut roots = Vec::new();
    for (field, cause) in [
        ("target_directory", ScanErrorCause::MetadataTargetDirectory),
        ("build_directory", ScanErrorCause::MetadataBuildDirectory),
    ] {
        let path = metadata
            .get(field)
            .and_then(serde_json::Value::as_str)
            .and_then(|value| normalize_absolute(Path::new(value)))
            .ok_or_else(|| {
                ScanError::new(
                    root,
                    cause,
                    format!("{field} is missing, non-string, or non-absolute"),
                )
            })?;
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    Ok(roots)
}

fn paths_intersect(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

fn validate_output_root(root: &Path, cause: ScanErrorCause) -> Result<(), ScanError> {
    let mut current = PathBuf::from("/");
    for component in root.components() {
        let Component::Normal(value) = component else {
            continue;
        };
        current.push(value);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(ScanError::new(
                    &current,
                    cause,
                    "metadata output root contains a symlink component",
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(ScanError::new(
                    &current,
                    cause,
                    "metadata output root component is not a directory",
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(ScanError::new(&current, cause, error.to_string())),
        }
    }
    Ok(())
}

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

    let mut member_roots = Vec::new();
    for member in members {
        let member_name = member.as_str().ok_or_else(|| {
            ScanError::new(
                "Cargo.toml",
                ScanErrorCause::WorkspaceMemberInvalid,
                "workspace member must be a string",
            )
        })?;
        let relative = normalize_relative(Path::new(member_name)).ok_or_else(|| {
            ScanError::new(
                "Cargo.toml",
                ScanErrorCause::WorkspaceMemberInvalid,
                format!("workspace member escapes the root: {member_name}"),
            )
        })?;
        let absolute = normalize_absolute(&root.join(&relative)).ok_or_else(|| {
            ScanError::new(
                &relative,
                ScanErrorCause::WorkspaceMemberInvalid,
                "member path is not absolute",
            )
        })?;
        let metadata = fs::symlink_metadata(&absolute).map_err(|error| {
            let cause = if error.kind() == std::io::ErrorKind::NotFound {
                ScanErrorCause::MemberManifestMissing
            } else {
                ScanErrorCause::Walk
            };
            ScanError::new(relative.join("Cargo.toml"), cause, error.to_string())
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ScanError::new(
                &relative,
                ScanErrorCause::SymlinkDirectory,
                "workspace member roots may not be symlinks",
            ));
        }
        if !absolute.join("Cargo.toml").is_file() {
            return Err(ScanError::new(
                relative.join("Cargo.toml"),
                ScanErrorCause::MemberManifestMissing,
                "member manifest does not exist",
            ));
        }
        member_roots.push((relative, absolute));
    }
    let output_roots = metadata_roots(root)?;
    for output_root in &output_roots {
        let cause = if output_root == &output_roots[0] {
            ScanErrorCause::MetadataTargetDirectory
        } else {
            ScanErrorCause::MetadataBuildDirectory
        };
        for (relative, member_root) in &member_roots {
            if paths_intersect(output_root, member_root) {
                return Err(ScanError::new(
                    output_root,
                    cause,
                    format!(
                        "metadata output root intersects member {}",
                        relative.display()
                    ),
                ));
            }
        }
        validate_output_root(output_root, cause)?;
    }

    let mut inventory = Inventory {
        findings: Vec::new(),
        files: Vec::new(),
        files_inspected: 0,
        nested_src_files: 0,
        build_scripts: 0,
        members: members.len(),
    };
    for (member_relative, _) in member_roots {
        let member_root = root.join(&member_relative);
        walk_member(
            root,
            &member_root,
            &member_relative,
            Path::new(""),
            &output_roots,
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
    output_roots: &[PathBuf],
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
            let target = fs::metadata(entry.path()).map_err(|error| {
                ScanError::new(
                    &workspace_relative,
                    ScanErrorCause::SymlinkTarget,
                    error.to_string(),
                )
            })?;
            if target.is_dir() {
                return Err(ScanError::new(
                    workspace_relative,
                    ScanErrorCause::SymlinkDirectory,
                    "symlinked directories are not scanned",
                ));
            }
            if target.is_file() && entry.path().extension().is_some_and(|value| value == "rs") {
                return Err(ScanError::new(
                    workspace_relative,
                    ScanErrorCause::SymlinkFile,
                    "symlinked Rust files are not scanned",
                ));
            }
            if target.is_file() {
                continue;
            }
            return Err(ScanError::new(
                workspace_relative,
                ScanErrorCause::SymlinkTarget,
                "symlink target is neither a regular file nor a directory",
            ));
        }
        if metadata.is_dir() {
            let child_absolute = normalize_absolute(&entry.path()).ok_or_else(|| {
                ScanError::new(
                    &workspace_relative,
                    ScanErrorCause::Walk,
                    "source path is not absolute",
                )
            })?;
            if !output_roots
                .iter()
                .any(|root| child_absolute.starts_with(root))
            {
                walk_member(
                    workspace_root,
                    member_root,
                    member_relative,
                    &child_relative,
                    output_roots,
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
    inventory.files.push(workspace_relative.to_owned());
    if path.file_name().is_some_and(|name| name == "build.rs") {
        inventory.build_scripts += 1;
    }
    if nested_under_src(member_root, workspace_relative, path)? {
        inventory.nested_src_files += 1;
    }
    let mut scanner = AstScanner {
        workspace_root,
        path: workspace_relative,
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

fn nested_under_src(
    member_root: &Path,
    workspace_relative: &Path,
    path: &Path,
) -> Result<bool, ScanError> {
    let relative = path.strip_prefix(member_root).map_err(|error| {
        ScanError::new(workspace_relative, ScanErrorCause::Walk, error.to_string())
    })?;
    Ok(relative
        .components()
        .next()
        .is_some_and(|first| first.as_os_str() == "src" && relative.components().count() > 2))
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
    path: &'a Path,
    findings: &'a mut Vec<Finding>,
    ancestry: Vec<AncestryNode>,
    ordinal: usize,
    active_allowance: Option<usize>,
    function_attributes: bool,
    error: Option<ScanError>,
}

fn attribute_identity(attribute: &Attribute) -> AttributeIdentity {
    match &attribute.meta {
        Meta::List(list) if list.path.is_ident("cfg") && list.tokens.to_string() == "test" => {
            AttributeIdentity::CfgTest
        }
        Meta::Path(path) if path.is_ident("test") => AttributeIdentity::Test,
        Meta::Path(path) if path.is_ident("ignore") => AttributeIdentity::Ignore,
        Meta::List(list)
            if list.path.is_ident("allow") && list.tokens.to_string() == "unsafe_code" =>
        {
            AttributeIdentity::AllowUnsafeCode
        }
        _ => AttributeIdentity::Other,
    }
}

fn function_identity(
    attrs: &[Attribute],
    visibility: &syn::Visibility,
    signature: &syn::Signature,
) -> FunctionIdentity {
    let visibility = match visibility {
        syn::Visibility::Inherited => VisibilityIdentity::Inherited,
        syn::Visibility::Public(_) => VisibilityIdentity::Public,
        syn::Visibility::Restricted(_) => VisibilityIdentity::Restricted,
    };
    let parameters = signature
        .inputs
        .iter()
        .map(|argument| match argument {
            syn::FnArg::Receiver(_) => ParameterIdentity::Receiver,
            syn::FnArg::Typed(typed) => {
                let syn::Pat::Ident(pattern) = typed.pat.as_ref() else {
                    return ParameterIdentity::Other;
                };
                let syn::Type::Reference(reference) = typed.ty.as_ref() else {
                    return ParameterIdentity::Other;
                };
                let syn::Type::Path(path) = reference.elem.as_ref() else {
                    return ParameterIdentity::Other;
                };
                if pattern.by_ref.is_none()
                    && pattern.mutability.is_none()
                    && pattern.subpat.is_none()
                    && reference.lifetime.is_none()
                    && reference.mutability.is_none()
                    && path.qself.is_none()
                    && path.path.leading_colon.is_none()
                    && path.path.segments.len() == 1
                    && path.path.segments[0].ident == "str"
                    && matches!(path.path.segments[0].arguments, syn::PathArguments::None)
                {
                    ParameterIdentity::SharedStr(pattern.ident.to_string())
                } else {
                    ParameterIdentity::Other
                }
            }
        })
        .collect();
    FunctionIdentity {
        visibility,
        is_const: signature.constness.is_some(),
        is_async: signature.asyncness.is_some(),
        is_unsafe: signature.unsafety.is_some(),
        abi: signature.abi.as_ref().map(|abi| {
            abi.name
                .as_ref()
                .map_or_else(|| "C".into(), syn::LitStr::value)
        }),
        has_generics: !signature.generics.params.is_empty(),
        has_where_clause: signature.generics.where_clause.is_some(),
        parameters,
        returns_default: matches!(signature.output, syn::ReturnType::Default),
        outer: attrs.iter().map(attribute_identity).collect(),
    }
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
            function: None,
            node: self.identity(),
            unsafe_blocks: Vec::new(),
            attribute_style: placement,
        };
        self.findings.push(finding);
        self.findings.len() - 1
    }

    fn with_item(&mut self, item: AncestryNode, operation: impl FnOnce(&mut Self)) {
        self.ancestry.push(item);
        operation(self);
        self.ancestry.pop();
    }

    fn visit_function(
        &mut self,
        name: String,
        attrs: &[Attribute],
        visibility: &syn::Visibility,
        signature: &syn::Signature,
        body: Option<&syn::Block>,
    ) {
        let identity = function_identity(attrs, visibility, signature);
        self.with_item(AncestryNode::Function(name), |scanner| {
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
                    scanner.findings.last_mut().unwrap().function = Some(identity.clone());
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
        self.inspect_meta(&attribute.meta, placement, false);
    }

    fn inspect_meta(&mut self, meta: &Meta, placement: AttributePlacement, nested_unsafe: bool) {
        if meta.path().is_ident("path") {
            self.set_error(
                ScanErrorCause::InvalidPathAttribute,
                &format!("path metadata is forbidden: {meta:?}"),
            );
            return;
        }
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
        let entries = match parser.parse2(list.tokens.clone()) {
            Ok(entries) => entries,
            Err(error) => {
                self.set_error(ScanErrorCause::NestedMetaParse, &error.to_string());
                return;
            }
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
            &node.vis,
            &node.sig,
            Some(&node.block),
        );
    }

    fn visit_impl_item_fn(&mut self, node: &'ast ImplItemFn) {
        self.visit_function(
            node.sig.ident.to_string(),
            &node.attrs,
            &node.vis,
            &node.sig,
            Some(&node.block),
        );
    }

    fn visit_trait_item_fn(&mut self, node: &'ast TraitItemFn) {
        self.visit_function(
            node.sig.ident.to_string(),
            &node.attrs,
            &syn::Visibility::Inherited,
            &node.sig,
            node.default.as_ref(),
        );
    }

    fn visit_foreign_item_fn(&mut self, node: &'ast ForeignItemFn) {
        self.visit_function(
            node.sig.ident.to_string(),
            &node.attrs,
            &node.vis,
            &node.sig,
            None,
        );
    }

    fn visit_attribute(&mut self, node: &'ast Attribute) {
        self.inspect_attribute(node);
        visit::visit_attribute(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        let detail = UnsafeBlockDetail {
            node: self.identity(),
            statement_count: node.block.stmts.len(),
            expressions: node.block.stmts.iter().map(expression_identity).collect(),
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
        self.with_item(AncestryNode::Trait(node.ident.to_string()), |scanner| {
            visit::visit_item_trait(scanner, node)
        });
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if node.unsafety.is_some() {
            self.finding(FindingKind::UnsafeImpl, None);
        }
        let trait_path = node.trait_.as_ref().map(|(_, path, _)| {
            path.segments
                .iter()
                .map(|segment| segment.ident.to_string())
                .collect()
        });
        self.with_item(AncestryNode::Impl { trait_path }, |scanner| {
            visit::visit_item_impl(scanner, node)
        });
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
        let outer = node.attrs.iter().map(attribute_identity).collect();
        self.with_item(
            AncestryNode::Module {
                name: node.ident.to_string(),
                outer,
            },
            |scanner| visit::visit_item_mod(scanner, node),
        );
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
        let path = node
            .path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect::<Vec<_>>()
            .join("::");
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
            && self.ancestry.last().is_some_and(
                |node| matches!(node, AncestryNode::Module { name, .. } if name == "generated"),
            )
            && node.tokens.to_string() == "concat ! (env ! (\"OUT_DIR\") , \"/tray_icons.rs\")"
            && self.workspace_root.join(self.path).is_file()
    }
}

fn expression_identity(statement: &Stmt) -> ExpressionIdentity {
    let Stmt::Expr(Expr::Call(call), semicolon) = statement else {
        return ExpressionIdentity::Other;
    };
    let Expr::Path(ExprPath {
        qself: None, path, ..
    }) = call.func.as_ref()
    else {
        return ExpressionIdentity::Other;
    };
    if path.leading_colon.is_none()
        || path
            .segments
            .iter()
            .any(|segment| !matches!(segment.arguments, syn::PathArguments::None))
    {
        return ExpressionIdentity::Other;
    }
    let mut arguments = Vec::new();
    for argument in &call.args {
        let Expr::Path(ExprPath {
            qself: None, path, ..
        }) = argument
        else {
            return ExpressionIdentity::Other;
        };
        if path.leading_colon.is_some()
            || path.segments.len() != 1
            || !matches!(path.segments[0].arguments, syn::PathArguments::None)
        {
            return ExpressionIdentity::Other;
        }
        arguments.push(path.segments[0].ident.to_string());
    }
    ExpressionIdentity::GlobalCall {
        segments: path
            .segments
            .iter()
            .map(|segment| segment.ident.to_string())
            .collect(),
        arguments,
        semicolon: semicolon.is_some(),
    }
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
    fs::create_dir(root.path().join("target")).expect("fixture metadata output root");
    fs::write(root.path().join("target/MALFORMED.rs"), "fn broken(\n")
        .expect("malformed output fixture");
    fs::write(
        root.path().join("target/UNSAFE.rs"),
        "fn hidden() { unsafe {} }\n",
    )
    .expect("unsafe output fixture");
    for member in ["crates/solstone-linux", "crates/helper"] {
        let member_root = root.path().join(member);
        fs::create_dir_all(member_root.join("src/nested")).expect("fixture source tree");
        fs::create_dir_all(member_root.join("src/target")).expect("target-named source tree");
        fs::create_dir_all(member_root.join("tests")).expect("fixture tests tree");
        fs::create_dir_all(member_root.join("examples")).expect("fixture examples tree");
        fs::create_dir_all(member_root.join("benches")).expect("fixture benches tree");
        fs::write(
            member_root.join("Cargo.toml"),
            format!(
                "[package]\nname = \"{}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{}",
                member.rsplit('/').next().expect("member name"),
                if member == "crates/solstone-linux" {
                    "[[bin]]\nname = \"target-tool\"\npath = \"src/target/tool.rs\"\n"
                } else {
                    ""
                }
            ),
        )
        .expect("fixture member manifest");
        fs::write(member_root.join("build.rs"), "fn main() {}\n").expect("fixture build script");
        fs::write(
            member_root.join("src/lib.rs"),
            "pub mod target;\npub fn safe() {}\n",
        )
        .expect("fixture lib");
        fs::write(
            member_root.join("src/target/mod.rs"),
            "pub fn safe_target() {}\n",
        )
        .expect("target module");
        if member == "crates/solstone-linux" {
            fs::write(member_root.join("src/target/tool.rs"), "fn main() {}\n")
                .expect("target bin");
        } else {
            fs::create_dir(member_root.join("target")).expect("member target source tree");
            fs::write(
                member_root.join("target/source.rs"),
                "fn safe_target_source() {}\n",
            )
            .expect("member target source");
            fs::create_dir(member_root.join("src/nested/target"))
                .expect("nested target source tree");
            fs::write(
                member_root.join("src/nested/target/source.rs"),
                "fn safe_nested_target() {}\n",
            )
            .expect("nested target source");
        }
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
    for (name, ancestry, parameters, outer, callee, arguments, semicolon) in [
        (
            "set_session_environment_variable",
            vec![AncestryNode::Function(
                "set_session_environment_variable".into(),
            )],
            vec![
                ParameterIdentity::SharedStr("name".into()),
                ParameterIdentity::SharedStr("value".into()),
            ],
            vec![AttributeIdentity::AllowUnsafeCode],
            "set_var",
            vec!["name".to_owned(), "value".to_owned()],
            false,
        ),
        (
            "session_environment_wrapper_assigns_and_restores",
            vec![
                AncestryNode::Module {
                    name: "tests".into(),
                    outer: vec![AttributeIdentity::CfgTest],
                },
                AncestryNode::Function("session_environment_wrapper_assigns_and_restores".into()),
            ],
            vec![],
            vec![AttributeIdentity::Test, AttributeIdentity::AllowUnsafeCode],
            "remove_var",
            vec!["NAME".to_owned()],
            false,
        ),
    ] {
        let finding = inventory
            .findings
            .iter()
            .find(|finding| {
                finding.node.ancestry.last() == Some(&AncestryNode::Function(name.to_owned()))
            })
            .ok_or_else(|| format!("missing reviewed seam {name}: {:#?}", inventory.findings))?;
        let expected_function = FunctionIdentity {
            visibility: VisibilityIdentity::Inherited,
            is_const: false,
            is_async: false,
            is_unsafe: false,
            abi: None,
            has_generics: false,
            has_where_clause: false,
            parameters,
            returns_default: true,
            outer,
        };
        let _diagnostic_ordinal = finding
            .unsafe_blocks
            .first()
            .map(|block| block.node.ordinal);
        if finding.path != Path::new("crates/solstone-linux/src/cli.rs")
            || finding.kind != FindingKind::UnsafeCodeAllowance
            || finding.attribute_style != Some(AttributePlacement::FunctionOuter)
            || finding.node.ancestry != ancestry
            || finding.function.as_ref() != Some(&expected_function)
            || finding.unsafe_blocks.len() != 1
            || finding.unsafe_blocks[0].statement_count != 1
            || finding.unsafe_blocks[0].expressions
                != vec![ExpressionIdentity::GlobalCall {
                    segments: vec!["std".into(), "env".into(), callee.into()],
                    arguments,
                    semicolon,
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

// AC: recursive enumeration covers exact conventional and target-named source paths but not metadata output roots.
#[test]
fn fixture_coverage_shape_is_computed_by_the_scanner() {
    let fixture = fixture_workspace();
    let inventory = scan_fixture(&fixture).expect("safe fixture scans");
    assert_eq!(inventory.members, 2);
    for path in [
        "crates/solstone-linux/src/target/mod.rs",
        "crates/solstone-linux/src/target/tool.rs",
        "crates/helper/target/source.rs",
        "crates/helper/src/nested/target/source.rs",
    ] {
        assert!(
            inventory.files.contains(&PathBuf::from(path)),
            "missing {path}: {inventory:#?}"
        );
    }
    assert!(
        !inventory
            .files
            .iter()
            .any(|path| path.starts_with("target"))
    );
    assert_eq!(inventory.build_scripts, 2);
    assert!(inventory.findings.is_empty());
}

// AC: a member-root target/source.rs is scanned when it is not a metadata output root.
#[test]
fn target_named_member_source_directories_are_scanned() {
    let fixture = fixture_workspace();
    let relative = Path::new("crates/helper/target/source.rs");
    fs::write(
        fixture.root.path().join(relative),
        "fn probe() { unsafe {} }\n",
    )
    .unwrap();
    let inventory = scan_fixture(&fixture).expect("target-named member source scans");
    assert!(
        inventory
            .findings
            .iter()
            .any(|finding| finding.path == relative)
    );
}

// AC: a declared bin under src/target/tool.rs remains covered by recursive source scanning.
#[test]
fn target_named_declared_bin_source_is_scanned() {
    let fixture = fixture_workspace();
    let relative = Path::new("crates/solstone-linux/src/target/tool.rs");
    fs::write(
        fixture.root.path().join(relative),
        "fn main() { unsafe {} }\n",
    )
    .unwrap();
    let inventory = scan_fixture(&fixture).expect("target-named bin scans");
    assert!(
        inventory
            .findings
            .iter()
            .any(|finding| finding.path == relative)
    );
}

// AC: a sibling nested source subtree named target is scanned and reports its exact finding path.
#[test]
fn nested_target_named_source_is_scanned() {
    let fixture = fixture_workspace();
    let relative = Path::new("crates/helper/src/nested/target/source.rs");
    fs::write(
        fixture.root.path().join(relative),
        "fn probe() { unsafe {} }\n",
    )
    .unwrap();
    let inventory = scan_fixture(&fixture).expect("nested target-named source scans");
    assert!(
        inventory
            .findings
            .iter()
            .any(|finding| finding.path == relative)
    );
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
#[allow(unsafe_code)]
fn set_session_environment_variable(name: &str, value: &str) {
    unsafe { ::std::env::set_var(name, value) };
}
#[cfg(test)]
mod tests {
#[test]
#[allow(unsafe_code)]
fn session_environment_wrapper_assigns_and_restores() {
    const NAME: &str = "NAME";
    unsafe { ::std::env::remove_var(NAME) }
}
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
            "unsafe { ::std::env::set_var(name, value) };",
            "unsafe { let _extra = 1; ::std::env::set_var(name, value) };",
        ),
        REVIEWED_FIXTURE_SEAMS.replace(
            "unsafe { ::std::env::set_var(name, value) };",
            "unsafe { ::std::env::set_var(name, value) }; unsafe { ::std::env::set_var(name, value) };",
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
            "::std::env::set_var(name, value)",
            "std::env::set_var(name, value)",
        ),
        REVIEWED_FIXTURE_SEAMS.replace(
            "::std::env::remove_var(NAME)",
            "::std::env::remove_var(NAME, NAME)",
        ),
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

// AC: local modules and import aliases cannot satisfy the globally rooted reviewed call identity.
#[test]
fn reviewed_seam_shadow_paths_fail_identity() {
    for prefix in [
        "mod env { pub unsafe fn set_var(_: &str, _: &str) {} }\n",
        "use std::env as env;\n",
    ] {
        let fixture = fixture_workspace();
        let source = format!(
            "{prefix}{}",
            REVIEWED_FIXTURE_SEAMS.replacen("::std::env::set_var", "env::set_var", 1)
        );
        write_reviewed_fixture_seams(&fixture, &source);
        let inventory = scan_fixture(&fixture).expect("shadow fixture scans");
        assert!(reviewed_seams_error(&inventory).is_err(), "{source}");
    }
}

// AC: reviewed expressions match directly and expression wrappers are never normalized away.
#[test]
fn reviewed_seam_wrapped_expressions_fail_identity() {
    for call in [
        "(::std::env::set_var)(name, value)",
        "{ ::std::env::set_var }(name, value)",
        "(*&::std::env::set_var)(name, value)",
        "(::std::env::set_var as unsafe fn(&str, &str))(name, value)",
        "::std::env::set_var(&name, value)",
        "::std::env::set_var(name.field, value)",
        "::std::env::set_var(name[0], value)",
        "::std::env::set_var(make!(), value)",
    ] {
        let fixture = fixture_workspace();
        let source = REVIEWED_FIXTURE_SEAMS.replace("::std::env::set_var(name, value)", call);
        write_reviewed_fixture_seams(&fixture, &source);
        let inventory = scan_fixture(&fixture).expect("wrapper fixture parses");
        assert!(reviewed_seams_error(&inventory).is_err(), "{call}");
    }
}

// AC: typed ancestry prevents impl and trait methods from impersonating reviewed free functions.
#[test]
fn impl_and_trait_methods_cannot_impersonate_free_function_seams() {
    for replacement in [
        "struct Holder; impl Holder { #[allow(unsafe_code)] fn set_session_environment_variable(name: &str, value: &str) { unsafe { ::std::env::set_var(name, value) }; } }",
        "trait Holder { #[allow(unsafe_code)] fn set_session_environment_variable(name: &str, value: &str) { unsafe { ::std::env::set_var(name, value) }; } }",
    ] {
        let fixture = fixture_workspace();
        let start = REVIEWED_FIXTURE_SEAMS
            .find("#[allow(unsafe_code)]\nfn set_session_environment_variable")
            .unwrap();
        let end = REVIEWED_FIXTURE_SEAMS.find("#[cfg(test)]").unwrap();
        let source = format!(
            "{}{}\n{}",
            &REVIEWED_FIXTURE_SEAMS[..start],
            replacement,
            &REVIEWED_FIXTURE_SEAMS[end..]
        );
        write_reviewed_fixture_seams(&fixture, &source);
        let inventory = scan_fixture(&fixture).expect("method fixture scans");
        assert!(reviewed_seams_error(&inventory).is_err(), "{source}");
    }
}

// AC: the restoration seam requires precisely one cfg(test) tests module and exact test attributes.
#[test]
fn reviewed_test_seam_requires_test_module_and_attributes() {
    let mutations = [
        REVIEWED_FIXTURE_SEAMS.replace("#[cfg(test)]", "#[cfg(test)]\n#[cfg(test)]"),
        REVIEWED_FIXTURE_SEAMS.replace("#[cfg(test)]", "#[cfg(test)]\n#[cfg(unix)]"),
        REVIEWED_FIXTURE_SEAMS.replace("#[cfg(test)]", "#[cfg(feature = \"x\")]"),
        REVIEWED_FIXTURE_SEAMS.replace("#[cfg(test)]", "#[cfg(all(test, unix))]"),
        REVIEWED_FIXTURE_SEAMS.replace("#[cfg(test)]", "#[cfg(any(test, unix))]"),
        REVIEWED_FIXTURE_SEAMS.replace("#[cfg(test)]", "#[cfg(not(test))]"),
        REVIEWED_FIXTURE_SEAMS.replace("#[cfg(test)]", "#[cfg_attr(test, cfg(test))]"),
        REVIEWED_FIXTURE_SEAMS.replace("mod tests", "mod renamed"),
        format!("mod outer {{\n{REVIEWED_FIXTURE_SEAMS}\n}}\n"),
        REVIEWED_FIXTURE_SEAMS.replacen("#[test]\n", "", 1),
        REVIEWED_FIXTURE_SEAMS.replace("#[test]", "#[test]\n#[ignore]"),
    ];
    for source in mutations {
        let fixture = fixture_workspace();
        write_reviewed_fixture_seams(&fixture, &source);
        let inventory = scan_fixture(&fixture).expect("module identity fixture scans");
        assert!(reviewed_seams_error(&inventory).is_err(), "{source}");
    }
}

// AC: reviewed authorization includes the complete span-free production function signature.
#[test]
fn reviewed_seam_signature_mutations_fail_identity() {
    for signature in [
        "pub fn set_session_environment_variable(name: &str, value: &str)",
        "const fn set_session_environment_variable(name: &str, value: &str)",
        "async fn set_session_environment_variable(name: &str, value: &str)",
        "unsafe fn set_session_environment_variable(name: &str, value: &str)",
        "extern \"C\" fn set_session_environment_variable(name: &str, value: &str)",
        "fn set_session_environment_variable<T>(name: &str, value: &str)",
        "fn set_session_environment_variable<'a>(name: &'a str, value: &str)",
        "fn set_session_environment_variable(name: &str, value: &str) where String: Clone",
        "fn set_session_environment_variable(mut name: &str, value: &str)",
        "fn set_session_environment_variable(name: &String, value: &str)",
        "fn set_session_environment_variable(name: &str, value: &str) -> ()",
    ] {
        let fixture = fixture_workspace();
        let source = REVIEWED_FIXTURE_SEAMS.replace(
            "fn set_session_environment_variable(name: &str, value: &str)",
            signature,
        );
        write_reviewed_fixture_seams(&fixture, &source);
        let inventory = scan_fixture(&fixture).expect("signature fixture parses");
        assert!(reviewed_seams_error(&inventory).is_err(), "{signature}");
    }
}

// AC: each reviewed function owns one exact direct outer unsafe-code allowance.
#[test]
fn reviewed_seam_allowance_must_be_unique_and_direct() {
    for replacement in [
        "#[allow(unsafe_code)]\n#[allow(unsafe_code)]",
        "#[allow(unsafe_code, dead_code)]",
        "#[cfg_attr(test, allow(unsafe_code))]",
        "#[allow(dead_code)]",
    ] {
        let fixture = fixture_workspace();
        let source = REVIEWED_FIXTURE_SEAMS.replacen("#[allow(unsafe_code)]", replacement, 1);
        write_reviewed_fixture_seams(&fixture, &source);
        let inventory = scan_fixture(&fixture).expect("allowance fixture parses");
        assert!(reviewed_seams_error(&inventory).is_err(), "{source}");
    }
}

// AC: discovery ordinal remains diagnostic and an unrelated preceding safe item cannot alter authorization.
#[test]
fn reviewed_seam_identity_ignores_diagnostic_ordinal() {
    let fixture = fixture_workspace();
    let source = format!("fn earlier_safe_item() {{}}\n{REVIEWED_FIXTURE_SEAMS}");
    write_reviewed_fixture_seams(&fixture, &source);
    let inventory = scan_fixture(&fixture).expect("ordinal fixture scans");
    assert_eq!(reviewed_seams_error(&inventory), Ok(()));
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

// AC: direct path metadata is rejected before target resolution regardless of payload shape.
#[test]
fn rejects_escaping_and_nonliteral_path_attributes() {
    for source in [
        "#[path = \"../../../escape.rs\"] mod escape;",
        "#[path(test)] mod escape;",
    ] {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate path attribute");
        let error = scan_fixture(&fixture).expect_err("path attribute must fail");
        assert_eq!(error.cause, ScanErrorCause::InvalidPathAttribute, "{error}");
        assert_eq!(error.path, Path::new("crates/solstone-linux/src/lib.rs"));
    }
}

// AC: path metadata is rejected because alternate source routing is unsupported, not because a directory is excluded.
#[test]
fn rejects_path_attributes_hidden_from_recursive_enumeration() {
    for source in [
        "#[path = \"hidden.txt\"] mod hidden;",
        "#[path = \"target/mod.rs\"] mod generated;",
    ] {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate hidden path attribute");
        let error = scan_fixture(&fixture).expect_err("path metadata must fail");
        assert_eq!(error.cause, ScanErrorCause::InvalidPathAttribute);
        assert_eq!(error.path, Path::new("crates/solstone-linux/src/lib.rs"));
    }
}

// AC: every syntactic route to path metadata reaches one fail-closed dispatch point.
#[test]
fn all_path_meta_forms_are_rejected() {
    for source in [
        "#[path = \"x.rs\"] mod x;",
        "#[path = FOO] mod x;",
        "#[path] mod x;",
        "#[path(x)] mod x;",
        "#[cfg_attr(unix, path = \"x.rs\")] mod x;",
        "#[cfg_attr(windows, path = \"x.rs\")] mod x;",
        "#[cfg_attr(a, cfg_attr(b, path = \"x.rs\"))] mod x;",
        "#[cfg_attr(a, allow(dead_code), path = \"x.rs\")] mod x;",
    ] {
        let fixture = fixture_workspace();
        fs::write(&fixture.primary_lib, source).expect("mutate path metadata");
        let error = scan_fixture(&fixture).expect_err("path metadata must fail");
        assert_eq!(
            error.cause,
            ScanErrorCause::InvalidPathAttribute,
            "{source}: {error}"
        );
        assert_eq!(error.path, Path::new("crates/solstone-linux/src/lib.rs"));
        assert!(error.detail.contains("path"), "{error}");
    }
}

// AC: malformed nested attribute metadata fails with the source path instead of being ignored.
#[test]
fn rejects_malformed_nested_attribute_metadata() {
    let fixture = fixture_workspace();
    fs::write(
        &fixture.primary_lib,
        "#[allow(unsafe_code +)] fn probe() {}\n",
    )
    .expect("mutate malformed attribute");
    let error = scan_fixture(&fixture).expect_err("malformed nested metadata must fail");
    assert_eq!(error.cause, ScanErrorCause::NestedMetaParse);
    assert_eq!(error.path, Path::new("crates/solstone-linux/src/lib.rs"));
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

// AC: a symlink cannot substitute an external directory for a declared workspace member.
#[test]
fn rejects_symlinked_workspace_member_root() {
    use std::os::unix::fs::symlink;

    let fixture = fixture_workspace();
    let member = fixture.root.path().join("crates/helper");
    let outside = fixture.root.path().join("outside-helper");
    fs::rename(&member, &outside).expect("move member outside declared path");
    symlink(&outside, &member).expect("symlink workspace member");
    let error = scan_fixture(&fixture).expect_err("symlinked member root must fail");
    assert_eq!(error.cause, ScanErrorCause::SymlinkDirectory);
    assert_eq!(error.path, Path::new("crates/helper"));
}

// AC: non-Rust file symlinks are ignored while broken symlinks fail closed with their path.
#[test]
fn classifies_non_rust_and_broken_symlinks_by_resolved_target() {
    use std::os::unix::fs::symlink;

    let fixture = fixture_workspace();
    let member = fixture.root.path().join("crates/solstone-linux");
    let outside = fixture.root.path().join("notes.txt");
    fs::write(&outside, "not Rust source\n").expect("outside non-Rust file");
    symlink(&outside, member.join("notes.txt")).expect("non-Rust file symlink");
    scan_fixture(&fixture).expect("non-Rust file symlink is ignored");

    symlink(
        fixture.root.path().join("missing-target"),
        member.join("broken"),
    )
    .expect("broken symlink");
    let error = scan_fixture(&fixture).expect_err("broken symlink must fail closed");
    assert_eq!(error.cause, ScanErrorCause::SymlinkTarget);
    assert_eq!(error.path, Path::new("crates/solstone-linux/broken"));
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

fn fake_cargo(root: &Path, name: &str, body: &[u8]) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = root.join(name);
    fs::write(&path, body).expect("fake cargo");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("fake cargo mode");
    path
}

fn metadata_json(root: &Path, target: Option<&Path>, build: Option<&Path>) -> String {
    let mut value = serde_json::json!({ "workspace_root": root });
    if let Some(target) = target {
        value["target_directory"] = serde_json::json!(target);
    }
    if let Some(build) = build {
        value["build_directory"] = serde_json::json!(build);
    }
    value.to_string()
}

fn output_script(json: &str) -> Vec<u8> {
    format!("#!/bin/sh\nprintf '%s' '{}'\n", json).into_bytes()
}

// AC: all seven metadata failure discriminants are reachable and carry a path.
#[test]
fn cargo_metadata_failures_are_discriminated_and_path_bearing() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let target = root.join("target");
    let cases = [
        (
            fake_cargo(root, "cargo-exit", b"#!/bin/sh\nexit 19\n"),
            ScanErrorCause::MetadataExit,
        ),
        (
            fake_cargo(root, "cargo-utf8", b"#!/bin/sh\nprintf '\\377'\n"),
            ScanErrorCause::MetadataStdoutUtf8,
        ),
        (
            fake_cargo(root, "cargo-json", b"#!/bin/sh\nprintf '{'\n"),
            ScanErrorCause::MetadataJson,
        ),
        (
            fake_cargo(
                root,
                "cargo-target",
                &output_script(&metadata_json(root, None, Some(&target))),
            ),
            ScanErrorCause::MetadataTargetDirectory,
        ),
        (
            fake_cargo(
                root,
                "cargo-build",
                &output_script(&metadata_json(root, Some(&target), None)),
            ),
            ScanErrorCause::MetadataBuildDirectory,
        ),
        (
            fake_cargo(
                root,
                "cargo-workspace",
                &output_script(&metadata_json(
                    Path::new("/wrong/workspace"),
                    Some(&target),
                    Some(&target),
                )),
            ),
            ScanErrorCause::MetadataWorkspaceRootMismatch,
        ),
    ];
    for (command, expected) in cases {
        let error = metadata_roots_with_command(root, &command).expect_err("metadata must fail");
        assert_eq!(error.cause, expected, "{error}");
        assert!(!error.path.as_os_str().is_empty(), "{error}");
    }
    let missing = root.join("does-not-exist");
    let error = metadata_roots_with_command(root, &missing).expect_err("spawn must fail");
    assert_eq!(error.cause, ScanErrorCause::MetadataExecution);
    assert_eq!(error.path, missing);
}

// AC: every symlink component in a metadata output root fails without regard to its destination.
#[test]
fn metadata_output_root_symlink_component_always_fails() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let member = temp.path().join("member");
    let harmless = temp.path().join("harmless");
    fs::create_dir(&member).unwrap();
    fs::create_dir(&harmless).unwrap();
    for (name, destination) in [("into-member", &member), ("harmless-link", &harmless)] {
        let link = temp.path().join(name);
        symlink(destination, &link).unwrap();
        let error = validate_output_root(
            &link.join("nested"),
            ScanErrorCause::MetadataTargetDirectory,
        )
        .expect_err("symlink output component must fail");
        assert_eq!(error.cause, ScanErrorCause::MetadataTargetDirectory);
        assert_eq!(error.path, link);
    }
}

// AC: a nonexistent metadata output root is a valid clean-tree pruning boundary.
#[test]
fn nonexistent_metadata_output_root_is_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("missing/target");
    assert!(validate_output_root(&root, ScanErrorCause::MetadataTargetDirectory).is_ok());
}

// AC: an output root intersecting a member tree in either direction is rejected by the guard predicate.
#[test]
fn metadata_output_root_intersection_with_member_fails() {
    let base = Path::new("/workspace");
    assert!(paths_intersect(
        &base.join("member/target"),
        &base.join("member")
    ));
    assert!(paths_intersect(base, &base.join("member")));
    assert!(!paths_intersect(&base.join("target"), &base.join("member")));
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
