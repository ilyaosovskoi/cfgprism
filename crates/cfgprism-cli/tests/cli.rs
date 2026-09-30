//! CLI integration tests: spawn the built binary, feed stdin/files,
//! assert stdout/stderr/exit codes. No network, no temp crates.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_cfgprism"))
}

fn write_tmp(name: &str, content: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    // Keep the original extension so format sniffing by extension works.
    let filename = match name.rsplit_once('.') {
        Some((stem, ext)) => format!("cfgprism-{stem}-{nanos}.{ext}"),
        None => format!("cfgprism-{name}-{nanos}"),
    };
    let mut p = std::env::temp_dir();
    p.push(filename);
    std::fs::write(&p, content).expect("write tmp");
    p
}

#[test]
fn convert_json_file_to_json_stdout() {
    let input = write_tmp("in.json", r#"{"b": 1, "a": 2}"#);
    let out = Command::new(bin())
        .args(["convert"])
        .arg(&input)
        .args(["-t", "json"])
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    // Verbatim round-trip: bytes (and key order) preserved exactly.
    assert_eq!(stdout, "{\"b\": 1, \"a\": 2}");
    std::fs::remove_file(input).ok();
}

#[test]
fn convert_reads_stdin_with_explicit_from() {
    let mut child = Command::new(bin())
        .args(["convert", "-f", "json", "-t", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(br#"{"x": true}"#)
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).expect("utf8"),
        "{\"x\": true}"
    );
}

#[test]
fn convert_writes_output_file() {
    let input = write_tmp("in.json", r#"{"a": 1}"#);
    let dest = std::env::temp_dir().join(format!(
        "cfgprism-out-{}.json",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    let out = Command::new(bin())
        .args(["convert"])
        .arg(&input)
        .args(["-t", "json", "-o"])
        .arg(&dest)
        .output()
        .expect("spawn");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&dest).expect("read"), "{\"a\": 1}");
    std::fs::remove_file(input).ok();
    std::fs::remove_file(dest).ok();
}

#[test]
fn unknown_target_format_fails() {
    let input = write_tmp("in.json", r#"{"a": 1}"#);
    let out = Command::new(bin())
        .args(["convert"])
        .arg(&input)
        .args(["-t", "yaml"])
        .output()
        .expect("spawn");
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    assert!(stderr.contains("unsupported target format"), "{stderr}");
    std::fs::remove_file(input).ok();
}

#[test]
fn stdin_without_from_fails_with_hint() {
    let mut child = Command::new(bin())
        .args(["convert", "-t", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(br#"{"a": 1}"#)
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert!(!out.status.success());
    let stderr = String::from_utf8(out.stderr).expect("utf8");
    assert!(stderr.contains("-f/--from"), "{stderr}");
}

#[test]
fn formats_subcommand_lists_json() {
    let out = Command::new(bin()).arg("formats").output().expect("spawn");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(stdout.lines().any(|l| l.trim() == "json"), "{stdout}");
}
