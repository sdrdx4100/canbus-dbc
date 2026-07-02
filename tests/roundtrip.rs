//! End-to-end tests: synthesize a BLF file, decode with a DBC, check the
//! CSV and Parquet outputs.

use blf_decoder::convert::{ConvertOptions, OutputLayout, TimestampMode, convert, output_path};
use blf_decoder::export::OutputFormat;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use std::io::Write;
use std::path::PathBuf;

const OBJ_SIGNATURE: &[u8; 4] = b"LOBJ";

fn file_header() -> Vec<u8> {
    let mut h = vec![0u8; 144];
    h[0..4].copy_from_slice(b"LOGG");
    h[4..8].copy_from_slice(&144u32.to_le_bytes());
    // measurement start 2024-01-02 03:04:05.000 UTC at offset 56
    let st: [u16; 8] = [2024, 1, 2, 2, 3, 4, 5, 0];
    for (i, v) in st.iter().enumerate() {
        h[56 + i * 2..58 + i * 2].copy_from_slice(&v.to_le_bytes());
    }
    h
}

fn can_message(timestamp_ns: u64, id: u32, data: &[u8]) -> Vec<u8> {
    let mut obj = Vec::new();
    obj.extend_from_slice(OBJ_SIGNATURE);
    obj.extend_from_slice(&32u16.to_le_bytes());
    obj.extend_from_slice(&1u16.to_le_bytes());
    obj.extend_from_slice(&48u32.to_le_bytes());
    obj.extend_from_slice(&1u32.to_le_bytes()); // CAN_MESSAGE
    obj.extend_from_slice(&2u32.to_le_bytes()); // flags: nanosecond timestamps
    obj.extend_from_slice(&0u32.to_le_bytes()); // client index + object version
    obj.extend_from_slice(&timestamp_ns.to_le_bytes());
    obj.extend_from_slice(&1u16.to_le_bytes()); // channel
    obj.push(0); // message flags
    obj.push(data.len() as u8); // dlc
    obj.extend_from_slice(&id.to_le_bytes());
    let mut payload = [0u8; 8];
    payload[..data.len()].copy_from_slice(data);
    obj.extend_from_slice(&payload);
    obj
}

fn container(objects: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(objects).unwrap();
    let payload = enc.finish().unwrap();
    let mut obj = Vec::new();
    obj.extend_from_slice(OBJ_SIGNATURE);
    obj.extend_from_slice(&16u16.to_le_bytes());
    obj.extend_from_slice(&1u16.to_le_bytes());
    obj.extend_from_slice(&((32 + payload.len()) as u32).to_le_bytes());
    obj.extend_from_slice(&10u32.to_le_bytes()); // LOG_CONTAINER
    obj.extend_from_slice(&2u16.to_le_bytes()); // zlib
    obj.extend_from_slice(&[0u8; 6]);
    obj.extend_from_slice(&(objects.len() as u32).to_le_bytes());
    obj.extend_from_slice(&[0u8; 4]);
    obj.extend_from_slice(&payload);
    // BLF convention: pad with object_size % 4 bytes
    obj.extend(std::iter::repeat_n(0u8, (32 + payload.len()) % 4));
    obj
}

const DBC: &str = r#"
BO_ 256 Engine: 8 ECU
 SG_ EngineSpeed : 0|16@1+ (0.125,0) [0|8000] "rpm" Vector__XXX
 SG_ EngineTemp : 16|8@1- (1,-40) [-40|215] "degC" Vector__XXX
BO_ 512 Vehicle: 8 ECU
 SG_ VehicleSpeed : 7|16@0+ (0.01,0) [0|655.35] "km/h" Vector__XXX
"#;

struct Fixture {
    dir: PathBuf,
    blf: PathBuf,
    dbc: PathBuf,
}

