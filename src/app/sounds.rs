//! Where a person's sounds come from (step 116): the libraries that a page can list and open a session file from, besides the computer that the page is on.
//!
//! A *sound* is a session file (`.toml`). A *library* is a place that holds some: a folder on the machine that runs Tang (`Folder`: the one that sessions are saved to, and the ones that
//! `config.toml` names), and, when it is built, a server that people upload to, which Tang downloads from. The page never names a path: it names a library by its `id` and a sound by the
//! `id` that the library gave it, and the library says where that is (and refuses what is not inside it). `Library` is the whole of what a place has to do, so that a place that is far away is one
//! more implementation and nothing else changes: the web server asks for a listing and for a text on a thread of its own (`web::desk`), where a slow answer holds nobody up.
//!
//! What a library gives is the text of a session file, which goes the way of every other (`Intent::OpenSession`: read first, and handed to the program that owns the controller).

#![cfg_attr(not(any(test, feature = "web")), expect(dead_code, reason = "only the web runtime lists sounds"))]

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::app::protocol::{ErrorCode, WireError};

/// The longest session file that a library gives, in bytes: what the connection that carries it takes (`MAX_OPEN_MESSAGE_BYTES`), which is far more than the files that people write (some kilobytes; the state of a
/// plugin that is kept in a session makes it some hundreds).
pub const MAX_SOUND_BYTES: u64 = 8 * 1024 * 1024;

/// The longest session file that a listing reads to say what is in it (`describe`): a larger one is listed by its size, and opened all the same.
const MAX_DESCRIBED_BYTES: u64 = 1024 * 1024;

/// The most sounds that a folder lists: a folder that has more (a person who names their home) is cut, and says so (`Listing::truncated`).
pub const MAX_LISTED: usize = 1000;

/// How many folders deep a folder lists: a library is a few folders of sounds, not a tree.
const MAX_DEPTH: usize = 4;

/// What a client is told of a library, in the hello: how to ask for it, what to call it, and where it is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SoundLibrary {
    /// What a request names it by.
    pub id: String,
    /// What a person calls it.
    pub label: String,
    /// Where it is, for a person to read: a folder's path, a server's address.
    pub place: String,
}

/// One sound of a library, as a client is told of it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "web", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
pub struct SoundEntry {
    /// What a request names it by, inside its library.
    pub id: String,
    /// What a person calls it: the file's name without `.toml`, with the folders it is in.
    pub name: String,
    /// What is in it, in a few words (`describe`), or why it cannot be opened.
    pub detail: String,
    /// Whether it can be opened: a file that is not a session, or is too big, cannot.
    pub openable: bool,
    /// When it was last changed, in seconds since 1970, if the library knows.
    pub modified: Option<u64>,
}

/// What a library lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Listing {
    /// Newest first, and by name among those that are as new.
    pub entries: Vec<SoundEntry>,
    /// The library has more than it lists (`MAX_LISTED`).
    pub truncated: bool,
}

/// A place that holds sounds. It is asked on a thread that can wait (`web::desk`), so that a library that is slow to answer (a server) is no trouble to the page.
pub trait Library: Send + Sync + std::fmt::Debug {
    /// What a client is told of it.
    fn info(&self) -> SoundLibrary;
    /// What it has.
    fn list(&self) -> Result<Listing, WireError>;
    /// The text of the session file that `id` (one of those it listed) names.
    fn read(&self, id: &str) -> Result<String, WireError>;
}

/// The libraries that a program has, in the order that a page shows them.
#[derive(Debug, Default)]
pub struct Sounds {
    libraries: Vec<Box<dyn Library>>,
}

impl Sounds {
    pub fn new(libraries: Vec<Box<dyn Library>>) -> Sounds {
        Sounds { libraries }
    }

