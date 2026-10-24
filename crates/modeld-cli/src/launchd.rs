//! Start-at-login for the daemon: a per-user launchd agent.
//!
//! `install` copies the running binary to `~/.modeld/bin/modeld` (a build
//! directory can vanish or be rebuilt under the agent), writes
//! `~/Library/LaunchAgents/dev.modeld.daemon.plist`, and (re)loads it with
//! `launchctl bootstrap gui/<uid>`. The agent runs `modeld daemon` at login,
//! restarts it only after a failure exit, runs it as a Background process
//! (throttled CPU and I/O), and appends its log to `~/.modeld/daemon.log`.
//!
//! launchd does not inherit the shell environment, so `HOME` and the
//! model-location variables set at install time (`HF_HOME`, `HF_HUB_CACHE`,
//! `OLLAMA_MODELS`) are written into the agent explicitly. [`installed_agent`]
//! reads them back, so other callers (the Mac app) can scan the same folders.

use serde::Deserialize;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// launchd label, also the plist file stem.
pub const LABEL: &str = "dev.modeld.daemon";

/// Variables that move provider caches; copied into the agent when set.
const MODEL_ENV_VARS: [&str; 3] = ["HF_HOME", "HF_HUB_CACHE", "OLLAMA_MODELS"];

/// How many log lines `daemon status` shows.
pub const RECENT_LOG_LINES: usize = 5;

/// Seconds launchd waits after SIGTERM before SIGKILL: long enough for a pass
/// hashing a large new download to finish and journal cleanly.
const EXIT_TIMEOUT_SECS: u32 = 120;

/// Everything the agent definition depends on.
#[derive(Debug)]
pub struct Agent {
    pub binary: PathBuf,
    /// Arguments after the binary, starting with `daemon`.
    pub arguments: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub log: PathBuf,
}

/// Where the agent's pieces live for a given home directory.
#[derive(Debug)]
pub struct AgentPaths {
    pub plist: PathBuf,
    pub binary: PathBuf,
    pub log: PathBuf,
}

impl AgentPaths {
    pub fn for_home(home: &Path) -> Self {
        Self {
            plist: home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist")),
            binary: home.join(".modeld/bin/modeld"),
            log: home.join(".modeld/daemon.log"),
        }
    }
}

/// What `launchctl` knows about the agent right now.
#[derive(Debug)]
pub struct AgentStatus {
    pub installed: bool,
    pub loaded: bool,
    pub pid: Option<u32>,
}

/// The agent's lifecycle state, from not installed to running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentState {
    Running {
        pid: u32,
    },
    /// Loaded into launchd, but no process right now.
    Loaded,
    /// The plist exists but is not loaded; it starts at the next login.
    Installed,
    NotInstalled,
}

impl AgentStatus {
    /// Collapses the status into one lifecycle state.
    #[must_use]
    pub fn state(&self) -> AgentState {
        match (self.installed, self.loaded, self.pid) {
            (_, true, Some(pid)) => AgentState::Running { pid },
            (_, true, None) => AgentState::Loaded,
            (true, false, _) => AgentState::Installed,
            (false, false, _) => AgentState::NotInstalled,
        }
    }
}

/// What the installed agent's plist says it runs, and with which variables.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstalledAgent {
    /// Arguments after the binary, starting with `daemon`.
    pub arguments: Vec<String>,
    pub environment: BTreeMap<String, String>,
}

/// The last pass the daemon logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastPass<'a> {
    /// The log line's RFC 3339 UTC timestamp, such as `2026-09-23T21:45:21Z`.
    pub at: &'a str,
    /// The logged outcome, such as `pass: nothing new`.
    pub summary: &'a str,
    /// The pass could not run (`pass failed: ...`).
    pub failed: bool,
}

/// Installs (or updates) the agent for `home` and starts it now.
///
/// # Errors
/// Copying the binary, writing the plist, or a `launchctl` call failed.
pub fn install(home: &Path, arguments: Vec<String>) -> Result<AgentPaths, String> {
    let paths = AgentPaths::for_home(home);
    let current =
        std::env::current_exe().map_err(|error| format!("cannot locate modeld: {error}"))?;
    copy_binary(&current, &paths.binary)?;
    let agent = Agent {
        binary: paths.binary.clone(),
        arguments,
        environment: agent_environment(home),
        log: paths.log.clone(),
    };
    write_atomically(&paths.plist, render_plist(&agent).as_bytes())?;
    stop_agent()?;
    launchctl(&["bootstrap", &domain(), &paths.plist.to_string_lossy()])?;
    Ok(paths)
}

