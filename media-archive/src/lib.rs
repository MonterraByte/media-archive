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

mod file_hash;

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use prae::Wrapper;
use relative_path::{PathExt, RelativePath, RelativePathBuf};
use thiserror::Error;
use tracing::{info, warn};

pub use crate::file_hash::FileHash;

const MEDIA_ARCHIVE_DIRECTORY: &str = ".media-archive";
const STORE_DIRECTORY: &str = "store";

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
    #[tracing::instrument(err)]
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
    #[must_use]
    fn get_path_of_stored_file(&self, hash: &FileHash) -> PathBuf {
        const SUBDIR_NAME_LEN: usize = 2;
        let subdir: &str = std::str::from_utf8(
            hash.as_bytes()
                .first_chunk::<SUBDIR_NAME_LEN>()
                .expect("string is at least 2 bytes"),
        )
        .expect("string is ASCII");

        let mut path = self.archive_path.clone();
        path.push(STORE_DIRECTORY);
        path.push(subdir);
        path.push::<&str>(hash.as_ref());
        path
    }

    /// Stores a file in the archive.
    ///
    /// Files in the archive are identified by their hash value, and this function will return
    /// this value after storing the file.
    ///
    /// If `move_file` is true, the file is moved instead.
    #[tracing::instrument(skip(self), err)]
    pub fn store_file(&self, path: &Path, move_file: bool) -> Result<FileHash, StoreFileError> {
        let metadata = path.symlink_metadata().map_err(StoreFileError::Metadata)?;
        if metadata.is_dir() {
            return Err(StoreFileError::IsDirectory);
        }
        if metadata.is_symlink() && move_file {
            return Err(StoreFileError::IsSymlink);
        }
        if metadata.is_symlink() && path.metadata().map_err(StoreFileError::Metadata)?.is_dir() {
            return Err(StoreFileError::IsDirectory);
        }

        let hash = {
            let file = File::open(path).map_err(StoreFileError::Open)?;
            let mut hasher = blake3::Hasher::new();
            hasher.update_reader(file).map_err(StoreFileError::Read)?;
            FileHash::new(hasher.finalize().to_hex()).expect("hash is a valid hash")
        };

        let target_path = self.get_path_of_stored_file(&hash);
        if target_path.exists() {
            return Err(StoreFileError::AlreadyExists(hash));
        }

        let parent = target_path.parent().expect("target path should have a parent");
        fs::create_dir_all(parent).map_err(StoreFileError::CreateParentDir)?;

        if move_file {
            fs::rename(path, &target_path).map_err(StoreFileError::Store)?;
        } else {
            reflink_copy::reflink_or_copy(path, &target_path).map_err(StoreFileError::Store)?;
        }

        match fs::metadata(&target_path) {
            Ok(metadata) => {
                let mut permissions = metadata.permissions();
                permissions.set_readonly(true);
                if let Err(err) = fs::set_permissions(&target_path, permissions) {
                    warn!("failed to set file '{}' as read only: {}", target_path.display(), err);
                }
            }
            Err(err) => warn!("failed to get metadata of file '{}': {}", target_path.display(), err),
        }

        info!("stored file successfully");
        Ok(hash)
    }

    /// Deploys a file with the given hash to the deployment directory.
    ///
    /// `target_path` is a relative path from the root of the deployment directory.
    #[tracing::instrument(skip(self), err)]
    pub fn deploy_file(
        &self,
        hash: &FileHash,
        target_path: &RelativePath,
        method: DeployMethod,
    ) -> Result<(), DeployError> {
        let deploy_path = self.deploy_path.as_ref().ok_or(DeployError::IsBareArchive)?;

        let target_path = {
            let full_target_path = target_path.to_logical_path(deploy_path);
            if !full_target_path.starts_with(deploy_path) || &full_target_path == deploy_path {
                return Err(DeployError::InvalidPath(target_path.to_owned()));
            }
            full_target_path
        };

        match target_path.symlink_metadata() {
            Ok(_) => return Err(DeployError::AlreadyExists(target_path)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (),
            Err(err) => {
                return Err(DeployError::Metadata {
                    path: target_path,
                    source: err,
                })
            }
        }

        let source_path = self.get_path_of_stored_file(hash);
        match source_path.symlink_metadata() {
            Ok(metadata) if !metadata.is_file() => {
                return Err(DeployError::SourceExistsButIsNotAFile(source_path));
            }
            Ok(_) => (),
            Err(err) => {
                return Err(DeployError::Metadata {
                    path: source_path,
                    source: err,
                })
            }
        }

        let parent = target_path.parent().expect("target path should have a parent");
        fs::create_dir_all(parent).map_err(DeployError::CreateParentDir)?;

        let result = match method {
            DeployMethod::Copy => reflink_copy::reflink_or_copy(&source_path, &target_path).and(Ok(())),
            DeployMethod::Symlink => {
                #[cfg(any(target_family = "windows", target_family = "unix"))]
                {
                    let relative_source_path = source_path
                        .relative_to(parent)
                        .map_err(|source| DeployError::SymlinkRelativePathConstruction {
                            source_path: source_path.clone(),
                            target_parent: parent.to_owned(),
                            source,
                        })?
                        .to_path("");

                    #[cfg(target_family = "unix")]
                    {
                        std::os::unix::fs::symlink(&relative_source_path, &target_path)
                    }
                    #[cfg(target_family = "windows")]
                    {
                        std::os::windows::fs::symlink_file(&relative_source_path, &target_path)
                    }
                }
                #[cfg(all(not(target_family = "windows"), not(target_family = "unix")))]
                return Err(DeployError::NotSupported);
            }
            DeployMethod::Hardlink => fs::hard_link(&source_path, &target_path),
        };

        match result {
            Ok(()) => {
                info!("deployed file successfully");
                Ok(())
            }
            Err(err) if err.kind() == io::ErrorKind::Unsupported => Err(DeployError::NotSupported),
            Err(err) => Err(DeployError::Deploy {
                from: source_path,
                to: target_path,
                source: err,
            }),
        }
    }
}

