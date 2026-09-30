//! `cfgprism convert` / `cfgprism check`.
//!
//! - `convert <in> [-f FROM] -t TO [-o OUT] [--strict]`: convert between
//!   formats (`-`/omitted input = stdin; format guessed from extension).
//! - `check <A> <B>…`: verify files in (possibly different) formats carry
//!   the same data; exits 1 on mismatch (for CI sync checks).
//! - `--strict`: any warning becomes a hard error (non-zero exit).
//! - Warnings always go to stderr; converted text goes to stdout/file.

use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cfgprism_core::{Error, Options};
use cfgprism_formats::{all_formats, convert_text, detect_format, supported_names};
use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "cfgprism",
    version,
    about = "config converter with explicit warnings"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Convert a config file between formats.
    Convert(ConvertArgs),
    /// Check that files in (possibly different) formats carry the same data.
    Check(CheckArgs),
    /// List supported formats.
    Formats,
}

/// CLI args for `cfgprism convert`.
#[derive(Debug, Parser)]
struct ConvertArgs {
    /// Input file (`-` or omitted = stdin).
    #[arg(value_name = "INPUT")]
    input: Option<PathBuf>,

    /// Source format (default: guess from input extension).
    #[arg(short = 'f', long = "from", value_name = "FROM")]
    from: Option<String>,

    /// Target format (required).
    #[arg(short = 't', long = "to", value_name = "TO")]
    to: String,

    /// Output file (default: stdout).
    #[arg(short = 'o', long = "output", value_name = "OUT")]
    output: Option<PathBuf>,

    /// Turn every warning into a hard error.
    #[arg(long = "strict")]
    strict: bool,

    /// Spaces per indent level.
    #[arg(long = "indent", default_value_t = 2)]
    indent: usize,
}

/// CLI args for `cfgprism check`.
#[derive(Debug, Parser)]
struct CheckArgs {
    /// Files to compare (at least two). Each file's format is guessed from
    /// its extension unless overridden with `--as`.
    #[arg(value_name = "FILE", required = true)]
    files: Vec<PathBuf>,

    /// Explicit `path,format` overrides (repeatable), e.g.
    /// `--as config.txt,json`.
    #[arg(long = "as", value_name = "PATH,FORMAT")]
    as_format: Vec<String>,

    /// Compare ignoring key order (for targets that reorder, e.g. TOML).
    /// Default is order-sensitive.
    #[arg(long = "unordered")]
    unordered: bool,
}

fn run_check(a: CheckArgs) -> Result<(), Error> {
    if a.files.len() < 2 {
        return Err(Error::io("check needs at least two files"));
    }
    let mut overrides: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for item in &a.as_format {
        let (path, fmt) = item
            .split_once(',')
            .ok_or_else(|| Error::io(format!("bad --as value '{item}' (want PATH,FORMAT)")))?;
        overrides.insert(path.to_string(), fmt.to_string());
    }
    let registry = all_formats();
    let mut docs: Vec<(String, cfgprism_core::Doc)> = Vec::new();
    for file in &a.files {
        let display = file.display().to_string();
        let fmt_name = overrides.get(&display).cloned().unwrap_or_else(|| {
            detect_format(&display)
                .map(str::to_string)
                .unwrap_or_default()
        });
        if fmt_name.is_empty() {
            return Err(Error::unsupported_format(format!(
                "cannot detect format of '{display}'; pass --as {display},FORMAT"
            )));
        }
        let fmt = registry.find(&fmt_name).ok_or_else(|| {
            Error::unsupported_format(format!(
                "unsupported format '{fmt_name}' (supported: {})",
                supported_names().join(", ")
            ))
        })?;
        let src = read_file(file)?;
        let doc = fmt.parse(&src)?;
        docs.push((display, doc));
    }
    let (first_name, first) = &docs[0];
    let mut failed = false;
    for (name, doc) in docs.iter().skip(1) {
        let equal = if a.unordered {
            cfgprism_core::values_equal_unordered(&first.root, &doc.root)
        } else {
            cfgprism_core::values_equal(&first.root, &doc.root)
        };
        if !equal {
            failed = true;
            match first_difference(&first.root, &doc.root, "") {
                Some(diff) => eprintln!("cfgprism: mismatch: {first_name} vs {name} at {diff}"),
                None => eprintln!("cfgprism: mismatch: {first_name} vs {name}"),
            }
        }
    }
    if failed {
        Err(Error::new(
            cfgprism_core::ErrorKind::Other("mismatch".to_string()),
            1,
            1,
            "checked files differ",
        ))
    } else {
        println!("cfgprism: {} file(s) in sync", docs.len());
        Ok(())
    }
}

