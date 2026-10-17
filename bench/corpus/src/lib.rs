//! The benchmark's request corpus and keys, generated from a fixed seed. The committed files under
//! `corpus/data/` are what runs use; [`check`] proves they are what the generator produces.

#![forbid(unsafe_code)]

pub mod entries;
pub mod keys;
pub mod prng;

use std::path::{Path, PathBuf};

pub use entries::{Entry, Template, generate};

/// The seed of the committed corpus.
pub const SEED: u64 = 20_260_930;

#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("cannot write {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
    #[error("{path} is not a corpus entry: {source}")]
    Parse {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("no corpus entry named {0}")]
    Unknown(String),
    #[error("{0} differs from what the generator produces with its seed")]
    Stale(PathBuf),
}

fn file(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}.json"))
}

/// The entry `name`: from `dir` when it is committed there, else generated with [`SEED`].
pub fn load(dir: &Path, name: &str) -> Result<Entry, CorpusError> {
    let path = file(dir, name);
    if !entries::COMMITTED.contains(&name) {
        return generate(SEED, name).ok_or_else(|| CorpusError::Unknown(name.to_owned()));
    }
    let text = std::fs::read_to_string(&path).map_err(|source| CorpusError::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_str(&text).map_err(|source| CorpusError::Parse { path, source })
}

fn render(entry: &Entry) -> String {
    let mut text = serde_json::to_string_pretty(entry).unwrap_or_default();
    text.push('\n');
    text
}

/// Writes every committed entry into `dir`.
pub fn write(dir: &Path, seed: u64) -> Result<(), CorpusError> {
    std::fs::create_dir_all(dir).map_err(|source| CorpusError::Write {
        path: dir.to_owned(),
        source,
    })?;
    for name in entries::COMMITTED {
        let entry = generate(seed, name).ok_or_else(|| CorpusError::Unknown((*name).to_owned()))?;
        let path = file(dir, name);
        std::fs::write(&path, render(&entry)).map_err(|source| CorpusError::Write { path, source })?;
    }
    Ok(())
}

/// Checks that every committed entry in `dir` is exactly what [`SEED`] generates.
pub fn check(dir: &Path) -> Result<(), CorpusError> {
    for name in entries::COMMITTED {
        let path = file(dir, name);
        let committed = std::fs::read_to_string(&path).map_err(|source| CorpusError::Read {
            path: path.clone(),
            source,
        })?;
        let entry = generate(SEED, name).ok_or_else(|| CorpusError::Unknown((*name).to_owned()))?;
        if committed != render(&entry) {
            return Err(CorpusError::Stale(path));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn committed_corpus_is_current() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("data");
        check(&dir).unwrap();
    }
}
