//! End-to-end conversion pipeline: BLF + DBC -> CSV/Parquet.

use crate::blf::BlfReader;
use crate::dbc::Dbc;
use crate::decode::Decoder;
use crate::export::csv::CsvExporter;
use crate::export::parquet::ParquetExporter;
use crate::export::{Exporter, OutputFormat};
use crate::shape::Resampler;
use anyhow::{Context, Result, bail};
use std::collections::HashSet;
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

#[derive(Debug, Clone)]
pub struct ConvertOptions {
    pub format: OutputFormat,
    pub layout: OutputLayout,
    /// Grid spacing in milliseconds when `layout == Resampled`.
    pub resample_ms: f64,
    pub timestamp: TimestampMode,
    /// Add a `CanId` column with the frame's CAN ID (per-frame layout only).
    pub keep_can_id: bool,
    /// When false, a frame whose CAN ID is missing from the DBC aborts the
    /// conversion with an error naming the ID. When true (default), unknown
    /// IDs are silently skipped.
    pub skip_unknown_ids: bool,
    /// When false, refuse to replace an existing output file.
    pub overwrite: bool,
    /// Restrict the output to these column names (see `decode::list_columns`).
    /// `None` exports every signal in the DBC.
    pub signal_filter: Option<HashSet<String>>,
    /// Drop columns for signals that never carry a value in this BLF.
    /// Requires an extra scan pass over the input file.
    pub drop_empty_columns: bool,
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self {
            format: OutputFormat::Csv,
            layout: OutputLayout::Resampled,
            resample_ms: 100.0,
            timestamp: TimestampMode::RelativeSeconds,
            keep_can_id: false,
            skip_unknown_ids: true,
            overwrite: true,
            signal_filter: None,
            drop_empty_columns: false,
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

/// Periodic progress report passed to the progress callback.
pub struct ProgressUpdate<'a> {
    pub progress: Progress,
    /// DBC name of the most recently decoded message, when known.
    pub current_message: Option<&'a str>,
    /// True during the pre-scan pass used by `drop_empty_columns`.
    pub scanning: bool,
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

/// Pre-scan pass for `drop_empty_columns`: decode every frame and record
/// which columns receive at least one value.
fn scan_observed_columns(
    blf_path: &Path,
    decoder: &Decoder,
    progress: &mut dyn FnMut(ProgressUpdate<'_>),
) -> Result<Vec<bool>> {
    let file = File::open(blf_path)
        .with_context(|| format!("failed to open BLF file {}", blf_path.display()))?;
    let total_bytes = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mut reader = BlfReader::new(BufReader::with_capacity(1 << 20, file))?;

    let mut observed = vec![false; decoder.columns().len()];
    let mut remaining = observed.len();
    let mut state = Progress {
        total_bytes,
        ..Default::default()
    };
    let mut cells: Vec<(usize, f64)> = Vec::new();
    while let Some(frame) = reader.next_frame()? {
        state.frames_read += 1;
        if decoder.decode(&frame, &mut cells) {
            state.frames_decoded += 1;
            for &(col, _) in &cells {
                if !observed[col] {
                    observed[col] = true;
                    remaining -= 1;
                }
            }
            // Every column seen: no need to read the rest of the file.
            if remaining == 0 {
                break;
            }
        }
        if state.frames_read.is_multiple_of(4096) {
            state.bytes_read = reader.bytes_read();
            progress(ProgressUpdate {
                progress: state,
                current_message: None,
                scanning: true,
            });
        }
    }
    Ok(observed)
}

/// Run the full conversion. `progress` is invoked periodically (every few
/// thousand frames) with cumulative counters; use it to drive a progress bar.
pub fn convert(
    blf_path: &Path,
    dbc_path: &Path,
    out_dir: &Path,
    options: ConvertOptions,
    progress: &mut dyn FnMut(ProgressUpdate<'_>),
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
    let mut decoder = Decoder::with_filter(&dbc, options.signal_filter.as_ref());
    if decoder.columns().is_empty() {
        bail!("no signals selected for export");
    }

    // Optional pre-scan: narrow the columns down to signals that actually
    // carry data somewhere in this BLF.
    if options.drop_empty_columns {
        let observed = scan_observed_columns(blf_path, &decoder, progress)?;
        let kept: HashSet<String> = decoder
            .columns()
            .iter()
            .zip(&observed)
            .filter(|(_, seen)| **seen)
            .map(|(name, _)| name.clone())
            .collect();
        if kept.is_empty() {
            bail!("no signals with data found in this BLF");
        }
        if kept.len() < decoder.columns().len() {
            decoder = Decoder::with_filter(&dbc, Some(&kept));
        }
    }

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
    if !options.overwrite && out_path.exists() {
        bail!("output file already exists: {}", out_path.display());
    }

    // The CanId column only makes sense per frame; a forward-filled grid row
    // mixes many messages.
    let keep_can_id = options.keep_can_id && options.layout == OutputLayout::PerFrame;
    let mut header: Vec<String> = Vec::with_capacity(decoder.columns().len() + 1);
    if keep_can_id {
        header.push("CanId".to_string());
    }
    header.extend(decoder.columns().iter().cloned());

    let mut exporter: Box<dyn Exporter> = match options.format {
        OutputFormat::Csv => Box::new(CsvExporter::create(&out_path, &header)?),
        OutputFormat::Parquet => Box::new(ParquetExporter::create(&out_path, &header)?),
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
    let mut row: Vec<(usize, f64)> = Vec::new();
    let mut last_decoded_key: Option<(u32, bool)> = None;
    let mut last_frame_for_name: Option<crate::blf::CanFrame> = None;

    while let Some(frame) = reader.next_frame()? {
        state.frames_read += 1;
        if decoder.decode(&frame, &mut cells) {
            state.frames_decoded += 1;
            let timestamp = time_base + frame.timestamp_ns as f64 / 1e9;
            match &mut resampler {
                Some(resampler) => {
                    resampler.push(timestamp, &cells, &mut |ts, grid_row| {
                        rows_written += 1;
                        exporter.write_row(ts, grid_row)
                    })?;
                }
                None => {
                    rows_written += 1;
                    if keep_can_id {
                        row.clear();
                        row.push((0, frame.id as f64));
                        row.extend(cells.iter().map(|&(c, v)| (c + 1, v)));
                        exporter.write_row(timestamp, &row)?;
                    } else {
                        exporter.write_row(timestamp, &cells)?;
                    }
                }
            }
            if last_decoded_key != Some((frame.id, frame.is_extended)) {
                last_decoded_key = Some((frame.id, frame.is_extended));
                last_frame_for_name = Some(frame.clone());
            }
        } else if !options.skip_unknown_ids && !frame.is_remote && !decoder.contains_id(&frame) {
            bail!(
                "DBC does not contain message 0x{:X}{}",
                frame.id,
                if frame.is_extended { " (extended)" } else { "" }
            );
        }
        if state.frames_read.is_multiple_of(4096) {
            state.bytes_read = reader.bytes_read();
            progress(ProgressUpdate {
                progress: state,
                current_message: last_frame_for_name
                    .as_ref()
                    .and_then(|f| decoder.message_name(f)),
                scanning: false,
            });
        }
    }
    if let Some(resampler) = &mut resampler {
        resampler.finish(&mut |ts, grid_row| {
            rows_written += 1;
            exporter.write_row(ts, grid_row)
        })?;
    }
    exporter.finish()?;
    state.bytes_read = reader.bytes_read();
    progress(ProgressUpdate {
        progress: state,
        current_message: None,
        scanning: false,
    });

    Ok(Summary {
        output_path: out_path,
        frames_read: state.frames_read,
        frames_decoded: state.frames_decoded,
        rows_written,
        signal_columns: decoder.columns().len(),
    })
}
