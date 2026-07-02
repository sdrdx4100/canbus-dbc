// Hide the console window for the GUI build on Windows release binaries.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod gui;

use anyhow::{Context, Result, bail};
use blf_decoder::convert::{Progress, convert};
use blf_decoder::export::OutputFormat;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return gui::run().map_err(|e| anyhow::anyhow!("failed to start GUI: {e}"));
    }
    run_cli(&args)
}

const USAGE: &str =
    "Usage: blf_decoder --blf <file.blf> --dbc <file.dbc> --out <dir> [--format csv|parquet]
Run without arguments to start the GUI.";

/// Minimal CLI (development aid / future extension per the requirements).
fn run_cli(args: &[String]) -> Result<()> {
    let mut blf: Option<PathBuf> = None;
    let mut dbc: Option<PathBuf> = None;
    let mut out: Option<PathBuf> = None;
    let mut format = OutputFormat::Csv;

    let mut it = args.iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--blf" | "-b" => blf = it.next().map(PathBuf::from),
            "--dbc" | "-d" => dbc = it.next().map(PathBuf::from),
            "--out" | "-o" => out = it.next().map(PathBuf::from),
            "--format" | "-f" => {
                format = match it.next().map(String::as_str) {
                    Some("csv") => OutputFormat::Csv,
                    Some("parquet") => OutputFormat::Parquet,
                    other => bail!("unknown format {other:?}\n{USAGE}"),
                }
            }
            "--help" | "-h" => {
                println!("{USAGE}");
                return Ok(());
            }
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    let blf = blf.with_context(|| format!("missing --blf\n{USAGE}"))?;
    let dbc = dbc.with_context(|| format!("missing --dbc\n{USAGE}"))?;
    let out = out.with_context(|| format!("missing --out\n{USAGE}"))?;

    let mut last_percent = u32::MAX;
    let mut on_progress = |p: Progress| {
        let percent = (p.fraction() * 100.0) as u32;
        if percent != last_percent {
            eprint!("\rConverting... {percent}% ({} frames)", p.frames_read);
            last_percent = percent;
        }
    };
    let summary = convert(&blf, &dbc, &out, format, &mut on_progress)?;
    eprintln!();
    println!(
        "Done: {} ({} of {} frames decoded, {} signal columns)",
        summary.output_path.display(),
        summary.frames_decoded,
        summary.frames_read,
        summary.signal_columns
    );
    Ok(())
}
