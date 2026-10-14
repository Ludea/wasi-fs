use std::collections::VecDeque;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;

use wasip3::filesystem::types::{DescriptorFlags, OpenFlags, PathFlags};

use crate::file::{File, OpenOptions};
use crate::metadata::{FileType, Metadata};
use crate::sys::{self, io_err};

pub async fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    let fd = sys::open(path.as_ref(), true, OpenFlags::empty(), DescriptorFlags::READ).await?;
    let mut out = Vec::new();
    sys::read_all(&fd, 0, &mut out).await?;
    Ok(out)
}

pub async fn read_to_string(path: impl AsRef<Path>) -> io::Result<String> {
    let bytes = read(path).await?;
    String::from_utf8(bytes)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "stream did not contain valid UTF-8"))
}

pub async fn write(path: impl AsRef<Path>, contents: impl AsRef<[u8]>) -> io::Result<()> {
    let mut file = File::create(path).await?;
    file.write_all(contents.as_ref()).await
}

pub async fn copy(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<u64> {
    let data = read(from).await?;
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).open(to).await?;
    file.write_all(&data).await?;
    Ok(data.len() as u64)
}

pub async fn metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    stat(path.as_ref(), true).await
}

pub async fn symlink_metadata(path: impl AsRef<Path>) -> io::Result<Metadata> {
    stat(path.as_ref(), false).await
}

async fn stat(path: &Path, follow: bool) -> io::Result<Metadata> {
    let (dir, rel) = sys::resolve(path)?;
    let flags = if follow { PathFlags::SYMLINK_FOLLOW } else { PathFlags::empty() };
    let s = dir.stat_at(flags, rel).await.map_err(io_err)?;
    Ok(Metadata::from_stat(&s))
}

pub async fn try_exists(path: impl AsRef<Path>) -> io::Result<bool> {
    match metadata(path).await {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Lexical normalization only: WASI has no realpath, so symlinks are not resolved.
/// Fails with `NotFound` if the path does not exist.
pub async fn canonicalize(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    metadata(path.as_ref()).await?;
    sys::normalize(path.as_ref())
}

pub async fn create_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let (dir, rel) = sys::resolve(path.as_ref())?;
    dir.create_directory_at(rel).await.map_err(io_err)
}

pub async fn create_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    let (abs, comps) = sys::components(sys::path_str(path.as_ref())?);
    let mut cur = PathBuf::from(if abs { "/" } else { "" });
    for c in comps {
        cur.push(c);
        if let Err(e) = create_dir(&cur).await {
            // Existing directories (including the preopen root itself) are fine.
            let is_dir = metadata(&cur).await.map(|m| m.is_dir()).unwrap_or(false);
            if !is_dir {
                return Err(e);
            }
        }
    }
    Ok(())
}

pub async fn remove_file(path: impl AsRef<Path>) -> io::Result<()> {
    let (dir, rel) = sys::resolve(path.as_ref())?;
    dir.unlink_file_at(rel).await.map_err(io_err)
}

pub async fn remove_dir(path: impl AsRef<Path>) -> io::Result<()> {
    let (dir, rel) = sys::resolve(path.as_ref())?;
    dir.remove_directory_at(rel).await.map_err(io_err)
}

pub async fn remove_dir_all(path: impl AsRef<Path>) -> io::Result<()> {
    remove_dir_all_inner(path.as_ref().to_path_buf()).await
}

fn remove_dir_all_inner(path: PathBuf) -> Pin<Box<dyn Future<Output = io::Result<()>>>> {
    Box::pin(async move {
        let meta = symlink_metadata(&path).await?;
        if meta.is_symlink() {
            return remove_file(&path).await;
        }
        if !meta.is_dir() {
            return Err(io::Error::new(io::ErrorKind::NotADirectory, "not a directory"));
        }
        let mut entries = read_dir(&path).await?;
        while let Some(entry) = entries.next_entry().await? {
            remove_dir_all_inner(entry.path()).await?;
        }
        remove_dir(&path).await
    })
}

pub async fn rename(from: impl AsRef<Path>, to: impl AsRef<Path>) -> io::Result<()> {
    let (old_dir, old_rel) = sys::resolve(from.as_ref())?;
    let (new_dir, new_rel) = sys::resolve(to.as_ref())?;
    old_dir.rename_at(old_rel, &new_dir, new_rel).await.map_err(io_err)
}

pub async fn hard_link(original: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    let (old_dir, old_rel) = sys::resolve(original.as_ref())?;
    let (new_dir, new_rel) = sys::resolve(link.as_ref())?;
    old_dir
        .link_at(PathFlags::empty(), old_rel, &new_dir, new_rel)
        .await
        .map_err(io_err)
}

/// `original` is stored as-is in the link, like `symlink(2)`.
pub async fn symlink(original: impl AsRef<Path>, link: impl AsRef<Path>) -> io::Result<()> {
    let target = sys::path_str(original.as_ref())?.to_string();
    let (dir, rel) = sys::resolve(link.as_ref())?;
    dir.symlink_at(target, rel).await.map_err(io_err)
}

pub async fn read_link(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    let (dir, rel) = sys::resolve(path.as_ref())?;
    dir.readlink_at(rel).await.map(PathBuf::from).map_err(io_err)
}

/// Lists a directory. Entries are fetched eagerly when the call completes.
pub async fn read_dir(path: impl AsRef<Path>) -> io::Result<ReadDir> {
    let dir_path = path.as_ref().to_path_buf();
    let fd = sys::open(
        &dir_path,
        true,
        OpenFlags::DIRECTORY,
        DescriptorFlags::READ,
    )
    .await?;

    let (mut rx, result) = fd.read_directory();
    let mut entries = VecDeque::new();
    loop {
        let (_, batch) = rx.read(Vec::with_capacity(32)).await;
        if batch.is_empty() {
            break;
        }
        for e in batch {
            entries.push_back((e.name, FileType::from_descriptor(&e.type_)));
        }
    }
    result.await.map_err(io_err)?;

    Ok(ReadDir { dir: dir_path, entries })
}

pub struct ReadDir {
    dir: PathBuf,
    entries: VecDeque<(String, FileType)>,
}

impl ReadDir {
    pub async fn next_entry(&mut self) -> io::Result<Option<DirEntry>> {
        Ok(self.entries.pop_front().map(|(name, file_type)| DirEntry {
            dir: self.dir.clone(),
            name,
            file_type,
        }))
    }
}

pub struct DirEntry {
    dir: PathBuf,
    name: String,
    file_type: FileType,
}

impl DirEntry {
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    pub fn file_name(&self) -> std::ffi::OsString {
        self.name.clone().into()
    }

    pub async fn file_type(&self) -> io::Result<FileType> {
        Ok(self.file_type)
    }

    pub async fn metadata(&self) -> io::Result<Metadata> {
        symlink_metadata(self.path()).await
    }
}

#[derive(Debug, Default)]
pub struct DirBuilder {
    recursive: bool,
}

impl DirBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn recursive(&mut self, recursive: bool) -> &mut Self {
        self.recursive = recursive;
        self
    }

    pub async fn create(&self, path: impl AsRef<Path>) -> io::Result<()> {
        if self.recursive {
            create_dir_all(path).await
        } else {
            create_dir(path).await
        }
    }
}
