//! Row shaping: turn sparse per-frame signal updates into a dense,
//! fixed-interval time series (sample & hold / forward fill).
//!
//! CAN logs update only a few signals per frame, so the raw per-frame table
//! is mostly empty cells. For analysis a regular grid where every row carries
//! the latest value of every signal is usually what people want.

use anyhow::Result;

/// Row consumer: receives `(timestamp, sparse cells)` for each output row.
pub type RowSink<'a> = dyn FnMut(f64, &[(usize, f64)]) -> Result<()> + 'a;

/// Forward-fill resampler. Feed frames in time order; rows are emitted on a
/// fixed grid anchored at the first frame's timestamp. Each emitted row
/// contains the last known value of every signal seen so far (signals not
/// yet observed stay empty).
pub struct Resampler {
    /// Grid spacing in integer nanoseconds: exact arithmetic, so timestamps
    /// come out clean (0.3, not 0.30000000000000004).
    interval_ns: i64,
    /// Grid anchor in nanoseconds (first interval multiple at or after the
    /// first frame), so timestamps are round multiples of the interval.
    t0_ns: Option<i64>,
    /// Index of the next grid row to emit.
    grid_index: i64,
    /// Last known value per column.
    current: Vec<Option<f64>>,
    /// True when frames were merged after the last emitted row.
    dirty: bool,
    /// Scratch buffer for emitting rows.
    row: Vec<(usize, f64)>,
}

impl Resampler {
    pub fn new(columns: usize, interval_seconds: f64) -> Self {
        let interval_ns = (interval_seconds * 1e9).round() as i64;
        assert!(interval_ns > 0, "resample interval must be at least 1 ns");
        Self {
            interval_ns,
            t0_ns: None,
            grid_index: 0,
            current: vec![None; columns],
            dirty: false,
            row: Vec::new(),
        }
    }

    fn grid_time(&self) -> f64 {
        (self.t0_ns.unwrap_or(0) + self.grid_index * self.interval_ns) as f64 / 1e9
    }

    fn emit_row(&mut self, timestamp: f64, emit: &mut RowSink<'_>) -> Result<()> {
        self.row.clear();
        for (col, value) in self.current.iter().enumerate() {
            if let Some(v) = value {
                self.row.push((col, *v));
            }
        }
        emit(timestamp, &self.row)
    }

    /// Merge one frame. Grid rows strictly before `timestamp` are emitted
    /// first (so a frame exactly on a grid point is included in that row).
    pub fn push(
        &mut self,
        timestamp: f64,
        cells: &[(usize, f64)],
        emit: &mut RowSink<'_>,
    ) -> Result<()> {
        if self.t0_ns.is_none() {
            // Snap the grid to multiples of the interval so timestamps come
            // out as round numbers (0.1, 0.2, ... rather than 0.1037, ...).
            let ts_ns = (timestamp * 1e9).round() as i64;
            self.t0_ns = Some(
                ts_ns.div_euclid(self.interval_ns) * self.interval_ns
                    + if ts_ns.rem_euclid(self.interval_ns) != 0 {
                        self.interval_ns
                    } else {
                        0
                    },
            );
        }
        while self.grid_time() < timestamp {
            self.emit_row(self.grid_time(), emit)?;
            self.grid_index += 1;
            self.dirty = false;
        }
        for &(col, value) in cells {
            self.current[col] = Some(value);
        }
        self.dirty = true;
        Ok(())
    }

    /// Emit the trailing partial row so the final signal values are not lost.
    pub fn finish(&mut self, emit: &mut RowSink<'_>) -> Result<()> {
        if self.t0_ns.is_some() && self.dirty {
            self.emit_row(self.grid_time(), emit)?;
            self.dirty = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(
        frames: &[(f64, Vec<(usize, f64)>)],
        cols: usize,
        dt: f64,
    ) -> Vec<(f64, Vec<(usize, f64)>)> {
        let mut rs = Resampler::new(cols, dt);
        let mut out = Vec::new();
        let mut emit = |ts: f64, cells: &[(usize, f64)]| {
            out.push((ts, cells.to_vec()));
            Ok(())
        };
        for (ts, cells) in frames {
            rs.push(*ts, cells, &mut emit).unwrap();
        }
        rs.finish(&mut emit).unwrap();
        out
    }

    #[test]
    fn forward_fills_on_grid() {
        let rows = collect(
            &[
                (0.00, vec![(0, 1.0)]),
                (0.04, vec![(1, 10.0)]),
                (0.13, vec![(0, 2.0)]),
                (0.31, vec![(1, 20.0)]),
            ],
            2,
            0.1,
        );
        assert_eq!(
            rows,
            vec![
                (0.0, vec![(0, 1.0)]),
                (0.1, vec![(0, 1.0), (1, 10.0)]),
                (0.2, vec![(0, 2.0), (1, 10.0)]),
                (0.3, vec![(0, 2.0), (1, 10.0)]),
                (0.4, vec![(0, 2.0), (1, 20.0)]), // trailing flush
            ]
        );
    }

    #[test]
    fn frame_on_grid_point_included() {
        let rows = collect(&[(0.0, vec![(0, 1.0)]), (0.1, vec![(0, 2.0)])], 1, 0.1);
        // Grid row 0.0 contains the frame at exactly 0.0; row 0.1 flushes the
        // final value.
        assert_eq!(rows, vec![(0.0, vec![(0, 1.0)]), (0.1, vec![(0, 2.0)])]);
    }

    #[test]
    fn unseen_columns_stay_empty() {
        let rows = collect(&[(0.0, vec![(1, 5.0)]), (0.25, vec![(1, 6.0)])], 3, 0.1);
        for (_, cells) in &rows {
            assert!(cells.iter().all(|(col, _)| *col == 1));
        }
        assert_eq!(rows.len(), 4); // 0.0, 0.1, 0.2 grids + trailing flush
    }

    #[test]
    fn grid_snaps_to_round_interval_multiples() {
        let rows = collect(&[(0.1037, vec![(0, 1.0)]), (0.42, vec![(0, 2.0)])], 1, 0.1);
        // Grid anchored at 0.2 (first multiple of 0.1 at/after 0.1037).
        assert_eq!(
            rows,
            vec![
                (0.2, vec![(0, 1.0)]),
                (0.3, vec![(0, 1.0)]),
                (0.4, vec![(0, 1.0)]),
                (0.5, vec![(0, 2.0)]), // trailing flush
            ]
        );
    }

    #[test]
    fn empty_input_emits_nothing() {
        let rows = collect(&[], 2, 0.1);
        assert!(rows.is_empty());
    }
}
