#![deny(unsafe_code)]
#![deny(clippy::all)]
#![deny(unreachable_pub)]

//! Bounded, synchronous 9P2000.L service for one admitted Realm export.
//!
//! This crate owns protocol parsing and per-mount FID state. It does not own
//! authority: the supplied [`FileSystem`] is already scoped to one principal,
//! Realm, and export root by the outer capsule.

use std::{collections::BTreeMap, fmt};

/// Largest negotiated 9P message accepted by the seed transport.
pub const MAX_MESSAGE_BYTES: usize = 64 * 1024;
/// Smallest negotiated message size accepted by the Linux 9P client.
pub const MIN_MESSAGE_BYTES: usize = 4096;
/// Maximum live FIDs admitted for one mounted export.
pub const MAX_FIDS: usize = 1024;

const HEADER_BYTES: usize = 7;
const NOTAG: u16 = u16::MAX;
const NOFID: u32 = u32::MAX;
const MAX_WALK_ELEMENTS: usize = 16;
const MAX_NAME_BYTES: usize = 255;
const MAX_DIRECTORY_ENTRIES: usize = 4096;
const MAX_QID_PATHS: usize = 16 * 1024;
const VERSION: &str = "9P2000.L";

const TSTATFS: u8 = 8;
const TLOPEN: u8 = 12;
const TLCREATE: u8 = 14;
const TGETATTR: u8 = 24;
const TSETATTR: u8 = 26;
const TREADDIR: u8 = 40;
const TFSYNC: u8 = 50;
const TMKDIR: u8 = 72;
const TRENAMEAT: u8 = 74;
const TUNLINKAT: u8 = 76;
const TVERSION: u8 = 100;
const TATTACH: u8 = 104;
const TFLUSH: u8 = 108;
const TWALK: u8 = 110;
const TREAD: u8 = 116;
const TWRITE: u8 = 118;
const TCLUNK: u8 = 120;
const TREMOVE: u8 = 122;

const RLERROR: u8 = 7;
const QID_DIRECTORY: u8 = 0x80;
const DT_DIRECTORY: u8 = 4;
const DT_REGULAR: u8 = 8;
const MODE_DIRECTORY: u32 = 0o040_000;
const MODE_REGULAR: u32 = 0o100_000;
const OPEN_ACCESS_MASK: u32 = 3;
const OPEN_WRITE_ONLY: u32 = 1;
const OPEN_READ_WRITE: u32 = 2;
const OPEN_EXCLUSIVE: u32 = 0o200;
const OPEN_TRUNCATE: u32 = 0o1000;
const AT_REMOVE_DIR: u32 = 0x200;
const ATTR_SIZE: u32 = 1 << 3;
const STATS_BASIC: u64 = 0x7ff;

/// Stable protocol error classes converted directly to Linux errno values.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Errno {
    /// Operation is not permitted by the export.
    Permission = 1,
    /// Path or FID target does not exist.
    NotFound = 2,
    /// Backend I/O failure.
    Io = 5,
    /// FID is absent or not usable for the operation.
    BadFileDescriptor = 9,
    /// A create target or FID already exists.
    AlreadyExists = 17,
    /// A path component expected to be a directory is not one.
    NotDirectory = 20,
    /// A path expected to be a regular file is a directory.
    IsDirectory = 21,
    /// Malformed or unsupported argument.
    InvalidArgument = 22,
    /// Export quota is exhausted.
    NoSpace = 28,
    /// Per-mount FID admission is exhausted.
    TooManyOpenFiles = 24,
    /// The export is read-only.
    ReadOnly = 30,
    /// A path component exceeds the admitted name limit.
    NameTooLong = 36,
    /// Requested protocol operation is not implemented.
    NotSupported = 95,
    /// A directory expected to be empty still contains entries.
    NotEmpty = 39,
    /// Request or response exceeds the negotiated message size.
    MessageTooLarge = 90,
    /// The requested protocol dialect is unavailable.
    ProtocolNotSupported = 93,
    /// A retained reference no longer names the current attachment epoch.
    Stale = 116,
}

impl Errno {
    const fn code(self) -> u32 {
        self as u32
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "9P operation failed with errno {}", self.code())
    }
}

impl std::error::Error for Errno {}

/// Guest-visible filesystem node category admitted by this server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    /// Regular byte-addressable file.
    File,
    /// Directory containing named children.
    Directory,
}

/// Metadata required to serve Linux 9P2000.L inode operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Metadata {
    /// Node category.
    pub kind: NodeKind,
    /// File byte length; zero for directories.
    pub len: u64,
    /// Permission bits. File-type bits are supplied by the protocol server.
    pub mode: u32,
    /// Seconds since the Unix epoch, or zero when the backing store has no time.
    pub modified_seconds: u64,
    /// Backend data generation used as the QID version.
    pub generation: u64,
}

/// One materialized child returned by [`FileSystem::read_dir`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Single UTF-8 path component, never an absolute or nested path.
    pub name: String,
    /// Metadata captured with this directory enumeration.
    pub metadata: Metadata,
}

/// Capacity information returned to Linux `statfs(2)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileSystemStats {
    /// Fundamental block size.
    pub block_size: u32,
    /// Total addressable blocks.
    pub blocks: u64,
    /// Unallocated blocks.
    pub blocks_free: u64,
    /// Blocks available to the mounted principal.
    pub blocks_available: u64,
    /// Total addressable file nodes.
    pub files: u64,
    /// Unallocated file nodes.
    pub files_free: u64,
}

impl Default for FileSystemStats {
    fn default() -> Self {
        Self {
            block_size: 4096,
            blocks: 0,
            blocks_free: 0,
            blocks_available: 0,
            files: 0,
            files_free: 0,
        }
    }
}

