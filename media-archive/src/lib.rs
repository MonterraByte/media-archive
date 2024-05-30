// Copyright © 2024 Joaquim Monteiro
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as
// published by the Free Software Foundation, either version 3 of the
// License, or (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

#![forbid(unsafe_code)]

use std::fs;
use std::io;
use std::path::PathBuf;

use thiserror::Error;

const MEDIA_ARCHIVE_DIRECTORY: &str = ".media-archive";
const STORE_DIRECTORY: &str = "store";

const HASH_HEX_LEN: usize = blake3::OUT_LEN * 2;
type HashString = arrayvec::ArrayString<HASH_HEX_LEN>;

#[derive(Debug)]
pub struct MediaArchive {
    archive_path: PathBuf,
    deploy_path: Option<PathBuf>,
}

impl MediaArchive {
    /// Opens a directory as a media archive.
    ///
    /// The directory will be created if it doesn't already exist,
    /// and so will any needed media archive files.
    ///
    /// By default, an archive directory will be created inside `path`,
    /// and `path` will be the directory where media files are deployed to.
    /// If `bare` is true, no media files will be deployed, and `path`
    /// will be treated as the archive directory (similar to Git's bare repositories).
    pub fn open(path: PathBuf, bare: bool) -> Result<Self, OpenMediaArchiveError> {
        let (archive_path, deploy_path) = if bare {
            (path, None)
        } else {
            (path.join(MEDIA_ARCHIVE_DIRECTORY), Some(path))
        };
        fs::create_dir_all(&archive_path).map_err(OpenMediaArchiveError::CreateDir)?;

        Ok(Self {
            archive_path,
            deploy_path,
        })
    }

    /// Returns the path to a stored file from its hash.
    ///
    /// The file does not need to exist.
    ///
    /// # Panics
    ///
    /// Panics if `hash` is not a hex-encoded Blake3 hash.
    #[must_use]
    fn get_path_of_stored_file(&self, hash: &str) -> PathBuf {
        if hash.len() != HASH_HEX_LEN {
            panic!(
                "string is length {}, should be {}",
                hash.len(),
                HASH_HEX_LEN
            );
        }

        for ch in hash.bytes() {
            match ch {
                b'0'..=b'9' | b'a'..=b'f' | b'A'..=b'F' => (),
                other => panic!("string has non hex character '0x{:x}'", other),
            }
        }

        let hash_lower = hash.to_ascii_lowercase();

        const SUBDIR_NAME_LEN: usize = 2;
        let subdir: &str = std::str::from_utf8(
            hash_lower
                .as_bytes()
                .first_chunk::<SUBDIR_NAME_LEN>()
                .expect("string is at least 2 bytes"),
        )
        .expect("string is ASCII");

        let mut path = self.archive_path.clone();
        path.push(STORE_DIRECTORY);
        path.push(subdir);
        path.push(hash_lower);
        path
    }
}

#[derive(Debug, Error)]
pub enum OpenMediaArchiveError {
    #[error("failed to create base directory: {0}")]
    CreateDir(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_media_archive() -> MediaArchive {
        MediaArchive {
            archive_path: PathBuf::from("."),
            deploy_path: None,
        }
    }

    #[test]
    fn path_of_stored_file() {
        let archive = test_media_archive();

        let hash = "0011223344556677889900aabbccddeeff0011223344556677889900aabbccdd";
        let path = archive.get_path_of_stored_file(hash);
        let expected: PathBuf = [".", STORE_DIRECTORY, "00", hash].iter().collect();
        assert_eq!(path, expected);
    }

    #[test]
    fn path_of_stored_file_uppercase() {
        let archive = test_media_archive();

        let hash = "0011223344556677889900AABBCCDDEEFF0011223344556677889900aabbccdd";
        let path = archive.get_path_of_stored_file(hash);
        let expected: PathBuf = [".", STORE_DIRECTORY, "00", &hash.to_ascii_lowercase()]
            .iter()
            .collect();
        assert_eq!(path, expected);
    }

    #[should_panic]
    #[test]
    fn path_of_stored_file_length() {
        let archive = test_media_archive();
        let _ = archive.get_path_of_stored_file("001122");
    }

    #[should_panic]
    #[test]
    fn path_of_stored_file_non_hex() {
        let archive = test_media_archive();
        let _ = archive.get_path_of_stored_file(
            "あxyz3344556677889900AABBCCDDEEFF0011223344556677889900aabbccdd",
        );
    }
}
