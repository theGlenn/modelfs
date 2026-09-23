//! Printing into a pipe whose reader already quit (`modeld ls | head -1`).

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

#[test]
fn command_ends_quietly_when_stdout_reader_is_gone() {
    let home = tempfile::tempdir().expect("create temp home");
    let (reader, writer) = std::io::pipe().expect("create pipe");
    drop(reader);

    let output = Command::new(env!("CARGO_BIN_EXE_modeld"))
        .arg("doctor")
        .env("HOME", home.path())
        .env_remove("HF_HOME")
        .env_remove("HF_HUB_CACHE")
        .env_remove("OLLAMA_MODELS")
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .output()
        .expect("run modeld");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "modeld panicked: {stderr}");
    assert_eq!(output.status.signal(), Some(libc::SIGPIPE));
}
