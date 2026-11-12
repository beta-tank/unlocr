use crate::options::{OcrOutput, OutputMode, ResultFormat};
use crate::Res;
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// Resolve the output `.md` path for one input. Shared by the CLI (`ocr::run_pdf`)
/// and the GUI (`run_ocr`) so both agree on where a result is written.
///
/// - `out_dir`: the chosen output folder. A relative `out_file` is joined under it;
///   the default `{stem}.md` is written into it.
/// - `out_file`: optional explicit filename/path (single-input only). An absolute
///   path is used verbatim (ignoring `out_dir`). When it has no extension, `.md`
///   is appended; a non-`.md` extension is left exactly as typed.
/// - `stem`: input file stem, used for the default `{stem}.md` when `out_file` is None.
pub fn resolve_output_path(out_dir: &Path, out_file: Option<&Path>, stem: &str) -> PathBuf {
    match out_file {
        None => out_dir.join(format!("{stem}.md")),
        Some(p) => {
            // Append .md only when no extension is present; respect a typed extension.
            let p = if p.extension().is_none() {
                p.with_extension("md")
            } else {
                p.to_path_buf()
            };
            if p.is_absolute() {
                p
            } else {
                out_dir.join(p)
            }
        }
    }
}

/// Write assembled OCR output to disk per `mode`, returning every path written
/// (combined file first in `Both`). Shared by the CLI (`ocr::run_pdf`) and the
/// GUI (`run_ocr`) so both front ends agree on layout. The caller owns any
/// read-allowlist (the GUI inserts these into `AppState.read_allow`); this fn
/// only writes files + their parent dirs.
///
/// - `Single`: one `{stem}.md` (or `out_file` if given) holding `output.combined`.
/// - `Pages`: a `{out_dir}/{stem}/page-N.md` folder, one file per page. `out_file`
///   is ignored for the folder name (the caller warns when it was set). Page
///   numbers are zero-padded to the width of the largest page number so files
///   sort lexicographically (page-01 before page-10).
/// - `Both`: the combined file plus the per-page folder.
pub fn write_markdown_output(
    mode: OutputMode,
    out_dir: &Path,
    out_file: Option<&Path>,
    stem: &str,
    output: &OcrOutput,
) -> Res<Vec<PathBuf>> {
    let mut written: Vec<PathBuf> = Vec::new();

    if matches!(mode, OutputMode::Single | OutputMode::Both) {
        let combined_path = resolve_output_path(out_dir, out_file, stem);
        if let Some(parent) = combined_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&combined_path, &output.combined)?;
        written.push(combined_path);
    }

    if matches!(mode, OutputMode::Pages | OutputMode::Both) {
        // Guard against path traversal: a file stem of ".." (file named "...pdf")
        // or "." ("..pdf") would resolve out_dir.join(stem) to an unintended parent
        // or same directory. Reject both before any I/O. See also FINDING-001/FINDING-002.
        if stem == "." || stem == ".." {
            return Err(
                format!("invalid output stem '{stem}': stems must not be '.' or '..'").into(),
            );
        }
        let folder = out_dir.join(stem);
        std::fs::create_dir_all(&folder)?;
        // Clear stale `page-*.md` from a prior run into the SAME folder before
        // writing. Without this, OCR'ing a shorter document (or a different
        // same-stem input) over an earlier run leaves the earlier run's
        // higher-numbered pages behind, silently mixing two documents' pages.
        if let Ok(rd) = std::fs::read_dir(&folder) {
            for e in rd.flatten() {
                let p = e.path();
                let is_page_md = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.starts_with("page-") && n.ends_with(".md"))
                    .unwrap_or(false);
                if is_page_md {
                    let _ = std::fs::remove_file(&p);
                }
            }
        }
        // Zero-pad to the largest page number's width (min 2) so a listing sorts
        // page-01 before page-10. Width defaults to 2 when there are no pages
        // (defensive: ocr_pages errors on zero pages before we get here).
        let width = output
            .pages
            .last()
            .map(|(n, _)| n.to_string().len())
            .unwrap_or(2)
            .max(2);
        for (page_num, text) in &output.pages {
            let path = folder.join(format!("page-{page_num:0width$}.md"));
            std::fs::write(&path, text)?;
            written.push(path);
        }
    }

    Ok(written)
}