    /// The libraries that a person has: the folder that sessions are saved to (`sessions`, which is a library even before there is a session in it: the first save makes the folder), and the folders that `config.toml` names (`[[sounds.folder]]`).
    pub fn standard(sessions: Option<PathBuf>, folders: Vec<(Option<String>, PathBuf)>) -> Sounds {
        let mut libraries: Vec<Box<dyn Library>> = Vec::new();
        if let Some(dir) = sessions {
            libraries.push(Box::new(Folder::new("sessions", "Saved sessions", dir).empty_until_made()));
        }
        for (at, (name, path)) in folders.into_iter().enumerate() {
            let label = name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| path.file_name().map_or_else(|| path.display().to_string(), |name| name.to_string_lossy().into_owned()));
            libraries.push(Box::new(Folder::new(&format!("folder{}", at + 1), &label, path)));
        }
        Sounds::new(libraries)
    }

    pub fn is_empty(&self) -> bool {
        self.libraries.is_empty()
    }

    /// What a client is told of each.
    pub fn infos(&self) -> Vec<SoundLibrary> {
        self.libraries.iter().map(|library| library.info()).collect()
    }

    fn library(&self, id: &str) -> Result<&dyn Library, WireError> {
        self.libraries.iter().map(|library| library.as_ref()).find(|library| library.info().id == id).ok_or_else(|| WireError::new(ErrorCode::NotFound, format!("there is no library of sounds called {id:?}")))
    }

    pub fn list(&self, library: &str) -> Result<Listing, WireError> {
        self.library(library)?.list()
    }

    pub fn read(&self, library: &str, id: &str) -> Result<String, WireError> {
        self.library(library)?.read(id)
    }
}

/// A folder on this machine, with the session files in it (and in the folders in it, a few deep).
#[derive(Debug)]
pub struct Folder {
    id: String,
    label: String,
    root: PathBuf,
    /// A folder that is not there is an empty library, not a fault (the folder that sessions are saved to is made by the first save).
    empty_until_made: bool,
}

impl Folder {
    pub fn new(id: &str, label: &str, root: PathBuf) -> Folder {
        Folder { id: id.to_string(), label: label.to_string(), root, empty_until_made: false }
    }

    /// This folder, which a program may not have made yet: until it is, it has no sounds.
    pub fn empty_until_made(mut self) -> Folder {
        self.empty_until_made = true;
        self
    }

    /// The path of the sound `id` (`folder/name.toml`, with slashes whatever the system) if it is a session file inside this folder, which is read from where it really is: a name that goes up out of the folder, an
    /// absolute one, and a link that leads out are not.
    fn path_of(&self, id: &str) -> Result<PathBuf, WireError> {
        let outside = || WireError::new(ErrorCode::Invalid, format!("{id:?} is not a sound in this folder"));
        let relative = Path::new(id);
        if id.is_empty() || id.contains('\\') || !relative.components().all(|part| matches!(part, Component::Normal(_))) {
            return Err(outside());
        }
        // (Only the id that a listing gave: with no slash at the end, none doubled, none to stand for the folder itself.)
        if relative.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/") != id {
            return Err(outside());
        }
        if !has_toml_extension(relative) {
            return Err(outside());
        }
        let root = self.root.canonicalize().map_err(|e| WireError::new(ErrorCode::Io, format!("the folder cannot be read: {e}")))?;
        let path = root.join(relative).canonicalize().map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => WireError::new(ErrorCode::NotFound, format!("there is no sound {id:?} in this folder any more")),
            _ => WireError::new(ErrorCode::Io, format!("{id:?} cannot be read: {e}")),
        })?;
        if !path.starts_with(&root) {
            return Err(outside());
        }
        Ok(path)
    }
}

impl Library for Folder {
    fn info(&self) -> SoundLibrary {
        SoundLibrary { id: self.id.clone(), label: self.label.clone(), place: self.root.display().to_string() }
    }

