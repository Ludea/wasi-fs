use std::io;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use wasip3::filesystem::types::{DescriptorStat, DescriptorType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Dir,
    File,
    Symlink,
    Other,
}

impl From<&DescriptorType> for Kind {
    fn from(t: &DescriptorType) -> Self {
        match t {
            DescriptorType::Directory => Kind::Dir,
            DescriptorType::RegularFile => Kind::File,
            DescriptorType::SymbolicLink => Kind::Symlink,
            _ => Kind::Other,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileType(Kind);

impl FileType {
    pub(crate) fn from_descriptor(t: &DescriptorType) -> Self {
        FileType(Kind::from(t))
    }

    pub fn is_dir(&self) -> bool {
        self.0 == Kind::Dir
    }

    pub fn is_file(&self) -> bool {
        self.0 == Kind::File
    }

    pub fn is_symlink(&self) -> bool {
        self.0 == Kind::Symlink
    }
}

#[derive(Clone, Debug)]
pub struct Metadata {
    file_type: FileType,
    len: u64,
    accessed: Option<SystemTime>,
    modified: Option<SystemTime>,
    changed: Option<SystemTime>,
}

fn to_system_time(seconds: i64, nanos: u32) -> SystemTime {
    if seconds >= 0 {
        UNIX_EPOCH + Duration::new(seconds as u64, nanos)
    } else {
        UNIX_EPOCH - Duration::new(seconds.unsigned_abs(), 0) + Duration::from_nanos(nanos as u64)
    }
}

fn time(t: &Option<wasip3::clocks::system_clock::Instant>) -> Option<SystemTime> {
    t.as_ref().map(|t| to_system_time(t.seconds, t.nanoseconds))
}

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "timestamp not available on this platform",
    )
}

impl Metadata {
    pub(crate) fn from_stat(s: &DescriptorStat) -> Self {
        Metadata {
            file_type: FileType::from_descriptor(&s.type_),
            len: s.size,
            accessed: time(&s.data_access_timestamp),
            modified: time(&s.data_modification_timestamp),
            changed: time(&s.status_change_timestamp),
        }
    }

    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    pub fn is_dir(&self) -> bool {
        self.file_type.is_dir()
    }

    pub fn is_file(&self) -> bool {
        self.file_type.is_file()
    }

    pub fn is_symlink(&self) -> bool {
        self.file_type.is_symlink()
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn accessed(&self) -> io::Result<SystemTime> {
        self.accessed.ok_or_else(unsupported)
    }

    pub fn modified(&self) -> io::Result<SystemTime> {
        self.modified.ok_or_else(unsupported)
    }

    /// Status change time (WASI has no creation time).
    pub fn changed(&self) -> io::Result<SystemTime> {
        self.changed.ok_or_else(unsupported)
    }
}
