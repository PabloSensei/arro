use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};

use crate::GitRunner;

/// One `git cat-file --batch` that stays up: a process per gutter refresh would cost a
/// spawn each time. Not shared between threads, each owner keeps its own.
pub struct BlobReader {
    child: Child,
    input: ChildStdin,
    output: BufReader<ChildStdout>,
}

impl BlobReader {
    pub fn start(git: &GitRunner, root: &Path) -> io::Result<Self> {
        let mut command = git.command(root);
        command
            .args(["cat-file", "--batch"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = command.spawn()?;
        let input = child.stdin.take().ok_or(io::ErrorKind::BrokenPipe)?;
        let output = BufReader::new(child.stdout.take().ok_or(io::ErrorKind::BrokenPipe)?);
        Ok(Self {
            child,
            input,
            output,
        })
    }

    /// The content of an object like `HEAD:src/a.txt`, `None` when there is no such one.
    pub fn read(&mut self, object: &str) -> io::Result<Option<Vec<u8>>> {
        if object.contains('\n') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a newline in a name",
            ));
        }
        writeln!(self.input, "{object}")?;
        self.input.flush()?;
        let mut header = String::new();
        if self.output.read_line(&mut header)? == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        // `<oid> <type> <size>`, or `<name> missing` and the like with no content.
        let mut fields = header.trim_end().rsplitn(3, ' ');
        let size = fields.next().and_then(|size| size.parse::<usize>().ok());
        let kind = fields.next();
        let Some(size) = size.filter(|_| kind.is_some()) else {
            return Ok(None);
        };
        let mut content = vec![0; size + 1];
        self.output.read_exact(&mut content)?;
        content.pop();
        Ok((kind == Some("blob")).then_some(content))
    }
}

impl Drop for BlobReader {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
