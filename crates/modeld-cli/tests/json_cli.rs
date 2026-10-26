//! `--json` end to end: the commands the Mac app runs, against a temp home.

use serde_json::Value;
use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, SystemTime};

const WEIGHTS: &[u8] = b"identical model weights";

/// A home directory with one configured model folder, `~/models`.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("create temp home");
        std::fs::create_dir_all(dir.path().join("models")).expect("create model folder");
        std::fs::create_dir_all(dir.path().join(".modeld")).expect("create store");
        std::fs::write(
            dir.path().join(".modeld/config.toml"),
            "[scan]\nroots = [\"~/models\"]\n",
        )
        .expect("write config");
        Self { dir }
    }

    fn store(&self) -> PathBuf {
        self.dir.path().join(".modeld")
    }

    /// Writes a model file backdated past the five-minute settle window.
    fn settled_model(&self, name: &str) -> PathBuf {
        self.settled_model_of(name, WEIGHTS)
    }

    fn settled_model_of(&self, name: &str, content: &[u8]) -> PathBuf {
        let path = self.dir.path().join("models").join(name);
        std::fs::write(&path, content).expect("write model");
        let old = SystemTime::now() - Duration::from_hours(1);
        std::fs::File::open(&path)
            .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(old)))
            .expect("backdate model");
        path
    }

    fn modeld(&self, arguments: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_modeld"));
        command
            .args(arguments)
            .env("HOME", self.dir.path())
            .env_remove("HF_HOME")
            .env_remove("HF_HUB_CACHE")
            .env_remove("OLLAMA_MODELS");
        command
    }

    /// Runs modeld and decodes every stdout line, which must be JSON.
    fn json(&self, arguments: &[&str]) -> (Vec<Value>, ExitStatus) {
        let output = self.modeld(arguments).output().expect("run modeld");
        let lines = String::from_utf8(output.stdout)
            .expect("UTF-8 output")
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|_| panic!("not JSON: {line}")))
            .collect();
        (lines, output.status)
    }

    fn generation(&self) -> Option<String> {
        std::fs::read_to_string(self.store().join("generation")).ok()
    }

    fn journal_entries(&self) -> usize {
        std::fs::read_to_string(self.store().join("journal.jsonl"))
            .map_or(0, |journal| journal.lines().count())
    }
}

/// Checks the envelope every line shares and returns the final line.
fn last_line(lines: &[Value]) -> &Value {
    for line in lines {
        assert_eq!(line["schema"], 1, "{line}");
        assert!(
            ["event", "result", "error"].contains(&line["type"].as_str().unwrap_or_default()),
            "{line}"
        );
    }
    lines.last().expect("at least one line")
}

#[test]
fn watch_adds_a_folder_and_doctor_picks_it_up_without_restart() {
    let home = Home::new();
    let folder = home.dir.path().join("more models");
    std::fs::create_dir_all(&folder).expect("new folder");
    let path = folder.to_str().expect("path");
    let (lines, status) = home.json(&["watch", path, "--json"]);
    assert!(status.success());
    let result = &last_line(&lines)["result"];
    assert_eq!(
        result["path"].as_str(),
        folder.canonicalize().expect("canonical").to_str()
    );
    assert_eq!(result["added"], true);
    assert_eq!(
        result["config"].as_str(),
        home.store().join("config.toml").to_str()
    );
    let generation = home.generation();
    assert!(generation.is_some());
    let (second, status) = home.json(&["watch", path, "--json"]);
    assert!(status.success());
    assert_eq!(last_line(&second)["result"]["added"], false);
    assert_eq!(home.generation(), generation);
    let (doctor, status) = home.json(&["doctor", "--json"]);
    assert!(status.success());
    assert!(
        last_line(&doctor)["result"]["roots"]
            .as_array()
            .expect("roots")
            .iter()
            .any(|root| root["path"] == result["path"] && root["provider"] == "manual")
    );
}

