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

#[derive(Debug)]
pub struct MediaArchive {
    archive_path: PathBuf,
    deploy_path: PathBuf,
}

impl MediaArchive {
    /// Opens a directory as a media archive.
    ///
    /// The directory will be created if it doesn't already exist,
    /// and so will any needed media archive files.
    pub fn open(path: PathBuf) -> Result<Self, OpenMediaArchiveError> {
        let archive_path = path.join(MEDIA_ARCHIVE_DIRECTORY);
        fs::create_dir_all(&archive_path).map_err(OpenMediaArchiveError::CreateDir)?;

        Ok(Self {
            archive_path,
            deploy_path: path,
        })
    }
}

#[derive(Debug, Error)]
pub enum OpenMediaArchiveError {
    #[error("failed to create base directory: {0}")]
    CreateDir(#[source] io::Error),
}
