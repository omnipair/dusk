use std::{env, path::PathBuf, process::ExitCode};

use anyhow::{bail, Context, Result};
use v1_vs_dusk_benchmark::{config::BenchmarkConfig, stages};

const STAGES: &[&str] = &[
    "extract",
    "audit",
    "reconcile-v1",
    "calibrate",
    "evaluate",
    "verify",
    "render",
];

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<()> {
    let mut args = env::args().skip(1);
    let Some(stage) = args.next() else {
        print_usage();
        bail!("a stage is required");
    };
    if stage == "--help" || stage == "-h" {
        print_usage();
        return Ok(());
    }
    if !STAGES.contains(&stage.as_str()) {
        print_usage();
        bail!("unknown stage {stage:?}");
    }

    let mut config_path = PathBuf::from("benchmark.toml");
    let mut force = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--config" => {
                config_path =
                    PathBuf::from(args.next().context("--config requires a path argument")?);
            }
            "--force" => force = true,
            "--help" | "-h" => {
                print_usage();
                return Ok(());
            }
            other => bail!("unknown option {other:?}"),
        }
    }

    let config = BenchmarkConfig::from_path(&config_path)?;
    stages::run(&stage, &config_path, config, force).await
}

fn print_usage() {
    eprintln!(
        "Usage: v1-vs-dusk <stage> [--config PATH] [--force]\n\nStages:\n  {}",
        STAGES.join("\n  ")
    );
}
