//! Recursive directory search (`-R`): turn the command-line inputs into the
//! list of files to search.
//!
//! Directories are walked depth-first with entries sorted by name, so the
//! output order is stable. Hidden entries (`.git`, `.cache`, ...) and the
//! usual build/dependency folders are skipped unless asked for. Symbolic links
//! to directories are not followed, so link loops cannot hang the search.

use crate::matcher::{Matcher, Mode};
use std::fs;
use std::path::{Path, PathBuf};

/// Folders skipped by default (on top of hidden ones).
const SKIP_DIRS: &[&str] = &["node_modules", "target", "__pycache__", "bower_components"];

/// File extensions searched by default in XML mode.
pub const XML_EXTENSIONS: &[&str] = &[
    "xml", "xsd", "xsl", "xslt", "svg", "xhtml", "rss", "atom", "wsdl", "plist", "kml", "gpx", "xaml", "csproj",
    "vbproj", "fsproj", "props", "targets", "nuspec", "resx", "config", "pom", "dita", "docbook", "fodt",
];

/// File extensions searched by default in HTML mode.
pub const HTML_EXTENSIONS: &[&str] = &["html", "htm", "xhtml", "shtml"];

pub struct WalkOptions {
    /// File-name globs to search; empty means "the default extensions".
    pub include: Vec<String>,
    /// File-name globs to skip.
    pub exclude: Vec<String>,
    /// Directory-name globs to skip.
    pub exclude_dir: Vec<String>,
    /// Also search hidden files and folders and the default-skipped folders.
    pub hidden: bool,
    /// Extensions used when `include` is empty.
    pub default_extensions: &'static [&'static str],
}

fn globs(patterns: &[String]) -> Vec<Matcher> {
    patterns.iter().map(|p| Matcher::with_fuzzy(p, Mode::Glob, false, None)).collect()
}

/// Expand `inputs`: directories are walked recursively, everything else
/// (files, `-`) is kept as given. Unreadable directories are reported in `errors`.
pub fn expand(inputs: &[String], opts: &WalkOptions, errors: &mut Vec<String>) -> Vec<String> {
    let include = globs(&opts.include);
    let exclude = globs(&opts.exclude);
    let exclude_dir = globs(&opts.exclude_dir);
    let wanted = |name: &str| {
        if exclude.iter().any(|g| g.is_match(name)) {
            return false;
        }
        if include.is_empty() {
            let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
            opts.default_extensions.iter().any(|e| e.eq_ignore_ascii_case(ext))
        } else {
            include.iter().any(|g| g.is_match(name))
        }
    };
    let skip_dir = |name: &str| {
        exclude_dir.iter().any(|g| g.is_match(name))
            || (!opts.hidden && (name.starts_with('.') || SKIP_DIRS.contains(&name)))
    };

    let mut out = Vec::new();
    for input in inputs {
        let path = Path::new(input);
        if input == "-" || !path.is_dir() {
            out.push(input.clone());
            continue;
        }
        let mut stack: Vec<PathBuf> = vec![path.to_path_buf()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    errors.push(format!("{}: cannot read directory: {e}", dir.display()));
                    continue;
                }
            };
            let mut files = Vec::new();
            let mut dirs = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                let Ok(kind) = entry.file_type() else { continue };
                if kind.is_dir() {
                    if !skip_dir(&name) {
                        dirs.push(entry.path());
                    }
                } else if (kind.is_file() || (kind.is_symlink() && entry.path().is_file()))
                    && (opts.hidden || !name.starts_with('.'))
                    && wanted(&name)
                {
                    files.push(entry.path());
                }
            }
            files.sort();
            dirs.sort();
            out.extend(files.into_iter().map(|p| p.to_string_lossy().into_owned()));
            // Reverse so the alphabetically first directory is walked first.
            stack.extend(dirs.into_iter().rev());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> WalkOptions {
        WalkOptions {
            include: vec![],
            exclude: vec![],
            exclude_dir: vec![],
            hidden: false,
            default_extensions: XML_EXTENSIONS,
        }
    }

    #[test]
    fn walks_sorted_and_filters() {
        let root = std::env::temp_dir().join(format!("rxgrep-walk-{}", std::process::id()));
        let mk = |p: &str, body: &str| {
            let f = root.join(p);
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, body).unwrap();
        };
        mk("b.xml", "<a/>");
        mk("a.svg", "<svg/>");
        mk("notes.txt", "x");
        mk("sub/c.XML", "<c/>");
        mk("sub/deeper/d.xml", "<d/>");
        mk(".git/config.xml", "<g/>");
        mk("node_modules/m.xml", "<m/>");
        mk(".hidden.xml", "<h/>");
        let r = root.to_string_lossy().into_owned();
        let rel = |v: Vec<String>| -> Vec<String> {
            v.iter().map(|p| p.strip_prefix(&r).unwrap().trim_start_matches('/').to_string()).collect()
        };
        let mut errs = Vec::new();

        let got = rel(expand(std::slice::from_ref(&r), &opts(), &mut errs));
        assert_eq!(got, ["a.svg", "b.xml", "sub/c.XML", "sub/deeper/d.xml"]);

        let hidden = WalkOptions { hidden: true, ..opts() };
        let got = rel(expand(std::slice::from_ref(&r), &hidden, &mut errs));
        assert!(got.contains(&".git/config.xml".to_string()) && got.contains(&".hidden.xml".to_string()));
        assert!(got.contains(&"node_modules/m.xml".to_string()));

        let inc = WalkOptions { include: vec!["*.txt".into()], ..opts() };
        assert_eq!(rel(expand(std::slice::from_ref(&r), &inc, &mut errs)), ["notes.txt"]);

        let exc = WalkOptions { exclude: vec!["b.*".into()], exclude_dir: vec!["deep*".into()], ..opts() };
        assert_eq!(rel(expand(std::slice::from_ref(&r), &exc, &mut errs)), ["a.svg", "sub/c.XML"]);

        // explicit files are always kept, whatever their extension
        let file = root.join("notes.txt").to_string_lossy().into_owned();
        assert_eq!(expand(&[file.clone(), "-".into()], &opts(), &mut errs), [file, "-".to_string()]);
        assert!(errs.is_empty());
        fs::remove_dir_all(&root).unwrap();
    }
}
