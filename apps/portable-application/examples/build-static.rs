use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use ato_portable_application::build_static_bundle;
use sha2::{Digest, Sha256};

fn required_argument(
    arguments: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<OsString> {
    arguments
        .next()
        .with_context(|| format!("missing {name} argument"))
}

fn main() -> Result<()> {
    let mut arguments = env::args_os().skip(1);
    let source_root = PathBuf::from(required_argument(&mut arguments, "source root")?);
    let output = PathBuf::from(required_argument(&mut arguments, "output")?);
    let title = required_argument(&mut arguments, "title")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("title is not UTF-8"))?;
    if arguments.next().is_some() {
        bail!("usage: build-static <source-root> <output.capsule> <title>");
    }

    let (bytes, bundle) = build_static_bundle(&source_root, &title)?;
    fs::write(&output, &bytes)
        .with_context(|| format!("write portable bundle at {}", output.display()))?;
    let transport_sha256 = format!("sha256:{:x}", Sha256::digest(&bytes));
    println!("bundle_sha256={transport_sha256}");
    println!("contract_ref={}", bundle.index.root_contract_ref);
    for derivation in bundle.index.derivations {
        println!("derivation_ref={derivation}");
    }
    Ok(())
}
