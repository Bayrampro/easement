//! Shared input hashing and the versioned SCIP cache container.
use super::*;
use crate::scan::AnalysisGap;
use serde::{Deserialize, Serialize};

const MAGIC: &[u8] = b"easement-index-v2\n";

#[derive(Serialize, Deserialize)]
struct Metadata {
    index_len: u64,
    digest: [u8; 32],
    gaps: Vec<AnalysisGap>,
}

pub(super) struct Artifact<'a> {
    pub index: &'a [u8],
    pub gaps: Vec<AnalysisGap>,
}

pub(super) fn encode(index: &[u8], gaps: &[AnalysisGap]) -> Result<Vec<u8>> {
    let metadata = serde_json::to_vec(&Metadata {
        index_len: u64::try_from(index.len())?,
        digest: *blake3::hash(index).as_bytes(),
        gaps: gaps.to_vec(),
    })?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + 8 + metadata.len() + index.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&u64::try_from(metadata.len())?.to_le_bytes());
    bytes.extend_from_slice(&metadata);
    bytes.extend_from_slice(index);
    bytes.extend_from_slice(blake3::hash(&bytes).as_bytes());
    Ok(bytes)
}

pub(super) fn decode(bytes: &[u8]) -> Result<Artifact<'_>> {
    let split = bytes
        .len()
        .checked_sub(32)
        .context("truncated cache checksum")?;
    let (payload, checksum) = bytes.split_at(split);
    if blake3::hash(payload).as_bytes() != checksum {
        bail!("corrupt cached artifact");
    }
    let bytes = payload
        .strip_prefix(MAGIC)
        .context("unknown index cache format")?;
    let length = bytes.get(..8).context("truncated cache header")?;
    let length = usize::try_from(u64::from_le_bytes(length.try_into()?))?;
    let payload = &bytes[8..];
    let metadata: Metadata =
        serde_json::from_slice(payload.get(..length).context("truncated cache metadata")?)?;
    let index = &payload[length..];
    if usize::try_from(metadata.index_len)? != index.len()
        || blake3::hash(index).as_bytes() != &metadata.digest
    {
        bail!("corrupt cached SCIP index");
    }
    Ok(Artifact {
        index,
        gaps: metadata.gaps,
    })
}

pub(super) fn write(path: &Path, prefix: &str, index: &[u8], gaps: &[AnalysisGap]) {
    let Some(parent) = path.parent() else {
        return;
    };
    let result = || -> Result<()> {
        let bytes = encode(index, gaps)?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        std::io::Write::write_all(&mut file, &bytes)?;
        file.persist(path)?;
        prune(parent, prefix);
        Ok(())
    };
    let _ = result();
}

fn identity(path: &Path, root: &Path) -> PathBuf {
    path.strip_prefix(root).unwrap_or(path).to_path_buf()
}

fn path_input(
    path: &Path,
    root: &Path,
    tree: bool,
    seen: &mut BTreeSet<PathBuf>,
    hash: &mut StableHasher,
) -> Result<()> {
    identity(path, root).hash(hash);
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !tree => {
            "missing".hash(hash);
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    if metadata.file_type().is_symlink() {
        "symlink".hash(hash);
        std::fs::read_link(path)?.hash(hash);
    }
    let canonical = dunce::canonicalize(path)?;
    identity(&canonical, root).hash(hash);
    if !seen.insert(canonical.clone()) {
        "alias".hash(hash);
        return Ok(());
    }
    let metadata = std::fs::metadata(&canonical)?;
    if metadata.is_dir() && tree {
        let mut children = std::fs::read_dir(&canonical)?.collect::<std::io::Result<Vec<_>>>()?;
        children.retain(|child| child.file_name() != ".git");
        children.sort_by_key(|child| child.file_name());
        "directory".hash(hash);
        children.len().hash(hash);
        for child in children {
            path_input(&child.path(), root, true, seen, hash)?;
        }
    } else if metadata.is_file() {
        use std::io::Read;
        "file".hash(hash);
        let mut file = std::fs::File::open(canonical)?;
        let mut content = blake3::Hasher::new();
        let mut buffer = [0; 65536];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            content.update(&buffer[..read]);
        }
        content.finalize().as_bytes().hash(hash);
    } else {
        bail!(
            "cache input is not a regular file or directory: {}",
            path.display()
        );
    }
    Ok(())
}

