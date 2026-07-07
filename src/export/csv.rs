//! Streaming CSV exporter. Rows are written straight to a buffered file, so
//! memory use is independent of input size.

use super::Exporter;
use anyhow::{Context, Result};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

pub struct CsvExporter {
    writer: BufWriter<File>,
    /// Scratch row, reused across frames; `touched` tracks which cells to
    /// clear so resetting is O(cells) instead of O(columns).
    row: Vec<Option<f64>>,
    touched: Vec<usize>,
    float_buf: ryu::Buffer,
}

impl CsvExporter {
    pub fn create(path: &Path, columns: &[String]) -> Result<Self> {
        let file =
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
        let mut writer = BufWriter::with_capacity(1 << 20, file);
        write!(writer, "Timestamp")?;
        for name in columns {
            write!(writer, ",{}", escape(name))?;
        }
        writeln!(writer)?;
        Ok(Self {
            writer,
            row: vec![None; columns.len()],
            touched: Vec::new(),
            float_buf: ryu::Buffer::new(),
        })
    }
}

/// Quote a CSV field if it contains a delimiter, quote or newline.
fn escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

impl Exporter for CsvExporter {
    fn write_row(&mut self, timestamp: f64, cells: &[(usize, f64)]) -> Result<()> {
        for &(col, value) in cells {
            self.row[col] = Some(value);
            self.touched.push(col);
        }
        self.writer
            .write_all(self.float_buf.format(timestamp).as_bytes())?;
        for cell in &self.row {
            self.writer.write_all(b",")?;
            if let Some(value) = cell {
                self.writer
                    .write_all(self.float_buf.format(*value).as_bytes())?;
            }
        }
        self.writer.write_all(b"\n")?;
        for col in self.touched.drain(..) {
            self.row[col] = None;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.writer.flush().context("failed to flush CSV output")
    }
}
