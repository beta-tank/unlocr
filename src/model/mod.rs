use crate::Res;
use std::fs;
use std::path::{Path, PathBuf};

mod download;
#[cfg(test)]
mod tests;

pub use download::{ensure, ensure_with_overrides, ensure_with_progress};

// Local llama.cpp path: AtomicChat's quantized GGUF build of Unlimited-OCR-2.
pub(crate) const REPO: &str = "AtomicChat/Unlimited-OCR-2-GGUF";
pub(crate) const MMPROJ: &str = "mmproj-Unlimited-OCR-2-F16.gguf";

/// Pinned model revision (a commit sha, NOT the mutable `main` ref).
pub(crate) const REV: &str = "b2cda9bb25ad9dd5585194a528d66a8f2d068c54";

/// sha256 of each shipped GGUF at REV. Verified live against HF's file-metadata
/// API (`/api/models/{REPO}/revision/{REV}?blobs=true`, each entry's `lfs.sha256`)
/// at the exact pinned REV commit.
pub(crate) const DIGESTS: &[(&str, &str)] = &[
    (
        "Unlimited-OCR-2-BF16.gguf",
        "a286e69e43573db6fbc06557210f517dab745ac0c18330bab019c2ffea28a21b",
    ),
    (
        "Unlimited-OCR-2-Q8_0.gguf",
        "a643ec02822ce653c7aed99aa4279cc5b8cf3711be46221c460c16931b2cbfde",
    ),
    (
        "Unlimited-OCR-2-AD-Q6_K.gguf",
        "4adf52bcbc836aa42dc22dcf6076811ff130282600bcc25d68ad81b3a59259da",
    ),
    (
        "Unlimited-OCR-2-AD-Q5_K.gguf",
        "84c4e29c71dfc66b34184bd1a550bc85e9d7e78a2ec265103411a446fe7d5ee3",
    ),
    (
        "Unlimited-OCR-2-AD-Q4_K.gguf",
        "38765f1267120535de6f46bf2d0e6c27ebdbfc45fd83996643265f2b7a8f3538",
    ),
    (
        "mmproj-Unlimited-OCR-2-F16.gguf",
        "6e00b887bc6171c5f16dba4b96d894e1b8f0fcf3fcaa978d89e30dd5ec3e017b",
    ),
];

/// One entry in the published quant lineup for `REPO`. `size_bytes` is the exact
/// HF file size (GUI display / download-size estimate); `tier` is `Some` only
/// for the 3 quants the CLI's `Quality` enum (`cli_args.rs`) aliases to
/// best/good/less, `None` for the rest (no friendly alias, selected by exact tag).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct QuantInfo {
    pub name: &'static str,
    pub size_bytes: u64,
    pub tier: Option<&'static str>,
}

/// All five published language-model quants for `REPO` (sizes/names verified
/// against the same HF API call as `DIGESTS`).
pub const KNOWN_QUANTS: &[QuantInfo] = &[
    QuantInfo {
        name: "BF16",
        size_bytes: 5_884_466_304,
        tier: Some("best"),
    },
    QuantInfo {
        name: "Q8_0",
        size_bytes: 3_134_027_904,
        tier: Some("good"),
    },
    QuantInfo {
        name: "AD-Q6_K",
        size_bytes: 2_742_941_824,
        tier: None,
    },
    QuantInfo {
        name: "AD-Q5_K",
        size_bytes: 2_276_161_664,
        tier: None,
    },
    QuantInfo {
        name: "AD-Q4_K",
        size_bytes: 2_023_848_064,
        tier: Some("less"),
    },
];

/// The full published quant lineup (name/size/tier), for a GUI quant picker.
/// Advisory/display metadata only, not an allowlist: `validate_quant` remains
/// the sole enforcement point, so a custom/future quant tag still works even
/// though it has no entry here.
pub fn known_quants() -> &'static [QuantInfo] {
    KNOWN_QUANTS
}

/// Paths to the resolved model and multimodal projector files.
#[derive(Debug)]
pub struct ModelFiles {
    /// Resolved path to the GGUF model file.
    pub model: PathBuf,
    /// Resolved path to the GGUF multimodal projector file.
    pub mmproj: PathBuf,
}

/// Outcome of comparing a downloaded file's sha256 to the pinned DIGESTS table.
#[derive(Debug)]
pub(crate) enum DigestCheck {
    /// Hash matches the pinned digest for this filename.
    Match,
    /// Hash does NOT match the pinned digest; reject the download.
    Mismatch { expected: String },
    /// No digest pinned for this filename (a custom quant); caller warns and proceeds.
    Unpinned,
}