pub(super) fn hash_inputs(
    mut inputs: Vec<Input>,
    root: &Path,
    hash: &mut StableHasher,
) -> Result<()> {
    let canonical_root = dunce::canonicalize(root)?;
    for input in &mut inputs {
        if let Input::File(path) | Input::Tree(path) = input
            && let Ok(relative) = path.strip_prefix(root)
        {
            *path = canonical_root.join(relative);
        }
    }
    let root = canonical_root.as_path();
    inputs.sort();
    inputs.dedup();
    inputs.len().hash(hash);
    let mut seen = BTreeSet::new();
    for input in inputs {
        match input {
            Input::File(path) => {
                "file input".hash(hash);
                path_input(&path, root, false, &mut seen, hash)?;
            }
            Input::Tree(path) => {
                "tree input".hash(hash);
                path_input(&path, root, true, &mut seen, hash)?;
            }
            Input::Value(name, value) => {
                "value input".hash(hash);
                name.hash(hash);
                value.hash(hash);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_preserves_scip_and_extraction_diagnostics_and_rejects_corruption() {
        let gaps = vec![AnalysisGap {
            file: "lib/file.dart".into(),
            line: Some(2),
            reason: "incomplete extraction".into(),
        }];
        let bytes = encode(b"raw SCIP bytes", &gaps).unwrap();
        let artifact = decode(&bytes).unwrap();
        assert_eq!(artifact.index, b"raw SCIP bytes");
        assert_eq!(artifact.gaps, gaps);
        assert!(decode(b"raw legacy SCIP").is_err());
        assert!(decode(br#"{"version":1,"index":[],"gaps":[]}"#).is_err());
        for end in [0, MAGIC.len(), MAGIC.len() + 4, bytes.len() - 1] {
            assert!(decode(&bytes[..end]).is_err());
        }
        for offset in [MAGIC.len() + 9, bytes.len() - 33, bytes.len() - 1] {
            let mut changed = bytes.clone();
            changed[offset] ^= 1;
            assert!(decode(&changed).is_err());
        }
        let mut extra = bytes;
        extra.push(0);
        assert!(decode(&extra).is_err());
    }

    fn key(inputs: Vec<Input>, root: &Path) -> Result<u64> {
        let mut hash = StableHasher::default();
        hash_inputs(inputs, root, &mut hash)?;
        Ok(hash.finish())
    }

    #[test]
    fn typed_inputs_track_hidden_trees_missing_files_and_content_not_mtime() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir(root.join(".hidden")).unwrap();
        let file = root.join(".hidden/config.json");
        std::fs::write(&file, "first").unwrap();
        let inputs = || {
            vec![
                Input::Tree(root.join(".hidden")),
                Input::File(root.join("lock")),
                Input::Value("profile".into(), b"v1".to_vec()),
            ]
        };
        let first = key(inputs(), root).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(3600))
            .unwrap();
        assert_eq!(first, key(inputs(), root).unwrap());
        std::fs::write(&file, "other").unwrap();
        let second = key(inputs(), root).unwrap();
        assert_ne!(first, second);
        std::fs::write(root.join("lock"), "created").unwrap();
        assert_ne!(second, key(inputs(), root).unwrap());
        assert!(key(vec![Input::Tree(root.join("missing"))], root).is_err());
    }

    #[test]
    fn project_file_keys_are_stable_across_checkouts_and_input_order() {
        let tmp = tempfile::tempdir().unwrap();
        let mut keys = Vec::new();
        for checkout in ["first", "second"] {
            let root = tmp.path().join(checkout);
            std::fs::create_dir(&root).unwrap();
            std::fs::write(root.join("a"), "a").unwrap();
            std::fs::write(root.join("b"), "b").unwrap();
            let files = vec![Input::File(root.join("b")), Input::File(root.join("a"))];
            let first = key(files.clone(), &root).unwrap();
            assert_eq!(
                first,
                key(files.into_iter().rev().collect(), &root).unwrap()
            );
            keys.push(first);
        }
        assert_eq!(keys[0], keys[1]);
    }
    #[cfg(unix)]
    #[test]
    fn tree_keys_follow_symlinks_and_track_their_resolution() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let tree = root.join("tree");
        std::fs::create_dir(&tree).unwrap();
        std::fs::write(root.join("first"), "same").unwrap();
        std::fs::write(root.join("second"), "same").unwrap();
        let link = tree.join("dependency");
        symlink("../first", &link).unwrap();
        let inputs = || vec![Input::Tree(tree.clone())];
        let first = key(inputs(), root).unwrap();
        std::fs::write(root.join("first"), "edit").unwrap();
        assert_ne!(first, key(inputs(), root).unwrap());
        std::fs::write(root.join("first"), "same").unwrap();
        assert_eq!(first, key(inputs(), root).unwrap());
        std::fs::remove_file(&link).unwrap();
        symlink("../second", &link).unwrap();
        assert_ne!(first, key(inputs(), root).unwrap());
        // A cycle terminates deterministically rather than hiding changed files.
        symlink(".", tree.join("cycle")).unwrap();
        assert_eq!(key(inputs(), root).unwrap(), key(inputs(), root).unwrap());
        std::fs::remove_file(root.join("second")).unwrap();
        assert!(key(inputs(), root).is_err());
    }
}
