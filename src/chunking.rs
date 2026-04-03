use anyhow::{Context, Result};
use opencodesearchparser::recursive_character_text_splitter::RecursiveCharacterTextSplitter;
use opencodesearchparser::{CodeLanguage, parse_str};
use regex::Regex;
use std::path::Path;

use crate::types::CodeChunk;

/// Parse and split one source file using the required multi-stage pipeline.
pub fn chunk_file(path: &Path, context_size: usize) -> Result<Vec<CodeChunk>> {
    // Load file as UTF-8 text. Non-UTF files are skipped by caller.
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed reading source file {}", path.display()))?;

    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    // Decide parser language from extension for `opencodesearchparser::parse_str`.
    let language = guess_language(path);

    // Stage 1: AST-based parsing with parse_str when language is supported.
    let mut blocks = match language {
        Some(lang) => match parse_str(&content, lang, 4) {
            Ok(items) if !items.is_empty() => items,
            _ => regex_fallback_blocks(&content),
        },
        None => regex_fallback_blocks(&content),
    };

    if blocks.is_empty() {
        blocks.push(content.clone());
    }

    // Combine neighboring small blocks so chunks are closer to the target size.
    blocks = merge_neighboring_blocks(blocks, context_size);

    // Stage 2: fallback split oversized blocks with recursive splitter and overlap.
    let splitter = RecursiveCharacterTextSplitter::new(None, context_size.max(128), 64);
    let mut chunks = Vec::new();

    for block in blocks {
        if block.len() > context_size {
            let split = splitter.split_text(&block);
            for piece in split {
                if !piece.trim().is_empty() {
                    chunks.push(piece);
                }
            }
        } else if !block.trim().is_empty() {
            chunks.push(block);
        }
    }

    // Convert plain chunks into chunk structs with stable IDs and line ranges.
    let absolute_path = absolute_path_string(path);
    let mut output = Vec::new();
    for (idx, snippet) in chunks.into_iter().enumerate() {
        let (start_line, end_line) = line_range_for_snippet(&content, &snippet);
        let id = format!("{}:{}:{}:{}", absolute_path, idx, start_line, end_line);

        output.push(CodeChunk {
            id,
            path: absolute_path.clone(),
            snippet,
            start_line,
            end_line,
        });
    }

    Ok(output)
}

fn merge_neighboring_blocks(blocks: Vec<String>, context_size: usize) -> Vec<String> {
    let target = context_size.max(1);
    let mut merged = Vec::new();
    let mut current = String::new();

    for raw_block in blocks {
        if raw_block.trim().is_empty() {
            continue;
        }

        let block = raw_block.trim().to_string();
        let separator_len = if current.is_empty() { 0 } else { 2 };
        let candidate_len = current.len() + separator_len + block.len();

        if candidate_len <= target {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(&block);
            continue;
        }

        if !current.is_empty() {
            merged.push(current);
            current = String::new();
        }

        if block.len() > target {
            merged.push(block);
        } else {
            current.push_str(&block);
        }
    }

    if !current.is_empty() {
        merged.push(current);
    }

    merged
}

fn guess_language(path: &Path) -> Option<CodeLanguage> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some(CodeLanguage::Rust),
        "py" => Some(CodeLanguage::Python),
        "js" => Some(CodeLanguage::JavaScript),
        "c" => Some(CodeLanguage::C),
        "cpp" | "cc" | "cxx" => Some(CodeLanguage::Cpp),
        _ => None,
    }
}

fn regex_fallback_blocks(content: &str) -> Vec<String> {
    // Keep regex simple and robust across popular languages.
    let re = Regex::new(
        r"(?ms)(^\s*(?:def|fn|function|class|impl)\s+[^\n\{\(]+[\{\(:]?.*?)(?=^\s*(?:def|fn|function|class|impl)\s+|\z)",
    );

    if let Ok(regex) = re {
        let mut blocks = Vec::new();
        for cap in regex.captures_iter(content) {
            if let Some(full) = cap.get(0) {
                let candidate = full.as_str().trim();
                if !candidate.is_empty() {
                    blocks.push(candidate.to_string());
                }
            }
        }
        blocks
    } else {
        Vec::new()
    }
}

fn line_range_for_snippet(content: &str, snippet: &str) -> (usize, usize) {
    // Best-effort lookup from snippet position in source.
    if let Some(byte_idx) = content.find(snippet) {
        let start_line = content[..byte_idx].chars().filter(|c| *c == '\n').count() + 1;
        let line_count = snippet.chars().filter(|c| *c == '\n').count();
        let end_line = start_line + line_count.max(0);
        (start_line, end_line.max(start_line))
    } else {
        (1, 1)
    }
}

fn absolute_path_string(path: &Path) -> String {
    if let Ok(abs) = path.canonicalize() {
        return abs.display().to_string();
    }

    if path.is_absolute() {
        return path.display().to_string();
    }

    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path).display().to_string(),
        Err(_) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn chunks_python_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("demo.py");

        let mut fh = std::fs::File::create(&file).expect("create file");
        writeln!(fh, "def a():\n    return 1\n\ndef b():\n    return 2").expect("write");

        let chunks = chunk_file(&file, 200).expect("chunking should work");
        assert!(!chunks.is_empty());
        assert!(chunks.iter().any(|c| c.snippet.contains("def a")));
    }

    #[test]
    fn merges_neighboring_small_blocks() {
        let blocks = vec![
            "def a():\n    return 1".to_string(),
            "def b():\n    return 2".to_string(),
            "def c():\n    return 3".to_string(),
        ];

        let merged = merge_neighboring_blocks(blocks, 52);
        assert_eq!(merged.len(), 2);
        assert!(merged[0].contains("def a()"));
        assert!(merged[0].contains("def b()"));
        assert!(merged[1].contains("def c()"));
    }
}