    fn list(&self) -> Result<Listing, WireError> {
        if self.empty_until_made && !self.root.exists() {
            return Ok(Listing { entries: Vec::new(), truncated: false });
        }
        if !self.root.is_dir() {
            let why = if self.root.exists() { "it is not a folder" } else { "it does not exist" };
            return Err(WireError::new(ErrorCode::NotFound, format!("{}: {why}", self.root.display())));
        }
        let mut entries = Vec::new();
        let mut truncated = false;
        let mut folders = vec![(self.root.clone(), 0usize)];
        'walk: while let Some((dir, depth)) = folders.pop() {
            // (A folder that cannot be read in the middle is left out, and the rest is listed: the one at the top is said above.)
            let Ok(read) = fs::read_dir(&dir) else { continue };
            for item in read.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') {
                    continue;
                }
                let path = item.path();
                let Ok(kind) = item.file_type() else { continue };
                // (A link to a folder is not followed, which would let a loop make an endless walk; a link to a file is a file.)
                if kind.is_dir() {
                    if depth < MAX_DEPTH && name != "target" && name != "node_modules" {
                        folders.push((path, depth + 1));
                    }
                    continue;
                }
                if !has_toml_extension(&path) {
                    continue;
                }
                let Ok(meta) = fs::metadata(&path) else { continue };
                if !meta.is_file() {
                    continue;
                }
                if entries.len() >= MAX_LISTED {
                    truncated = true;
                    break 'walk;
                }
                let Ok(relative) = path.strip_prefix(&self.root) else { continue };
                let id = relative.components().map(|part| part.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                // (The extension is `toml` in some case, and so five characters, which the name leaves out.)
                let name = id[..id.len() - ".toml".len()].to_string();
                let modified = meta.modified().ok().and_then(|at| at.duration_since(UNIX_EPOCH).ok()).map(|since| since.as_secs());
                let (detail, openable) = detail_of(&path, meta.len());
                entries.push(SoundEntry { id, name, detail, openable, modified });
            }
        }
        // (Newest first: the sound that a person made last is the one they come back for.)
        entries.sort_by(newest_first);
        Ok(Listing { entries, truncated })
    }

    fn read(&self, id: &str) -> Result<String, WireError> {
        let path = self.path_of(id)?;
        let meta = fs::metadata(&path).map_err(|e| WireError::new(ErrorCode::Io, format!("{id:?} cannot be read: {e}")))?;
        if !meta.is_file() {
            return Err(WireError::new(ErrorCode::Invalid, format!("{id:?} is not a sound in this folder")));
        }
        if meta.len() > MAX_SOUND_BYTES {
            return Err(WireError::new(ErrorCode::Invalid, format!("{id:?} is too big to open ({} MB; the most is {} MB)", megabytes(meta.len()), MAX_SOUND_BYTES / (1024 * 1024))));
        }
        fs::read_to_string(&path).map_err(|e| WireError::new(ErrorCode::Io, format!("{id:?} cannot be read as text: {e}")))
    }
}

/// The order of a listing: newest first (a sound whose library does not know when it was changed, last), then by name without regard to case, then by id (a file system that keeps case can have two sounds whose names differ only in it).
fn newest_first(a: &SoundEntry, b: &SoundEntry) -> std::cmp::Ordering {
    b.modified.cmp(&a.modified).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.id.cmp(&b.id))
}

fn has_toml_extension(path: &Path) -> bool {
    path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("toml"))
}

/// A size in megabytes, to a tenth, as a person reads it.
fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024.0 * 1024.0))
}

/// What is said of the file at `path`, and whether it can be opened: what is in it if it is a session, why not if it is not.
fn detail_of(path: &Path, bytes: u64) -> (String, bool) {
    if bytes > MAX_SOUND_BYTES {
        return (format!("too big to open ({} MB)", megabytes(bytes)), false);
    }
    if bytes > MAX_DESCRIBED_BYTES {
        return (format!("a large session ({} MB)", megabytes(bytes)), true);
    }
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(_) => return ("cannot be read as text".to_string(), false),
    };
    match crate::session::parse(&text) {
        Ok(config) => (describe(&config), true),
        // (The first line of what is wrong: the rest of a parse error quotes the file.)
        Err(error) => (format!("not a session: {}", format!("{error:#}").lines().next().unwrap_or("")), false),
    }
}