/// Filesystem operations after the outer capsule has selected an export root.
///
/// Every path is relative, normalized, and contains no `.` or `..` component.
/// Implementations must retain their own beneath-root or capability check at
/// every operation; a path string is not authority.
pub trait FileSystem {
    /// Inspect one node. The empty path denotes the export root.
    fn metadata(&mut self, path: &str) -> Result<Metadata, Errno>;
    /// Enumerate one directory in stable name order.
    fn read_dir(&mut self, path: &str) -> Result<Vec<DirectoryEntry>, Errno>;
    /// Positional file read.
    fn read(&mut self, path: &str, offset: u64, count: u32) -> Result<Vec<u8>, Errno>;
    /// Positional file write.
    fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u32, Errno>;
    /// Create a regular file, applying exclusive and truncate semantics.
    fn create_file(
        &mut self,
        path: &str,
        mode: u32,
        exclusive: bool,
        truncate: bool,
    ) -> Result<(), Errno>;
    /// Create one directory beneath an existing parent.
    fn create_dir(&mut self, path: &str, mode: u32) -> Result<(), Errno>;
    /// Truncate or extend a regular file.
    fn set_len(&mut self, path: &str, len: u64) -> Result<(), Errno>;
    /// Remove one non-directory node.
    fn remove_file(&mut self, path: &str) -> Result<(), Errno>;
    /// Remove one empty directory.
    fn remove_dir(&mut self, path: &str) -> Result<(), Errno>;
    /// Rename within this one export.
    fn rename(&mut self, source: &str, destination: &str) -> Result<(), Errno>;
    /// Flush file data and metadata selected by a FID.
    fn sync(&mut self, path: &str, data_only: bool) -> Result<(), Errno>;
    /// Report export capacity without revealing a physical host filesystem.
    fn statfs(&mut self) -> Result<FileSystemStats, Errno> {
        Ok(FileSystemStats::default())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Qid {
    kind: NodeKind,
    version: u32,
    path: u64,
}

#[derive(Clone, Debug)]
struct Fid {
    path: String,
    open_flags: Option<u32>,
    stale: bool,
}

/// Stateful server for one Linux mount and one already-authorized export.
pub struct Session<F> {
    filesystem: F,
    export_name: String,
    message_size: usize,
    fids: BTreeMap<u32, Fid>,
    qid_paths: BTreeMap<String, u64>,
    next_qid_path: u64,
}

impl<F: FileSystem> Session<F> {
    /// Construct an unmounted 9P2000.L session for one exact `aname` export.
    pub fn new(filesystem: F, export_name: impl Into<String>) -> Result<Self, Errno> {
        let export_name = export_name.into();
        if export_name.is_empty()
            || export_name.len() > MAX_NAME_BYTES
            || export_name.contains('/')
            || export_name.chars().any(char::is_control)
        {
            return Err(Errno::InvalidArgument);
        }
        let mut qid_paths = BTreeMap::new();
        qid_paths.insert(String::new(), 1);
        Ok(Self {
            filesystem,
            export_name,
            message_size: MAX_MESSAGE_BYTES,
            fids: BTreeMap::new(),
            qid_paths,
            next_qid_path: 2,
        })
    }

    /// Borrow the backing filesystem for status or test inspection.
    #[must_use]
    pub const fn filesystem(&self) -> &F {
        &self.filesystem
    }

    /// Mutably borrow the backing filesystem.
    pub const fn filesystem_mut(&mut self) -> &mut F {
        &mut self.filesystem
    }

    /// Parse and execute exactly one complete request message.
    ///
    /// The returned buffer is always a complete 9P response. Protocol and
    /// backend errors use `Rlerror`; transport failure is reserved for the SBI
    /// boundary outside this crate.
    #[must_use]
    pub fn serve(&mut self, request: &[u8]) -> Vec<u8> {
        let tag = request
            .get(5..7)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u16::from_le_bytes)
            .unwrap_or(NOTAG);
        match self.serve_inner(request, tag) {
            Ok(response) => response,
            Err(errno) => error_response(tag, errno),
        }
    }

    fn serve_inner(&mut self, request: &[u8], tag: u16) -> Result<Vec<u8>, Errno> {
        if request.len() < HEADER_BYTES || request.len() > self.message_size {
            return Err(Errno::MessageTooLarge);
        }
        let declared = u32::from_le_bytes(
            request[0..4]
                .try_into()
                .map_err(|_| Errno::InvalidArgument)?,
        );
        if usize::try_from(declared).map_err(|_| Errno::MessageTooLarge)? != request.len() {
            return Err(Errno::InvalidArgument);
        }
        let message_type = request[4];
        if message_type == TVERSION && tag != NOTAG {
            return Err(Errno::InvalidArgument);
        }
        let mut reader = Reader::new(&request[HEADER_BYTES..]);
        let mut response = Writer::response(message_type.wrapping_add(1), tag);
        match message_type {
            TVERSION => self.version(&mut reader, &mut response)?,
            TATTACH => self.attach(&mut reader, &mut response)?,
            TFLUSH => self.flush(&mut reader)?,
            TWALK => self.walk(&mut reader, &mut response)?,
            TLOPEN => self.open(&mut reader, &mut response)?,
            TLCREATE => self.create(&mut reader, &mut response)?,
            TREAD => self.read(&mut reader, &mut response)?,
            TWRITE => self.write(&mut reader, &mut response)?,
            TCLUNK => self.clunk(&mut reader)?,
            TREMOVE => self.remove(&mut reader)?,
            TGETATTR => self.getattr(&mut reader, &mut response)?,
            TSETATTR => self.setattr(&mut reader)?,
            TSTATFS => self.statfs(&mut reader, &mut response)?,
            TREADDIR => self.readdir(&mut reader, &mut response)?,
            TFSYNC => self.fsync(&mut reader)?,
            TMKDIR => self.mkdir(&mut reader, &mut response)?,
            TRENAMEAT => self.renameat(&mut reader)?,
            TUNLINKAT => self.unlinkat(&mut reader)?,
            _ => return Err(Errno::NotSupported),
        }
        if !reader.is_empty() {
            return Err(Errno::InvalidArgument);
        }
        response.finish(self.message_size)
    }

    fn version(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let requested_size = usize::try_from(reader.u32()?).map_err(|_| Errno::MessageTooLarge)?;
        let version = reader.string()?;
        reader.finish()?;
        if version != VERSION {
            return Err(Errno::ProtocolNotSupported);
        }
        if requested_size < MIN_MESSAGE_BYTES {
            return Err(Errno::InvalidArgument);
        }
        self.message_size = requested_size.min(MAX_MESSAGE_BYTES);
        self.fids.clear();
        self.qid_paths.clear();
        self.qid_paths.insert(String::new(), 1);
        self.next_qid_path = 2;
        response.u32(self.message_size as u32);
        response.string(VERSION)?;
        Ok(())
    }

    fn attach(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let afid = reader.u32()?;
        let _uname = reader.string()?;
        let aname = reader.string()?;
        let _numeric_uname = reader.u32()?;
        reader.finish()?;
        if afid != NOFID || aname != self.export_name {
            return Err(Errno::Permission);
        }
        let metadata = self.filesystem.metadata("")?;
        if metadata.kind != NodeKind::Directory {
            return Err(Errno::NotDirectory);
        }
        let qid = self.qid_for("", &metadata)?;
        self.insert_fid(
            fid,
            Fid {
                path: String::new(),
                open_flags: None,
                stale: false,
            },
        )?;
        response.qid(qid);
        Ok(())
    }

    fn flush(&mut self, reader: &mut Reader<'_>) -> Result<(), Errno> {
        let _old_tag = reader.u16()?;
        reader.finish()?;
        Ok(())
    }

    fn walk(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let new_fid = reader.u32()?;
        let count = usize::from(reader.u16()?);
        if count > MAX_WALK_ELEMENTS {
            return Err(Errno::InvalidArgument);
        }
        let mut names = Vec::with_capacity(count);
        for _ in 0..count {
            names.push(reader.string()?.to_string());
        }
        reader.finish()?;
        if new_fid != fid && self.fids.contains_key(&new_fid) {
            return Err(Errno::AlreadyExists);
        }
        let original = self.fid(fid)?.clone();
        let mut path = original.path;
        let mut qids = Vec::with_capacity(count);
        for name in names {
            let candidate = walk_path(&path, &name)?;
            match self.filesystem.metadata(&candidate) {
                Ok(metadata) => {
                    path = candidate;
                    qids.push(self.qid_for(&path, &metadata)?);
                }
                Err(error) if qids.is_empty() => return Err(error),
                Err(_) => break,
            }
        }
        if count == 0 || !qids.is_empty() {
            let replacement = Fid {
                path,
                open_flags: None,
                stale: false,
            };
            if new_fid == fid {
                self.fids.insert(fid, replacement);
            } else {
                self.insert_fid(new_fid, replacement)?;
            }
        }
        response.u16(u16::try_from(qids.len()).map_err(|_| Errno::InvalidArgument)?);
        for qid in qids {
            response.qid(qid);
        }
        Ok(())
    }

    fn open(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let flags = reader.u32()?;
        reader.finish()?;
        let record = self.fid(fid)?;
        if record.open_flags.is_some() {
            return Err(Errno::InvalidArgument);
        }
        let path = record.path.clone();
        let mut metadata = self.filesystem.metadata(&path)?;
        if metadata.kind == NodeKind::Directory
            && matches!(flags & OPEN_ACCESS_MASK, OPEN_WRITE_ONLY | OPEN_READ_WRITE)
        {
            return Err(Errno::IsDirectory);
        }
        if metadata.kind == NodeKind::File && flags & OPEN_TRUNCATE != 0 {
            if flags & OPEN_ACCESS_MASK == 0 {
                return Err(Errno::Permission);
            }
            self.filesystem.set_len(&path, 0)?;
            metadata = self.filesystem.metadata(&path)?;
        }
        self.fids.get_mut(&fid).expect("FID checked").open_flags = Some(flags);
        let qid = self.qid_for(&path, &metadata)?;
        response.qid(qid);
        response.u32(0);
        Ok(())
    }

    fn create(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let name = reader.string()?.to_string();
        let flags = reader.u32()?;
        let mode = reader.u32()?;
        let _gid = reader.u32()?;
        reader.finish()?;
        let parent = self.fid(fid)?.path.clone();
        if self.filesystem.metadata(&parent)?.kind != NodeKind::Directory {
            return Err(Errno::NotDirectory);
        }
        let path = child_path(&parent, &name)?;
        self.filesystem.create_file(
            &path,
            mode,
            flags & OPEN_EXCLUSIVE != 0,
            flags & OPEN_TRUNCATE != 0,
        )?;
        let metadata = self.filesystem.metadata(&path)?;
        self.fids.insert(
            fid,
            Fid {
                path: path.clone(),
                open_flags: Some(flags),
                stale: false,
            },
        );
        let qid = self.qid_for(&path, &metadata)?;
        response.qid(qid);
        response.u32(0);
        Ok(())
    }

    fn read(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let offset = reader.u64()?;
        let requested = reader.u32()?;
        reader.finish()?;
        let record = self.fid(fid)?;
        let Some(flags) = record.open_flags else {
            return Err(Errno::BadFileDescriptor);
        };
        if flags & OPEN_ACCESS_MASK == OPEN_WRITE_ONLY {
            return Err(Errno::BadFileDescriptor);
        }
        let path = record.path.clone();
        if self.filesystem.metadata(&path)?.kind == NodeKind::Directory {
            return Err(Errno::IsDirectory);
        }
        let maximum = self
            .message_size
            .saturating_sub(HEADER_BYTES + 4)
            .min(usize::try_from(requested).map_err(|_| Errno::MessageTooLarge)?);
        let bytes = self.filesystem.read(
            &path,
            offset,
            u32::try_from(maximum).map_err(|_| Errno::MessageTooLarge)?,
        )?;
        if bytes.len() > maximum {
            return Err(Errno::Io);
        }
        response.bytes_with_u32_len(&bytes)?;
        Ok(())
    }

    fn write(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let offset = reader.u64()?;
        let count = usize::try_from(reader.u32()?).map_err(|_| Errno::MessageTooLarge)?;
        let data = reader.bytes(count)?;
        reader.finish()?;
        let record = self.fid(fid)?;
        let Some(flags) = record.open_flags else {
            return Err(Errno::BadFileDescriptor);
        };
        if flags & OPEN_ACCESS_MASK == 0 {
            return Err(Errno::BadFileDescriptor);
        }
        let path = record.path.clone();
        if self.filesystem.metadata(&path)?.kind == NodeKind::Directory {
            return Err(Errno::IsDirectory);
        }
        let written = self.filesystem.write(&path, offset, data)?;
        if usize::try_from(written).map_err(|_| Errno::Io)? > data.len() {
            return Err(Errno::Io);
        }
        response.u32(written);
        Ok(())
    }

    fn clunk(&mut self, reader: &mut Reader<'_>) -> Result<(), Errno> {
        let fid = reader.u32()?;
        reader.finish()?;
        self.fids
            .remove(&fid)
            .map(|_| ())
            .ok_or(Errno::BadFileDescriptor)
    }

    fn remove(&mut self, reader: &mut Reader<'_>) -> Result<(), Errno> {
        let fid = reader.u32()?;
        reader.finish()?;
        let record = self.fids.remove(&fid).ok_or(Errno::BadFileDescriptor)?;
        if record.path.is_empty() {
            return Err(Errno::Permission);
        }
        let metadata = self.filesystem.metadata(&record.path)?;
        if metadata.kind == NodeKind::Directory {
            self.filesystem.remove_dir(&record.path)?;
        } else {
            self.filesystem.remove_file(&record.path)?;
        }
        self.forget_qid_path(&record.path);
        Ok(())
    }

    fn getattr(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let _requested = reader.u64()?;
        reader.finish()?;
        let path = self.fid(fid)?.path.clone();
        let metadata = self.filesystem.metadata(&path)?;
        let qid = self.qid_for(&path, &metadata)?;
        response.u64(STATS_BASIC);
        response.qid(qid);
        response.u32(protocol_mode(&metadata));
        response.u32(1000);
        response.u32(1000);
        response.u64(if metadata.kind == NodeKind::Directory {
            2
        } else {
            1
        });
        response.u64(0);
        response.u64(metadata.len);
        response.u64(4096);
        response.u64(metadata.len.div_ceil(512));
        for value in [
            metadata.modified_seconds,
            0,
            metadata.modified_seconds,
            0,
            metadata.modified_seconds,
            0,
            0,
            0,
            metadata.generation,
            metadata.generation,
        ] {
            response.u64(value);
        }
        Ok(())
    }

    fn setattr(&mut self, reader: &mut Reader<'_>) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let valid = reader.u32()?;
        let _mode = reader.u32()?;
        let _uid = reader.u32()?;
        let _gid = reader.u32()?;
        let size = reader.u64()?;
        let _atime_seconds = reader.u64()?;
        let _atime_nanoseconds = reader.u64()?;
        let _mtime_seconds = reader.u64()?;
        let _mtime_nanoseconds = reader.u64()?;
        reader.finish()?;
        if valid & !ATTR_SIZE != 0 {
            return Err(Errno::NotSupported);
        }
        if valid & ATTR_SIZE != 0 {
            let path = self.fid(fid)?.path.clone();
            self.filesystem.set_len(&path, size)?;
        } else {
            self.fid(fid)?;
        }
        Ok(())
    }

    fn statfs(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        self.fid(reader.u32()?)?;
        reader.finish()?;
        let stats = self.filesystem.statfs()?;
        response.u32(0x0102_1997);
        response.u32(stats.block_size);
        response.u64(stats.blocks);
        response.u64(stats.blocks_free);
        response.u64(stats.blocks_available);
        response.u64(stats.files);
        response.u64(stats.files_free);
        response.u64(1);
        response.u32(MAX_NAME_BYTES as u32);
        Ok(())
    }

    fn readdir(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let offset = usize::try_from(reader.u64()?).map_err(|_| Errno::InvalidArgument)?;
        let requested = usize::try_from(reader.u32()?).map_err(|_| Errno::MessageTooLarge)?;
        reader.finish()?;
        let record = self.fid(fid)?;
        if record.open_flags.is_none() {
            return Err(Errno::BadFileDescriptor);
        }
        let path = record.path.clone();
        if self.filesystem.metadata(&path)?.kind != NodeKind::Directory {
            return Err(Errno::NotDirectory);
        }
        let mut entries = self.filesystem.read_dir(&path)?;
        if entries.len() > MAX_DIRECTORY_ENTRIES {
            return Err(Errno::NoSpace);
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        for entry in &entries {
            validate_name(&entry.name)?;
        }
        let maximum = requested.min(self.message_size.saturating_sub(HEADER_BYTES + 4));
        let mut encoded = Writer::plain();
        for (index, entry) in entries.into_iter().enumerate().skip(offset) {
            let child = child_path(&path, &entry.name)?;
            let qid = self.qid_for(&child, &entry.metadata)?;
            let mut candidate = Writer::plain();
            candidate.qid(qid);
            candidate.u64(u64::try_from(index + 1).map_err(|_| Errno::InvalidArgument)?);
            candidate.u8(match entry.metadata.kind {
                NodeKind::Directory => DT_DIRECTORY,
                NodeKind::File => DT_REGULAR,
            });
            candidate.string(&entry.name)?;
            if encoded.len().saturating_add(candidate.len()) > maximum {
                break;
            }
            encoded.extend(candidate.into_bytes());
        }
        response.bytes_with_u32_len(&encoded.into_bytes())?;
        Ok(())
    }

    fn fsync(&mut self, reader: &mut Reader<'_>) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let data_only = reader.u32()? != 0;
        reader.finish()?;
        let record = self.fid(fid)?;
        if record.open_flags.is_none() {
            return Err(Errno::BadFileDescriptor);
        }
        let path = record.path.clone();
        self.filesystem.sync(&path, data_only)
    }

    fn mkdir(&mut self, reader: &mut Reader<'_>, response: &mut Writer) -> Result<(), Errno> {
        let fid = reader.u32()?;
        let name = reader.string()?.to_string();
        let mode = reader.u32()?;
        let _gid = reader.u32()?;
        reader.finish()?;
        let parent = self.fid(fid)?.path.clone();
        if self.filesystem.metadata(&parent)?.kind != NodeKind::Directory {
            return Err(Errno::NotDirectory);
        }
        let path = child_path(&parent, &name)?;
        self.filesystem.create_dir(&path, mode)?;
        let metadata = self.filesystem.metadata(&path)?;
        let qid = self.qid_for(&path, &metadata)?;
        response.qid(qid);
        Ok(())
    }

    fn renameat(&mut self, reader: &mut Reader<'_>) -> Result<(), Errno> {
        let old_directory = self.fid(reader.u32()?)?.path.clone();
        let old_name = reader.string()?.to_string();
        let new_directory = self.fid(reader.u32()?)?.path.clone();
        let new_name = reader.string()?.to_string();
        reader.finish()?;
        let source = child_path(&old_directory, &old_name)?;
        let destination = child_path(&new_directory, &new_name)?;
        if source == destination {
            return Ok(());
        }
        self.filesystem.rename(&source, &destination)?;
        // A backend may atomically replace the destination. Path-backed FIDs
        // to that prior node must become stale rather than silently retargeting
        // to the renamed source.
        self.forget_qid_path(&destination);
        self.rename_retained_paths(&source, &destination);
        Ok(())
    }

    fn unlinkat(&mut self, reader: &mut Reader<'_>) -> Result<(), Errno> {
        let directory = self.fid(reader.u32()?)?.path.clone();
        let name = reader.string()?.to_string();
        let flags = reader.u32()?;
        reader.finish()?;
        if flags & !AT_REMOVE_DIR != 0 {
            return Err(Errno::InvalidArgument);
        }
        let path = child_path(&directory, &name)?;
        if flags & AT_REMOVE_DIR != 0 {
            self.filesystem.remove_dir(&path)?;
        } else {
            self.filesystem.remove_file(&path)?;
        }
        self.forget_qid_path(&path);
        Ok(())
    }

    fn fid(&self, fid: u32) -> Result<&Fid, Errno> {
        match self.fids.get(&fid) {
            Some(record) if record.stale => Err(Errno::Stale),
            Some(record) => Ok(record),
            None => Err(Errno::BadFileDescriptor),
        }
    }

    fn insert_fid(&mut self, fid: u32, record: Fid) -> Result<(), Errno> {
        if self.fids.contains_key(&fid) {
            return Err(Errno::AlreadyExists);
        }
        if self.fids.len() == MAX_FIDS {
            return Err(Errno::TooManyOpenFiles);
        }
        self.fids.insert(fid, record);
        Ok(())
    }

    fn qid_for(&mut self, path: &str, metadata: &Metadata) -> Result<Qid, Errno> {
        let identity = if let Some(identity) = self.qid_paths.get(path) {
            *identity
        } else {
            if self.qid_paths.len() == MAX_QID_PATHS {
                return Err(Errno::NoSpace);
            }
            let identity = self.next_qid_path;
            self.next_qid_path = self.next_qid_path.checked_add(1).ok_or(Errno::NoSpace)?;
            self.qid_paths.insert(path.to_string(), identity);
            identity
        };
        Ok(Qid {
            kind: metadata.kind,
            version: metadata.generation.min(u64::from(u32::MAX)) as u32,
            path: identity,
        })
    }

    fn forget_qid_path(&mut self, path: &str) {
        let prefix = format!("{path}/");
        self.qid_paths
            .retain(|candidate, _| candidate != path && !candidate.starts_with(&prefix));
        for fid in self.fids.values_mut() {
            if fid.path == path || fid.path.starts_with(&prefix) {
                fid.stale = true;
            }
        }
    }

    fn rename_retained_paths(&mut self, source: &str, destination: &str) {
        let prefix = format!("{source}/");
        let moved_qids: Vec<_> = self
            .qid_paths
            .iter()
            .filter(|(path, _)| path.as_str() == source || path.starts_with(&prefix))
            .map(|(path, identity)| (path.clone(), *identity))
            .collect();
        for (old_path, identity) in moved_qids {
            self.qid_paths.remove(&old_path);
            let suffix = old_path
                .strip_prefix(source)
                .expect("selected source prefix");
            self.qid_paths
                .insert(format!("{destination}{suffix}"), identity);
        }
        for fid in self.fids.values_mut() {
            if fid.path == source || fid.path.starts_with(&prefix) {
                let suffix = fid
                    .path
                    .strip_prefix(source)
                    .expect("selected source prefix");
                fid.path = format!("{destination}{suffix}");
            }
        }
    }
}

fn protocol_mode(metadata: &Metadata) -> u32 {
    let kind = match metadata.kind {
        NodeKind::File => MODE_REGULAR,
        NodeKind::Directory => MODE_DIRECTORY,
    };
    kind | (metadata.mode & 0o7777)
}

fn validate_name(name: &str) -> Result<(), Errno> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\0')
        || name.chars().any(char::is_control)
    {
        return Err(Errno::InvalidArgument);
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(Errno::NameTooLong);
    }
    Ok(())
}

fn child_path(parent: &str, name: &str) -> Result<String, Errno> {
    validate_name(name)?;
    Ok(if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    })
}

