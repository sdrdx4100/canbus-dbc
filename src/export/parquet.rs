//! Streaming Parquet exporter built on the low-level `parquet` column writer
//! API (no Arrow dependency). Rows are buffered per column and flushed as a
//! row group once the batch is full, keeping memory bounded.

use super::Exporter;
use anyhow::{Context, Result, anyhow};
use parquet::basic::{Compression, Repetition, Type as PhysicalType};
use parquet::data_type::DoubleType;
use parquet::file::properties::WriterProperties;
use parquet::file::writer::SerializedFileWriter;
use parquet::schema::types::Type as SchemaType;
use std::fs::File;
use std::path::Path;
use std::sync::Arc;

/// Target number of buffered cells (rows x columns) per row group. Keeps the
/// in-memory batch around a few tens of MB even for very wide DBCs.
const TARGET_CELLS_PER_GROUP: usize = 8_000_000;
const MIN_ROWS_PER_GROUP: usize = 1_024;
const MAX_ROWS_PER_GROUP: usize = 262_144;

struct ColumnBuffer {
    /// 1 = value present, 0 = null; one entry per row.
    def_levels: Vec<i16>,
    /// Densely packed values for rows where def level is 1.
    values: Vec<f64>,
}

pub struct ParquetExporter {
    writer: Option<SerializedFileWriter<File>>,
    timestamps: Vec<f64>,
    columns: Vec<ColumnBuffer>,
    rows_per_group: usize,
}

impl ParquetExporter {
    pub fn create(path: &Path, columns: &[String]) -> Result<Self> {
        let mut fields: Vec<Arc<SchemaType>> = Vec::with_capacity(columns.len() + 1);
        fields.push(Arc::new(
            SchemaType::primitive_type_builder("Timestamp", PhysicalType::DOUBLE)
                .with_repetition(Repetition::REQUIRED)
                .build()?,
        ));
        for name in columns {
            fields.push(Arc::new(
                SchemaType::primitive_type_builder(name, PhysicalType::DOUBLE)
                    .with_repetition(Repetition::OPTIONAL)
                    .build()
                    .with_context(|| format!("invalid Parquet column name {name:?}"))?,
            ));
        }
        let schema = Arc::new(
            SchemaType::group_type_builder("schema")
                .with_fields(fields)
                .build()?,
        );
        let props = Arc::new(
            WriterProperties::builder()
                .set_compression(Compression::SNAPPY)
                .build(),
        );
        let file =
            File::create(path).with_context(|| format!("failed to create {}", path.display()))?;
        let writer = SerializedFileWriter::new(file, schema, props)?;

        let rows_per_group = (TARGET_CELLS_PER_GROUP / columns.len().max(1))
            .clamp(MIN_ROWS_PER_GROUP, MAX_ROWS_PER_GROUP);
        Ok(Self {
            writer: Some(writer),
            timestamps: Vec::new(),
            columns: columns
                .iter()
                .map(|_| ColumnBuffer {
                    def_levels: Vec::new(),
                    values: Vec::new(),
                })
                .collect(),
            rows_per_group,
        })
    }

    fn flush_row_group(&mut self) -> Result<()> {
        if self.timestamps.is_empty() {
            return Ok(());
        }
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| anyhow!("Parquet writer already closed"))?;
        let mut row_group = writer.next_row_group()?;

        let mut column_writer = row_group
            .next_column()?
            .ok_or_else(|| anyhow!("missing Timestamp column writer"))?;
        column_writer
            .typed::<DoubleType>()
            .write_batch(&self.timestamps, None, None)?;
        column_writer.close()?;

        for buffer in &mut self.columns {
            let mut column_writer = row_group
                .next_column()?
                .ok_or_else(|| anyhow!("missing signal column writer"))?;
            column_writer.typed::<DoubleType>().write_batch(
                &buffer.values,
                Some(&buffer.def_levels),
                None,
            )?;
            column_writer.close()?;
            buffer.def_levels.clear();
            buffer.values.clear();
        }
        row_group.close()?;
        self.timestamps.clear();
        Ok(())
    }
}

impl Exporter for ParquetExporter {
    fn write_row(&mut self, timestamp: f64, cells: &[(usize, f64)]) -> Result<()> {
        self.timestamps.push(timestamp);
        for buffer in &mut self.columns {
            buffer.def_levels.push(0);
        }
        let row = self.timestamps.len() - 1;
        for &(col, value) in cells {
            let buffer = &mut self.columns[col];
            buffer.def_levels[row] = 1;
            buffer.values.push(value);
        }
        if self.timestamps.len() >= self.rows_per_group {
            self.flush_row_group()?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        self.flush_row_group()?;
        if let Some(writer) = self.writer.take() {
            writer.close().context("failed to finalize Parquet file")?;
        }
        Ok(())
    }
}
