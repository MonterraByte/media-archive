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

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};

use relative_path::{PathExt, RelativePath, RelativePathBuf};
use thiserror::Error;
use tracing::{info, warn};

pub use blake3::Hash;

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
    /// If `disk_structure` is [`DiskStructure::Deployable`], an archive directory will be created inside `path`,
    /// and `path` will be the directory where media files are deployed to.
    /// If `disk_structure` is [`DiskStructure::Bare`], no media files will be deployed, and `path`
    /// will be treated as the archive directory (similar to Git's bare repositories).
    #[tracing::instrument(err)]
    pub fn open(path: PathBuf, disk_structure: DiskStructure) -> Result<Self, OpenMediaArchiveError> {
        let (archive_path, deploy_path) = match disk_structure {
            DiskStructure::Bare => (path, None),
            DiskStructure::Deployable => (path.join(MEDIA_ARCHIVE_DIRECTORY), Some(path)),
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
    fn get_path_of_stored_file(&self, hash: &Hash) -> PathBuf {
        const SUBDIR_COUNT: usize = 2;
        const SUBDIR_NAME_LEN: usize = 2;
        const _: () = assert!(SUBDIR_COUNT * SUBDIR_NAME_LEN <= blake3::OUT_LEN * 2);

        let hash = hash.to_hex();
        let mut path = self.archive_path.clone();
        path.push(STORE_DIRECTORY);

        let mut subdir_name_iterator = hash
            .as_bytes()
            .chunks_exact(SUBDIR_NAME_LEN)
            .map(|chunk| std::str::from_utf8(chunk).expect("string is ASCII"));

        for _ in 0..SUBDIR_COUNT {
            let subdir = subdir_name_iterator.next().expect("hash length is big enough");
            path.push(subdir);
        }

        path.push(hash.as_str());
        path
    }

    /// Stores a file in the archive.
    ///
    /// Files in the archive are identified by their hash value, and this function will return
    /// this value after storing the file.
    #[tracing::instrument(skip(self), err)]
    pub fn store_file(&self, path: &Path, method: StoreMethod) -> Result<Hash, StoreFileError> {
        let metadata = path.symlink_metadata().map_err(StoreFileError::Metadata)?;
        if metadata.is_dir() {
            return Err(StoreFileError::IsDirectory);
        }
        if metadata.is_symlink() && method == StoreMethod::Move {
            return Err(StoreFileError::IsSymlink);
        }
        if metadata.is_symlink() && path.metadata().map_err(StoreFileError::Metadata)?.is_dir() {
            return Err(StoreFileError::IsDirectory);
        }

        let hash = {
            let file = File::open(path).map_err(StoreFileError::Open)?;
            let mut hasher = blake3::Hasher::new();
            hasher.update_reader(file).map_err(StoreFileError::Read)?;
            hasher.finalize()
        };

        let target_path = self.get_path_of_stored_file(&hash);
        if target_path.exists() {
            return Err(StoreFileError::AlreadyExists(hash));
        }

        let parent = target_path.parent().expect("target path should have a parent");
        fs::create_dir_all(parent).map_err(StoreFileError::CreateParentDir)?;

        match method {
            StoreMethod::Copy => {
                reflink_copy::reflink_or_copy(path, &target_path).map_err(StoreFileError::Store)?;
            }
            StoreMethod::Move => {
                fs::rename(path, &target_path).map_err(StoreFileError::Store)?;
            }
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
        hash: &Hash,
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
pub enum DiskStructure {
    /// A media archive that doesn't support deploying files.
    Bare,
    /// A media archive that supports deploying files for offline browsing.
    ///
    /// The base directory will be to where files are deployed,
    /// and the media archive's files will be stored in a subdirectory.
    Deployable,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StoreMethod {
    /// The file is copied to the store.
    Copy,
    /// The file is moved to the store.
    Move,
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
    AlreadyExists(Hash),
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
    NotFound(Hash),
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

    use assert_fs::fixture::ChildPath;
    use assert_fs::prelude::*;
    use assert_fs::{NamedTempFile, TempDir};
    use file_id::get_file_id;
    use predicates::prelude::*;

    fn temp_media_archive(disk_structure: DiskStructure) -> (TempDir, MediaArchive) {
        let temp_dir = TempDir::new().expect("failed to create temporary directory for test");
        let archive = MediaArchive::open(temp_dir.to_path_buf(), disk_structure).expect("failed to open media archive");
        (temp_dir, archive)
    }

    #[test]
    fn create_media_archive() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);

        assert_eq!(archive.deploy_path.as_deref(), Some(temp_dir.path()));
        assert_eq!(archive.archive_path, temp_dir.child(MEDIA_ARCHIVE_DIRECTORY).path());

        temp_dir
            .child(MEDIA_ARCHIVE_DIRECTORY)
            .assert(predicate::path::is_dir());
    }

    #[test]
    fn create_bare_media_archive() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Bare);

        assert_eq!(archive.deploy_path, None);
        assert_eq!(archive.archive_path, temp_dir.path());

        temp_dir
            .child(MEDIA_ARCHIVE_DIRECTORY)
            .assert(predicate::path::missing());
    }

    #[test]
    fn path_of_stored_file() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Bare);

        let hash_str = "0011223344556677889900aabbccddeeff0011223344556677889900aabbccdd";
        let hash = Hash::from_hex(hash_str).unwrap();

        let path = archive.get_path_of_stored_file(&hash);
        let expected = {
            let mut path = temp_dir.to_path_buf();
            path.push(STORE_DIRECTORY);
            path.push("00");
            path.push("11");
            path.push(hash_str);
            path
        };
        assert_eq!(path, expected);
    }

    const TEST_DATA: &str = "test data";
    const TEST_DATA_HASH: &str = "6a953581d60dbebc9749b56d2383277fb02b58d260b4ccf6f119108fa0f1d4ef";
    const ZERO_HASH: Hash = Hash::from_bytes([0; 32]);

    #[allow(clippy::type_complexity)]
    fn store_file_first_part(
        method: StoreMethod,
        setup_fn: Option<Box<dyn Fn(&TempDir, &NamedTempFile)>>,
    ) -> (NamedTempFile, Result<(), StoreFileError>) {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Bare);

        let file_to_store = NamedTempFile::new("test.txt").unwrap();
        file_to_store.write_str(TEST_DATA).unwrap();

        if let Some(setup_fn) = setup_fn {
            setup_fn(&temp_dir, &file_to_store);
        }

        let returned_hash = match archive.store_file(file_to_store.path(), method) {
            Ok(hash) => hash,
            Err(err) => return (file_to_store, Err(err)),
        };
        assert_eq!(returned_hash.to_hex().as_str(), TEST_DATA_HASH);

        let stored_file = temp_dir
            .child(STORE_DIRECTORY)
            .child("6a")
            .child("95")
            .child(TEST_DATA_HASH);

        let stored_file_metadata = stored_file.symlink_metadata().unwrap();
        assert!(stored_file_metadata.is_file());
        assert!(!stored_file_metadata.is_symlink());
        assert!(stored_file_metadata.permissions().readonly());

        stored_file.assert(TEST_DATA);

        (file_to_store, Ok(()))
    }

    #[test]
    fn store_file_copy() {
        let (file_to_store, result) = store_file_first_part(StoreMethod::Copy, None);
        assert!(matches!(result, Ok(())));
        file_to_store.assert(predicate::path::exists());
    }

    #[test]
    fn store_file_move() {
        let (file_to_store, result) = store_file_first_part(StoreMethod::Move, None);
        assert!(matches!(result, Ok(())));
        file_to_store.assert(predicate::path::missing());
    }

    #[test]
    fn store_file_already_exists() {
        let (file_to_store, result) = store_file_first_part(
            StoreMethod::Move,
            Some(Box::new(|temp_dir, _| {
                temp_dir
                    .child(STORE_DIRECTORY)
                    .child("6a")
                    .child("95")
                    .child(TEST_DATA_HASH)
                    .touch()
                    .unwrap();
            })),
        );
        assert!(matches!(result, Err(StoreFileError::AlreadyExists(_))));
        file_to_store.assert(predicate::path::exists());
    }

    #[test]
    fn store_file_is_directory() {
        let (_, result) = store_file_first_part(
            StoreMethod::Copy,
            Some(Box::new(|_, file_to_store| {
                fs::remove_file(file_to_store).unwrap();
                fs::create_dir(file_to_store).unwrap();
            })),
        );
        assert!(matches!(result, Err(StoreFileError::IsDirectory)));
    }

    #[test]
    fn store_file_is_directory_behind_symlink() {
        let (_, result) = store_file_first_part(
            StoreMethod::Copy,
            Some(Box::new(|_, file_to_store| {
                let symlink_target_path = file_to_store.parent().unwrap().join("actual_file.txt");
                fs::remove_file(file_to_store).unwrap();
                fs::create_dir(&symlink_target_path).unwrap();
                #[cfg(target_family = "unix")]
                std::os::unix::fs::symlink(&symlink_target_path, file_to_store).unwrap();
                #[cfg(target_family = "windows")]
                std::os::windows::fs::symlink_dir(&symlink_target_path, file_to_store).unwrap();
                #[cfg(not(any(target_family = "unix", target_family = "windows")))]
                unimplemented!();
            })),
        );
        assert!(matches!(result, Err(StoreFileError::IsDirectory)));
    }

    fn store_file_symlink_setup(_: &TempDir, file_to_store: &NamedTempFile) {
        let symlink_target_path = file_to_store.parent().unwrap().join("actual_file.txt");
        fs::rename(file_to_store, &symlink_target_path).unwrap();
        #[cfg(target_family = "unix")]
        std::os::unix::fs::symlink(&symlink_target_path, file_to_store).unwrap();
        #[cfg(target_family = "windows")]
        std::os::windows::fs::symlink_file(&symlink_target_path, file_to_store).unwrap();
        #[cfg(not(any(target_family = "unix", target_family = "windows")))]
        unimplemented!();
    }

    #[test]
    #[cfg(any(target_family = "unix", target_family = "windows"))]
    fn store_file_copy_symlink() {
        let (_, result) = store_file_first_part(StoreMethod::Copy, Some(Box::new(store_file_symlink_setup)));
        assert!(matches!(result, Ok(())));
    }

    #[test]
    fn store_file_move_symlink() {
        let (_, result) = store_file_first_part(StoreMethod::Move, Some(Box::new(store_file_symlink_setup)));
        assert!(matches!(result, Err(StoreFileError::IsSymlink)));
    }

    #[test]
    fn deploy_file_bare_archive() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Bare);

        assert!(matches!(
            archive.deploy_file(&ZERO_HASH, RelativePath::new("test"), DeployMethod::Copy),
            Err(DeployError::IsBareArchive)
        ));
        temp_dir.child("test").assert(predicate::path::missing());
    }

    fn deploy_file_first_part(
        temp_dir: &TempDir,
        archive: &MediaArchive,
        method: DeployMethod,
    ) -> Result<(ChildPath, ChildPath), DeployError> {
        let stored_file = temp_dir
            .child(MEDIA_ARCHIVE_DIRECTORY)
            .child(STORE_DIRECTORY)
            .child("00/00/0000000000000000000000000000000000000000000000000000000000000000");
        stored_file.write_str(TEST_DATA).unwrap();

        const DEPLOY_PATH: &str = "a/b/c";
        archive.deploy_file(&ZERO_HASH, RelativePath::new(DEPLOY_PATH), method)?;

        let deployed_file = temp_dir.child(DEPLOY_PATH);
        deployed_file.assert(TEST_DATA);

        Ok((stored_file, deployed_file))
    }

    #[test]
    fn deploy_file_copy() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);
        let (stored_file, deployed_file) =
            deploy_file_first_part(&temp_dir, &archive, DeployMethod::Copy).expect("failed to deploy file");

        let deployed_file_metadata = deployed_file.symlink_metadata().unwrap();
        assert!(!deployed_file_metadata.is_symlink());

        #[cfg(any(target_family = "windows", target_family = "unix"))]
        {
            let stored_file_id = get_file_id(stored_file).unwrap();
            let deployed_file_id = get_file_id(deployed_file).unwrap();
            assert_ne!(stored_file_id, deployed_file_id);
        }
    }

    #[test]
    fn deploy_file_hardlink() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);
        let (stored_file, deployed_file) =
            deploy_file_first_part(&temp_dir, &archive, DeployMethod::Hardlink).expect("failed to deploy file");

        let deployed_file_metadata = deployed_file.symlink_metadata().unwrap();
        assert!(!deployed_file_metadata.is_symlink());

        #[cfg(any(target_family = "windows", target_family = "unix"))]
        {
            let stored_file_id = get_file_id(stored_file).unwrap();
            let deployed_file_id = get_file_id(deployed_file).unwrap();
            assert_eq!(stored_file_id, deployed_file_id);
        }
    }

    #[test]
    fn deploy_file_symlink() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);
        let (_, deployed_file) = match deploy_file_first_part(&temp_dir, &archive, DeployMethod::Symlink) {
            Ok((s, d)) => (s, d),
            Err(DeployError::NotSupported) => {
                // Platform doesn't support symlinks, test should be skipped.
                // However, Rust doesn't support skipping tests, so we'll just return.
                return;
            }
            Err(err) => panic!("failed to deploy file: {}", err),
        };

        let deployed_file_metadata = deployed_file.symlink_metadata().unwrap();
        assert!(deployed_file_metadata.is_symlink());
    }

    #[test]
    fn deploy_file_already_exists() {
        let (temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);
        temp_dir.child("a/b/c").touch().unwrap();

        assert!(matches!(
            deploy_file_first_part(&temp_dir, &archive, DeployMethod::Copy),
            Err(DeployError::AlreadyExists(_))
        ));
    }

    #[test]
    fn deploy_file_empty_path() {
        let (_temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);
        assert!(matches!(
            archive.deploy_file(&ZERO_HASH, RelativePath::new(""), DeployMethod::Copy),
            Err(DeployError::InvalidPath(_))
        ));
    }

    #[test]
    fn deploy_file_current_dir() {
        let (_temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);
        assert!(matches!(
            archive.deploy_file(&ZERO_HASH, RelativePath::new("."), DeployMethod::Copy),
            Err(DeployError::InvalidPath(_))
        ));
    }

    #[test]
    fn deploy_file_escape() {
        let (_temp_dir, archive) = temp_media_archive(DiskStructure::Deployable);
        assert!(matches!(
            archive.deploy_file(
                &ZERO_HASH,
                RelativePath::new("test/../../important-file"),
                DeployMethod::Copy
            ),
            Err(DeployError::InvalidPath(_))
        ));
    }
}
