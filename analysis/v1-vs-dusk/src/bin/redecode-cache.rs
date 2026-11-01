use std::{env, path::PathBuf, process::ExitCode};

use anyhow::{bail, Context, Result};
use v1_vs_dusk_benchmark::pipeline::redecode_cached_history;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let data_dir = PathBuf::from(args.next().context("missing DATA_DIR argument")?);
    let program_id = args.next().context("missing PROGRAM_ID argument")?;
    let decoder_registry = PathBuf::from(args.next().context("missing DECODER_REGISTRY argument")?);
    if args.next().is_some() {
        bail!("usage: redecode-cache DATA_DIR PROGRAM_ID DECODER_REGISTRY");
    }
    let summary = redecode_cached_history(&data_dir, &program_id, &decoder_registry)?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}