fn walk_path(parent: &str, name: &str) -> Result<String, Errno> {
    match name {
        "." => Ok(parent.to_string()),
        ".." => Ok(parent
            .rsplit_once('/')
            .map(|(ancestor, _)| ancestor)
            .unwrap_or("")
            .to_string()),
        _ => child_path(parent, name),
    }
}

fn error_response(tag: u16, errno: Errno) -> Vec<u8> {
    let mut response = Writer::response(RLERROR, tag);
    response.u32(errno.code());
    response
        .finish(MAX_MESSAGE_BYTES)
        .expect("fixed Rlerror response is bounded")
}

struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    fn bytes(&mut self, count: usize) -> Result<&'a [u8], Errno> {
        let end = self
            .offset
            .checked_add(count)
            .ok_or(Errno::MessageTooLarge)?;
        let bytes = self
            .bytes
            .get(self.offset..end)
            .ok_or(Errno::InvalidArgument)?;
        self.offset = end;
        Ok(bytes)
    }

    fn u16(&mut self) -> Result<u16, Errno> {
        Ok(u16::from_le_bytes(
            self.bytes(2)?
                .try_into()
                .map_err(|_| Errno::InvalidArgument)?,
        ))
    }

    fn u32(&mut self) -> Result<u32, Errno> {
        Ok(u32::from_le_bytes(
            self.bytes(4)?
                .try_into()
                .map_err(|_| Errno::InvalidArgument)?,
        ))
    }

    fn u64(&mut self) -> Result<u64, Errno> {
        Ok(u64::from_le_bytes(
            self.bytes(8)?
                .try_into()
                .map_err(|_| Errno::InvalidArgument)?,
        ))
    }

    fn string(&mut self) -> Result<&'a str, Errno> {
        let count = usize::from(self.u16()?);
        std::str::from_utf8(self.bytes(count)?).map_err(|_| Errno::InvalidArgument)
    }

    fn finish(&self) -> Result<(), Errno> {
        self.is_empty().then_some(()).ok_or(Errno::InvalidArgument)
    }
}