/// Stops the agent and removes its plist; returns whether it was installed.
///
/// The installed binary and the log stay, so history remains readable.
///
/// # Errors
/// The agent could not be stopped or its plist not removed.
pub fn uninstall(home: &Path) -> Result<bool, String> {
    stop_agent()?;
    match std::fs::remove_file(AgentPaths::for_home(home).plist) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("cannot remove agent plist: {error}")),
    }
}

/// Reports whether the agent is installed, loaded, and running.
pub fn status(home: &Path) -> AgentStatus {
    let printed = Command::new("/bin/launchctl")
        .args(["print", &service()])
        .output()
        .ok()
        .filter(|output| output.status.success());
    AgentStatus {
        installed: AgentPaths::for_home(home).plist.exists(),
        loaded: printed.is_some(),
        pid: printed.and_then(|output| parse_pid(&String::from_utf8_lossy(&output.stdout))),
    }
}

/// Reads the installed agent's arguments and environment from its plist.
///
/// Returns `None` when no agent is installed or its plist cannot be read.
#[must_use]
pub fn installed_agent(home: &Path) -> Option<InstalledAgent> {
    read_agent_definition(&AgentPaths::for_home(home).plist)
}

/// Whether the agent's binary copy has the same bytes as the running modeld.
///
/// `None` when either binary cannot be read, as when no agent was installed.
/// A mismatch means `install` would update the agent.
#[must_use]
pub fn binary_matches_current(home: &Path) -> Option<bool> {
    let current = std::env::current_exe().ok()?;
    files_match(&AgentPaths::for_home(home).binary, &current)
}

/// The last `pass: ...` or `pass failed: ...` line in the daemon log.
#[must_use]
pub fn last_pass(log: &str) -> Option<LastPass<'_>> {
    log.lines().rev().find_map(|line| {
        let (at, message) = line.split_once(' ')?;
        let failed = message.starts_with("pass failed: ");
        (failed || message.starts_with("pass: ")).then_some(LastPass {
            at,
            summary: message,
            failed,
        })
    })
}

/// The last `count` lines of the daemon log.
#[must_use]
pub fn recent_lines(log: &str, count: usize) -> Vec<&str> {
    let lines: Vec<&str> = log.lines().collect();
    lines[lines.len().saturating_sub(count)..].to_vec()
}

/// Converts a plist to JSON with `plutil` and reads the agent's fields.
fn read_agent_definition(plist: &Path) -> Option<InstalledAgent> {
    /// The plist keys `render_plist` writes that callers read back.
    #[derive(Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Definition {
        #[serde(default)]
        program_arguments: Vec<String>,
        #[serde(default)]
        environment_variables: BTreeMap<String, String>,
    }

    if !plist.is_file() {
        return None;
    }
    let output = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-"])
        .arg(plist)
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let definition: Definition = serde_json::from_slice(&output.stdout).ok()?;
    Some(InstalledAgent {
        arguments: definition.program_arguments.into_iter().skip(1).collect(),
        environment: definition.environment_variables,
    })
}

/// Whether two files hold the same bytes; `None` if either is unreadable.
fn files_match(a: &Path, b: &Path) -> Option<bool> {
    let (a_len, b_len) = (
        std::fs::metadata(a).ok()?.len(),
        std::fs::metadata(b).ok()?.len(),
    );
    if a_len != b_len {
        return Some(false);
    }
    let a_digest = modeld_core::Digest::sha256_file(a).ok()?;
    let b_digest = modeld_core::Digest::sha256_file(b).ok()?;
    Some(a_digest == b_digest)
}