fn setup(name: &str) -> Fixture {
    let dir = std::env::temp_dir().join(format!("blf_decoder_test_{name}_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let mut objects = Vec::new();
    // EngineSpeed raw 6688 (0x1A20) -> 836 rpm; EngineTemp raw 30 -> -10 degC
    objects.extend_from_slice(&can_message(
        100_000_000,
        256,
        &[0x20, 0x1A, 30, 0, 0, 0, 0, 0],
    ));
    // VehicleSpeed Motorola raw 0x1770 = 6000 -> 60.0 km/h
    objects.extend_from_slice(&can_message(
        200_000_000,
        512,
        &[0x17, 0x70, 0, 0, 0, 0, 0, 0],
    ));
    // Unknown ID: counted as read but not decoded
    objects.extend_from_slice(&can_message(300_000_000, 0x700, &[1, 2, 3, 4]));

    let mut blf_bytes = file_header();
    blf_bytes.extend_from_slice(&container(&objects));

    let blf = dir.join("sample.blf");
    let dbc = dir.join("sample.dbc");
    std::fs::write(&blf, &blf_bytes).unwrap();
    std::fs::write(&dbc, DBC).unwrap();
    Fixture { dir, blf, dbc }
}

const T0: f64 = 1704164645.0; // 2024-01-02 03:04:05 UTC

/// Options matching the v0.1 behaviour: one row per frame, epoch timestamps.
fn raw_epoch(format: OutputFormat) -> ConvertOptions {
    ConvertOptions {
        format,
        layout: OutputLayout::PerFrame,
        timestamp: TimestampMode::EpochSeconds,
        ..Default::default()
    }
}

#[test]
fn csv_roundtrip() {
    let fx = setup("csv");
    let summary = convert(
        &fx.blf,
        &fx.dbc,
        &fx.dir,
        raw_epoch(OutputFormat::Csv),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(summary.frames_read, 3);
    assert_eq!(summary.frames_decoded, 2);
    assert_eq!(summary.signal_columns, 3);
    assert_eq!(
        summary.output_path,
        output_path(&fx.blf, &fx.dir, OutputFormat::Csv)
    );

    let text = std::fs::read_to_string(&summary.output_path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0], "Timestamp,EngineSpeed,EngineTemp,VehicleSpeed");

    let row1: Vec<&str> = lines[1].split(',').collect();
    assert!((row1[0].parse::<f64>().unwrap() - (T0 + 0.1)).abs() < 1e-6);
    assert_eq!(row1[1].parse::<f64>().unwrap(), 836.0);
    assert_eq!(row1[2].parse::<f64>().unwrap(), -10.0);
    assert_eq!(row1[3], ""); // VehicleSpeed absent on Engine frames

    let row2: Vec<&str> = lines[2].split(',').collect();
    assert!((row2[0].parse::<f64>().unwrap() - (T0 + 0.2)).abs() < 1e-6);
    assert_eq!(row2[1], "");
    assert_eq!(row2[2], "");
    assert!((row2[3].parse::<f64>().unwrap() - 60.0).abs() < 1e-9);

    std::fs::remove_dir_all(&fx.dir).ok();
}

#[test]
fn parquet_roundtrip() {
    let fx = setup("parquet");
    let summary = convert(
        &fx.blf,
        &fx.dbc,
        &fx.dir,
        raw_epoch(OutputFormat::Parquet),
        &mut |_| {},
    )
    .unwrap();
    assert_eq!(summary.frames_decoded, 2);

    // Read the file back with the parquet crate's row API.
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let file = std::fs::File::open(&summary.output_path).unwrap();
    let reader = SerializedFileReader::new(file).unwrap();
    let schema = reader.metadata().file_metadata().schema();
    let names: Vec<String> = schema
        .get_fields()
        .iter()
        .map(|f| f.name().to_string())
        .collect();
    assert_eq!(
        names,
        vec!["Timestamp", "EngineSpeed", "EngineTemp", "VehicleSpeed"]
    );

    let rows: Vec<_> = reader
        .get_row_iter(None)
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(rows.len(), 2);

    use parquet::record::Field;
    let get = |row: usize, col: usize| -> Option<f64> {
        match rows[row].get_column_iter().nth(col).unwrap().1 {
            Field::Double(d) => Some(*d),
            Field::Null => None,
            other => panic!("unexpected field type {other:?}"),
        }
    };
    assert!((get(0, 0).unwrap() - (T0 + 0.1)).abs() < 1e-6);
    assert_eq!(get(0, 1), Some(836.0));
    assert_eq!(get(0, 2), Some(-10.0));
    assert_eq!(get(0, 3), None);

    assert_eq!(get(1, 1), None);
    assert_eq!(get(1, 2), None);
    assert!((get(1, 3).unwrap() - 60.0).abs() < 1e-9);

    std::fs::remove_dir_all(&fx.dir).ok();
}

#[test]
fn progress_reaches_completion() {
    let fx = setup("progress");
    let mut last = None;
    convert(
        &fx.blf,
        &fx.dbc,
        &fx.dir,
        raw_epoch(OutputFormat::Csv),
        &mut |u| {
            last = Some(u.progress);
        },
    )
    .unwrap();
    let last = last.unwrap();
    assert_eq!(last.bytes_read, last.total_bytes);
    assert!((last.fraction() - 1.0).abs() < f32::EPSILON);
    std::fs::remove_dir_all(&fx.dir).ok();
}

#[test]
fn strict_mode_errors_on_unknown_id() {
    let fx = setup("strict");
    let options = ConvertOptions {
        skip_unknown_ids: false,
        ..raw_epoch(OutputFormat::Csv)
    };
    let err = convert(&fx.blf, &fx.dbc, &fx.dir, options, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("0x700"), "{err}");
    std::fs::remove_dir_all(&fx.dir).ok();
}

#[test]
fn no_overwrite_refuses_existing_output() {
    let fx = setup("overwrite");
    convert(
        &fx.blf,
        &fx.dbc,
        &fx.dir,
        raw_epoch(OutputFormat::Csv),
        &mut |_| {},
    )
    .unwrap();
    let options = ConvertOptions {
        overwrite: false,
        ..raw_epoch(OutputFormat::Csv)
    };
    let err = convert(&fx.blf, &fx.dbc, &fx.dir, options, &mut |_| {}).unwrap_err();
    assert!(err.to_string().contains("already exists"), "{err}");
    std::fs::remove_dir_all(&fx.dir).ok();
}

#[test]
fn can_id_column_in_raw_layout() {
    let fx = setup("canid");
    let options = ConvertOptions {
        keep_can_id: true,
        ..raw_epoch(OutputFormat::Csv)
    };
    let summary = convert(&fx.blf, &fx.dbc, &fx.dir, options, &mut |_| {}).unwrap();
    let text = std::fs::read_to_string(&summary.output_path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[0],
        "Timestamp,CanId,EngineSpeed,EngineTemp,VehicleSpeed"
    );
    let row1: Vec<&str> = lines[1].split(',').collect();
    assert_eq!(row1[1].parse::<f64>().unwrap(), 256.0);
    assert_eq!(row1[2].parse::<f64>().unwrap(), 836.0);
    let row2: Vec<&str> = lines[2].split(',').collect();
    assert_eq!(row2[1].parse::<f64>().unwrap(), 512.0);
    std::fs::remove_dir_all(&fx.dir).ok();
}

#[test]
fn signal_filter_limits_output_columns() {
    let fx = setup("filter");
    let filter: std::collections::HashSet<String> = ["EngineSpeed", "VehicleSpeed"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let options = ConvertOptions {
        signal_filter: Some(filter),
        ..raw_epoch(OutputFormat::Csv)
    };
    let summary = convert(&fx.blf, &fx.dbc, &fx.dir, options, &mut |_| {}).unwrap();
    assert_eq!(summary.signal_columns, 2);
    let text = std::fs::read_to_string(&summary.output_path).unwrap();
    assert_eq!(
        text.lines().next().unwrap(),
        "Timestamp,EngineSpeed,VehicleSpeed"
    );
    std::fs::remove_dir_all(&fx.dir).ok();
}

#[test]
fn resampled_csv_forward_fills() {
    let fx = setup("resample");
    // Frames: Engine @0.1s (836 rpm, -10 degC), Vehicle @0.2s (60 km/h).
    // Default options: 100 ms grid, relative timestamps, forward fill.
    let summary = convert(
        &fx.blf,
        &fx.dbc,
        &fx.dir,
        ConvertOptions::default(),
        &mut |_| {},
    )
    .unwrap();

    let text = std::fs::read_to_string(&summary.output_path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "Timestamp,EngineSpeed,EngineTemp,VehicleSpeed");
    // Grid anchored at first decoded frame (t = 0.1 s):
    //   row 1: t=0.1 grid closed when 0.2s frame arrives -> engine values only
    //   row 2: t=0.2 trailing flush -> engine values held + vehicle speed
    assert_eq!(lines.len(), 3);
    assert_eq!(summary.rows_written, 2);

    let row1: Vec<&str> = lines[1].split(',').collect();
    assert!((row1[0].parse::<f64>().unwrap() - 0.1).abs() < 1e-9);
    assert_eq!(row1[1].parse::<f64>().unwrap(), 836.0);
    assert_eq!(row1[2].parse::<f64>().unwrap(), -10.0);
    assert_eq!(row1[3], "");

    let row2: Vec<&str> = lines[2].split(',').collect();
    assert!((row2[0].parse::<f64>().unwrap() - 0.2).abs() < 1e-9);
    // Forward-filled from the previous frame:
    assert_eq!(row2[1].parse::<f64>().unwrap(), 836.0);
    assert_eq!(row2[2].parse::<f64>().unwrap(), -10.0);
    assert!((row2[3].parse::<f64>().unwrap() - 60.0).abs() < 1e-9);

    std::fs::remove_dir_all(&fx.dir).ok();
}
