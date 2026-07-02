//! End-to-end conversion pipeline: BLF + DBC -> CSV/Parquet.

use crate::blf::BlfReader;
use crate::dbc::Dbc;
use crate::decode::Decoder;
use crate::export::csv::CsvExporter;
use crate::export::parquet::ParquetExporter;
use crate::export::{Exporter, OutputFormat};
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Default)]
pub struct Progress {
    /// Compressed bytes consumed from the BLF file.
    pub bytes_read: u64,
    pub total_bytes: u64,
    pub frames_read: u64,
    pub frames_decoded: u64,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        if self.total_bytes == 0 {
            0.0
        } else {
            (self.bytes_read as f64 / self.total_bytes as f64).min(1.0) as f32
        }
    }
}

#[derive(Debug, Clone)]
pub struct Summary {
    pub output_path: PathBuf,
    pub frames_read: u64,
    /// Frames whose CAN ID matched a DBC message.
    pub frames_decoded: u64,
    pub signal_columns: usize,
}

/// Derive the output file path: `<out_dir>/<blf stem>.<ext>`.
pub fn output_path(blf_path: &Path, out_dir: &Path, format: OutputFormat) -> PathBuf {
    let stem = blf_path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "output".to_string());
    out_dir.join(format!("{stem}.{}", format.extension()))
}

/// Run the full conversion. `progress` is invoked periodically (every few
/// thousand frames) with cumulative counters; use it to drive a progress bar.
pub fn convert(
    blf_path: &Path,
    dbc_path: &Path,
    out_dir: &Path,
    format: OutputFormat,
    progress: &mut dyn FnMut(Progress),
) -> Result<Summary> {
    let dbc = Dbc::from_file(dbc_path)?;
    if dbc.messages.is_empty() {
        bail!(
            "no messages found in DBC file {} (is it a valid DBC?)",
            dbc_path.display()
        );
    }
    let decoder = Decoder::new(&dbc);

    let file = File::open(blf_path)
        .with_context(|| format!("failed to open BLF file {}", blf_path.display()))?;
    let total_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BlfReader::new(BufReader::with_capacity(1 << 20, file))?;
    let time_base = reader.start_time.to_epoch_seconds();

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create output folder {}", out_dir.display()))?;
    let out_path = output_path(blf_path, out_dir, format);
    let mut exporter: Box<dyn Exporter> = match format {
        OutputFormat::Csv => Box::new(CsvExporter::create(&out_path, decoder.columns())?),
        OutputFormat::Parquet => Box::new(ParquetExporter::create(&out_path, decoder.columns())?),
    };

    let mut state = Progress {
        total_bytes,
        ..Default::default()
    };
    let mut cells: Vec<(usize, f64)> = Vec::new();
    while let Some(frame) = reader.next_frame()? {
        state.frames_read += 1;
        if decoder.decode(&frame, &mut cells) {
            state.frames_decoded += 1;
            let timestamp = time_base + frame.timestamp_ns as f64 / 1e9;
            exporter.write_row(timestamp, &cells)?;
        }
        if state.frames_read.is_multiple_of(4096) {
            state.bytes_read = reader.bytes_read();
            progress(state);
        }
    }
    exporter.finish()?;
    state.bytes_read = reader.bytes_read();
    progress(state);

    Ok(Summary {
        output_path: out_path,
        frames_read: state.frames_read,
        frames_decoded: state.frames_decoded,
        signal_columns: decoder.columns().len(),
    })
}
