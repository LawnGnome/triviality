use std::{
    collections::{BTreeSet, HashMap},
    ffi::OsStr,
    fs::File,
    io::Read,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
};

use clap::Parser;
use itertools::Itertools;
use lazy_static::lazy_static;
use semver::Version;
use serde::Deserialize;
use walkdir::WalkDir;

/// Scans paths containing one or more extracted crate files to see if those crates implement
/// non-trivial code.
#[derive(Parser)]
struct Opt {
    /// Also display non-trivial crates.
    #[arg(short, long)]
    verbose: bool,

    /// Paths to scan.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let opt = Opt::parse();

    for path in opt.paths.iter() {
        let crate_roots = WalkDir::new(path)
            .into_iter()
            .filter_ok(|entry| entry.file_type().is_file() && is_manifest(entry.file_name()))
            .map_ok(|entry| -> anyhow::Result<_> {
                let manifest_path = entry.path();
                let root = manifest_path
                    .parent()
                    .ok_or_else(|| {
                        anyhow::anyhow!("unexpected lack of parent for {manifest_path:?}")
                    })?
                    .to_path_buf();

                let mut raw = String::new();
                File::open(manifest_path)?.read_to_string(&mut raw)?;
                let manifest: Manifest = toml::from_str(&raw)?;

                Ok(Root { root, manifest })
            })
            .flatten()
            .fold_ok(HashMap::<String, BTreeSet<Root>>::new(), |mut acc, root| {
                acc.entry(root.manifest.package.name.clone())
                    .or_default()
                    .insert(root);
                acc
            })?;

        // FIXME: do something to not scan nested manifests within crate files.

        for (name, version_roots) in crate_roots.into_iter() {
            if crate_has_non_trivial_code(version_roots.into_iter())? {
                if opt.verbose {
                    println!("non trivial: {name}");
                }
            } else {
                println!("{name}");
            }
        }
    }

    Ok(())
}

fn crate_has_non_trivial_code(roots: impl Iterator<Item = Root>) -> anyhow::Result<bool> {
    for root in roots {
        if root.has_non_trivial_code()? {
            return Ok(true);
        }
    }

    Ok(false)
}

#[derive(Debug, Eq)]
struct Root {
    root: PathBuf,
    manifest: Manifest,
}

impl Root {
    fn has_non_trivial_code(&self) -> anyhow::Result<bool> {
        for bin in self.bins() {
            if is_bin_non_trivial(bin)? {
                return Ok(true);
            }
        }

        if let Some(lib) = self.lib() {
            if is_lib_non_trivial(lib)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    fn bins(&self) -> impl Iterator<Item = PathBuf> {
        match &self.manifest.bins {
            Some(bins) if !bins.is_empty() => bins
                .iter()
                .filter_map(|bin| bin.path.as_ref().map(|path| self.root.join(path)))
                .collect_vec()
                .into_iter(),
            _ => {
                let default = self.root.join("src").join("main.rs");
                if default.exists() {
                    vec![default].into_iter()
                } else {
                    Vec::new().into_iter()
                }
            }
        }
    }

    fn lib(&self) -> Option<PathBuf> {
        if let Some(lib) = &self.manifest.lib {
            if let Some(path) = &lib.path {
                let path = self.root.join(path);
                if path.exists() {
                    return Some(path);
                }
            }
        }

        let default = self.root.join("src").join("lib.rs");
        if default.exists() {
            Some(default)
        } else {
            None
        }
    }
}

impl PartialEq for Root {
    fn eq(&self, other: &Self) -> bool {
        self.manifest == other.manifest
    }
}

impl Ord for Root {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.manifest.cmp(&other.manifest)
    }
}

impl PartialOrd for Root {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

lazy_static! {
    // static ref FUNCTION_RE: Regex = { Regex::new(r#""#).unwrap() };
    // static ref MULTI_LINE_RE: Regex = { Regex::new(r#""#).unwrap() };
    // static ref NON_PRINTLN_MAIN_RE: Regex = { Regex::new(r#""#).unwrap() };
    static ref MANIFEST_PATHS: Vec<&'static OsStr> = {
        vec![
            OsStr::from_bytes(b"Cargo.toml"),
            OsStr::from_bytes(b"cargo.toml"),
        ]
    };
}

fn is_bin_non_trivial(path: impl AsRef<Path>) -> anyhow::Result<bool> {
    // We want:
    //
    // (a) any function other than main, or
    // (b) a main that has more than one line, or
    // (c) a main with one line that is not println!

    let mut content = Vec::new();
    File::open(path)?.read_to_end(&mut content)?;

    let tree = rust_parser()?
        .parse(&content, None)
        .ok_or_else(|| anyhow::anyhow!("parsing failed"))?;
    let root = tree.root_node();
    for child in root
        .children(&mut root.walk())
        .filter(|node| node.kind() == "function_item")
    {
        let mut cursor = child.walk();
        let name = child
            .children_by_field_name("name", &mut cursor)
            .next()
            .ok_or_else(|| anyhow::anyhow!("function item does not have a name: {child:?}"))?
            .utf8_text(&content)?;

        if name != "main" {
            return Ok(true);
        }

        let body = child
            .children_by_field_name("body", &mut cursor)
            .next()
            .ok_or_else(|| anyhow::anyhow!("function item does not have a body: {child:?}"))?
            .utf8_text(&content)?;

        if body.chars().filter(|c| *c == '\n').count() > 2 {
            return Ok(true);
        }

        if !body.contains("println!") {
            return Ok(true);
        }
    }

    Ok(false)
}

fn is_lib_non_trivial(path: impl AsRef<Path>) -> anyhow::Result<bool> {
    // We want:
    //
    // (a) literally any pub fn, enum, struct, or type.

    let mut content = Vec::new();
    File::open(path)?.read_to_end(&mut content)?;

    let tree = rust_parser()?
        .parse(&content, None)
        .ok_or_else(|| anyhow::anyhow!("parsing failed"))?;
    let root = tree.root_node();
    for child in root.children(&mut root.walk()).filter(|node| {
        matches!(
            node.kind(),
            "function_item"
                | "const_item"
                | "enum_item"
                | "foreign_mod_item"
                | "mod_item"
                | "struct_item"
                | "static_item"
                | "trait_item"
                | "type_item"
                | "use_declaration"
        )
    }) {
        // Try to find a visibility modifier.
        let mut cursor = child.walk();
        if let Some(vis) = child
            .children(&mut cursor)
            .find(|node| node.kind() == "visibility_modifier")
        {
            if vis.utf8_text(&content)? == "pub" {
                return Ok(true);
            }
        };
    }

    Ok(false)
}

fn is_manifest(path: &OsStr) -> bool {
    MANIFEST_PATHS.contains(&path)
}

fn rust_parser() -> anyhow::Result<tree_sitter::Parser> {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(tree_sitter_rust::language()).unwrap();
    Ok(parser)
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Manifest {
    package: Package,
    lib: Option<Lib>,
    bins: Option<Vec<Bin>>,
}

impl Ord for Manifest {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.package.cmp(&other.package)
    }
}

impl PartialOrd for Manifest {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct Package {
    name: String,
    version: Version,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Lib {
    path: Option<PathBuf>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct Bin {
    path: Option<PathBuf>,
}