struct Writer {
    bytes: Vec<u8>,
}

impl Writer {
    fn response(message_type: u8, tag: u16) -> Self {
        let mut writer = Self::plain();
        writer.u32(0);
        writer.u8(message_type);
        writer.u16(tag);
        writer
    }

    const fn plain() -> Self {
        Self { bytes: Vec::new() }
    }

    fn len(&self) -> usize {
        self.bytes.len()
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn extend(&mut self, bytes: Vec<u8>) {
        self.bytes.extend(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn u16(&mut self, value: u16) {
        self.bytes.extend(value.to_le_bytes());
    }

    fn u32(&mut self, value: u32) {
        self.bytes.extend(value.to_le_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.bytes.extend(value.to_le_bytes());
    }

    fn string(&mut self, value: &str) -> Result<(), Errno> {
        let len = u16::try_from(value.len()).map_err(|_| Errno::NameTooLong)?;
        self.u16(len);
        self.bytes.extend(value.as_bytes());
        Ok(())
    }

    fn qid(&mut self, qid: Qid) {
        self.u8(match qid.kind {
            NodeKind::File => 0,
            NodeKind::Directory => QID_DIRECTORY,
        });
        self.u32(qid.version);
        self.u64(qid.path);
    }

    fn bytes_with_u32_len(&mut self, value: &[u8]) -> Result<(), Errno> {
        self.u32(u32::try_from(value.len()).map_err(|_| Errno::MessageTooLarge)?);
        self.bytes.extend(value);
        Ok(())
    }

    fn finish(mut self, maximum: usize) -> Result<Vec<u8>, Errno> {
        if self.bytes.len() < HEADER_BYTES || self.bytes.len() > maximum {
            return Err(Errno::MessageTooLarge);
        }
        let size = u32::try_from(self.bytes.len()).map_err(|_| Errno::MessageTooLarge)?;
        self.bytes[0..4].copy_from_slice(&size.to_le_bytes());
        Ok(self.bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Debug)]
    struct MemoryNode {
        metadata: Metadata,
        data: Vec<u8>,
    }

    #[derive(Debug)]
    struct MemoryFileSystem {
        nodes: BTreeMap<String, MemoryNode>,
        generation: u64,
    }

    impl MemoryFileSystem {
        fn new() -> Self {
            let mut nodes = BTreeMap::new();
            nodes.insert(
                String::new(),
                MemoryNode {
                    metadata: metadata(NodeKind::Directory, 0, 0o755, 1),
                    data: Vec::new(),
                },
            );
            Self {
                nodes,
                generation: 1,
            }
        }

        fn parent(path: &str) -> &str {
            path.rsplit_once('/')
                .map(|(parent, _)| parent)
                .unwrap_or("")
        }

        fn next_generation(&mut self) -> u64 {
            self.generation += 1;
            self.generation
        }

        fn require_parent(&self, path: &str) -> Result<(), Errno> {
            match self.nodes.get(Self::parent(path)) {
                Some(node) if node.metadata.kind == NodeKind::Directory => Ok(()),
                Some(_) => Err(Errno::NotDirectory),
                None => Err(Errno::NotFound),
            }
        }
    }

    impl FileSystem for MemoryFileSystem {
        fn metadata(&mut self, path: &str) -> Result<Metadata, Errno> {
            self.nodes
                .get(path)
                .map(|node| node.metadata.clone())
                .ok_or(Errno::NotFound)
        }

        fn read_dir(&mut self, path: &str) -> Result<Vec<DirectoryEntry>, Errno> {
            let node = self.nodes.get(path).ok_or(Errno::NotFound)?;
            if node.metadata.kind != NodeKind::Directory {
                return Err(Errno::NotDirectory);
            }
            let prefix = if path.is_empty() {
                String::new()
            } else {
                format!("{path}/")
            };
            Ok(self
                .nodes
                .iter()
                .filter_map(|(candidate, node)| {
                    let suffix = candidate.strip_prefix(&prefix)?;
                    (!suffix.is_empty() && !suffix.contains('/')).then(|| DirectoryEntry {
                        name: suffix.to_string(),
                        metadata: node.metadata.clone(),
                    })
                })
                .collect())
        }

        fn read(&mut self, path: &str, offset: u64, count: u32) -> Result<Vec<u8>, Errno> {
            let node = self.nodes.get(path).ok_or(Errno::NotFound)?;
            if node.metadata.kind != NodeKind::File {
                return Err(Errno::IsDirectory);
            }
            let start = usize::try_from(offset).map_err(|_| Errno::InvalidArgument)?;
            if start >= node.data.len() {
                return Ok(Vec::new());
            }
            let end = start
                .saturating_add(usize::try_from(count).map_err(|_| Errno::MessageTooLarge)?)
                .min(node.data.len());
            Ok(node.data[start..end].to_vec())
        }

        fn write(&mut self, path: &str, offset: u64, data: &[u8]) -> Result<u32, Errno> {
            let start = usize::try_from(offset).map_err(|_| Errno::NoSpace)?;
            let end = start.checked_add(data.len()).ok_or(Errno::NoSpace)?;
            let generation = self.next_generation();
            let node = self.nodes.get_mut(path).ok_or(Errno::NotFound)?;
            if node.metadata.kind != NodeKind::File {
                return Err(Errno::IsDirectory);
            }
            node.data.resize(node.data.len().max(end), 0);
            node.data[start..end].copy_from_slice(data);
            node.metadata.len = node.data.len() as u64;
            node.metadata.generation = generation;
            u32::try_from(data.len()).map_err(|_| Errno::MessageTooLarge)
        }

        fn create_file(
            &mut self,
            path: &str,
            mode: u32,
            exclusive: bool,
            truncate: bool,
        ) -> Result<(), Errno> {
            self.require_parent(path)?;
            if let Some(existing) = self.nodes.get(path) {
                if exclusive {
                    return Err(Errno::AlreadyExists);
                }
                if existing.metadata.kind != NodeKind::File {
                    return Err(Errno::IsDirectory);
                }
            }
            let generation = self.next_generation();
            let node = self.nodes.entry(path.to_string()).or_insert(MemoryNode {
                metadata: metadata(NodeKind::File, 0, mode, generation),
                data: Vec::new(),
            });
            if truncate {
                node.data.clear();
                node.metadata.len = 0;
                node.metadata.generation = generation;
            }
            Ok(())
        }

        fn create_dir(&mut self, path: &str, mode: u32) -> Result<(), Errno> {
            self.require_parent(path)?;
            if self.nodes.contains_key(path) {
                return Err(Errno::AlreadyExists);
            }
            let generation = self.next_generation();
            self.nodes.insert(
                path.to_string(),
                MemoryNode {
                    metadata: metadata(NodeKind::Directory, 0, mode, generation),
                    data: Vec::new(),
                },
            );
            Ok(())
        }

        fn set_len(&mut self, path: &str, len: u64) -> Result<(), Errno> {
            let len = usize::try_from(len).map_err(|_| Errno::NoSpace)?;
            let generation = self.next_generation();
            let node = self.nodes.get_mut(path).ok_or(Errno::NotFound)?;
            if node.metadata.kind != NodeKind::File {
                return Err(Errno::IsDirectory);
            }
            node.data.resize(len, 0);
            node.metadata.len = len as u64;
            node.metadata.generation = generation;
            Ok(())
        }

        fn remove_file(&mut self, path: &str) -> Result<(), Errno> {
            match self.nodes.get(path) {
                Some(node) if node.metadata.kind == NodeKind::File => {}
                Some(_) => return Err(Errno::IsDirectory),
                None => return Err(Errno::NotFound),
            }
            self.nodes.remove(path);
            Ok(())
        }

        fn remove_dir(&mut self, path: &str) -> Result<(), Errno> {
            match self.nodes.get(path) {
                Some(node) if node.metadata.kind == NodeKind::Directory => {}
                Some(_) => return Err(Errno::NotDirectory),
                None => return Err(Errno::NotFound),
            }
            let prefix = format!("{path}/");
            if self
                .nodes
                .keys()
                .any(|candidate| candidate.starts_with(&prefix))
            {
                return Err(Errno::NotEmpty);
            }
            self.nodes.remove(path);
            Ok(())
        }

        fn rename(&mut self, source: &str, destination: &str) -> Result<(), Errno> {
            self.require_parent(destination)?;
            if !self.nodes.contains_key(source) {
                return Err(Errno::NotFound);
            }
            if self.nodes.contains_key(destination) {
                return Err(Errno::AlreadyExists);
            }
            let prefix = format!("{source}/");
            let moved: Vec<_> = self
                .nodes
                .iter()
                .filter(|(path, _)| path.as_str() == source || path.starts_with(&prefix))
                .map(|(path, node)| (path.clone(), node.clone()))
                .collect();
            for (path, _) in &moved {
                self.nodes.remove(path);
            }
            for (path, node) in moved {
                let suffix = path.strip_prefix(source).expect("selected source prefix");
                self.nodes.insert(format!("{destination}{suffix}"), node);
            }
            Ok(())
        }

        fn sync(&mut self, path: &str, _data_only: bool) -> Result<(), Errno> {
            self.nodes
                .contains_key(path)
                .then_some(())
                .ok_or(Errno::NotFound)
        }
    }

    fn metadata(kind: NodeKind, len: u64, mode: u32, generation: u64) -> Metadata {
        Metadata {
            kind,
            len,
            mode,
            modified_seconds: generation,
            generation,
        }
    }

    fn message(message_type: u8, tag: u16, body: impl FnOnce(&mut Writer)) -> Vec<u8> {
        let mut writer = Writer::response(message_type, tag);
        body(&mut writer);
        writer
            .finish(MAX_MESSAGE_BYTES)
            .expect("bounded test message")
    }

    fn response_type(response: &[u8]) -> u8 {
        response[4]
    }

    fn response_body(response: &[u8]) -> Reader<'_> {
        Reader::new(&response[HEADER_BYTES..])
    }

    fn assert_success(response: &[u8], request_type: u8) {
        assert_eq!(response_type(response), request_type + 1, "{response:?}");
    }

    fn negotiate_and_attach(session: &mut Session<MemoryFileSystem>) {
        let version = message(TVERSION, NOTAG, |writer| {
            writer.u32(MAX_MESSAGE_BYTES as u32);
            writer.string(VERSION).expect("version");
        });
        let response = session.serve(&version);
        assert_success(&response, TVERSION);

        let attach = message(TATTACH, 1, |writer| {
            writer.u32(1);
            writer.u32(NOFID);
            writer.string("agent").expect("uname");
            writer.string("home").expect("aname");
            writer.u32(1000);
        });
        assert_success(&session.serve(&attach), TATTACH);
    }

    #[test]
    fn malformed_messages_and_wrong_exports_fail_as_rlerror() {
        let mut session = Session::new(MemoryFileSystem::new(), "home").expect("session");
        let mut malformed = message(TVERSION, 9, |writer| {
            writer.u32(MAX_MESSAGE_BYTES as u32);
            writer.string(VERSION).expect("version");
        });
        malformed[0] = 1;
        let response = session.serve(&malformed);
        assert_eq!(response_type(&response), RLERROR);
        assert_eq!(
            response_body(&response).u32(),
            Ok(Errno::InvalidArgument.code())
        );

        let version = message(TVERSION, NOTAG, |writer| {
            writer.u32(MAX_MESSAGE_BYTES as u32);
            writer.string(VERSION).expect("version");
        });
        assert_success(&session.serve(&version), TVERSION);
        let attach = message(TATTACH, 10, |writer| {
            writer.u32(1);
            writer.u32(NOFID);
            writer.string("agent").expect("uname");
            writer.string("workspace").expect("aname");
            writer.u32(1000);
        });
        let response = session.serve(&attach);
        assert_eq!(response_type(&response), RLERROR);
        assert_eq!(response_body(&response).u32(), Ok(Errno::Permission.code()));

        let mut strict = Session::new(MemoryFileSystem::new(), "home").expect("session");
        negotiate_and_attach(&mut strict);
        let mkdir_with_trailing_data = message(TMKDIR, 11, |writer| {
            writer.u32(1);
            writer.string("must-not-exist").expect("name");
            writer.u32(0o755);
            writer.u32(1000);
            writer.u8(0xff);
        });
        let response = strict.serve(&mkdir_with_trailing_data);
        assert_eq!(response_type(&response), RLERROR);
        assert!(!strict.filesystem().nodes.contains_key("must-not-exist"));
    }

    #[test]
    fn linux_file_lifecycle_is_positional_bounded_and_rename_safe() {
        let mut session = Session::new(MemoryFileSystem::new(), "home").expect("session");
        negotiate_and_attach(&mut session);

        let mkdir = message(TMKDIR, 2, |writer| {
            writer.u32(1);
            writer.string("src").expect("name");
            writer.u32(0o755);
            writer.u32(1000);
        });
        assert_success(&session.serve(&mkdir), TMKDIR);

        let walk_src = message(TWALK, 3, |writer| {
            writer.u32(1);
            writer.u32(2);
            writer.u16(1);
            writer.string("src").expect("name");
        });
        assert_success(&session.serve(&walk_src), TWALK);

        let create = message(TLCREATE, 4, |writer| {
            writer.u32(2);
            writer.string("main.rs").expect("name");
            writer.u32(OPEN_WRITE_ONLY | OPEN_EXCLUSIVE | OPEN_TRUNCATE);
            writer.u32(0o644);
            writer.u32(1000);
        });
        assert_success(&session.serve(&create), TLCREATE);

        let source = b"fn main() {}\n";
        let write = message(TWRITE, 5, |writer| {
            writer.u32(2);
            writer.u64(0);
            writer.u32(source.len() as u32);
            writer.extend(source.to_vec());
        });
        let response = session.serve(&write);
        assert_success(&response, TWRITE);
        assert_eq!(response_body(&response).u32(), Ok(source.len() as u32));

        let clunk = message(TCLUNK, 6, |writer| writer.u32(2));
        assert_success(&session.serve(&clunk), TCLUNK);

        let walk_file = message(TWALK, 7, |writer| {
            writer.u32(1);
            writer.u32(3);
            writer.u16(2);
            writer.string("src").expect("name");
            writer.string("main.rs").expect("name");
        });
        assert_success(&session.serve(&walk_file), TWALK);
        let open = message(TLOPEN, 8, |writer| {
            writer.u32(3);
            writer.u32(0);
        });
        assert_success(&session.serve(&open), TLOPEN);
        let read = message(TREAD, 9, |writer| {
            writer.u32(3);
            writer.u64(0);
            writer.u32(4096);
        });
        let response = session.serve(&read);
        assert_success(&response, TREAD);
        let mut body = response_body(&response);
        assert_eq!(body.u32(), Ok(source.len() as u32));
        assert_eq!(body.bytes(source.len()), Ok(source.as_slice()));

        let rename = message(TRENAMEAT, 10, |writer| {
            writer.u32(1);
            writer.string("src").expect("old name");
            writer.u32(1);
            writer.string("code").expect("new name");
        });
        assert_success(&session.serve(&rename), TRENAMEAT);
        assert_eq!(
            session.fids.get(&3).map(|fid| fid.path.as_str()),
            Some("code/main.rs")
        );

        let open_root = message(TLOPEN, 11, |writer| {
            writer.u32(1);
            writer.u32(0);
        });
        assert_success(&session.serve(&open_root), TLOPEN);
        let readdir = message(TREADDIR, 12, |writer| {
            writer.u32(1);
            writer.u64(0);
            writer.u32(4096);
        });
        let response = session.serve(&readdir);
        assert_success(&response, TREADDIR);
        assert!(response.windows(4).any(|window| window == b"code"));

        let unlink = message(TUNLINKAT, 13, |writer| {
            writer.u32(1);
            writer.string("code").expect("name");
            writer.u32(AT_REMOVE_DIR);
        });
        let response = session.serve(&unlink);
        assert_eq!(response_type(&response), RLERROR);
        assert_eq!(response_body(&response).u32(), Ok(Errno::NotEmpty.code()));
    }

    #[test]
    fn partial_walk_binds_only_the_last_existing_component() {
        let mut filesystem = MemoryFileSystem::new();
        filesystem
            .create_dir("present", 0o755)
            .expect("seed directory");
        let mut session = Session::new(filesystem, "home").expect("session");
        negotiate_and_attach(&mut session);

        let walk = message(TWALK, 20, |writer| {
            writer.u32(1);
            writer.u32(2);
            writer.u16(2);
            writer.string("present").expect("name");
            writer.string("missing").expect("name");
        });
        let response = session.serve(&walk);
        assert_success(&response, TWALK);
        assert_eq!(response_body(&response).u16(), Ok(1));
        assert_eq!(
            session.fids.get(&2).map(|fid| fid.path.as_str()),
            Some("present")
        );
    }

    #[test]
    fn unlink_marks_other_fids_to_the_removed_path_stale() {
        let mut filesystem = MemoryFileSystem::new();
        filesystem
            .create_file("victim", 0o644, true, false)
            .expect("seed file");
        let mut session = Session::new(filesystem, "home").expect("session");
        negotiate_and_attach(&mut session);

        for (tag, fid) in [(40, 2), (41, 3)] {
            let walk = message(TWALK, tag, |writer| {
                writer.u32(1);
                writer.u32(fid);
                writer.u16(1);
                writer.string("victim").expect("name");
            });
            assert_success(&session.serve(&walk), TWALK);
        }
        let unlink = message(TUNLINKAT, 42, |writer| {
            writer.u32(1);
            writer.string("victim").expect("name");
            writer.u32(0);
        });
        assert_success(&session.serve(&unlink), TUNLINKAT);

        let open_stale = message(TLOPEN, 43, |writer| {
            writer.u32(2);
            writer.u32(0);
        });
        let response = session.serve(&open_stale);
        assert_eq!(response_type(&response), RLERROR);
        assert_eq!(response_body(&response).u32(), Ok(Errno::Stale.code()));
        assert!(matches!(session.fid(3), Err(Errno::Stale)));
    }

    #[test]
    fn qid_identity_table_is_bounded_across_path_churn() {
        let mut session = Session::new(MemoryFileSystem::new(), "workspace").expect("session");
        let metadata = metadata(NodeKind::File, 0, 0o644, 1);

        for index in 1..MAX_QID_PATHS {
            session
                .qid_for(&format!("synthetic-{index}"), &metadata)
                .expect("identity within cap");
        }
        assert_eq!(session.qid_paths.len(), MAX_QID_PATHS);
        assert_eq!(
            session.qid_for("one-too-many", &metadata),
            Err(Errno::NoSpace)
        );
    }

    #[test]
    fn materialized_directory_is_rejected_above_the_backend_cap() {
        let mut filesystem = MemoryFileSystem::new();
        for index in 0..=MAX_DIRECTORY_ENTRIES {
            filesystem
                .create_file(&format!("entry-{index}"), 0o644, true, false)
                .expect("seed bounded directory");
        }
        let mut session = Session::new(filesystem, "home").expect("session");
        negotiate_and_attach(&mut session);
        let open_root = message(TLOPEN, 30, |writer| {
            writer.u32(1);
            writer.u32(0);
        });
        assert_success(&session.serve(&open_root), TLOPEN);
        let readdir = message(TREADDIR, 31, |writer| {
            writer.u32(1);
            writer.u64(0);
            writer.u32(4096);
        });
        let response = session.serve(&readdir);

        assert_eq!(response_type(&response), RLERROR);
        assert_eq!(response_body(&response).u32(), Ok(Errno::NoSpace.code()));
    }
}
