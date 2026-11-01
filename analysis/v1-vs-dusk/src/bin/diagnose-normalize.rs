use std::{
    env,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use v1_vs_dusk_benchmark::{
    decode::{DecodedTransactionV1, TransactionExecutionProvenanceV1},
    normalize::NormalizationStateV1,
};

fn find_record<T: serde::de::DeserializeOwned>(path: &Path, signature: &str) -> Result<T> {
    let reader =
        BufReader::new(File::open(path).with_context(|| format!("open {}", path.display()))?);
    for line in reader.lines() {
        let line = line?;
        if line.contains(signature) {
            let record: T = serde_json::from_str(&line)
                .with_context(|| format!("decode matching row in {}", path.display()))?;
            return Ok(record);
        }
    }
    bail!("signature {signature} is absent from {}", path.display())
}

fn main() -> Result<()> {
    let mut args = env::args().skip(1);
    let data_dir = PathBuf::from(args.next().context("missing data directory")?);
    let signature = args.next().context("missing transaction signature")?;
    let program_id = args.next().context("missing program id")?;
    if args.next().is_some() {
        bail!("usage: diagnose-normalize <data-dir> <signature> <program-id>");
    }

    let transaction: DecodedTransactionV1 =
        find_record(&data_dir.join("decoded-transactions.jsonl"), &signature)?;
    let provenance: TransactionExecutionProvenanceV1 =
        find_record(&data_dir.join("execution-provenance.jsonl"), &signature)?;
    let mut state = NormalizationStateV1::new("mainnet-beta", &program_id);
    let batch = state.push_transaction_with_provenance(transaction, &provenance)?;
    println!("{}", serde_json::to_string_pretty(&batch)?);
    Ok(())
}