/// Unloads the agent if loaded, waiting until launchd has let it go.
///
/// A running daemon gets SIGTERM and finishes its pass first, so this can
/// take up to the agent's exit timeout.
fn stop_agent() -> Result<(), String> {
    // Fails harmlessly when the agent is not loaded.
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &service()])
        .output();
    let deadline = Instant::now() + Duration::from_secs(u64::from(EXIT_TIMEOUT_SECS) + 10);
    while Instant::now() < deadline {
        let loaded = Command::new("/bin/launchctl")
            .args(["print", &service()])
            .output()
            .is_ok_and(|output| output.status.success());
        if !loaded {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("the running agent did not stop in time".to_string())
}

fn launchctl(arguments: &[&str]) -> Result<Output, String> {
    let output = Command::new("/bin/launchctl")
        .args(arguments)
        .output()
        .map_err(|error| format!("cannot run launchctl: {error}"))?;
    if output.status.success() {
        return Ok(output);
    }
    Err(format!(
        "launchctl {} failed: {}",
        arguments.join(" "),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// The logged-in user's launchd domain, `gui/<uid>`.
fn domain() -> String {
    // SAFETY: getuid has no preconditions, cannot fail, and touches no memory
    // owned by this program.
    let uid = unsafe { libc::getuid() };
    format!("gui/{uid}")
}

fn service() -> String {
    format!("{}/{LABEL}", domain())
}

/// `HOME` plus any model-location variables set in this shell.
fn agent_environment(home: &Path) -> Vec<(String, String)> {
    let mut environment = vec![("HOME".to_string(), home.to_string_lossy().into_owned())];
    environment.extend(
        MODEL_ENV_VARS
            .iter()
            .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?))),
    );
    environment
}

/// Copies the running binary to its stable install path, atomically.
fn copy_binary(current: &Path, installed: &Path) -> Result<(), String> {
    let same = installed.canonicalize().is_ok_and(|installed| {
        current
            .canonicalize()
            .is_ok_and(|current| current == installed)
    });
    if same {
        return Ok(());
    }
    let bytes = std::fs::read(current).map_err(|error| format!("cannot read modeld: {error}"))?;
    write_atomically(installed, &bytes)?;
    std::fs::set_permissions(
        installed,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .map_err(|error| format!("cannot make modeld executable: {error}"))
}

/// Writes `bytes` to `path` via a sibling temp file and a rename.
///
/// Replacing a running binary this way is safe: the old process keeps its inode.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path.parent().ok_or("path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    let temp = path.with_extension(format!("modeld-install-{}", std::process::id()));
    std::fs::write(&temp, bytes)
        .and_then(|()| std::fs::rename(&temp, path))
        .map_err(|error| {
            let _ = std::fs::remove_file(&temp);
            format!("cannot write {}: {error}", path.display())
        })
}

/// Renders the agent as a launchd property list.
pub fn render_plist(agent: &Agent) -> String {
    let string = |value: &str| format!("<string>{}</string>", xml_escape(value));
    let mut program = String::new();
    let binary = agent.binary.to_string_lossy();
    for argument in
        std::iter::once(binary.as_ref()).chain(agent.arguments.iter().map(String::as_str))
    {
        let _ = writeln!(program, "\t\t{}", string(argument));
    }
    let mut environment = String::new();
    for (key, value) in &agent.environment {
        let _ = writeln!(
            environment,
            "\t\t<key>{}</key>\n\t\t{}",
            xml_escape(key),
            string(value)
        );
    }
    let log = string(&agent.log.to_string_lossy());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	{label}
	<key>ProgramArguments</key>
	<array>
{program}	</array>
	<key>EnvironmentVariables</key>
	<dict>
{environment}	</dict>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Background</string>
	<key>ExitTimeOut</key>
	<integer>{EXIT_TIMEOUT_SECS}</integer>
	<key>StandardOutPath</key>
	{log}
	<key>StandardErrorPath</key>
	{log}
</dict>
</plist>
"#,
        label = string(LABEL),
    )
}

/// `daemon` plus the flags that differ from their defaults.
pub fn daemon_arguments(dry_run: bool, min_size: u64) -> Vec<String> {
    let mut arguments = vec!["daemon".to_string()];
    if dry_run {
        arguments.push("--dry-run".to_string());
    }
    if min_size != modeld_providers::scan::DEFAULT_MIN_SIZE {
        arguments.extend(["--min-size".to_string(), min_size.to_string()]);
    }
    arguments
}

