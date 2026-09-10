mod collector;
mod dumper;
mod scanner;
mod util;

use anyhow::{Context, Result};
use memmap2::Mmap;
use std::fs::{self, File};
use std::path::Path;

const USAGE: &str = "\
Usage: steam-protobuf-dumper [--debug] <binary>... <output_dir>

Extracts embedded protobuf definitions from Steam client binaries.

Arguments:
  <binary>...    One or more Steam binary files to scan (e.g. steamclient.so, steamui.so)
  <output_dir>   Directory to write extracted .proto files

Options:
  --debug        Enable debug logging to stderr";

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Parse --debug flag and collect positional args
    let debug = args.iter().any(|a| a == "--debug");
    let positional: Vec<&str> = args
        .iter()
        .filter(|a| *a != "--debug")
        .map(|s| s.as_str())
        .collect();

    // Initialize logging
    if debug {
        env_logger::Builder::new()
            .filter_level(log::LevelFilter::Debug)
            .format_timestamp(None)
            .init();
    }

    // Need at least 1 binary + 1 output dir
    if positional.len() < 2 {
        anyhow::bail!("{}", USAGE);
    }

    let output_dir = Path::new(positional.last().expect("checked above"));
    let binary_paths = &positional[..positional.len() - 1];

    // Scan all binaries for protobuf candidates
    let mut collector = collector::ProtobufCollector::new();

    for path_str in binary_paths {
        let path = Path::new(path_str);
        if !path.is_file() {
            anyhow::bail!("Binary not found: {}", path.display());
        }

        log::debug!("Memory-mapping '{}'...", path.display());

        // Memory-map the file instead of reading it into heap memory.
        // This avoids allocating 150MB+ per binary — the OS kernel handles
        // paging and the file never fully resides in process memory.
        let file =
            File::open(path).with_context(|| format!("Failed to open '{}'", path.display()))?;

        // SAFETY: We don't modify the file while it's mapped, and the mmap
        // lives only for the duration of this loop iteration.
        #[allow(clippy::needless_borrows_for_generic_args)]
        let mmap = unsafe { Mmap::map(&file) }
            .with_context(|| format!("Failed to mmap '{}'", path.display()))?;

        log::debug!(
            "Scanning '{}' ({:.1} MB)...",
            path.display(),
            mmap.len() as f64 / 1_048_576.0
        );

        scan_binary(&mmap, &mut collector);
    }

    log::debug!(
        "Collected {} protobuf descriptors",
        collector.candidates.len()
    );

    if collector.candidates.is_empty() {
        anyhow::bail!("No protobuf descriptors found in any input binaries");
    }

    // Analyze dependencies and dump .proto files
    let mut dumper_instance = dumper::ProtobufDumper::new(collector.candidates);

    if dumper_instance.analyze() {
        fs::create_dir_all(output_dir).context("Failed to create output directory")?;

        dumper_instance.dump_files(|name, content| {
            let output_file = output_dir.join(name);

            if let Some(parent) = output_file.parent() {
                if let Err(e) = fs::create_dir_all(parent) {
                    log::debug!("Failed to create directory {}: {}", parent.display(), e);
                    return;
                }
            }

            log::debug!("Writing '{}'", output_file.display());

            if let Err(e) = fs::write(&output_file, content) {
                log::debug!("Failed to write {}: {}", output_file.display(), e);
            }
        });
    } else {
        anyhow::bail!("Dump failed. Not all dependencies and types were found.");
    }

    Ok(())
}

/// Scans a single binary for protobuf candidates, adding them to the collector.
fn scan_binary(data: &[u8], collector: &mut collector::ProtobufCollector) {
    scanner::scan_file(data, |name, buffer| {
        if collector.has_candidate(name) {
            return Some(1);
        }

        let (result, bytes_consumed) = collector.try_parse_candidate(buffer);

        match result {
            collector::CandidateResult::Ok => {
                log::debug!("{name}... OK!");
            }
            collector::CandidateResult::Invalid(err) => {
                log::debug!("{name}... invalid: {err}");
            }
        }

        if bytes_consumed > 0 {
            Some(bytes_consumed)
        } else {
            None
        }
    });
}
