//! What the plugin gives a host to keep in its project, and takes back: the session, as the text of a session file (which has what the plugins inside keep in them), after a few bytes that say what it is. A host keeps
//! the bytes as they are and gives them back to the same plugin, possibly a version of it that is years later: the first bytes say which format they are, so that a later one can read this and a mistake can be told from a session.

/// What the bytes begin with: "Tang session".
const MAGIC: &[u8; 8] = b"TANGSESS";

/// The version of the format that follows the magic; 1 is the session's text, in UTF-8.
const VERSION: u8 = 1;

/// The most that is taken as a session (a project that is more has a session that is not).
pub const MAX_BYTES: usize = 512 * 1024 * 1024;

/// The bytes that a host keeps for the session `text`.
pub fn encode(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(MAGIC.len() + 1 + text.len());
    bytes.extend_from_slice(MAGIC);
    bytes.push(VERSION);
    bytes.extend_from_slice(text.as_bytes());
    bytes
}

/// The session that `bytes` are, or why they are not one. Bytes that have no magic and are text are taken as a session file as it is (a person who pastes one into a project gets a plugin that has it).
pub fn decode(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() > MAX_BYTES {
        return Err(format!("the state is {} bytes, more than the {MAX_BYTES} that is taken", bytes.len()));
    }
    let Some(rest) = bytes.strip_prefix(MAGIC) else {
        return String::from_utf8(bytes.to_vec()).map_err(|_| "the state is not a session of Tang's".to_string());
    };
    match rest.split_first() {
        Some((&VERSION, text)) => String::from_utf8(text.to_vec()).map_err(|_| "the session in the state is not text".to_string()),
        Some((version, _)) => Err(format!("the state is of version {version}, which this Tang does not know (it knows {VERSION}): it was made by a later one")),
        None => Err("the state is cut short".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_comes_back_as_it_went() {
        for text in ["", "[[instrument]]\nplugin = \"builtin:osc\"\n", "åäö — ✓\n"] {
            assert_eq!(decode(&encode(text)).as_deref(), Ok(text));
        }
    }

    #[test]
    fn the_bytes_say_what_they_are_first() {
        let bytes = encode("x");
        assert_eq!(&bytes[..8], b"TANGSESS");
        assert_eq!(bytes[8], 1);
        assert_eq!(&bytes[9..], b"x");
    }

    #[test]
    fn a_session_file_as_it_is_is_taken_for_one() {
        assert_eq!(decode(b"[[instrument]]\nplugin = \"builtin:osc\"\n").as_deref(), Ok("[[instrument]]\nplugin = \"builtin:osc\"\n"));
        assert!(decode(&[0xFF, 0xFE, 0x00]).is_err(), "bytes that are neither");
    }

    #[test]
    fn a_later_version_and_a_cut_state_are_refused_with_the_reason() {
        let mut later = encode("x");
        later[8] = 2;
        assert!(decode(&later).unwrap_err().contains("version 2"));
        assert!(decode(b"TANGSESS").unwrap_err().contains("cut short"));
        let mut bad = encode("");
        bad.push(0xFF);
        assert!(decode(&bad).is_err(), "text that is not UTF-8");
    }
}
