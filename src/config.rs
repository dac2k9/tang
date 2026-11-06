#[cfg(feature = "web")]
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;

use serde::Deserialize;

static CONFIG: OnceLock<Config> = OnceLock::new();

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub plugin_paths: PluginPaths,
}

#[derive(Default, Deserialize)]
#[serde(default)]
pub struct PluginPaths {
    pub clap: Vec<PathBuf>,
    pub vst3: Vec<PathBuf>,
    pub lv2: Vec<PathBuf>,
}

pub fn init(config: Config) {
    CONFIG.set(config).ok();
}

pub fn extra_clap_paths() -> &'static [PathBuf] {
    CONFIG
        .get()
        .map(|c| c.plugin_paths.clap.as_slice())
        .unwrap_or(&[])
}

#[allow(dead_code)]
pub fn extra_vst3_paths() -> &'static [PathBuf] {
    CONFIG
        .get()
        .map(|c| c.plugin_paths.vst3.as_slice())
        .unwrap_or(&[])
}

pub fn extra_lv2_paths() -> &'static [PathBuf] {
    CONFIG
        .get()
        .map(|c| c.plugin_paths.lv2.as_slice())
        .unwrap_or(&[])
}

/// A `[[sounds.folder]]` table of the config file (step 116): a folder of session files that the page lists, to open one of.
#[cfg(feature = "web")]
#[derive(Deserialize)]
struct SoundFolder {
    /// What the page calls it; the folder's own name if the file says none.
    name: Option<String>,
    path: PathBuf,
}

#[cfg(feature = "web")]
#[derive(Default, Deserialize)]
#[serde(default)]
struct SoundsFile {
    folder: Vec<SoundFolder>,
}

/// The folders that the config file's `[[sounds.folder]]` tables name, each with the name it was given (if any) and its path, in the order of the file, with a leading `~` made the `home`. `[plugin_paths]` is not looked at: a
/// file that is right in the one is not refused for the other. What is wrong with the file or with a table is the error, in words.
#[cfg(feature = "web")]
pub fn sound_folders(text: &str, home: Option<&Path>) -> Result<Vec<(Option<String>, PathBuf)>, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| e.message().to_string())?;
    let Some(sounds) = table.get("sounds") else { return Ok(Vec::new()) };
    let sounds: SoundsFile = sounds.clone().try_into().map_err(|e: toml::de::Error| format!("[sounds] is not right: {}", e.message()))?;
    Ok(sounds.folder.into_iter().map(|folder| (folder.name, with_home(folder.path, home))).collect())
}

/// `path` with a leading `~` made the `home` (`~` alone, or `~/` and what follows); any other path as it is.
#[cfg(feature = "web")]
fn with_home(path: PathBuf, home: Option<&Path>) -> PathBuf {
    match (path.strip_prefix("~"), home) {
        (Ok(rest), Some(home)) => home.join(rest),
        _ => path,
    }
}

/// The folders that the config file in `dir` names for the page to list (`sound_folders`), and what is to be said of the file, if anything: none, and nothing to say, if there is no file; none, with why, if the file cannot be read or is not right
/// (a sound that cannot be found in a folder is no reason to stop a plugin that is playing).
#[cfg(feature = "web")]
pub fn sound_folders_in(dir: &Path, home: Option<&Path>) -> (Vec<(Option<String>, PathBuf)>, Option<String>) {
    let path = dir.join("config.toml");
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), None),
        Err(e) => return (Vec::new(), Some(format!("Could not read {}: {e}", path.display()))),
    };
    match sound_folders(&text, home) {
        Ok(folders) => (folders, None),
        Err(e) => (Vec::new(), Some(format!("The sound folders of {} are left out: {e}", path.display()))),
    }
}

/// The folders that the config file in `dir` names for the page to list, as `sound_folders_in` has them, with what it has to say in the log. This is read by the plugin that a DAW runs, which has not read the file with the rest of the
/// config (the command line does that, once).
#[cfg(feature = "web")]
pub fn read_sound_folders(dir: &Path, home: Option<&Path>) -> Vec<(Option<String>, PathBuf)> {
    let (folders, warning) = sound_folders_in(dir, home);
    if let Some(warning) = warning {
        log::warn!("{warning}");
    }
    folders
}

