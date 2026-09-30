//! `cfgprism convert <in> [-f FROM] -t TO [-o OUT] [--strict]`
//!
//! - `<in>`: file path, `-`, or omitted (= stdin).
//! - `-f/--from`: source format; guessed from the input extension otherwise.
//! - `-t/--to`: target format (required).
//! - `-o/--output`: output file; stdout otherwise.
//! - `--strict`: any warning becomes a hard error (non-zero exit).
//! - Warnings always go to stderr; converted text goes to stdout/file.

use std::io::{IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use cfgprism_core::{convert, Error, Options};
use cfgprism_formats::{all_formats, detect_format, supported_names};
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
        Cmd::Formats => {
            for name in supported_names() {
                println!("{name}");
            }
            Ok(())
        }
    }
}

fn run_convert(a: ConvertArgs) -> Result<(), Error> {
    let registry = all_formats();

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

    let from = registry.find(&from_name).ok_or_else(|| {
        Error::unsupported_format(format!(
            "unsupported source format '{from_name}' (supported: {})",
            supported_names().join(", ")
        ))
    })?;
    let to = registry.find(&a.to).ok_or_else(|| {
        Error::unsupported_format(format!(
            "unsupported target format '{}' (supported: {})",
            a.to,
            supported_names().join(", ")
        ))
    })?;

    let opt = Options {
        indent: a.indent.max(1),
    };
    let out = convert(from, to, &src, &opt)?;

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