#[test]
fn watch_reports_invalid_folder_and_config_as_json_errors() {
    let home = Home::new();
    let (missing, status) = home.json(&["watch", "/not-a-modeld-test-folder", "--json"]);
    assert!(!status.success());
    assert_eq!(last_line(&missing)["error"]["code"], "watch_failed");
    std::fs::write(home.store().join("config.toml"), "[broken").expect("config");
    let (broken, status) = home.json(&["watch", home.dir.path().to_str().expect("path"), "--json"]);
    assert!(!status.success());
    assert_eq!(last_line(&broken)["error"]["code"], "watch_failed");
    assert_eq!(
        std::fs::read_to_string(home.store().join("config.toml")).expect("config"),
        "[broken"
    );
}

#[test]
fn xybrid_models_are_detected_and_recorded_as_their_own_provider() {
    let home = Home::new();
    let model = home
        .dir
        .path()
        .join(".xybrid/cache/models/example/universal.xyb");
    std::fs::create_dir_all(model.parent().expect("parent")).expect("folder");
    std::fs::write(&model, WEIGHTS).expect("weights");
    std::fs::File::open(&model)
        .and_then(|file| {
            file.set_times(
                std::fs::FileTimes::new().set_modified(SystemTime::now() - Duration::from_hours(1)),
            )
        })
        .expect("backdate model");
    let (doctor, status) = home.json(&["doctor", "--json"]);
    assert!(status.success());
    assert!(
        last_line(&doctor)["result"]["roots"]
            .as_array()
            .expect("roots")
            .iter()
            .any(|root| root["provider"] == "xybrid")
    );
    let (pass, status) = home.json(&["pass", "--dry-run", "--min-size", "1", "--json"]);
    assert!(status.success(), "{pass:?}");
    let (ls, status) = home.json(&["ls", "--json"]);
    assert!(status.success());
    let models = last_line(&ls)["result"]["models"]
        .as_array()
        .expect("models");
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["format"], "XYB");
    assert_eq!(models[0]["references"][0]["provider"], "xybrid");
}

#[test]
fn watching_xybrid_or_home_does_not_reimport_excluded_runtime_files_as_manual() {
    let home = Home::new();
    let model = home
        .dir
        .path()
        .join(".xybrid/cache/models/example/model.gguf");
    std::fs::create_dir_all(model.parent().expect("parent")).expect("models");
    std::fs::write(&model, WEIGHTS).expect("model");
    for relative in [
        ".xybrid/cache/xcframework/test/model.gguf",
        ".xybrid/traces/test.gguf",
        // These providers have no detected models/hub root at all.
        ".lmstudio/.internal/staging.gguf",
        ".cache/huggingface/xet/chunk.gguf",
    ] {
        let path = home.dir.path().join(relative);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("folder");
        std::fs::write(path, WEIGHTS).expect("runtime file");
    }
    for folder in [
        home.dir.path().join(".xybrid"),
        home.dir.path().to_path_buf(),
    ] {
        let (_, status) = home.json(&["watch", folder.to_str().expect("path"), "--json"]);
        assert!(status.success());
        let output = home
            .modeld(&["scan", "--min-size", "1"])
            .output()
            .expect("scan");
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).expect("UTF-8");
        assert!(text.contains("Found 1 model artifacts"), "{text}");
        assert!(!text.contains("xcframework"), "{text}");
        assert!(!text.contains("test.gguf"), "{text}");
    }
}

#[test]
fn pass_clones_a_duplicate_and_ls_shows_both_copies_sharing() {
    let home = Home::new();
    home.settled_model("a.gguf");
    home.settled_model("b.gguf");

    let (pass, status) = home.json(&["pass", "--json", "--min-size", "1"]);
    let (ls, ls_status) = home.json(&["ls", "--json"]);

    assert!(status.success());
    let result = &last_line(&pass)["result"];
    assert_eq!(
        result["cloned"].as_array().map(Vec::len),
        Some(1),
        "{result}"
    );
    assert_eq!(pass[0]["event"]["kind"], "locked");
    assert!(ls_status.success());
    let ls = &last_line(&ls)["result"];
    let references = ls["models"][0]["references"]
        .as_array()
        .expect("references");
    assert_eq!(references.len(), 2);
    assert!(
        references
            .iter()
            .all(|reference| reference["state"] == "clone")
    );
    assert_eq!(ls["totals"]["saved"], WEIGHTS.len());
    assert_eq!(ls["totals"]["pending"], 0);
}