/// Pure comparison of an `actual` sha256 hex against the pinned digest for `name`.
pub(crate) fn check_digest(name: &str, actual_hex: &str) -> DigestCheck {
    match DIGESTS.iter().find(|(n, _)| *n == name) {
        Some((_, expected)) if expected.eq_ignore_ascii_case(actual_hex) => DigestCheck::Match,
        Some((_, expected)) => DigestCheck::Mismatch {
            expected: (*expected).to_string(),
        },
        None => DigestCheck::Unpinned,
    }
}

/// sha256 of a byte slice as lowercase hex. Shared with the Windows tools
/// downloader (`tools.rs`) so there is one integrity-hash implementation.
pub fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex_from_digest(hasher.finalize())
}

/// Lowercase-hex encode a sha256 digest (the shared tail of `file_sha256` and
/// `sha256_hex`).
pub(crate) fn hex_from_digest(digest: impl AsRef<[u8]>) -> String {
    use std::fmt::Write;
    let mut hex = String::with_capacity(64);
    for b in digest.as_ref() {
        let _ = write!(hex, "{b:02x}");
    }
    hex
}

/// sha256 of a file as lowercase hex, streamed so a multi-GB GGUF never loads into
/// memory. Hashing the finished `.part` (rather than incrementally during the
/// stream) keeps the resume path correct: the bytes already on disk are included.
pub fn file_sha256(path: &Path) -> Res<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut f = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20]; // 1 MiB
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex_from_digest(hasher.finalize()))
}

/// `--model-dir` override, else the per-OS cache dir + `/unlocr`.
pub fn cache_dir(override_dir: Option<PathBuf>) -> Res<PathBuf> {
    let dir = match override_dir {
        Some(d) => d,
        None => base_cache_dir()?.join("unlocr"),
    };
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn base_cache_dir() -> Res<PathBuf> {
    if let Ok(x) = std::env::var("XDG_CACHE_HOME") {
        if !x.is_empty() {
            return Ok(PathBuf::from(x));
        }
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join("Library/Caches"))
    } else if cfg!(target_os = "windows") {
        let local = std::env::var("LOCALAPPDATA")?;
        Ok(PathBuf::from(local))
    } else {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join(".cache"))
    }
}

/// Per-OS app-DATA base dir (user data, not regenerable cache): mirrors
/// `base_cache_dir`'s XDG-first + OS switch but for data locations. Public so the
/// GUI's SQLite store (`db.rs`) resolves its data dir with the same logic instead
/// of re-deriving the OS ladder in a second crate.
pub fn base_data_dir() -> Res<PathBuf> {
    if let Ok(x) = std::env::var("XDG_DATA_HOME") {
        if !x.is_empty() {
            return Ok(PathBuf::from(x));
        }
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join("Library/Application Support"))
    } else if cfg!(target_os = "windows") {
        let appdata = std::env::var("APPDATA")?;
        Ok(PathBuf::from(appdata))
    } else {
        let home = std::env::var("HOME")?;
        Ok(PathBuf::from(home).join(".local/share"))
    }
}

/// GGUF filename for a quant tag, e.g. "Q8_0" -> "Unlimited-OCR-2-Q8_0.gguf".
pub(crate) fn model_filename(quant: &str) -> String {
    format!("Unlimited-OCR-2-{quant}.gguf")
}

/// Reject quant tags that could escape the cache dir or manipulate the download
/// URL.
pub fn validate_quant(quant: &str) -> Res<()> {
    let charset_ok = !quant.is_empty()
        && quant
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'));
    if !charset_ok || quant.contains("..") {
        return Err(
            format!("invalid quant {quant:?}: allowed characters are [A-Za-z0-9_.-]").into(),
        );
    }
    Ok(())
}

/// Check whether the model and projector GGUFs for `quant` exist in `cache`.
pub fn check_presence(cache: &Path, quant: &str) -> Res<(PathBuf, bool, PathBuf, bool)> {
    validate_quant(quant)?;
    let model = cache.join(model_filename(quant));
    let mmproj = cache.join(MMPROJ);
    Ok((
        model.clone(),
        model.is_file(),
        mmproj.clone(),
        mmproj.is_file(),
    ))
}

/// Quant tags whose v2 GGUF is already cached on disk, e.g. ["Q8_0", "AD-Q4_K"].
pub fn list_cached_quants(cache: &Path) -> Vec<String> {
    let prefix = "Unlimited-OCR-2-";
    let suffix = ".gguf";
    let mut quants: Vec<String> = match fs::read_dir(cache) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter_map(|name| {
                name.strip_prefix(prefix)
                    .and_then(|s| s.strip_suffix(suffix))
                    .map(|q| q.to_string())
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    quants.sort();
    quants
}
