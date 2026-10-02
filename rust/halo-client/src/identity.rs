//! The player's identity, kept between sessions.
//!
//! A SpacetimeDB identity is made by a server for whoever asks and is proved
//! by a token. The client keeps the token in a file, one per SpacetimeDB it
//! has been to (an instance signs the tokens it gives with its own keys, so a
//! token is only good on the instance that gave it, and on any other that
//! shares its keys); the same file is read by the server-list connection and by
//! every match's, so that the player is the same identity on the root
//! database and in every match of one instance, in every session. That is
//! what a ban names, and what keeps a seat for a player who comes back.

use std::path::{Path, PathBuf};

/// Where one SpacetimeDB's token is kept.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct IdentityFile {
    path: Option<PathBuf>,
}

impl IdentityFile {
    /// The file for the SpacetimeDB at `spacetimedb` (its URI) in `folder`;
    /// with no folder nothing is kept, and each session is a new identity.
    pub fn new(folder: Option<&Path>, spacetimedb: &str) -> IdentityFile {
        IdentityFile { path: folder.map(|f| f.join(file_name(spacetimedb))) }
    }

    pub fn none() -> IdentityFile {
        IdentityFile { path: None }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The token kept, if there is one.
    pub fn load(&self) -> Option<String> {
        let token = std::fs::read_to_string(self.path.as_ref()?).ok()?;
        let token = token.trim();
        (!token.is_empty()).then(|| token.to_string())
    }

    /// Forget the token: the server does not accept it (see
    /// [`is_rejected_token`]), so the next connection is a new identity.
    pub fn forget(&self) {
        if let Some(path) = &self.path {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Keep the token (only for the owner of the file: it is the identity). A
    /// failure is not the session's: the identity is then only this session's.
    pub fn save(&self, token: &str) -> Result<(), String> {
        let Some(path) = &self.path else { return Ok(()) };
        if self.load().as_deref() == Some(token) {
            return Ok(());
        }
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
        }
        std::fs::write(path, format!("{token}\n")).map_err(|e| format!("{}: {e}", path.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }
}

/// Whether a failure to connect says the server did not accept the token
/// (HTTP 401): it was given by another instance, or by this one before its keys
/// were made again, and no connection with it ever works.
pub fn is_rejected_token(error: &str) -> bool {
    error.contains("401") || error.to_ascii_lowercase().contains("unauthorized")
}

/// `http://127.0.0.1:3000` as a file name: `http_127.0.0.1_3000.token`.
fn file_name(spacetimedb: &str) -> String {
    let readable: String = spacetimedb
        .trim_end_matches('/')
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
        .collect();
    format!("{readable}.token")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_kept_per_spacetimedb_and_read_back() {
        let folder = std::env::temp_dir().join(format!("halo-identity-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let here = IdentityFile::new(Some(&folder), "http://127.0.0.1:3000");
        let there = IdentityFile::new(Some(&folder), "https://play.example.org:443/");
        assert_eq!(here.load(), None, "nothing before a token is kept");
        here.save("token-one").unwrap();
        assert_eq!(here.load().as_deref(), Some("token-one"));
        assert_eq!(IdentityFile::new(Some(&folder), "http://127.0.0.1:3000").load().as_deref(), Some("token-one"));
        assert_eq!(there.load(), None, "another SpacetimeDB has its own");
        assert_ne!(here.path(), there.path());
        here.save("token-two").unwrap();
        assert_eq!(here.load().as_deref(), Some("token-two"));
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn without_a_folder_nothing_is_kept() {
        let none = IdentityFile::new(None, "http://127.0.0.1:3000");
        none.save("token").unwrap();
        assert_eq!(none.load(), None);
    }
}