/// Extracts the running process id from `launchctl print` output.
fn parse_pid(print_output: &str) -> Option<u32> {
    print_output
        .lines()
        .find_map(|line| line.trim().strip_prefix("pid = "))
        .and_then(|pid| pid.trim().parse().ok())
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent() -> Agent {
        Agent {
            binary: PathBuf::from("/Users/me/.modeld/bin/modeld"),
            arguments: vec!["daemon".to_string()],
            environment: vec![
                ("HOME".to_string(), "/Users/me".to_string()),
                (
                    "HF_HOME".to_string(),
                    "/Volumes/models & more/hf".to_string(),
                ),
            ],
            log: PathBuf::from("/Users/me/.modeld/daemon.log"),
        }
    }

    #[test]
    fn plist_runs_the_installed_daemon_at_login_in_the_background() {
        let plist = render_plist(&agent());

        assert!(plist.contains("<string>dev.modeld.daemon</string>"));
        assert!(plist.contains(
            "<array>\n\t\t<string>/Users/me/.modeld/bin/modeld</string>\n\t\t<string>daemon</string>\n\t</array>"
        ));
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(plist.contains("<key>SuccessfulExit</key>\n\t\t<false/>"));
        assert!(plist.contains("<string>Background</string>"));
        assert!(plist.contains(
            "<key>StandardErrorPath</key>\n\t<string>/Users/me/.modeld/daemon.log</string>"
        ));
    }

    #[test]
    fn plist_escapes_environment_values() {
        let plist = render_plist(&agent());

        assert!(plist.contains("<string>/Volumes/models &amp; more/hf</string>"));
    }

    #[test]
    fn plist_is_valid_for_plutil() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("agent.plist");
        std::fs::write(&path, render_plist(&agent())).expect("write plist");

        let lint = std::process::Command::new("/usr/bin/plutil")
            .arg("-lint")
            .arg(&path)
            .output()
            .expect("run plutil");

        assert!(
            lint.status.success(),
            "{}",
            String::from_utf8_lossy(&lint.stdout)
        );
    }

    #[test]
    fn only_non_default_flags_reach_the_agent() {
        let default = modeld_providers::scan::DEFAULT_MIN_SIZE;

        assert_eq!(daemon_arguments(false, default), ["daemon"]);
        assert_eq!(
            daemon_arguments(true, 42),
            ["daemon", "--dry-run", "--min-size", "42"]
        );
    }

    #[test]
    fn installed_definition_reads_back_what_install_wrote() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("agent.plist");
        std::fs::write(&path, render_plist(&agent())).expect("write plist");

        let installed = read_agent_definition(&path).expect("readable plist");

        assert_eq!(installed.arguments, ["daemon"]);
        assert_eq!(
            installed.environment.get("HF_HOME").map(String::as_str),
            Some("/Volumes/models & more/hf")
        );
        assert_eq!(
            read_agent_definition(&dir.path().join("missing.plist")),
            None
        );
    }

    #[test]
    fn binaries_match_only_with_identical_bytes() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let write = |name: &str, bytes: &[u8]| {
            let path = dir.path().join(name);
            std::fs::write(&path, bytes).expect("write binary");
            path
        };
        let (a, same, other) = (write("a", b"v1"), write("b", b"v1"), write("c", b"v2"));

        assert_eq!(files_match(&a, &same), Some(true));
        assert_eq!(files_match(&a, &other), Some(false));
        assert_eq!(files_match(&a, &dir.path().join("missing")), None);
    }

    #[test]
    fn last_pass_is_the_latest_pass_line() {
        let log = "2026-09-23T21:40:02Z daemon started\n\
                   2026-09-23T21:40:03Z pass: nothing new\n\
                   2026-09-23T21:45:03Z hashing /m/a.gguf (2.6 GB)\n\
                   2026-09-23T21:45:21Z pass: 1 clone(s) made (2.6 GB freed)\n\
                   2026-09-23T21:46:00Z watching /m\n";

        assert_eq!(
            last_pass(log),
            Some(LastPass {
                at: "2026-09-23T21:45:21Z",
                summary: "pass: 1 clone(s) made (2.6 GB freed)",
                failed: false,
            })
        );
        assert_eq!(last_pass("2026-09-23T21:40:02Z daemon started\n"), None);
    }

    #[test]
    fn failed_pass_counts_as_the_last_pass() {
        let log = "2026-09-23T21:40:03Z pass: nothing new\n\
                   2026-09-23T21:41:03Z pass failed: registry error: locked\n";

        let last = last_pass(log).expect("pass line");

        assert!(last.failed);
        assert_eq!(last.at, "2026-09-23T21:41:03Z");
    }

    #[test]
    fn recent_lines_keep_the_tail() {
        assert_eq!(recent_lines("a\nb\nc\n", 2), ["b", "c"]);
        assert_eq!(recent_lines("a\n", 5), ["a"]);
    }

    #[test]
    fn agent_state_collapses_the_status() {
        let status = |installed, loaded, pid| AgentStatus {
            installed,
            loaded,
            pid,
        };

        assert_eq!(
            status(true, true, Some(7)).state(),
            AgentState::Running { pid: 7 }
        );
        assert_eq!(status(true, true, None).state(), AgentState::Loaded);
        assert_eq!(status(true, false, None).state(), AgentState::Installed);
        assert_eq!(status(false, false, None).state(), AgentState::NotInstalled);
    }

    #[test]
    fn pid_is_read_from_launchctl_print() {
        let running =
            "gui/501/dev.modeld.daemon = {\n\tactive count = 1\n\tstate = running\n\tpid = 4242\n}";
        let stopped = "gui/501/dev.modeld.daemon = {\n\tstate = not running\n}";

        assert_eq!(parse_pid(running), Some(4242));
        assert_eq!(parse_pid(stopped), None);
    }
}