#[test]
fn dry_run_pass_fills_the_store_and_leaves_copies_pending() {
    let home = Home::new();
    let first = home.settled_model("a.gguf");
    let second = home.settled_model("b.gguf");

    let (pass, status) = home.json(&["pass", "--json", "--dry-run", "--min-size", "1"]);
    let (ls, _) = home.json(&["ls", "--json"]);

    assert!(status.success());
    let result = &last_line(&pass)["result"];
    assert_eq!(result["dry_run"], true);
    assert_eq!(result["planned"].as_array().map(Vec::len), Some(1));
    assert_eq!(result["cloned"].as_array().map(Vec::len), Some(0));
    assert_eq!(home.journal_entries(), 0);
    assert_eq!(std::fs::read(first).expect("read first"), WEIGHTS);
    assert_eq!(std::fs::read(second).expect("read second"), WEIGHTS);
    assert_eq!(last_line(&ls)["result"]["totals"]["pending"], WEIGHTS.len());
}

#[test]
fn pass_completes_and_journals_when_stdout_is_closed() {
    let home = Home::new();
    home.settled_model("a.gguf");
    home.settled_model("b.gguf");
    let (reader, writer) = std::io::pipe().expect("create pipe");
    drop(reader);

    let output = home
        .modeld(&["pass", "--json", "--min-size", "1"])
        .stdout(Stdio::from(writer))
        .stderr(Stdio::piped())
        .output()
        .expect("run modeld");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "modeld panicked: {stderr}");
    assert_eq!(output.status.signal(), None, "killed: {:?}", output.status);
    assert!(output.status.success());
    assert_eq!(home.journal_entries(), 1);
}

#[test]
fn generation_moves_on_pass_but_not_on_ls() {
    let home = Home::new();
    home.settled_model("a.gguf");

    home.json(&["pass", "--json", "--min-size", "1"]);
    let after_pass = home.generation().expect("pass wrote a generation");
    home.json(&["ls", "--json"]);
    let after_ls = home.generation();
    home.json(&["pass", "--json", "--min-size", "1"]);

    assert_eq!(after_ls.as_deref(), Some(after_pass.as_str()));
    assert_ne!(home.generation(), after_ls);
}

#[test]
fn doctor_lists_the_configured_folder() {
    let home = Home::new();

    let (lines, status) = home.json(&["doctor", "--json"]);

    assert!(status.success());
    let roots = last_line(&lines)["result"]["roots"]
        .as_array()
        .expect("roots")
        .clone();
    assert_eq!(roots.len(), 1);
    assert_eq!(roots[0]["provider"], "manual");
    assert!(
        roots[0]["path"]
            .as_str()
            .is_some_and(|path| path.ends_with("/models"))
    );
}

#[test]
fn daemon_status_finds_no_agent_files_in_a_fresh_home() {
    let home = Home::new();

    let (lines, status) = home.json(&["daemon", "status", "--json"]);

    // `state` comes from launchctl, which sees this machine's real agent
    // whatever HOME says; only the home's own files are checked here.
    assert!(status.success());
    let result = &last_line(&lines)["result"];
    assert!(result["state"].is_string());
    assert_eq!(result["binary_is_current"], Value::Null);
    assert_eq!(result["last_pass"], Value::Null);
    assert_eq!(result["environment"], serde_json::json!({}));
}

#[test]
fn command_without_json_output_says_so_and_fails() {
    let home = Home::new();

    let (lines, status) = home.json(&["scan", "--json"]);

    assert_eq!(status.code(), Some(1));
    let error = &last_line(&lines)["error"];
    assert_eq!(error["code"], "unsupported");
}

