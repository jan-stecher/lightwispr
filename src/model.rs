//! Model registry: every downloadable model, pinned to a Hugging Face revision and
//! verified by SHA-256, stored in ~/.local/share/lightwispr/models/<id>/.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub enum Kind {
    Parakeet,
    Whisper,
    Llm,
}

pub struct Spec {
    pub id: &'static str,
    pub kind: Kind,
    /// Shown in the bar and in `download-model`.
    pub label: &'static str,
    repo: &'static str,
    revision: &'static str,
    /// (file, sha256, bytes)
    files: &'static [(&'static str, &'static str, u64)],
}

pub const PARAKEET: Spec = Spec {
    id: "parakeet-tdt-0.6b-v3-int8",
    kind: Kind::Parakeet,
    label: "Parakeet TDT 0.6B v3 (NVIDIA, CC-BY-4.0)",
    repo: "istupakov/parakeet-tdt-0.6b-v3-onnx",
    revision: "8f23f0c03c8761650bdb5b40aaf3e40d2c15f1ce",
    files: &[
        ("encoder-model.int8.onnx", "6139d2fa7e1b086097b277c7149725edbab89cc7c7ae64b23c741be4055aff09", 652_183_999),
        ("decoder_joint-model.int8.onnx", "eea7483ee3d1a30375daedc8ed83e3960c91b098812127a0d99d1c8977667a70", 18_202_004),
        ("nemo128.onnx", "a9fde1486ebfcc08f328d75ad4610c67835fea58c73ba57e3209a6f6cf019e9f", 139_764),
        ("vocab.txt", "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d", 93_939),
    ],
};

pub const WHISPER: Spec = Spec {
    id: "whisper-large-v3-turbo",
    kind: Kind::Whisper,
    label: "Whisper large-v3-turbo q5_0 (OpenAI, MIT)",
    repo: "ggerganov/whisper.cpp",
    revision: "5359861c739e955e79d9a303bcbc70fb988958b1",
    files: &[("ggml-large-v3-turbo-q5_0.bin", "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2", 574_041_195)],
};

/// Enhancement models (GGUF Q4_K_M). Defaults: see `default_llm`.
pub const LLMS: &[Spec] = &[
    Spec {
        id: "qwen3.5-2b",
        kind: Kind::Llm,
        label: "Qwen3.5 2B (Apache-2.0)",
        repo: "unsloth/Qwen3.5-2B-GGUF",
        revision: "f6d5376be1edb4d416d56da11e5397a961aca8ae",
        files: &[("Qwen3.5-2B-Q4_K_M.gguf", "aaf42c8b7c3cab2bf3d69c355048d4a0ee9973d48f16c731c0520ee914699223", 1_280_835_840)],
    },
    Spec {
        id: "qwen3.5-4b",
        kind: Kind::Llm,
        label: "Qwen3.5 4B (Apache-2.0)",
        repo: "unsloth/Qwen3.5-4B-GGUF",
        revision: "e87f176479d0855a907a41277aca2f8ee7a09523",
        files: &[("Qwen3.5-4B-Q4_K_M.gguf", "00fe7986ff5f6b463e62455821146049db6f9313603938a70800d1fb69ef11a4", 2_740_937_888)],
    },
    Spec {
        id: "qwen3.5-9b",
        kind: Kind::Llm,
        label: "Qwen3.5 9B (Apache-2.0)",
        repo: "unsloth/Qwen3.5-9B-GGUF",
        revision: "3885219b6810b007914f3a7950a8d1b469d598a5",
        files: &[("Qwen3.5-9B-Q4_K_M.gguf", "03b74727a860a56338e042c4420bb3f04b2fec5734175f4cb9fa853daf52b7e8", 5_680_522_464)],
    },
    Spec {
        id: "gemma4-e2b",
        kind: Kind::Llm,
        label: "Gemma 4 E2B (Apache-2.0)",
        repo: "unsloth/gemma-4-E2B-it-GGUF",
        revision: "0314792d7f1f7e229411f620751375812bb9faf2",
        files: &[("gemma-4-E2B-it-Q4_K_M.gguf", "740185b21d22ceb83a11c3aa62ad5842ef32c70f6096d756bbee85a1e4ec34b8", 3_106_738_272)],
    },
    Spec {
        id: "gemma4-e4b",
        kind: Kind::Llm,
        label: "Gemma 4 E4B (Apache-2.0)",
        repo: "unsloth/gemma-4-E4B-it-GGUF",
        revision: "bfc15c382204943c3a8fff0c750b94ae2364d7a3",
        files: &[("gemma-4-E4B-it-Q4_K_M.gguf", "85a896a047553e842f25297ee5b031d64ff30147d9c4af17b1e4b394cd1fab87", 4_977_171_584)],
    },
    Spec {
        id: "lfm2.5-1.2b",
        kind: Kind::Llm,
        label: "LFM2.5 1.2B (Liquid AI, LFM Open License)",
        repo: "LiquidAI/LFM2.5-1.2B-Instruct-GGUF",
        revision: "8ed288026e23958ad9dfa92d53ed773a8eee7125",
        files: &[("LFM2.5-1.2B-Instruct-Q4_K_M.gguf", "b1b3de114215d9507409a662a501a631095a479a419584e8a2ded6304b19b4f5", 730_895_168)],
    },
    Spec {
        id: "ministral3-8b",
        kind: Kind::Llm,
        label: "Ministral 3 8B (Mistral, Apache-2.0)",
        repo: "unsloth/Ministral-3-8B-Instruct-2512-GGUF",
        revision: "3731507ec3e867db16d620f73e14d689125758f4",
        files: &[("Ministral-3-8B-Instruct-2512-Q4_K_M.gguf", "5dbc3647eb563b9f8d3c70ec3d906cce84b86bb35c5e0b8a36e7df3937ab7174", 5_198_386_720)],
    },
];

