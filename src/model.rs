//! Downloads the Parakeet model from Hugging Face, pinned to a revision and verified
//! by SHA-256, into ~/.local/share/lightwispr/models/.

use std::fs::File;
use std::io::{Read, Write};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::stt::{MODEL_NAME, data_dir};

const REPO: &str = "istupakov/parakeet-tdt-0.6b-v3-onnx";
const REVISION: &str = "8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce";

/// (file, sha256)
const FILES: &[(&str, &str)] = &[
    ("encoder-model.int8.onnx", "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09"),
    ("decoder_joint-model.int8.onnx", "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70"),
    ("nemo128.onnx", "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f"),
    ("vocab.txt", "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
];

pub fn is_installed() -> bool {
    let dir = data_dir().join("models").join(MODEL_NAME);
    FILES.iter().all(|(f, _)| dir.join(f).is_file())
}

pub fn download() -> Result<()> {
    let dir = data_dir().join("models").join(MODEL_NAME);
    std::fs::create_dir_all(&dir)?;
    for (name, sha) in FILES {
        let path = dir.join(name);
        if path.is_file() && sha256_file(&path)? == *sha {
            eprintln!("{name}: ok");
            continue;
        }
        let url = format!("https://huggingface.co/{REPO}/resolve/{REVISION}/{name}");
        eprint!("{name}: downloading… ");
        let mut resp = ureq::get(&url).call().with_context(|| format!("GET {url}"))?;
        let total: Option<u64> = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse().ok());
        let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
        let part = path.with_extension("part");
        let mut out = File::create(&part)?;
        let mut hasher = Sha256::new();
        let (mut buf, mut done, mut last_pct) = (vec![0u8; 1 << 16], 0u64, 0u64);
        loop {
            let n = reader.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            hasher.update(&buf[..n]);
            done += n as u64;
            if let Some(t) = total {
                let pct = done * 100 / t.max(1);
                if pct >= last_pct + 10 {
                    last_pct = pct;
                    eprint!("{pct}% ");
                }
            }
        }
        out.sync_all()?;
        let got = hex(&hasher.finalize());
        if got != *sha {
            let _ = std::fs::remove_file(&part);
            bail!("{name}: checksum mismatch (got {got})");
        }
        std::fs::rename(&part, &path)?;
        eprintln!("verified");
    }
    eprintln!("model ready in {}", dir.display());
    Ok(())
}

fn sha256_file(path: &std::path::Path) -> Result<String> {
    let mut f = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