#[test]
fn interrupting_a_pass_that_waits_for_the_lock_cancels_it() {
    let home = Home::new();
    home.settled_model("a.gguf");
    let held = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.store().join("lock"))
        .expect("open store lock");
    held.lock().expect("hold store lock");
    let mut child = home
        .modeld(&["pass", "--json", "--min-size", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn modeld");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();

    let first: Value = serde_json::from_str(&lines.next().expect("a line").expect("read line"))
        .expect("JSON line");
    let pid = libc::pid_t::try_from(child.id()).expect("pid fits");
    // SAFETY: kill has no memory-safety preconditions; `pid` is our child.
    unsafe { libc::kill(pid, libc::SIGINT) };
    let rest: Vec<Value> = lines
        .map(|line| serde_json::from_str(&line.expect("read line")).expect("JSON line"))
        .collect();
    let status = child.wait().expect("wait for modeld");

    assert_eq!(first["event"]["kind"], "waiting_for_lock");
    assert_eq!(status.code(), Some(1));
    assert_eq!(last_line(&rest)["error"]["code"], "cancelled");
    assert_eq!(home.generation(), None, "a cancelled pass changed nothing");
    drop(held);
}

#[test]
fn interrupting_a_pass_that_holds_the_lock_lets_it_finish() {
    let home = Home::new();
    // Big enough that hashing outlasts the signal's delivery.
    let weights = vec![7u8; 64 * 1024 * 1024];
    home.settled_model_of("a.gguf", &weights);
    home.settled_model_of("b.gguf", &weights);
    let mut child = home
        .modeld(&["pass", "--json", "--min-size", "1"])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn modeld");
    let mut lines = BufReader::new(child.stdout.take().expect("stdout")).lines();

    let first: Value = serde_json::from_str(&lines.next().expect("a line").expect("read line"))
        .expect("JSON line");
    let pid = libc::pid_t::try_from(child.id()).expect("pid fits");
    // SAFETY: kill has no memory-safety preconditions; `pid` is our child.
    unsafe { libc::kill(pid, libc::SIGINT) };
    let rest: Vec<Value> = lines
        .map(|line| serde_json::from_str(&line.expect("read line")).expect("JSON line"))
        .collect();
    let status = child.wait().expect("wait for modeld");

    assert_eq!(first["event"]["kind"], "locked");
    assert!(status.success(), "{status:?}");
    let result = &last_line(&rest)["result"];
    assert_eq!(
        result["cloned"].as_array().map(Vec::len),
        Some(1),
        "{result}"
    );
    assert_eq!(home.journal_entries(), 1);
}

#[test]
fn text_pass_completes_when_stdout_and_stderr_are_closed() {
    let home = Home::new();
    home.settled_model("a.gguf");
    home.settled_model("b.gguf");
    let (out_reader, out_writer) = std::io::pipe().expect("create pipe");
    let (err_reader, err_writer) = std::io::pipe().expect("create pipe");
    drop((out_reader, err_reader));

    let status = home
        .modeld(&["pass", "--min-size", "1"])
        .stdout(Stdio::from(out_writer))
        .stderr(Stdio::from(err_writer))
        .status()
        .expect("run modeld");

    assert_eq!(status.signal(), None, "killed: {status:?}");
    assert!(status.success());
    assert_eq!(home.journal_entries(), 1);
}

#[test]
fn text_pass_prints_the_daemon_summary() {
    let home = Home::new();
    home.settled_model("a.gguf");

    let output = home
        .modeld(&["pass", "--min-size", "1"])
        .output()
        .expect("run modeld");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success());
    assert!(
        stdout
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("pass: ")),
        "{stdout}"
    );
}

/// Asserts `path` is inside the temp home, so no test touches a real store.
#[test]
fn tests_never_touch_the_real_store() {
    let home = Home::new();
    let (lines, _) = home.json(&["doctor", "--json"]);
    let store = last_line(&lines)["result"]["store"]
        .as_str()
        .expect("store")
        .to_string();

    assert!(Path::new(&store).starts_with(home.dir.path()), "{store}");
}

/// The digest `ls --json` reports for the only stored model.
fn only_digest(home: &Home) -> String {
    let (lines, _) = home.json(&["ls", "--json"]);
    last_line(&lines)["result"]["models"][0]["digest"]
        .as_str()
        .expect("digest")
        .to_string()
}