/// The enhancement model used when the config doesn't name one, picked by the bake-off
/// (docs/SPEC-enhancement.md): Gemma 4 E4B on the GPU (23/25, ~0.4 s), E2B on the CPU
/// (18/25, ~1.5 s once its prompt is cached).
pub fn default_llm(gpu: bool) -> &'static Spec {
    find(if gpu { "gemma4-e4b" } else { "gemma4-e2b" }).expect("default model in registry")
}

pub fn find(id: &str) -> Option<&'static Spec> {
    [&PARAKEET, &WHISPER].into_iter().chain(LLMS).find(|s| s.id == id)
}

pub fn data_dir() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share"))
        .join("lightwispr")
}

impl Spec {
    pub fn dir(&self) -> PathBuf {
        data_dir().join("models").join(self.id)
    }

    /// The main file (the GGUF/GGML for single-file models).
    pub fn path(&self) -> PathBuf {
        self.dir().join(self.files[0].0)
    }

    pub fn is_installed(&self) -> bool {
        let dir = self.dir();
        self.files.iter().all(|(f, _, _)| dir.join(f).is_file())
    }

    pub fn size_mb(&self) -> u64 {
        self.files.iter().map(|(_, _, b)| b).sum::<u64>() / 1_000_000
    }

    /// Downloads missing or broken files. `progress` gets (done, total) bytes.
    pub fn download(&self, mut progress: impl FnMut(u64, u64)) -> Result<()> {
        let dir = self.dir();
        std::fs::create_dir_all(&dir)?;
        let total: u64 = self.files.iter().map(|(_, _, b)| b).sum();
        let mut before = 0u64;
        for (name, sha, bytes) in self.files {
            let path = dir.join(name);
            if path.is_file() && sha256_file(&path)? == *sha {
                before += bytes;
                progress(before, total);
                continue;
            }
            let url = format!("https://huggingface.co/{}/resolve/{}/{name}", self.repo, self.revision);
            let mut resp = ureq::get(&url).call().with_context(|| format!("GET {url}"))?;
            let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
            let part = path.with_extension("part");
            let mut out = File::create(&part)?;
            let mut hasher = Sha256::new();
            let (mut buf, mut done) = (vec![0u8; 1 << 16], 0u64);
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
                hasher.update(&buf[..n]);
                done += n as u64;
                progress(before + done, total);
            }
            out.sync_all()?;
            let got = hex(&hasher.finalize());
            if got != *sha {
                let _ = std::fs::remove_file(&part);
                bail!("{name}: checksum mismatch (got {got})");
            }
            std::fs::rename(&part, &path)?;
            before += bytes;
        }
        Ok(())
    }
}

/// CLI: `lightwispr download-model [parakeet|whisper|enhance|<id>]`.
pub fn download_cli(which: Option<&str>, enhance_model: &str, gpu: bool) -> Result<()> {
    let spec = match which.unwrap_or("parakeet") {
        "parakeet" => &PARAKEET,
        "whisper" => &WHISPER,
        "enhance" => find(enhance_model).unwrap_or_else(|| default_llm(gpu)),
        id => find(id).with_context(|| {
            let ids: Vec<_> = LLMS.iter().map(|s| s.id).collect();
            format!("unknown model {id:?}; known: parakeet, whisper, enhance, {}", ids.join(", "))
        })?,
    };
    eprintln!("{} ({} MB) → {}", spec.label, spec.size_mb(), spec.dir().display());
    let mut last = 0;
    spec.download(|done, total| {
        let pct = done * 100 / total.max(1);
        if pct >= last + 10 {
            last = pct;
            eprint!("{pct}% ");
        }
    })?;
    eprintln!("\nverified, ready");
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
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
