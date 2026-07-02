//! End-to-end conversion pipeline: BLF + DBC -> CSV/Parquet.

use crate::blf::BlfReader;
use crate::dbc::Dbc;
use crate::decode::Decoder;
use crate::export::csv::CsvExporter;
use crate::export::parquet::ParquetExporter;
use crate::export::{Exporter, OutputFormat};
use crate::shape::Resampler;
use anyhow::{Context, Result, bail};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};

/// Shape of the output table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OutputLayout {
    /// Fixed-interval grid, forward-filled: every row carries the latest
    /// value of every signal (the "normal" analysis-ready table).
    #[default]
    Resampled,
    /// One row per CAN frame; only that frame's signals are filled.
    PerFrame,
}

/// What the Timestamp column contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TimestampMode {
    /// Seconds since the first frame (0.0, 0.1, ...).
    #[default]
    RelativeSeconds,
    /// Seconds since the Unix epoch (measurement start + offset).
    EpochSeconds,
}

#[derive(Debug, Clone, Copy)]
pub struct ConvertOptions {
    pub format: OutputFormat,
    pub layout: OutputLayout,
    /// Grid spacing in milliseconds when `layout == Resampled`.
    pub resample_ms: f64,
    pub timestamp: TimestampMode,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Csv,
            layout: OutputLayout::Resampled,
            resample_ms: 100.0,
            timestamp: TimestampMode::RelativeSeconds,
        }
    }
}

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
    /// Rows written to the output file.
    pub rows_written: u64,
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
    options: ConvertOptions,
    progress: &mut dyn FnMut(Progress),
) -> Result<Summary> {
    if options.layout == OutputLayout::Resampled
        && (!options.resample_ms.is_finite() || options.resample_ms <= 0.0)
    {
        bail!("resample interval must be greater than 0 ms");
    }
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
    let time_base = match options.timestamp {
        TimestampMode::RelativeSeconds => 0.0,
        TimestampMode::EpochSeconds => reader.start_time.to_epoch_seconds(),
    };

    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("failed to create output folder {}", out_dir.display()))?;
    let out_path = output_path(blf_path, out_dir, options.format);
    let mut exporter: Box<dyn Exporter> = match options.format {
        OutputFormat::Csv => Box::new(CsvExporter::create(&out_path, decoder.columns())?),
        OutputFormat::Parquet => Box::new(ParquetExporter::create(&out_path, decoder.columns())?),
    };

    let mut resampler = match options.layout {
        OutputLayout::Resampled => Some(Resampler::new(
            decoder.columns().len(),
            options.resample_ms / 1000.0,
        )),
        OutputLayout::PerFrame => None,
    };

    let mut state = Progress {
        total_bytes,
        ..Default::default()
    };
    let mut rows_written: u64 = 0;
    let mut cells: Vec<(usize, f64)> = Vec::new();
    while let Some(frame) = reader.next_frame()? {
        state.frames_read += 1;
        if decoder.decode(&frame, &mut cells) {
            state.frames_decoded += 1;
            let timestamp = time_base + frame.timestamp_ns as f64 / 1e9;
            match &mut resampler {
                Some(resampler) => {
                    resampler.push(timestamp, &cells, &mut |ts, row| {
                        rows_written += 1;
                        exporter.write_row(ts, row)
                    })?;
                }
                None => {
                    rows_written += 1;
                    exporter.write_row(timestamp, &cells)?;
                }
            }
        }
        if state.frames_read.is_multiple_of(4096) {
            state.bytes_read = reader.bytes_read();
            progress(state);
        }
    }
    if let Some(resampler) = &mut resampler {
        resampler.finish(&mut |ts, row| {
            rows_written += 1;
            exporter.write_row(ts, row)
        })?;
    }
    exporter.finish()?;
    state.bytes_read = reader.bytes_read();
    progress(state);

    Ok(Summary {
        output_path: out_path,
        frames_read: state.frames_read,
        frames_decoded: state.frames_decoded,
        rows_written,
        signal_columns: decoder.columns().len(),
    })
}