/// Stems shared by more than one input (sorted, deduped). Same-stem inputs from
/// different folders collide on a shared out dir, the `{stem}.md` file (single)
/// or the `{stem}/` pages folder (pages/both), so a later input silently
/// overwrites an earlier one. A batch caller warns on these before running.
pub fn duplicate_stems(inputs: &[PathBuf]) -> Vec<String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for input in inputs {
        let stem = input
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("output")
            .to_string();
        *counts.entry(stem).or_insert(0) += 1;
    }
    let mut dups: Vec<String> = counts
        .into_iter()
        .filter(|(_, n)| *n > 1)
        .map(|(s, _)| s)
        .collect();
    dups.sort();
    dups
}

/// Strip Unlimited-OCR layout annotations from a whole page of model output.
///
/// The Unlimited-OCR / DeepSeek-OCR family natively emits layout-grounded text:
/// `<|det|>label [x1, y1, x2, y2]<|/det|>content` with 0-999-normalized
/// coordinates, and upstream's Python `infer()` regex-cleans that to markdown.
/// Backends may keep special tokens (including OCR 2's `<PAGE>`) or drop them,
/// leaving `label [x, y, x, y]content` lines. This ports upstream's cleanup: it is the
/// final-text sink; the streaming path uses `AnnotationStripper` (same per-line
/// logic) because SSE chunks split annotations mid-prefix.
pub fn strip_layout_annotations(text: &str) -> String {
    clean_layout_annotations(text, false)
}

/// Convert a model response using the shared v1/v2 layout parser. Raw is an
/// identity operation, including whitespace and special tokens. This is a
/// page-level converter, not a document-mode cross-page merge implementation.
pub fn format_result(text: &str, format: ResultFormat) -> String {
    match format {
        ResultFormat::Markdown => clean_layout_annotations(text, false),
        ResultFormat::Text => clean_layout_annotations(text, true),
        ResultFormat::Raw => text.to_string(),
    }
}

pub(crate) fn clean_layout_annotations(text: &str, plain_text: bool) -> String {
    let mut out = String::with_capacity(text.len());
    let mut first = true;
    for line in text.lines() {
        if let Some(l) = clean_layout_line(line, plain_text) {
            if !first {
                out.push('\n');
            }
            out.push_str(&l);
            first = false;
        }
    }
    out
}

/// One line with its layout annotation removed, or `None` when the whole line
/// was annotation (a bare region like `image [138, 346, 868, 835]`).
///
/// Leaked `<|ref|>`/`<|det|>` markers are deleted first (defensive: llama-server
/// normally suppresses special tokens), then a line-start `label [n, n, n, n]`
/// prefix is removed. `title` lines become `# ` headings (materially better
/// markdown for one match arm); every other label keeps just its content.
fn clean_layout_line(line: &str, plain_text: bool) -> Option<String> {
    let cleaned = clean_layout_line_inner(line, plain_text)?;
    if plain_text {
        let lines: Vec<_> = cleaned.split('\n').filter_map(plain_result_line).collect();
        (!lines.is_empty()).then(|| lines.join("\n"))
    } else {
        Some(cleaned)
    }
}

