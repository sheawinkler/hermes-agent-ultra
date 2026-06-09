//! NDJSON line-buffered reader and writer for ACP over IPC transport.

use std::io;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Reads NDJSON lines from an async byte stream.
pub struct NdjsonReader<R> {
    inner: BufReader<R>,
    eof: bool,
}

impl<R: AsyncRead + Unpin> NdjsonReader<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner: BufReader::new(inner),
            eof: false,
        }
    }

    /// Read the next complete NDJSON line. Returns None on EOF.
    /// Strips trailing \r characters defensively.
    /// Skips blank lines automatically.
    pub async fn read_line(&mut self) -> Option<io::Result<String>> {
        loop {
            let mut buf = String::new();
            match self.inner.read_line(&mut buf).await {
                Ok(0) => {
                    self.eof = true;
                    return None;
                }
                Ok(_) => {
                    let line = buf.trim_end_matches('\n').trim_end_matches('\r');
                    if line.is_empty() {
                        continue;
                    }
                    return Some(Ok(line.to_string()));
                }
                Err(e) => {
                    self.eof = true;
                    return Some(Err(e));
                }
            }
        }
    }

    pub fn is_eof(&self) -> bool {
        self.eof
    }
}

/// Writes NDJSON lines to an async byte stream.
/// Always appends LF (\n), never CRLF.
pub struct NdjsonWriter<W> {
    inner: W,
}

impl<W: AsyncWrite + Unpin> NdjsonWriter<W> {
    pub fn new(inner: W) -> Self {
        Self { inner }
    }

    /// Serialize a value as JSON and write it as one NDJSON line.
    pub async fn write_json(&mut self, value: &serde_json::Value) -> io::Result<()> {
        let mut json = serde_json::to_string(value)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        json.push('\n');
        self.inner.write_all(json.as_bytes()).await?;
        self.inner.flush().await?;
        Ok(())
    }

    /// Write a raw string as one NDJSON line (appends \n if missing).
    pub async fn write_line(&mut self, line: &str) -> io::Result<()> {
        self.inner.write_all(line.as_bytes()).await?;
        if !line.ends_with('\n') {
            self.inner.write_all(b"\n").await?;
        }
        self.inner.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_reader_single_line() {
        let data = b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}\n";
        let mut reader = NdjsonReader::new(&data[..]);
        let line = reader.read_line().await.unwrap().unwrap();
        assert_eq!(line, "{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}");
        assert!(reader.read_line().await.is_none());
    }

    #[tokio::test]
    async fn test_reader_strips_crlf() {
        let data = b"{\"ok\":true}\r\n";
        let mut reader = NdjsonReader::new(&data[..]);
        let line = reader.read_line().await.unwrap().unwrap();
        assert_eq!(line, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn test_reader_skips_blank_lines() {
        let data = b"\n\n{\"ok\":true}\n\n";
        let mut reader = NdjsonReader::new(&data[..]);
        let line = reader.read_line().await.unwrap().unwrap();
        assert_eq!(line, "{\"ok\":true}");
    }

    #[tokio::test]
    async fn test_reader_multiple_lines() {
        let data = b"line1\nline2\n";
        let mut reader = NdjsonReader::new(&data[..]);
        assert_eq!(reader.read_line().await.unwrap().unwrap(), "line1");
        assert_eq!(reader.read_line().await.unwrap().unwrap(), "line2");
        assert!(reader.read_line().await.is_none());
    }

    #[tokio::test]
    async fn test_writer_appends_lf() {
        let mut buf = Vec::new();
        {
            let mut writer = NdjsonWriter::new(&mut buf);
            writer
                .write_json(&serde_json::json!({"hello": "world"}))
                .await
                .unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert!(s.ends_with('\n'));
        assert!(!s.contains("\r\n"));
        assert!(s.contains("\"hello\":\"world\""));
    }
}