#[cfg(all(test, feature = "web"))]
mod tests {
    use super::*;

    #[test]
    fn the_folders_of_the_config_file_come_in_the_order_of_the_file_with_the_names_it_gives() {
        let text = r#"
[plugin_paths]
clap = ["/somewhere"]

[[sounds.folder]]
name = "Examples"
path = "/src/examples"

[[sounds.folder]]
path = "/music/sounds"
"#;
        assert_eq!(
            sound_folders(text, None).unwrap(),
            [(Some("Examples".to_string()), PathBuf::from("/src/examples")), (None, PathBuf::from("/music/sounds"))]
        );
    }

    #[test]
    fn a_file_that_names_no_folder_or_has_no_such_table_has_none() {
        assert_eq!(sound_folders("", None).unwrap(), []);
        assert_eq!(sound_folders("[plugin_paths]\nclap = []\n", None).unwrap(), []);
        assert_eq!(sound_folders("[sounds]\n", None).unwrap(), []);
    }

    #[test]
    fn what_is_wrong_with_the_file_or_a_table_is_said() {
        assert!(sound_folders("this is {not toml", None).is_err());
        let error = sound_folders("[[sounds.folder]]\nname = \"No path\"\n", None).unwrap_err();
        assert!(error.starts_with("[sounds] is not right: "), "{error}");
        assert!(error.contains("path"), "{error}");
        assert!(sound_folders("sounds = 3", None).unwrap_err().starts_with("[sounds] is not right"));
    }

    #[test]
    fn a_leading_tilde_is_the_home_and_nothing_else_is() {
        let home = Path::new("/home/me");
        let text = "[[sounds.folder]]\npath = \"~/sounds\"\n[[sounds.folder]]\npath = \"~\"\n[[sounds.folder]]\npath = \"/a/~/b\"\n[[sounds.folder]]\npath = \"~other/x\"\n";
        let folders: Vec<PathBuf> = sound_folders(text, Some(home)).unwrap().into_iter().map(|(_, path)| path).collect();
        assert_eq!(folders, [PathBuf::from("/home/me/sounds"), PathBuf::from("/home/me"), PathBuf::from("/a/~/b"), PathBuf::from("~other/x")]);
        let without: Vec<PathBuf> = sound_folders("[[sounds.folder]]\npath = \"~/sounds\"\n", None).unwrap().into_iter().map(|(_, path)| path).collect();
        assert_eq!(without, [PathBuf::from("~/sounds")], "with no home it is left as it is");
    }

    #[test]
    fn the_config_file_of_a_directory_is_read_and_a_file_that_is_missing_or_wrong_gives_none_and_says_why_unless_it_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("config.toml");
        assert_eq!(sound_folders_in(dir.path(), None), (vec![], None), "no file is nothing to say: most people have none");
        assert_eq!(read_sound_folders(dir.path(), None), []);
        std::fs::write(&file, "[[sounds.folder]]\npath = \"/a\"\n").unwrap();
        assert_eq!(sound_folders_in(dir.path(), None), (vec![(None, PathBuf::from("/a"))], None));
        assert_eq!(read_sound_folders(dir.path(), None), [(None, PathBuf::from("/a"))]);
        std::fs::write(&file, "[[sounds.folder]]\nname = 3\n").unwrap();
        let (folders, warning) = sound_folders_in(dir.path(), None);
        assert_eq!(folders, [], "a table that is wrong leaves them all out");
        let warning = warning.expect("and says so");
        assert!(warning.starts_with("The sound folders of ") && warning.contains("config.toml") && warning.contains("are left out: "), "{warning}");
        assert_eq!(read_sound_folders(dir.path(), None), []);
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        let (folders, warning) = sound_folders_in(dir.path(), None);
        assert_eq!(folders, [], "a file that cannot be read");
        assert!(warning.expect("says so").starts_with("Could not read "), "and a file that is not there is not that");
    }
}