/// What a session has, in a few words: the instruments (by the plugins they are, a few of them), and how many effects and modulators.
pub fn describe(config: &crate::session::SessionConfig) -> String {
    const NAMED: usize = 3;
    let mut instruments: Vec<String> = Vec::new();
    for lane in &config.instruments {
        if let Some(instrument) = &lane.instrument {
            let label = plugin_label(&instrument.plugin);
            if !instruments.contains(&label) {
                instruments.push(label);
            }
        }
    }
    let effects: usize = config.instruments.iter().map(|lane| lane.effects.len()).sum::<usize>() + config.groups.iter().map(|group| group.effects.len()).sum::<usize>();
    let modulators = config.free_modulators.len() + config.instruments.iter().map(|lane| lane.modulators.len()).sum::<usize>() + config.groups.iter().map(|group| group.modulators.len()).sum::<usize>();
    let mut parts = Vec::new();
    if instruments.is_empty() {
        parts.push("no instrument".to_string());
    } else {
        let more = instruments.len().saturating_sub(NAMED);
        let mut named = instruments.into_iter().take(NAMED).collect::<Vec<_>>().join(", ");
        if more > 0 {
            named.push_str(&format!(" and {more} more"));
        }
        parts.push(named);
    }
    if effects > 0 {
        parts.push(format!("{effects} {}", if effects == 1 { "effect" } else { "effects" }));
    }
    if modulators > 0 {
        parts.push(format!("{modulators} {}", if modulators == 1 { "modulator" } else { "modulators" }));
    }
    parts.join(" · ")
}

