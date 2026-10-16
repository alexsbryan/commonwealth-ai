// SPDX-License-Identifier: AGPL-3.0-or-later
//! Who waits on the request a call serves (OICP v0.6 §2.17), carried with the
//! call to the engine sites that start model work. The daemon runs a request
//! under [`asked`]; the engine reads [`current`] where an embedding or a
//! generation turn starts, and holds a background request's step there.
//!
//! As in [`crate::engine_observe`], the value lives in a thread-local that
//! [`asked`] installs around each poll of the call's future, so it follows the
//! call across awaits whichever thread polls it. A task the call spawns does
//! not inherit it: work it starts is unasked, served as before §2.17.

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use oicp_types::RequestPriority;

/// A request's priority, and whether it holds its one background step.
#[derive(Debug, Clone)]
pub struct Asked {
    priority: RequestPriority,
    holding: Arc<AtomicBool>,
}

impl Asked {
    /// Who waits on the request.
    pub fn priority(&self) -> RequestPriority {
        self.priority
    }

    /// Whether the request already holds its background step, so a step
    /// started inside that one does not wait on its own request.
    pub fn holding(&self) -> bool {
        self.holding.load(Ordering::SeqCst)
    }

    /// Marks the request as holding its background step until the mark
    /// drops.
    pub fn hold(&self) -> Holding {
        self.holding.store(true, Ordering::SeqCst);
        Holding(Arc::clone(&self.holding))
    }
}

/// The mark [`Asked::hold`] returns; dropping it releases the step.
#[derive(Debug)]
pub struct Holding(Arc<AtomicBool>);

impl Drop for Holding {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

thread_local! {
    static ASKED: RefCell<Option<Asked>> = const { RefCell::new(None) };
}

/// The priority of the request the running call serves, if it named one.
pub fn current() -> Option<Asked> {
    ASKED.with(|a| a.borrow().clone())
}

/// Run `f` as a request asked with `priority`. `None` runs it unchanged.
pub async fn asked<F: Future>(priority: Option<RequestPriority>, f: F) -> F::Output {
    match priority {
        None => f.await,
        Some(priority) => {
            Scoped {
                asked: Asked {
                    priority,
                    holding: Arc::default(),
                },
                inner: Box::pin(f),
            }
            .await
        }
    }
}

/// Polls `inner` with `asked` installed, restoring what was there after.
struct Scoped<F: Future> {
    asked: Asked,
    inner: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let previous = ASKED.with(|a| a.replace(Some(self.asked.clone())));
        let _restore = Restore(previous);
        self.inner.as_mut().poll(cx)
    }
}

/// Puts back the previous value on drop, so nesting and panics leave the
/// thread as they found it.
struct Restore(Option<Asked>);

impl Drop for Restore {
    fn drop(&mut self) {
        let previous = self.0.take();
        ASKED.with(|a| *a.borrow_mut() = previous);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<F: Future>(f: F) -> F::Output {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut f = Box::pin(f);
        loop {
            if let Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                return v;
            }
        }
    }

    #[test]
    fn a_call_reads_its_requests_priority_and_nothing_outside_it() {
        assert!(current().is_none());
        let inside = block_on(asked(Some(RequestPriority::Background), async {
            current().map(|a| a.priority())
        }));
        assert_eq!(inside, Some(RequestPriority::Background));
        assert!(current().is_none(), "the priority leaked past its call");
        let unasked = block_on(asked(None, async { current().is_none() }));
        assert!(unasked);
    }

    #[test]
    fn a_nested_call_sees_its_own_priority_and_the_outer_one_after() {
        let seen = block_on(asked(Some(RequestPriority::Background), async {
            let inner = asked(Some(RequestPriority::Interactive), async {
                current().map(|a| a.priority())
            })
            .await;
            (inner, current().map(|a| a.priority()))
        }));
        assert_eq!(
            seen,
            (
                Some(RequestPriority::Interactive),
                Some(RequestPriority::Background)
            )
        );
    }

    #[test]
    fn holding_lasts_as_long_as_its_mark() {
        block_on(asked(Some(RequestPriority::Background), async {
            let a = current().expect("asked");
            assert!(!a.holding());
            let mark = a.hold();
            assert!(current().expect("asked").holding());
            drop(mark);
            assert!(!a.holding());
        }));
    }
}
