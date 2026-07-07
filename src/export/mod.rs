pub mod csv;
pub mod parquet;

use anyhow::Result;

/// Row-oriented sink for decoded frames. One row per CAN frame: a timestamp
/// plus sparse `(column index, value)` cells; absent columns are null/empty.
pub trait Exporter {
    fn write_row(&mut self, timestamp: f64, cells: &[(usize, f64)]) -> Result<()>;
    /// Flush and finalize the output file.
    fn finish(&mut self) -> Result<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputFormat {
    #[default]
    Csv,
    Parquet,
}

impl OutputFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            OutputFormat::Csv => "csv",
            OutputFormat::Parquet => "parquet",
        }
    }
}
