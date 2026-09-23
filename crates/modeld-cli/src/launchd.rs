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
//! `OLLAMA_MODELS`) are written into the agent explicitly.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// launchd label, also the plist file stem.
pub const LABEL: &str = "dev.modeld.daemon";

/// Variables that move provider caches; copied into the agent when set.
const MODEL_ENV_VARS: [&str; 3] = ["HF_HOME", "HF_HUB_CACHE", "OLLAMA_MODELS"];

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
    fn pid_is_read_from_launchctl_print() {
        let running =
            "gui/501/dev.modeld.daemon = {\n\tactive count = 1\n\tstate = running\n\tpid = 4242\n}";
        let stopped = "gui/501/dev.modeld.daemon = {\n\tstate = not running\n}";

        assert_eq!(parse_pid(running), Some(4242));
        assert_eq!(parse_pid(stopped), None);
    }
}
