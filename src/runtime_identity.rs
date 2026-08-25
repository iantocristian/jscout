use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

pub fn current_binary_fingerprint() -> Result<String> {
    let path = std::env::current_exe().context("locate current jscout executable")?;
    binary_fingerprint(&path)
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
}
