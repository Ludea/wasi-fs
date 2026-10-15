use std::io::{self, SeekFrom};
use std::path::Path;

use wasip3::filesystem::types::{DescriptorFlags, OpenFlags};

use crate::metadata::Metadata;
use crate::sys::{self, invalid, io_err, Fd};

/// An open file with a cursor, mirroring `tokio::fs::File`.
///
/// Reads and writes are inherent async methods (no `AsyncRead`/`AsyncWrite` impls).
pub struct File {
    fd: Fd,
    pos: u64,
    append: bool,
}

impl File {
    pub async fn open(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new().read(true).open(path).await
    }

    pub async fn create(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
    }

    pub async fn create_new(path: impl AsRef<Path>) -> io::Result<File> {
        OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)
            .await
    }

    pub fn options() -> OpenOptions {
        OpenOptions::new()
    }

    pub async fn metadata(&self) -> io::Result<Metadata> {
        let stat = self.fd.stat().await.map_err(io_err)?;
        Ok(Metadata::from_stat(&stat))
    }

    pub async fn set_len(&self, size: u64) -> io::Result<()> {
        self.fd.set_size(size).await.map_err(io_err)
    }

    pub async fn sync_all(&self) -> io::Result<()> {
        self.fd.sync().await.map_err(io_err)
    }

    pub async fn sync_data(&self) -> io::Result<()> {
        self.fd.sync_data().await.map_err(io_err)
    }

    /// Reads up to `buf.len()` bytes; returns 0 on EOF.
    pub async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = sys::read_at(&self.fd, self.pos, buf).await?;
        self.pos += n as u64;
        Ok(n)
    }

    pub async fn read_exact(&mut self, mut buf: &mut [u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read(buf).await? {
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "failed to fill whole buffer",
                    ))
                }
                n => buf = &mut buf[n..],
            }
        }
        Ok(())
    }

    pub async fn read_to_end(&mut self, out: &mut Vec<u8>) -> io::Result<usize> {
        let before = out.len();
        sys::read_all(&self.fd, self.pos, out).await?;
        let n = out.len() - before;
        self.pos += n as u64;
        Ok(n)
    }

    pub async fn read_to_string(&mut self, out: &mut String) -> io::Result<usize> {
        let mut bytes = Vec::new();
        let n = self.read_to_end(&mut bytes).await?;
        let s = String::from_utf8(bytes).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "stream did not contain valid UTF-8",
            )
        })?;
        out.push_str(&s);
        Ok(n)
    }

    /// Writes the whole buffer and returns its length.
    pub async fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.write_all(buf).await?;
        Ok(buf.len())
    }

    pub async fn write_all(&mut self, buf: &[u8]) -> io::Result<()> {
        if self.append {
            sys::write_at(&self.fd, None, buf).await?;
        } else {
            sys::write_at(&self.fd, Some(self.pos), buf).await?;
        }
        self.pos += buf.len() as u64;
        Ok(())
    }

    /// No-op: writes are submitted to the host as soon as `write` completes.
    pub async fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub async fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let new = match from {
            SeekFrom::Start(n) => Some(n),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(d) => {
                let size = self.fd.stat().await.map_err(io_err)?.size;
                size.checked_add_signed(d)
            }
        }
        .ok_or_else(|| invalid("invalid seek to a negative or overflowing position"))?;
        self.pos = new;
        Ok(new)
    }

    pub async fn stream_position(&mut self) -> io::Result<u64> {
        Ok(self.pos)
    }
}

#[derive(Clone, Debug, Default)]
pub struct OpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

impl OpenOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn read(&mut self, v: bool) -> &mut Self {
        self.read = v;
        self
    }

    pub fn write(&mut self, v: bool) -> &mut Self {
        self.write = v;
        self
    }

    pub fn append(&mut self, v: bool) -> &mut Self {
        self.append = v;
        self
    }

    pub fn truncate(&mut self, v: bool) -> &mut Self {
        self.truncate = v;
        self
    }

    pub fn create(&mut self, v: bool) -> &mut Self {
        self.create = v;
        self
    }

    pub fn create_new(&mut self, v: bool) -> &mut Self {
        self.create_new = v;
        self
    }

    pub async fn open(&self, path: impl AsRef<Path>) -> io::Result<File> {
        let writable = self.write || self.append;
        if !self.read && !writable {
            return Err(invalid(
                "must specify at least one of read, write or append",
            ));
        }
        if !writable && (self.truncate || self.create || self.create_new) {
            return Err(invalid("truncate/create require write or append access"));
        }
        if self.append && self.truncate && !self.create_new {
            return Err(invalid("append and truncate are mutually exclusive"));
        }

        let mut flags = DescriptorFlags::empty();
        if self.read {
            flags |= DescriptorFlags::READ;
        }
        if writable {
            flags |= DescriptorFlags::WRITE;
        }

        let mut open = OpenFlags::empty();
        if self.create_new {
            open |= OpenFlags::CREATE | OpenFlags::EXCLUSIVE;
        } else if self.create {
            open |= OpenFlags::CREATE;
        }
        if self.truncate {
            open |= OpenFlags::TRUNCATE;
        }

        let fd = sys::open(path.as_ref(), true, open, flags).await?;
        Ok(File {
            fd,
            pos: 0,
            append: self.append,
        })
    }
}