/// What a plugin is called, from how a session says where it is (`builtin:osc`, `vst3:Pianoteq 9`, `lv2:http://tytel.org/helm`, `./Plugin.clap`): the last part of it, without the extension of a bundle.
fn plugin_label(spec: &str) -> String {
    let last = spec.rsplit([':', '/', '\\']).find(|part| !part.is_empty()).unwrap_or(spec);
    let lower = last.to_ascii_lowercase();
    for extension in [".vst3", ".clap", ".lv2"] {
        if lower.ends_with(extension) {
            return last[..last.len() - extension.len()].to_string();
        }
    }
    last.to_string()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::{Duration, SystemTime};

    use super::*;

    const OSC: &str = "[[instrument]]\nplugin = \"builtin:osc\"\n";

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// The file `path` as it was last changed `seconds` after the start of 2024.
    fn changed(path: &Path, seconds: u64) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(1_704_067_200 + seconds)).unwrap();
    }

    fn folder(dir: &Path) -> Folder {
        Folder::new("here", "Here", dir.to_path_buf())
    }

    fn names(listing: &Listing) -> Vec<&str> {
        listing.entries.iter().map(|entry| entry.name.as_str()).collect()
    }

    #[test]
    fn a_folder_lists_its_session_files_newest_first_and_by_name_among_those_as_new() {
        let dir = tempfile::tempdir().unwrap();
        changed(&write(dir.path(), "old.toml", OSC), 10);
        changed(&write(dir.path(), "Bravo.toml", OSC), 50);
        changed(&write(dir.path(), "alpha.toml", OSC), 50);
        changed(&write(dir.path(), "new.toml", OSC), 90);
        let listing = folder(dir.path()).list().unwrap();
        assert_eq!(names(&listing), ["new", "alpha", "Bravo", "old"]);
        assert!(!listing.truncated);
        assert_eq!(listing.entries[0].modified, Some(1_704_067_200 + 90));
        assert_eq!(listing.entries[0].id, "new.toml");
    }

    #[test]
    fn a_listing_is_ordered_newest_first_then_by_name_without_regard_to_case_then_by_id_and_what_has_no_date_is_last() {
        let entry = |id: &str, modified: Option<u64>| SoundEntry { id: id.to_string(), name: id.trim_end_matches(".toml").to_string(), detail: String::new(), openable: true, modified };
        let mut entries = [entry("b.toml", Some(5)), entry("a.toml", Some(5)), entry("A.toml", Some(5)), entry("old.toml", Some(1)), entry("unknown.toml", None), entry("new.toml", Some(9))];
        entries.sort_by(newest_first);
        assert_eq!(entries.iter().map(|each| each.id.as_str()).collect::<Vec<_>>(), ["new.toml", "A.toml", "a.toml", "b.toml", "old.toml", "unknown.toml"]);
    }

    #[test]
    fn a_folder_lists_what_is_in_the_folders_in_it_by_a_name_that_has_them_and_an_id_with_slashes() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "drums/kick.toml", OSC);
        write(dir.path(), "drums/deep/sub.toml", OSC);
        let listing = folder(dir.path()).list().unwrap();
        assert_eq!(names(&listing), ["drums/deep/sub", "drums/kick"]);
        assert_eq!(listing.entries.iter().map(|entry| entry.id.as_str()).collect::<Vec<_>>(), ["drums/deep/sub.toml", "drums/kick.toml"]);
    }

    #[test]
    fn a_folder_leaves_out_what_is_not_a_session_file_by_its_name_and_what_is_hidden() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.toml", OSC);
        write(dir.path(), "B.TOML", OSC);
        write(dir.path(), "c.Toml", OSC);
        write(dir.path(), "notes.txt", "hello");
        write(dir.path(), "toml", "no extension");
        write(dir.path(), ".hidden.toml", OSC);
        write(dir.path(), ".git/c.toml", OSC);
        write(dir.path(), "target/d.toml", OSC);
        write(dir.path(), "node_modules/e.toml", OSC);
        fs::create_dir_all(dir.path().join("folder.toml")).unwrap();
        let listing = folder(dir.path()).list().unwrap();
        assert_eq!(names(&listing), ["a", "B", "c"], "an extension in any case is one, and is not in the name");
    }

    #[test]
    fn a_folder_goes_only_a_few_folders_deep() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a/b/c/d/ok.toml", OSC);
        write(dir.path(), "a/b/c/d/e/too-deep.toml", OSC);
        let listing = folder(dir.path()).list().unwrap();
        assert_eq!(names(&listing), ["a/b/c/d/ok"]);
    }

    #[test]
    fn a_folder_that_has_more_than_it_lists_says_so() {
        let dir = tempfile::tempdir().unwrap();
        for at in 0..MAX_LISTED + 3 {
            write(dir.path(), &format!("s{at:04}.toml"), "");
        }
        let listing = folder(dir.path()).list().unwrap();
        assert_eq!(listing.entries.len(), MAX_LISTED);
        assert!(listing.truncated);
        write(dir.path(), "zz.toml", "");
        let exact = tempfile::tempdir().unwrap();
        for at in 0..MAX_LISTED {
            write(exact.path(), &format!("s{at:04}.toml"), "");
        }
        let listing = folder(exact.path()).list().unwrap();
        assert_eq!((listing.entries.len(), listing.truncated), (MAX_LISTED, false), "a folder that has exactly as many is not cut");
    }

    #[test]
    fn each_sound_says_what_is_in_it_or_why_it_cannot_be_opened() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "pad.toml", "[[instrument]]\nplugin = \"vst3:Helm\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n");
        write(dir.path(), "broken.toml", "this is {not toml\nsecond line\n");
        write(dir.path(), "empty.toml", "");
        fs::write(dir.path().join("binary.toml"), [0xff, 0xfe, 0x00, 0x9f]).unwrap();
        let listing = folder(dir.path()).list().unwrap();
        let by_name: HashMap<&str, &SoundEntry> = listing.entries.iter().map(|entry| (entry.name.as_str(), entry)).collect();
        assert_eq!((by_name["pad"].detail.as_str(), by_name["pad"].openable), ("Helm · 1 effect", true));
        assert_eq!((by_name["binary"].detail.as_str(), by_name["binary"].openable), ("cannot be read as text", false));
        assert!(!by_name["broken"].openable);
        assert!(by_name["broken"].detail.starts_with("not a session: TOML parse error at line 1"), "the first line of what is wrong, which says where: {}", by_name["broken"].detail);
        assert!(!by_name["broken"].detail.contains('\n'), "a line, not a quote of the file: {}", by_name["broken"].detail);
        assert_eq!((by_name["empty"].detail.as_str(), by_name["empty"].openable), ("no instrument", true), "a file that has nothing in it is an empty session");
    }

    #[test]
    fn a_large_session_is_listed_by_its_size_and_a_file_that_is_too_big_cannot_be_opened() {
        let dir = tempfile::tempdir().unwrap();
        let padding = |bytes: u64| format!("{OSC}# {}\n", "x".repeat(bytes as usize));
        write(dir.path(), "large.toml", &padding(MAX_DESCRIBED_BYTES + 10));
        write(dir.path(), "huge.toml", &padding(MAX_SOUND_BYTES + 10));
        let listing = folder(dir.path()).list().unwrap();
        let by_name: HashMap<&str, &SoundEntry> = listing.entries.iter().map(|entry| (entry.name.as_str(), entry)).collect();
        assert_eq!((by_name["large"].detail.as_str(), by_name["large"].openable), ("a large session (1.0 MB)", true));
        assert_eq!((by_name["huge"].detail.as_str(), by_name["huge"].openable), ("too big to open (8.0 MB)", false));
    }

    /// The text of a session that is exactly `bytes` long: an instrument, and a comment that makes up the rest.
    fn exactly(bytes: u64) -> String {
        let text = format!("{OSC}# {}\n", "x".repeat(bytes as usize - OSC.len() - "# \n".len()));
        assert_eq!(text.len() as u64, bytes);
        text
    }

    #[test]
    fn the_sizes_that_a_listing_reads_and_a_library_opens_are_held_to_the_byte() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "read.toml", &exactly(MAX_DESCRIBED_BYTES));
        write(dir.path(), "large.toml", &exactly(MAX_DESCRIBED_BYTES + 1));
        write(dir.path(), "most.toml", &exactly(MAX_SOUND_BYTES));
        write(dir.path(), "over.toml", &exactly(MAX_SOUND_BYTES + 1));
        let library = folder(dir.path());
        let listing = library.list().unwrap();
        let by_name: HashMap<&str, &SoundEntry> = listing.entries.iter().map(|entry| (entry.name.as_str(), entry)).collect();
        assert_eq!((by_name["read"].detail.as_str(), by_name["read"].openable), ("osc", true), "as many bytes as are read are read");
        assert_eq!((by_name["large"].detail.as_str(), by_name["large"].openable), ("a large session (1.0 MB)", true), "one more is a large session");
        assert_eq!((by_name["most"].detail.as_str(), by_name["most"].openable), ("a large session (8.0 MB)", true), "as many as can be opened can");
        assert_eq!((by_name["over"].detail.as_str(), by_name["over"].openable), ("too big to open (8.0 MB)", false), "one more cannot");
        assert_eq!(library.read("most.toml").unwrap().len() as u64, MAX_SOUND_BYTES);
        assert_eq!(library.read("over.toml").unwrap_err().code, ErrorCode::Invalid);
    }

    #[test]
    fn a_folder_that_is_not_there_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let missing = Folder::new("x", "X", dir.path().join("gone"));
        let error = missing.list().unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound);
        assert!(error.message.ends_with("it does not exist"), "{}", error.message);
        let file = write(dir.path(), "plain.toml", OSC);
        let error = Folder::new("x", "X", file).list().unwrap_err();
        assert!(error.message.ends_with("it is not a folder"), "{}", error.message);
    }

    #[test]
    fn the_folder_that_sessions_are_saved_to_is_an_empty_library_until_the_first_save_makes_it() {
        let dir = tempfile::tempdir().unwrap();
        let made_later = Folder::new("sessions", "Saved sessions", dir.path().join("sessions")).empty_until_made();
        assert_eq!(made_later.list().unwrap(), Listing { entries: vec![], truncated: false });
        write(&dir.path().join("sessions"), "first.toml", OSC);
        assert_eq!(names(&made_later.list().unwrap()), ["first"]);
        let file = write(dir.path(), "plain.toml", OSC);
        assert!(Folder::new("x", "X", file).empty_until_made().list().is_err(), "but a file where the folder should be is a fault");
    }

    #[test]
    fn a_sound_is_read_by_the_id_that_the_listing_gave() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "drums/kick.toml", OSC);
        let library = folder(dir.path());
        assert_eq!(library.read("drums/kick.toml").unwrap(), OSC);
    }

    #[test]
    fn what_is_not_a_sound_inside_the_folder_is_not_read() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("lib");
        write(&inside, "ok.toml", OSC);
        write(dir.path(), "secret.toml", "secret = true");
        write(&inside, "notes.txt", "text");
        let library = folder(&inside);
        for id in ["", "../secret.toml", "a/../../secret.toml", "/etc/passwd", "/etc/hosts.toml", "..\\secret.toml", "./ok.toml", "ok.toml/", "sub//ok.toml", "notes.txt", "ok", "C:/x.toml"] {
            let error = library.read(id).expect_err(id);
            assert!(matches!(error.code, ErrorCode::Invalid | ErrorCode::NotFound), "{id:?}: {error:?}");
        }
        assert_eq!(library.read("ok.toml").unwrap(), OSC, "and the one that is, is");
        fs::create_dir_all(inside.join("dir.toml")).unwrap();
        assert_eq!(library.read("dir.toml").unwrap_err().code, ErrorCode::Invalid, "a folder that is called as a sound is not one");
    }

    #[cfg(unix)]
    #[test]
    fn a_link_that_leads_out_of_the_folder_is_not_followed_but_one_that_stays_in_is() {
        let dir = tempfile::tempdir().unwrap();
        let inside = dir.path().join("lib");
        write(&inside, "real.toml", OSC);
        write(dir.path(), "secret.toml", "secret = true");
        std::os::unix::fs::symlink(dir.path().join("secret.toml"), inside.join("leak.toml")).unwrap();
        std::os::unix::fs::symlink(inside.join("real.toml"), inside.join("alias.toml")).unwrap();
        std::os::unix::fs::symlink(dir.path(), inside.join("up")).unwrap();
        let library = folder(&inside);
        assert_eq!(library.read("leak.toml").unwrap_err().code, ErrorCode::Invalid);
        assert_eq!(library.read("alias.toml").unwrap(), OSC);
        assert_eq!(library.read("up/secret.toml").unwrap_err().code, ErrorCode::Invalid);
        // (A link to a folder is not walked.)
        let listed = library.list().unwrap();
        assert!(names(&listed).iter().all(|name| !name.starts_with("up/")), "{:?}", names(&listed));
        std::os::unix::fs::symlink(dir.path(), inside.join("folder.toml")).unwrap();
        assert!(!names(&library.list().unwrap()).contains(&"folder"), "a link to a folder is no sound, though it is called as one");
    }

    #[test]
    fn a_sound_that_is_gone_or_too_big_or_not_text_is_said_so() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "big.toml", &"x".repeat(MAX_SOUND_BYTES as usize + 1));
        fs::write(dir.path().join("binary.toml"), [0xff, 0xfe, 0x00, 0x9f]).unwrap();
        let library = folder(dir.path());
        let gone = library.read("gone.toml").unwrap_err();
        assert_eq!(gone.code, ErrorCode::NotFound);
        assert!(gone.message.contains("any more"), "{}", gone.message);
        let big = library.read("big.toml").unwrap_err();
        assert_eq!(big.code, ErrorCode::Invalid);
        assert!(big.message.contains("too big to open (8.0 MB; the most is 8 MB)"), "{}", big.message);
        assert_eq!(library.read("binary.toml").unwrap_err().code, ErrorCode::Io);
        let missing = Folder::new("x", "X", dir.path().join("gone"));
        assert_eq!(missing.read("a.toml").unwrap_err().code, ErrorCode::Io);
    }

    #[test]
    fn a_session_is_described_by_its_plugins_its_effects_and_its_modulators() {
        let described = |text: &str| describe(&crate::session::parse(text).unwrap());
        assert_eq!(described(OSC), "osc");
        assert_eq!(described(""), "no instrument");
        assert_eq!(
            described("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n[[instrument.effect]]\nplugin = \"builtin:delay\"\n[[modulator]]\ntype = \"lfo\"\n"),
            "osc · 2 effects · 1 modulator",
            "a plugin that two lanes have is named once"
        );
        assert_eq!(described("[[modulator]]\ntype = \"lfo\"\n[[modulator]]\ntype = \"lfo\"\n"), "no instrument · 2 modulators");
        assert_eq!(
            described("[[instrument]]\nplugin = \"builtin:osc\"\ngroup = 0\n[[group]]\nname = \"bus\"\n[[group.effect]]\nplugin = \"builtin:reverb\"\n[[group.effect]]\nplugin = \"builtin:delay\"\n"),
            "osc · 2 effects",
            "the effects of a group's bus are the session's too"
        );
        assert_eq!(described("[[instrument]]\nplugin = \"builtin:osc\"\n[[instrument.effect]]\nplugin = \"builtin:reverb\"\n"), "osc · 1 effect");
        let four = ["a", "b", "c", "d", "e"].map(|name| format!("[[instrument]]\nplugin = \"builtin:{name}\"\n")).concat();
        assert_eq!(described(&four), "a, b, c and 2 more");
    }

    #[test]
    fn a_plugin_is_called_by_the_last_part_of_where_a_session_says_it_is() {
        for (spec, label) in [
            ("builtin:osc", "osc"),
            ("vst3:Pianoteq 9", "Pianoteq 9"),
            ("lv2:http://tytel.org/helm", "helm"),
            ("./plugins/Acid.clap", "Acid"),
            ("./Reverb.lv2", "Reverb"),
            ("C:\\Plugins\\Helm.VST3", "Helm"),
            ("clap:com.example.synth", "com.example.synth"),
            ("lv2:http://x.org/", "x.org"),
            ("", ""),
        ] {
            assert_eq!(plugin_label(spec), label, "{spec}");
        }
    }

    #[test]
    fn the_standard_libraries_are_the_sessions_folder_and_the_folders_that_the_config_names_with_the_names_it_gives() {
        let sounds = Sounds::standard(
            Some(PathBuf::from("/home/me/.config/tang/sessions")),
            vec![(Some("Examples".to_string()), PathBuf::from("/src/examples")), (None, PathBuf::from("/music/sounds")), (Some("  ".to_string()), PathBuf::from("/more/patches"))],
        );
        let infos = sounds.infos();
        assert_eq!(infos.iter().map(|library| (library.id.as_str(), library.label.as_str())).collect::<Vec<_>>(), [("sessions", "Saved sessions"), ("folder1", "Examples"), ("folder2", "sounds"), ("folder3", "patches")]);
        assert_eq!(infos[1].place, PathBuf::from("/src/examples").display().to_string());
        assert!(!sounds.is_empty());
        assert!(Sounds::standard(None, vec![]).is_empty(), "a program with no place for its sessions has none");
        let dir = tempfile::tempdir().unwrap();
        let missing = Sounds::standard(Some(dir.path().join("sessions")), vec![(None, dir.path().join("gone"))]);
        assert_eq!(missing.list("sessions").unwrap(), Listing { entries: vec![], truncated: false }, "the folder of saved sessions is not a fault before the first save makes it");
        assert_eq!(missing.list("folder1").unwrap_err().code, ErrorCode::NotFound, "a folder that the config names, and that is not there, is");
    }

    #[test]
    fn a_library_that_is_not_there_is_not_found() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "a.toml", OSC);
        let sounds = Sounds::new(vec![Box::new(folder(dir.path()))]);
        assert_eq!(sounds.list("here").unwrap().entries.len(), 1);
        assert_eq!(sounds.read("here", "a.toml").unwrap(), OSC);
        for error in [sounds.list("elsewhere").unwrap_err(), sounds.read("elsewhere", "a.toml").unwrap_err()] {
            assert_eq!(error.code, ErrorCode::NotFound);
            assert!(error.message.contains("elsewhere"), "{}", error.message);
        }
    }
}
