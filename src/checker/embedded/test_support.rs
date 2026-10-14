//! Deterministic compiler fault injection, excluded from production builds.
use std::cell::Cell;

use super::{CheckerError, MemberQuery};

type QueryFault = fn(&MemberQuery) -> Result<(), CheckerError>;
thread_local! {
    static FAULT: Cell<Option<QueryFault>> = const { Cell::new(None) };
}

pub(crate) fn with_query_fault<T>(fault: QueryFault, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<QueryFault>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FAULT.set(self.0);
        }
    }
    let _restore = Restore(FAULT.replace(Some(fault)));
    run()
}

pub(super) fn query_fault(query: &MemberQuery) -> Result<(), CheckerError> {
    FAULT.get().map_or(Ok(()), |fault| fault(query))
}
