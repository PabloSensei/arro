//! On disk: `blobs/<hash>` holds compressed contents, `index.log` one revision per line.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use xxhash_rust::xxh3::xxh3_128;

use crate::{Cause, Revision};

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub id: u64,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub path: PathBuf,
    pub cause: Cause,
    pub content: Option<String>,
    pub label: Option<String>,
}

impl Entry {
    pub fn revision(&self) -> Revision {
        Revision {
            id: self.id,
            at: UNIX_EPOCH + Duration::from_millis(self.at),
            path: self.path.clone(),
            cause: self.cause,
            content: self.content.clone(),
            label: self.label.clone(),
        }
    }

    fn line(&self) -> String {
        let content = self.content.as_deref().unwrap_or("-");
        let label = self.label.as_deref().unwrap_or("");
        let path = self.path.to_string_lossy();
        format!(
            "{}\t{}\t{}\t{}\t{path}\t{label}\n",
            self.id,
            self.at,
            self.cause.key(),
            content
        )
    }

    fn parse(line: &str) -> Option<Self> {
        let mut fields = line.splitn(6, '\t');
        let id = fields.next()?.parse().ok()?;
        let at = fields.next()?.parse().ok()?;
        let cause = Cause::parse(fields.next()?)?;
        let content = Some(fields.next()?).filter(|content| *content != "-");
        let path = PathBuf::from(fields.next()?);
        let label = fields.next().filter(|label| !label.is_empty());
        Some(Self {
            id,
            at,
            path,
            cause,
            content: content.map(str::to_string),
            label: label.map(str::to_string),
        })
    }
}

pub(crate) struct Log {
    path: PathBuf,
}

impl Log {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    /// Every entry; a line cut short by a crash is skipped.
    pub fn read(&self) -> io::Result<Vec<Entry>> {
        let file = match File::open(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut entries = Vec::new();
        for line in BufReader::new(file).lines() {
            if let Some(entry) = Entry::parse(&line?) {
                entries.push(entry);
            }
        }
        Ok(entries)
    }

    pub fn append(&self, entry: &Entry) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(entry.line().as_bytes())
    }

    /// Writes the whole log to a side file first, so a crash keeps the old one.
    pub fn rewrite(&self, entries: &[Entry]) -> io::Result<()> {
        let next = self.path.with_extension("log.next");
        let text: String = entries.iter().map(Entry::line).collect();
        fs::write(&next, text)?;
        fs::rename(&next, &self.path)
    }
}

pub(crate) struct BlobStore {
    dir: PathBuf,
}

impl BlobStore {
    pub fn new(dir: PathBuf) -> io::Result<Self> {
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    fn path_of(&self, id: &str) -> PathBuf {
        self.dir.join(id)
    }

    /// Stores `bytes` once and returns their id. The length is part of the id, so two
    /// contents share an id only when the hash and the length match.
    pub fn put(&self, bytes: &[u8]) -> io::Result<String> {
        let id = format!("{:032x}-{}", xxh3_128(bytes), bytes.len());
        let path = self.path_of(&id);
        if !path.exists() {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(bytes)?;
            let next = path.with_extension("next");
            fs::write(&next, encoder.finish()?)?;
            fs::rename(&next, &path)?;
        }
        Ok(id)
    }

    pub fn get(&self, id: &str) -> io::Result<Vec<u8>> {
        let compressed = fs::read(self.path_of(id))?;
        let mut bytes = Vec::new();
        DeflateDecoder::new(compressed.as_slice()).read_to_end(&mut bytes)?;
        Ok(bytes)
    }

    pub fn remove(&self, id: &str) -> io::Result<()> {
        match fs::remove_file(self.path_of(id)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    /// Each stored content and its size on disk.
    pub fn sizes(&self) -> io::Result<Vec<(String, u64)>> {
        let mut sizes = Vec::new();
        for entry in fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".next") {
                sizes.push((name, entry.metadata()?.len()));
            }
        }
        Ok(sizes)
    }

    pub fn total(&self) -> io::Result<u64> {
        Ok(self.sizes()?.iter().map(|(_, size)| size).sum())
    }

    #[cfg(test)]
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }
}
