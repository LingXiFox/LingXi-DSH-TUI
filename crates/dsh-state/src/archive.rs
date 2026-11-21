//! Private, per-process NDJSON archives and a disposable transcript spool.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dsh_protocol::{protocol::Frame, stdio::read_frame};

pub const BODY_BUDGET: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Record {
    pub source: usize,
    pub offset: u64,
}

/// A projected transcript body, fetched only when visible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Text(Body);

#[derive(Clone, Debug, PartialEq, Eq)]
enum Body {
    Memory(Arc<String>),
    Disk { offset: u64, bytes: usize },
}

impl Default for Text {
    fn default() -> Self {
        Self(Body::Memory(Arc::default()))
    }
}

impl Text {
    /// Whether the projected body contains no text.
    pub fn is_empty(&self) -> bool {
        match &self.0 {
            Body::Memory(body) => body.is_empty(),
            Body::Disk { bytes, .. } => *bytes == 0,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct Archive {
    memory: Vec<Frame>,
    disk: Option<Disk>,
}

#[derive(Debug)]
struct Disk {
    directory: PathBuf,
    active: Option<Journal>,
    previous: Vec<PathBuf>,
    previous_reader: Option<(usize, BufReader<File>)>,
    spool: RefCell<Spool>,
}

#[derive(Debug)]
struct Journal {
    path: PathBuf,
    file: File,
    reader: BufReader<File>,
    offset: u64,
    count: usize,
}

#[derive(Debug)]
struct Spool {
    path: PathBuf,
    file: Option<File>,
    end: u64,
    cache: VecDeque<(u64, Arc<String>)>,
    bytes: usize,
}

impl Drop for Spool {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

fn create(path: &Path) -> io::Result<File> {
    let mut opts = OpenOptions::new();
    opts.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

impl Archive {
    pub fn open(directory: &Path, name: &str, record: bool) -> io::Result<Self> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(directory)?;
        let path = directory.join(format!("{name}.spool"));
        let spool = RefCell::new(Spool {
            file: Some(create(&path)?),
            path,
            end: 0,
            cache: VecDeque::new(),
            bytes: 0,
        });
        let active = if record {
            let path = directory.join(format!("{name}.ndjson"));
            let file = create(&path)?;
            let reader = BufReader::new(File::open(&path)?);
            Some(Journal {
                path,
                file,
                reader,
                offset: 0,
                count: 0,
            })
        } else {
            None
        };
        Ok(Self {
            memory: Vec::new(),
            disk: Some(Disk {
                directory: directory.into(),
                active,
                previous: Vec::new(),
                previous_reader: None,
                spool,
            }),
        })
    }

    pub fn count(&self) -> usize {
        self.disk.as_ref().map_or(self.memory.len(), |disk| {
            disk.active.as_ref().map_or(0, |journal| journal.count)
        })
    }

    pub fn append(&mut self, frame: &Frame) -> io::Result<Record> {
        let offset = if let Some(disk) = &mut self.disk {
            let journal = disk
                .active
                .as_mut()
                .ok_or_else(|| io::Error::other("replay archive is read-only"))?;
            let offset = journal.offset;
            let mut bytes = serde_json::to_vec(frame).map_err(io::Error::other)?;
            bytes.push(b'\n');
            journal.file.write_all(&bytes)?;
            journal.offset += bytes.len() as u64;
            journal.count += 1;
            offset
        } else {
            self.memory.push(frame.clone());
            (self.memory.len() - 1) as u64
        };
        Ok(Record { source: 0, offset })
    }

    pub fn read(&mut self, record: Record) -> io::Result<Frame> {
        let Some(disk) = &mut self.disk else {
            return Ok(self.memory[record.offset as usize].clone());
        };
        let reader = if record.source == 0 {
            &mut disk
                .active
                .as_mut()
                .ok_or_else(|| io::Error::other("archive journal missing"))?
                .reader
        } else {
            let reader = &mut disk.previous_reader;
            if reader
                .as_ref()
                .is_none_or(|(source, _)| *source != record.source)
            {
                let file = File::open(&disk.previous[record.source - 1])?;
                *reader = Some((record.source, BufReader::new(file)));
            }
            &mut reader
                .as_mut()
                .ok_or_else(|| io::Error::other("archive reader missing"))?
                .1
        };
        reader.seek(SeekFrom::Start(record.offset))?;
        read_frame(reader)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "missing archived frame"))
    }

    pub fn paths(&self, include_active: bool) -> io::Result<Vec<PathBuf>> {
        let Some(disk) = &self.disk else {
            return Ok(Vec::new());
        };
        let mut paths = Vec::new();
        for item in fs::read_dir(&disk.directory)? {
            let item = item?;
            let path = item.path();
            if (include_active
                || disk
                    .active
                    .as_ref()
                    .is_none_or(|active| path != active.path))
                && item.file_type()?.is_file()
                && path.extension().is_some_and(|ext| ext == "ndjson")
            {
                paths.push(path);
            }
        }
        paths.sort_unstable();
        Ok(paths)
    }

    pub fn source(&mut self, path: &Path) -> io::Result<usize> {
        let disk = self
            .disk
            .as_mut()
            .ok_or_else(|| io::Error::other("archive unavailable"))?;
        if let Some(index) = disk.previous.iter().position(|old| old == path) {
            return Ok(index + 1);
        }
        disk.previous.push(path.into());
        Ok(disk.previous.len())
    }

    pub fn sync(&self) -> io::Result<()> {
        self.disk
            .as_ref()
            .and_then(|disk| disk.active.as_ref())
            .map_or(Ok(()), |journal| journal.file.sync_data())
    }

    pub fn text(&mut self, text: String) -> io::Result<Text> {
        let Some(disk) = &mut self.disk else {
            return Ok(Text(Body::Memory(Arc::new(text))));
        };
        if text.is_empty() {
            return Ok(Text::default());
        }
        let spool = disk.spool.get_mut();
        let file = spool
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("transcript spool closed"))?;
        let offset = spool.end;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(text.as_bytes())?;
        spool.end += text.len() as u64;
        Ok(Text(Body::Disk {
            offset,
            bytes: text.len(),
        }))
    }

    pub fn body(&self, text: &Text) -> io::Result<Arc<String>> {
        let (offset, bytes) = match &text.0 {
            Body::Memory(body) => return Ok(body.clone()),
            Body::Disk { offset, bytes } => (*offset, *bytes),
        };
        let mut spool = self
            .disk
            .as_ref()
            .ok_or_else(|| io::Error::other("transcript spool missing"))?
            .spool
            .borrow_mut();
        if let Some(index) = spool.cache.iter().position(|(key, _)| *key == offset) {
            let entry = spool
                .cache
                .remove(index)
                .ok_or_else(|| io::Error::other("transcript cache entry missing"))?;
            let body = entry.1.clone();
            spool.cache.push_back(entry);
            return Ok(body);
        }
        let file = spool
            .file
            .as_mut()
            .ok_or_else(|| io::Error::other("transcript spool closed"))?;
        file.seek(SeekFrom::Start(offset))?;
        let mut data = vec![0; bytes];
        file.read_exact(&mut data)?;
        let body = Arc::new(String::from_utf8(data).map_err(io::Error::other)?);
        if bytes <= BODY_BUDGET {
            while spool.bytes + bytes > BODY_BUDGET || spool.cache.len() >= 256 {
                if let Some((_, old)) = spool.cache.pop_front() {
                    spool.bytes -= old.len();
                }
            }
            spool.bytes += bytes;
            spool.cache.push_back((offset, body.clone()));
        }
        Ok(body)
    }
}
