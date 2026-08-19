use std::io::{self, ErrorKind};

/// Filesystem races observed after an inventory was taken do not make that
/// inventory atomic. Treat them as absence and let watcher events or periodic
/// reconciliation converge on the next checkout state.
pub(crate) fn is_inventory_race(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::NotFound | ErrorKind::IsADirectory | ErrorKind::NotADirectory
    )
}

/// Resource and transport failures can affect an arbitrary part of the
/// corpus. They must abort the current phase so it can be retried without
/// publishing a clean-but-random subset.
pub(crate) fn is_retryable(error: &io::Error) -> bool {
    if is_inventory_race(error) {
        return false;
    }
    if matches!(
        error.kind(),
        ErrorKind::Interrupted
            | ErrorKind::WouldBlock
            | ErrorKind::TimedOut
            | ErrorKind::ConnectionRefused
            | ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::NotConnected
            | ErrorKind::BrokenPipe
            | ErrorKind::UnexpectedEof
    ) {
        return true;
    }
    retryable_os_error(error.raw_os_error())
}

#[cfg(unix)]
fn retryable_os_error(code: Option<i32>) -> bool {
    code.is_some_and(|code| {
        matches!(
            code,
            libc::EIO
                | libc::EINTR
                | libc::EAGAIN
                | libc::ENOMEM
                | libc::EBUSY
                | libc::EMFILE
                | libc::ENFILE
                | libc::ETIMEDOUT
                | libc::ENETDOWN
                | libc::ENETUNREACH
                | libc::ENETRESET
                | libc::ECONNABORTED
                | libc::ECONNRESET
                | libc::ENOBUFS
                | libc::ESTALE
        )
    })
}

#[cfg(not(unix))]
fn retryable_os_error(_code: Option<i32>) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_races_are_not_phase_failures() {
        for kind in [
            ErrorKind::NotFound,
            ErrorKind::IsADirectory,
            ErrorKind::NotADirectory,
        ] {
            let error = io::Error::from(kind);
            assert!(is_inventory_race(&error), "{kind:?}");
            assert!(!is_retryable(&error), "{kind:?}");
        }
    }

    #[test]
    fn resource_failures_are_retryable_but_permission_is_not() {
        for kind in [
            ErrorKind::Interrupted,
            ErrorKind::WouldBlock,
            ErrorKind::TimedOut,
            ErrorKind::ConnectionReset,
        ] {
            assert!(is_retryable(&io::Error::from(kind)), "{kind:?}");
        }
        for kind in [
            ErrorKind::InvalidData,
            ErrorKind::InvalidInput,
            ErrorKind::PermissionDenied,
        ] {
            assert!(!is_retryable(&io::Error::from(kind)), "{kind:?}");
        }
        #[cfg(unix)]
        {
            assert!(is_retryable(&io::Error::from_raw_os_error(libc::EMFILE)));
            assert!(is_retryable(&io::Error::from_raw_os_error(libc::ESTALE)));
        }
    }
}
