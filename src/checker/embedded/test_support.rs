//! Deterministic compiler fault injection, excluded from production builds.
use std::cell::Cell;

use super::{CheckerError, MemberQuery};

pub(super) type QueryFault = fn(&MemberQuery) -> Result<(), CheckerError>;
thread_local! {
    static FAULT: Cell<Option<QueryFault>> = const { Cell::new(None) };
}

pub(crate) fn with_query_fault<T>(fault: QueryFault, run: impl FnOnce() -> T) -> T {
    with_optional_query_fault(Some(fault), run)
}

pub(super) fn capture_query_fault() -> Option<QueryFault> {
    FAULT.get()
}

pub(super) fn with_optional_query_fault<T>(
    fault: Option<QueryFault>,
    run: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<QueryFault>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAULT.set(self.0);
        }
    }
    let _restore = Restore(FAULT.replace(fault));
    run()
}

pub(super) fn query_fault(query: &MemberQuery) -> Result<(), CheckerError> {
    FAULT.get().map_or(Ok(()), |fault| fault(query))
}