/// Text rendering of the headings and tables emitted by the model. Keep HTML
/// tables in Markdown (including rowspan/colspan); text retains their reading
/// order with tab-separated cells rather than leaking HTML tags.
fn plain_result_line(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if trimmed.len() >= 2 && trimmed.starts_with('|') && trimmed.ends_with('|') {
        let cells: Vec<_> = trimmed[1..trimmed.len() - 1]
            .split('|')
            .map(str::trim)
            .collect();
        if cells.iter().all(|cell| {
            let rule = cell.trim_matches(':');
            rule.len() >= 3 && rule.bytes().all(|b| b == b'-')
        }) {
            return None;
        }
        return Some(cells.join("\t"));
    }
    let hashes = trimmed.bytes().take_while(|b| *b == b'#').count();
    let line = if (1..=6).contains(&hashes) && trimmed.as_bytes().get(hashes) == Some(&b' ') {
        &trimmed[hashes + 1..]
    } else {
        line
    };
    let mut out = String::new();
    let mut rest = line;
    while let Some(start) = rest.find('<') {
        out.push_str(&rest[..start]);
        let Some(end) = rest[start..].find('>') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + end];
        let name = tag
            .split_whitespace()
            .next()
            .unwrap_or("")
            .trim_end_matches('/');
        match name.to_ascii_lowercase().as_str() {
            "/td" | "/th" => out.push('\t'),
            "/tr" | "br" => {
                while out.ends_with('\t') {
                    out.pop();
                }
                out.push('\n');
            }
            "table" | "/table" | "thead" | "/thead" | "tbody" | "/tbody" | "tfoot" | "/tfoot"
            | "tr" | "td" | "th" => {}
            _ => out.push_str(&rest[start..=start + end]),
        }
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    Some(
        out.trim_end_matches(['\t', '\n'])
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&nbsp;", " ")
            .replace("&amp;", "&"),
    )
}

fn clean_layout_line_inner(line: &str, plain_text: bool) -> Option<String> {
    // OCR 2 block headers are separate from their content. Discard even a
    // truncated header (<BLOCK>te) rather than leaking metadata into the text.
    // Several blocks may arrive on one line; preserve their boundaries.
    if line.contains("<BLOCK>") {
        let mut parts = line.split("<BLOCK>");
        let mut out = Vec::new();
        if let Some(prefix) = parts
            .next()
            .and_then(|s| clean_layout_line_inner(s, plain_text))
        {
            if !prefix.trim().is_empty() {
                out.push(prefix);
            }
        }
        for block in parts {
            let Some((header, content)) = block.split_once("<FORK>") else {
                continue;
            };
            let Some(content) = clean_layout_line_inner(content, plain_text) else {
                continue;
            };
            if header.trim_start().starts_with("title") && !plain_text && !content.is_empty() {
                let level = (1..=6)
                    .find(|n| header.contains(&format!("<LEVEL{n}>")))
                    .unwrap_or(1);
                out.push(format!("{} {}", "#".repeat(level), content.trim_start()));
            } else if !content.is_empty() {
                out.push(content);
            }
        }
        return (!out.is_empty()).then(|| out.join("\n"));
    }

    let marker = line.contains("<PAGE>") || line.contains("<MERGE>");
    let mut line = Cow::Borrowed(line);
    for tag in [
        "<PAGE>", "<MERGE>", "<|ref|>", "<|/ref|>", "<|det|>", "<|/det|>",
    ] {
        if line.contains(tag) {
            line = Cow::Owned(line.replace(tag, ""));
        }
    }
    if marker && line.trim().is_empty() {
        return None;
    }
    match split_layout_prefix(line.trim_start()) {
        None => Some(line.into_owned()),
        Some((label, rest, level)) => {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix("<FORK>").unwrap_or(rest).trim_start();
            if rest.is_empty() {
                None
            } else if label == "title" && !plain_text {
                Some(format!("{} {rest}", "#".repeat(level)))
            } else {
                Some(rest.to_string())
            }
        }
    }
}

