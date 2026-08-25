use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

pub fn current_binary_fingerprint() -> String {
    let result = std::env::current_exe()
        .context("locate current jscout executable")
        .and_then(|path| binary_fingerprint(&path));
    let (fingerprint, error) = fingerprint_or_unavailable(result);
    if let Some(error) = error {
        eprintln!("jscout binary fingerprint status=unavailable error={error:#}");
    }
    fingerprint
}

fn fingerprint_or_unavailable(result: Result<String>) -> (String, Option<anyhow::Error>) {
    match result {
        Ok(fingerprint) => (fingerprint, None),
        Err(error) => ("unavailable".to_string(), Some(error)),
    }
}

fn binary_fingerprint(path: &Path) -> Result<String> {
    let mut file =
        File::open(path).with_context(|| format!("open jscout executable {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read jscout executable {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_fingerprint_is_stable_hex_and_content_sensitive() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let first = directory.path().join("first");
        let second = directory.path().join("second");
        std::fs::write(&first, b"jscout-a")?;
        std::fs::write(&second, b"jscout-b")?;

        let fingerprint = binary_fingerprint(&first)?;
        assert_eq!(fingerprint.len(), 64);
        assert!(fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(fingerprint, binary_fingerprint(&first)?);
        assert_ne!(fingerprint, binary_fingerprint(&second)?);
        Ok(())
    }

    #[test]
    fn unavailable_binary_identity_is_a_stable_nonfatal_marker() {
        let (fingerprint, error) =
            fingerprint_or_unavailable(Err(anyhow::anyhow!("executable was replaced")));
        assert_eq!(fingerprint, "unavailable");
        assert_eq!(
            error.expect("diagnostic").to_string(),
            "executable was replaced"
        );
    }
}