/// First differing path between two IR trees (`None` when only trivia
/// differs, which still counts as a mismatch here only if values differ —
/// actually values_equal already covers it; this reports *where*).
fn first_difference(
    a: &cfgprism_core::Node,
    b: &cfgprism_core::Node,
    path: &str,
) -> Option<String> {
    use cfgprism_core::Value;
    let here = if path.is_empty() {
        "<root>".to_string()
    } else {
        path.to_string()
    };
    match (&a.value, &b.value) {
        (Value::Map(x), Value::Map(y)) => {
            if x.len() != y.len() {
                return Some(format!("{here}: {} vs {} keys", x.len(), y.len()));
            }
            for (ex, ey) in x.iter().zip(y.iter()) {
                if ex.key.text != ey.key.text {
                    return Some(format!(
                        "{here}: key '{}' vs '{}'",
                        ex.key.text, ey.key.text
                    ));
                }
                let sub = if here == "<root>" {
                    ex.key.text.clone()
                } else {
                    format!("{here}.{}", ex.key.text)
                };
                if !cfgprism_core::values_equal(&ex.value, &ey.value) {
                    return first_difference(&ex.value, &ey.value, &sub).or(Some(sub));
                }
            }
            None
        }
        (Value::Array(x), Value::Array(y)) => {
            if x.len() != y.len() {
                return Some(format!(
                    "{here}: [{x_len} vs {y_len} items]",
                    x_len = x.len(),
                    y_len = y.len()
                ));
            }
            for (i, (px, py)) in x.iter().zip(y.iter()).enumerate() {
                if !cfgprism_core::values_equal(px, py) {
                    return first_difference(px, py, &format!("{here}[{i}]"))
                        .or(Some(format!("{here}[{i}]")));
                }
            }
            None
        }
        _ => {
            if cfgprism_core::values_equal(a, b) {
                None
            } else {
                Some(format!(
                    "{here}: {} vs {}",
                    a.display_value(),
                    b.display_value()
                ))
            }
        }
    }
}

fn read_file(path: &Path) -> Result<String, Error> {
    std::fs::read_to_string(path)
        .map_err(|e| Error::io(format!("cannot read {}: {e}", path.display())))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("cfgprism: error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), Error> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Convert(a) => run_convert(a),
        Cmd::Check(a) => run_check(a),
        Cmd::Formats => {
            for name in supported_names() {
                println!("{name}");
            }
            Ok(())
        }
    }
}

fn run_convert(a: ConvertArgs) -> Result<(), Error> {
    let src = read_input(a.input.as_deref())?;

    let from_name = match a.from {
        Some(n) => n,
        None => match a.input.as_deref() {
            Some(p) if p.as_os_str() != "-" => {
                let s = p.to_string_lossy();
                detect_format(&s).map_or_else(
                    || {
                        Err(Error::unsupported_format(
                            "cannot detect source format from extension; pass -f/--from",
                        ))
                    },
                    |n| Ok(n.to_string()),
                )?
            }
            _ => {
                return Err(Error::unsupported_format(
                    "cannot detect source format from stdin; pass -f/--from",
                ));
            }
        },
    };

    let opt = Options {
        indent: a.indent.max(1),
    };
    // convert_text validates format names and reports supported lists.
    let out = convert_text(&from_name, &a.to, &src, &opt).map_err(|e| {
        if matches!(e.kind, cfgprism_core::ErrorKind::UnsupportedFormat) {
            Error::unsupported_format(format!("{e} (supported: {})", supported_names().join(", ")))
        } else {
            e
        }
    })?;

    for w in &out.warnings {
        eprintln!("cfgprism: warning: {w}");
    }
    if a.strict && !out.warnings.is_empty() {
        return Err(Error::new(
            cfgprism_core::ErrorKind::StrictWarnings,
            1,
            1,
            format!("{} warning(s) escalated by --strict", out.warnings.len()),
        ));
    }

    match a.output {
        Some(p) => std::fs::write(&p, out.text)
            .map_err(|e| Error::io(format!("cannot write {}: {e}", p.display())))?,
        None => {
            print!("{}", out.text);
        }
    }
    Ok(())
}

fn read_input(path: Option<&std::path::Path>) -> Result<String, Error> {
    let use_stdin = path.is_none_or(|p| p.as_os_str() == "-");
    if use_stdin {
        if std::io::stdin().is_terminal() {
            return Err(Error::io(
                "no input file and stdin is a terminal; pass a file or pipe stdin",
            ));
        }
        let mut buf = String::new();
        std::io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| Error::io(format!("cannot read stdin: {e}")))?;
        return Ok(buf);
    }
    match path {
        Some(p) => std::fs::read_to_string(p)
            .map_err(|e| Error::io(format!("cannot read {}: {e}", p.display()))),
        None => Err(Error::io("no input given")),
    }
}