/// Split a line-start layout annotation into `(label, rest, heading_level)`; `None` when the
/// line doesn't begin with one. Hand-rolled to avoid a regex dependency. The
/// shape mirrors upstream's det_pattern: a label (`[A-Za-z_][A-Za-z0-9_-]*`,
/// capped at 24 chars), optional spaces, then a box or list of boxes containing four comma-separated
/// unsigned integers. Each integer is capped at 3 digits (coordinates are
/// 0-999 normalized upstream), which keeps prose like `see [1, 2]` or larger
/// bracketed numbers out of the match.
fn split_layout_prefix(line: &str) -> Option<(&str, &str, usize)> {
    let b = line.as_bytes();
    if b.is_empty() || !(b[0].is_ascii_alphabetic() || b[0] == b'_') {
        return None;
    }
    let mut i = 1;
    while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'-') {
        i += 1;
    }
    if i > 24 {
        return None;
    }
    let label_end = i;
    while i < b.len() && b[i] == b' ' {
        i += 1;
    }
    // Some backends suppress <BLOCK> but retain LEVEL/FORK. Parse these as
    // header syntax rather than deleting them globally from document content.
    let mut level = 1;
    for n in 1..=6 {
        let tag = format!("<LEVEL{n}>");
        if line[i..].starts_with(&tag) {
            level = n;
            i += tag.len();
            while b.get(i) == Some(&b' ') {
                i += 1;
            }
            break;
        }
    }
    if i >= b.len() || b[i] != b'[' {
        return None;
    }
    i += 1;
    let nested = b.get(i) == Some(&b'[');
    loop {
        if nested {
            if b.get(i) != Some(&b'[') {
                return None;
            }
            i += 1;
        }
        for k in 0..4 {
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            let digits_start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            let n_digits = i - digits_start;
            if n_digits == 0 || n_digits > 3 {
                return None;
            }
            while i < b.len() && b[i] == b' ' {
                i += 1;
            }
            if k < 3 {
                if i >= b.len() || b[i] != b',' {
                    return None;
                }
                i += 1;
            }
        }
        if i >= b.len() || b[i] != b']' {
            return None;
        }
        i += 1;
        if !nested {
            break;
        }
        while b.get(i) == Some(&b' ') {
            i += 1;
        }
        match b.get(i) {
            Some(b']') => {
                i += 1;
                break;
            }
            Some(b',') => {
                i += 1;
                while b.get(i) == Some(&b' ') {
                    i += 1;
                }
            }
            _ => return None,
        }
    }
    Some((&line[..label_end], &line[i..], level))
}

/// Line-buffering wrapper around `clean_layout_line` for the streaming path.
/// SSE chunks split annotations mid-prefix (`"tit"`, `"le [168,"`), so chunks
/// are buffered until a newline completes each line. The tradeoff is that the
/// live transcript updates per line (one layout block) instead of per token.
pub(crate) struct AnnotationStripper {
    buf: String,
    plain_text: bool,
}

impl AnnotationStripper {
    #[cfg(test)]
    pub(crate) fn new() -> Self {
        Self::with_plain_text(false)
    }

    pub(crate) fn with_plain_text(plain_text: bool) -> Self {
        AnnotationStripper {
            buf: String::new(),
            plain_text,
        }
    }

    /// Feed one streamed chunk; returns the stripped complete lines that became
    /// available (each keeping its trailing newline), possibly empty.
    pub(crate) fn push(&mut self, chunk: &str) -> String {
        self.buf.push_str(chunk);
        let Some(last_nl) = self.buf.rfind('\n') else {
            return String::new();
        };
        let complete: String = self.buf.drain(..=last_nl).collect();
        let mut out = String::new();
        for line in complete.lines() {
            if let Some(l) = clean_layout_line(line, self.plain_text) {
                out.push_str(&l);
                out.push('\n');
            }
        }
        out
    }

    /// Strip and return the residual unterminated last line (empty when none).
    pub(crate) fn finish(&mut self) -> String {
        let rest = std::mem::take(&mut self.buf);
        if rest.is_empty() {
            return String::new();
        }
        clean_layout_line(&rest, self.plain_text).unwrap_or_default()
    }
}

/// Append one page's text with a `<!-- page N -->` delimiter (1-based).
/// Canonical implementation: ocr_pages (lib) and the CLI path (via run_pdf's
/// delegation) both route through this, so page-delimited markdown is identical
/// across the CLI and GUI callers (covered by the lib test below).
pub fn push_page(md: &mut String, idx: usize, text: &str) {
    md.push_str(&format!("\n\n<!-- page {} -->\n\n", idx + 1));
    md.push_str(text);
}
