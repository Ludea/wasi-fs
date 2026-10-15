//! Low-level glue between `std::io` and the wasi:filesystem 0.3 bindings.
//!
//! Every direct use of the generated bindings lives here or in the
//! thin call sites of `file.rs` / `ops.rs`, so a binding change stays local.

use std::cell::OnceCell;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wasip3::filesystem::preopens::get_directories;
use wasip3::filesystem::types::{Descriptor, DescriptorFlags, ErrorCode, OpenFlags, PathFlags};

pub type Fd = Arc<Descriptor>;

/// (descriptor, normalized components, is_absolute)
type Preopen = (Fd, Vec<String>, bool);

thread_local! {
    static PREOPENS: OnceCell<Vec<Preopen>> = const { OnceCell::new() };
}

pub fn io_err(code: ErrorCode) -> io::Error {
    use io::ErrorKind as K;
    let kind = match code {
        ErrorCode::Access | ErrorCode::NotPermitted | ErrorCode::ReadOnly => K::PermissionDenied,
        ErrorCode::NoEntry => K::NotFound,
        ErrorCode::Exist => K::AlreadyExists,
        ErrorCode::NotDirectory => K::NotADirectory,
        ErrorCode::IsDirectory => K::IsADirectory,
        ErrorCode::NotEmpty => K::DirectoryNotEmpty,
        ErrorCode::Invalid
        | ErrorCode::InvalidSeek
        | ErrorCode::NameTooLong
        | ErrorCode::IllegalByteSequence => K::InvalidInput,
        ErrorCode::Interrupted => K::Interrupted,
        ErrorCode::Unsupported => K::Unsupported,
        ErrorCode::Pipe => K::BrokenPipe,
        ErrorCode::InsufficientMemory => K::OutOfMemory,
        _ => K::Other,
    };
    io::Error::new(kind, format!("{code:?}"))
}

pub fn invalid(msg: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

pub fn path_str(path: &Path) -> io::Result<&str> {
    path.to_str()
        .ok_or_else(|| invalid("path is not valid UTF-8"))
}

/// Lexically splits a path into (is_absolute, components), resolving `.` and `..`.
pub fn components(path: &str) -> (bool, Vec<String>) {
    let abs = path.starts_with('/');
    let mut out: Vec<String> = Vec::new();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => match out.last() {
                Some(last) if last != ".." => {
                    out.pop();
                }
                _ if abs => {}
                _ => out.push("..".to_string()),
            },
            c => out.push(c.to_string()),
        }
    }
    (abs, out)
}

/// Lexical normalization (no symlink resolution, no cwd in WASI).
pub fn normalize(path: &Path) -> io::Result<PathBuf> {
    let (abs, comps) = components(path_str(path)?);
    let joined = comps.join("/");
    Ok(PathBuf::from(match (abs, joined.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }))
}

/// Maps a path to (preopened directory, path relative to it).
pub fn resolve(path: &Path) -> io::Result<(Fd, String)> {
    let (abs, comps) = components(path_str(path)?);

    PREOPENS.with(|cell| {
        let table = cell.get_or_init(|| {
            get_directories()
                .into_iter()
                .map(|(fd, name)| {
                    let (a, c) = components(&name);
                    (Arc::new(fd), c, a)
                })
                .collect()
        });

        // Longest matching preopen of the same kind (absolute vs relative).
        let best = table
            .iter()
            .filter(|(_, pc, pa)| *pa == abs && comps.starts_with(pc))
            .max_by_key(|(_, pc, _)| pc.len())
            // A relative path falls back to a "/" preopen when there is no "." one.
            .or_else(|| {
                if abs {
                    None
                } else {
                    table.iter().find(|(_, pc, pa)| *pa && pc.is_empty())
                }
            })
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "path is not under any preopened directory",
                )
            })?;

        let rest = &comps[best.1.len()..];
        let rel = if rest.is_empty() {
            ".".to_string()
        } else {
            rest.join("/")
        };
        Ok((best.0.clone(), rel))
    })
}

pub async fn open(
    path: &Path,
    follow: bool,
    open: OpenFlags,
    flags: DescriptorFlags,
) -> io::Result<Fd> {
    let (dir, rel) = resolve(path)?;
    let path_flags = if follow {
        PathFlags::SYMLINK_FOLLOW
    } else {
        PathFlags::empty()
    };
    dir.open_at(path_flags, rel, open, flags)
        .await
        .map(Arc::new)
        .map_err(io_err)
}

/// Reads at most `buf.len()` bytes at `offset`. Returns 0 on EOF.
pub async fn read_at(fd: &Descriptor, offset: u64, buf: &mut [u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    let (mut rx, result) = fd.read_via_stream(offset);
    let (_, chunk) = rx.read(Vec::with_capacity(buf.len())).await;
    if chunk.is_empty() {
        // Stream closed without data: EOF, or an error carried by the result future.
        drop(rx);
        result.await.map_err(io_err)?;
        return Ok(0);
    }
    // The stream is reopened on every call, so any surplus bytes are simply re-read later.
    let n = chunk.len().min(buf.len());
    buf[..n].copy_from_slice(&chunk[..n]);
    Ok(n)
}

/// Reads everything from `offset` to EOF using a single stream.
pub async fn read_all(fd: &Descriptor, offset: u64, out: &mut Vec<u8>) -> io::Result<()> {
    let (mut rx, result) = fd.read_via_stream(offset);
    loop {
        let (_, chunk) = rx.read(Vec::with_capacity(64 * 1024)).await;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
    }
    result.await.map_err(io_err)
}

/// Writes `data` at `offset`, or appends when `offset` is `None`.
pub async fn write_at(fd: &Descriptor, offset: Option<u64>, data: &[u8]) -> io::Result<()> {
    let (mut tx, rx) = wasip3::wit_stream::new();
    let call = async {
        match offset {
            Some(o) => fd.write_via_stream(rx, o).await,
            None => fd.append_via_stream(rx).await,
        }
    };
    let send = async move {
        tx.write_all(data.to_vec()).await;
        // Dropping the writer closes the stream and lets the call complete.
        drop(tx);
    };
    let (res, ()) = futures::join!(call, send);
    res.map_err(io_err)
}