#[derive(Copy, Clone, Debug)]
pub enum DeployMethod {
    /// The file is copied to the destination.
    Copy,
    /// The file is symlinked to the destination.
    Symlink,
    /// The file is hardlinked to the destination.
    Hardlink,
}

#[derive(Debug, Error)]
pub enum OpenMediaArchiveError {
    #[error("failed to create base directory: {0}")]
    CreateDir(#[source] io::Error),
}

#[derive(Debug, Error)]
pub enum StoreFileError {
    #[error("file with hash '{0}' already exists")]
    AlreadyExists(FileHash),
    #[error("cannot store a directory")]
    IsDirectory,
    #[error("cannot store a symlink")]
    IsSymlink,
    #[error("failed to get file metadata: {0}")]
    Metadata(#[source] io::Error),
    #[error("failed to create parent directory: {0}")]
    CreateParentDir(#[source] io::Error),
    #[error("failed to open file for hashing: {0}")]
    Open(#[source] io::Error),
    #[error("failed to read file while hashing: {0}")]
    Read(#[source] io::Error),
    #[error("failed to store file: {0}")]
    Store(#[source] io::Error),
}

#[derive(Debug, Error)]
pub enum DeployError {
    #[error("'{0}' already exists")]
    AlreadyExists(PathBuf),
    #[error("failed to create parent directory: {0}")]
    CreateParentDir(#[source] io::Error),
    #[error("failed to deploy file '{from}' to '{to}': {source}")]
    Deploy {
        from: PathBuf,
        to: PathBuf,
        source: io::Error,
    },
    #[error("target path '{0}' is empty, not relative, or outside of the media archive")]
    InvalidPath(RelativePathBuf),
    #[error("cannot deploy in bare media archive")]
    IsBareArchive,
    #[error("failed to get file metadata of file '{path}': {source}")]
    Metadata { path: PathBuf, source: io::Error },
    #[error("file with hash '{0}' not found in the archive")]
    NotFound(FileHash),
    #[error("deployment method not supported by the operating system or file system")]
    NotSupported,
    #[error("source '{0}' exists but is not a file")]
    SourceExistsButIsNotAFile(PathBuf),
    #[error("failed to construct relative path from the symlink target to its source")]
    SymlinkRelativePathConstruction {
        source_path: PathBuf,
        target_parent: PathBuf,
        source: relative_path::RelativeToError,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn test_media_archive() -> MediaArchive {
        MediaArchive {
            archive_path: PathBuf::from("."),
            deploy_path: None,
        }
    }

    fn test_non_bare_media_archive() -> MediaArchive {
        MediaArchive {
            archive_path: PathBuf::from("archive"),
            deploy_path: Some(PathBuf::from("deploy")),
        }
    }

    #[test]
    fn path_of_stored_file() -> Result<(), file_hash::FromStrError> {
        let archive = test_media_archive();

        let hash_str = "0011223344556677889900aabbccddeeff0011223344556677889900aabbccdd";
        let hash = FileHash::from_str(hash_str)?;

        let path = archive.get_path_of_stored_file(&hash);
        let expected: PathBuf = [".", STORE_DIRECTORY, "00", hash_str].iter().collect();
        assert_eq!(path, expected);
        Ok(())
    }

    #[test]
    fn deploy_path_bare_archive() {
        let archive = test_media_archive();
        assert!(matches!(
            archive.deploy_file(&FileHash::zero_filled(), RelativePath::new("test"), DeployMethod::Copy),
            Err(DeployError::IsBareArchive)
        ));
    }

    #[test]
    fn deploy_path_empty_path() {
        let archive = test_non_bare_media_archive();
        assert!(matches!(
            archive.deploy_file(&FileHash::zero_filled(), RelativePath::new(""), DeployMethod::Copy),
            Err(DeployError::InvalidPath(_))
        ));
    }

    #[test]
    fn deploy_path_current_dir() {
        let archive = test_non_bare_media_archive();
        assert!(matches!(
            archive.deploy_file(&FileHash::zero_filled(), RelativePath::new("."), DeployMethod::Copy),
            Err(DeployError::InvalidPath(_))
        ));
    }

    #[test]
    fn deploy_path_escape() {
        let archive = test_non_bare_media_archive();
        assert!(matches!(
            archive.deploy_file(
                &FileHash::zero_filled(),
                RelativePath::new("test/../../important-file"),
                DeployMethod::Copy
            ),
            Err(DeployError::InvalidPath(_))
        ));
    }
}