#[test]
fn rm_dry_run_plans_and_changes_nothing() {
    let home = Home::new();
    let model = home.settled_model("a.gguf");
    home.json(&["pass", "--json", "--min-size", "1"]);
    let digest = only_digest(&home);

    let (lines, status) = home.json(&["rm", &digest, "--everywhere", "--dry-run", "--json"]);

    assert!(status.success());
    let result = &last_line(&lines)["result"];
    assert_eq!(result["status"], "planned");
    let actions: Vec<&str> = result["plan"]["steps"]
        .as_array()
        .expect("steps")
        .iter()
        .filter_map(|step| step["action"].as_str())
        .collect();
    assert_eq!(actions, ["trash", "remove_stored_copy"]);
    assert_eq!(result["plan"]["plan_id"].as_str().map(str::len), Some(16));
    assert!(model.exists());
}

#[test]
fn rm_with_json_needs_an_expected_plan_or_yes() {
    let home = Home::new();
    home.settled_model("a.gguf");
    home.json(&["pass", "--json", "--min-size", "1"]);
    let digest = only_digest(&home);

    let (lines, status) = home.json(&["rm", &digest, "--everywhere", "--json"]);

    assert_eq!(status.code(), Some(1));
    assert_eq!(last_line(&lines)["error"]["code"], "confirmation_required");
}

#[test]
fn rm_with_a_stale_plan_id_exits_3_and_changes_nothing() {
    let home = Home::new();
    let model = home.settled_model("a.gguf");
    home.json(&["pass", "--json", "--min-size", "1"]);
    let digest = only_digest(&home);

    let (lines, status) = home.json(&[
        "rm",
        &digest,
        "--everywhere",
        "--expect-plan",
        "0000000000000000",
        "--json",
    ]);

    assert_eq!(status.code(), Some(3));
    let result = &last_line(&lines)["result"];
    assert_eq!(result["status"], "plan_changed");
    assert_eq!(result["outcome"], Value::Null);
    assert!(model.exists());
}

#[test]
fn rm_removes_a_hugging_face_model_it_planned() {
    let home = Home::new();
    let repo = home
        .dir
        .path()
        .join(".cache/huggingface/hub/models--org--tiny");
    let blob_name = "04b1c301231dd422b8860db31311ab2721511346a32cb1e079c4c4e5f1fe4696";
    let blob = repo.join("blobs").join(blob_name);
    std::fs::create_dir_all(blob.parent().expect("parent")).expect("mkdir");
    std::fs::write(&blob, WEIGHTS).expect("write blob");
    let old = SystemTime::now() - Duration::from_hours(1);
    std::fs::File::open(&blob)
        .and_then(|file| file.set_times(std::fs::FileTimes::new().set_modified(old)))
        .expect("backdate");
    let snapshot = repo.join("snapshots/abc");
    std::fs::create_dir_all(&snapshot).expect("mkdir");
    std::os::unix::fs::symlink(&blob, snapshot.join("model.gguf")).expect("link");
    home.json(&["pass", "--json", "--min-size", "1"]);
    let digest = only_digest(&home);
    let generation = home.generation();
    let (dry, _) = home.json(&[
        "rm",
        &digest,
        "--provider",
        "huggingface",
        "--dry-run",
        "--json",
    ]);
    let plan_id = last_line(&dry)["result"]["plan"]["plan_id"]
        .as_str()
        .expect("plan id")
        .to_string();

    let (lines, status) = home.json(&[
        "rm",
        &digest,
        "--provider",
        "huggingface",
        "--expect-plan",
        &plan_id,
        "--json",
    ]);
    let (ls, _) = home.json(&["ls", "--json"]);

    assert!(status.success(), "{lines:?}");
    let result = &last_line(&lines)["result"];
    assert_eq!(result["status"], "removed");
    assert!(lines.iter().any(|line| line["event"]["kind"] == "locked"));
    assert!(!repo.exists());
    assert_eq!(last_line(&ls)["result"]["models"], serde_json::json!([]));
    assert_ne!(home.generation(), generation);
}
