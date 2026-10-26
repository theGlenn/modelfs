//! Persists one watched folder without changing any model files.
//!
//! The daemon already watches `config.toml`; an atomic replacement wakes it
//! to re-detect roots. The store lock serializes CLI/app additions with each
//! other and with daemon passes. TOML comments and unrelated settings survive.

use crate::{Output, json};
use modeld_providers::config;
use modeld_store::StoreLock;
use std::io::Write;
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Table, value};

pub fn run(folder: &Path, output: Output) {
    match add(folder, &crate::home_dir()) {
        Ok(result) => {
            if result.added {
                crate::announce_change();
            }
            match output {
                Output::Json => json::emit_result(result),
                Output::Text => println!(
                    "{} {}\nThe background service picks up configured folders automatically.",
                    if result.added {
                        "Watching"
                    } else {
                        "Already watching"
                    },
                    result.path
                ),
            }
        }
        Err(message) => crate::fail(output, json::ErrorCode::WatchFailed, message),
    }
}

fn add(folder: &Path, home: &Path) -> Result<json::Watch, String> {
    let folder = if folder == Path::new("~") {
        home.to_path_buf()
    } else if let Ok(relative) = folder.strip_prefix("~/") {
        home.join(relative)
    } else {
        folder.to_path_buf()
    };
    let folder = folder
        .canonicalize()
        .map_err(|error| format!("cannot open folder {} ({error})", folder.display()))?;
    std::fs::read_dir(&folder)
        .map_err(|error| format!("cannot read folder {} ({error})", folder.display()))?;
    let path = folder
        .to_str()
        .ok_or("folder path is not valid UTF-8")?
        .to_string();
    let store = home.join(".modeld");
    let canonical_store = store.canonicalize().unwrap_or_else(|_| store.clone());
    if folder.starts_with(&canonical_store) {
        return Err("the ModelFS store cannot be added as a model folder".to_string());
    }
    let _lock = StoreLock::try_acquire(&store)
        .map_err(|error| format!("cannot lock configuration ({error})"))?
        .ok_or("another modeld process is working; try adding the folder again when it finishes")?;
    let config_path = store.join("config.toml");
    let metadata = match std::fs::symlink_metadata(&config_path) {
        Ok(metadata) if metadata.is_file() => Some(metadata),
        Ok(_) => {
            return Err("config.toml must be a regular file; it was left unchanged".to_string());
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(format!("cannot inspect config.toml ({error})")),
    };
    let config = config::load(&config_path)?;
    let mut result = json::Watch {
        path,
        added: false,
        config: config_path.display().to_string(),
    };
    if config::manual_roots(&config, home)
        .iter()
        .any(|root| root.root == folder)
    {
        return Ok(result);
    }
    let original = if metadata.is_some() {
        std::fs::read_to_string(&config_path)
            .map_err(|error| format!("cannot read config.toml ({error})"))?
    } else {
        String::new()
    };
    let mut document: DocumentMut = original
        .parse()
        .map_err(|error| format!("invalid config: {error}"))?;
    document.entry("scan").or_insert(Item::Table(Table::new()));
    if document["scan"].get("roots").is_none() {
        document["scan"]["roots"] = value(Array::new());
    }
    let roots = document["scan"]["roots"]
        .as_array_mut()
        .ok_or("scan.roots must be an array; config.toml was left unchanged")?;
    // Configuration roots are glob patterns. Escape metacharacters so a folder
    // named `models [local]` watches that literal folder, not a pattern.
    roots.push(glob::Pattern::escape(&result.path));
    let mut temp = tempfile::NamedTempFile::new_in(&store)
        .map_err(|error| format!("cannot create temporary config ({error})"))?;
    if let Some(metadata) = metadata {
        temp.as_file()
            .set_permissions(metadata.permissions())
            .map_err(|error| format!("cannot preserve config permissions ({error})"))?;
    }
    temp.write_all(document.to_string().as_bytes())
        .and_then(|()| temp.as_file().sync_all())
        .map_err(|error| format!("cannot write config.toml ({error})"))?;
    // An editor does not share the modeld lock. Refuse if its newer contents
    // appeared while this command prepared the replacement.
    let current = std::fs::read_to_string(&config_path);
    if current.as_ref().is_ok_and(|text| text != &original)
        || current.as_ref().is_err_and(|error| {
            error.kind() != std::io::ErrorKind::NotFound || !original.is_empty()
        })
    {
        return Err("config.toml changed while adding the folder; try again".to_string());
    }
    temp.persist(&config_path)
        .map_err(|error| format!("cannot replace config.toml ({error})"))?;
    result.added = true;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_literal_folder_and_preserves_comments_and_unrelated_settings() {
        let home = tempfile::tempdir().expect("home");
        let models = home.path().join("models [local]");
        std::fs::create_dir_all(&models).expect("mkdir");
        let store = home.path().join(".modeld");
        std::fs::create_dir_all(&store).expect("store");
        let config_path = store.join("config.toml");
        std::fs::write(
            &config_path,
            "# Keep this comment\n[future]\nsetting = 42\n",
        )
        .expect("config");

        let result = add(&models, home.path()).expect("watch");
        assert!(result.added);
        let text = std::fs::read_to_string(&config_path).expect("read");
        assert!(text.contains("# Keep this comment"));
        assert!(text.contains("setting = 42"));
        let config = config::load(&config_path).expect("load");
        let roots = config::manual_roots(&config, home.path());
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].root, models.canonicalize().expect("canonical"));
    }

    #[test]
    fn tilde_and_symlink_aliases_do_not_add_the_same_folder_twice() {
        let home = tempfile::tempdir().expect("home");
        let models = home.path().join("models");
        std::fs::create_dir_all(&models).expect("mkdir");
        let alias = home.path().join("alias");
        std::os::unix::fs::symlink(&models, &alias).expect("alias");
        assert!(add(Path::new("~/models"), home.path()).expect("add").added);
        let path = home.path().join(".modeld/config.toml");
        let original = std::fs::read_to_string(&path).expect("config");
        assert!(!add(&alias, home.path()).expect("duplicate").added);
        assert_eq!(std::fs::read_to_string(&path).expect("config"), original);
    }

    #[test]
    fn invalid_config_and_store_folder_are_refused_without_overwriting() {
        let home = tempfile::tempdir().expect("home");
        let store = home.path().join(".modeld");
        std::fs::create_dir_all(&store).expect("mkdir");
        let config_path = store.join("config.toml");
        std::fs::write(&config_path, "[scan\ninvalid").expect("config");
        assert!(add(home.path(), home.path()).is_err());
        assert_eq!(
            std::fs::read_to_string(&config_path).expect("read"),
            "[scan\ninvalid"
        );
        assert!(add(&store, home.path()).is_err());
    }

    #[test]
    fn non_directories_and_symlinked_config_are_refused() {
        let home = tempfile::tempdir().expect("home");
        let file = home.path().join("not-a-folder");
        std::fs::write(&file, b"file").expect("file");
        assert!(add(&file, home.path()).is_err());
        assert!(!home.path().join(".modeld").exists());
        let store = home.path().join(".modeld");
        std::fs::create_dir_all(&store).expect("store");
        std::os::unix::fs::symlink(&file, store.join("config.toml")).expect("symlink");
        assert!(add(home.path(), home.path()).is_err());
        assert_eq!(std::fs::read(&file).expect("original"), b"file");
    }
}
