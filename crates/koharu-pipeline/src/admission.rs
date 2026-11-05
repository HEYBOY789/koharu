use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use tokio::sync::{Mutex, OwnedMutexGuard};

use crate::StopToken;

/// A workflow keeps its scheduler and scene while an interactive Remove stroke
/// borrows the inference lane. Only drained executions may relinquish it.
#[derive(Default)]
pub(crate) struct Admission {
    lane: Arc<Mutex<()>>,
    interactive: AtomicUsize,
}

pub(crate) struct Permit {
    admission: Arc<Admission>,
    lane: Option<OwnedMutexGuard<()>>,
    interactive: bool,
}

struct Waiting(Arc<Admission>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.interactive.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Admission {
    pub(crate) async fn acquire(
        self: &Arc<Self>,
        interactive: bool,
        stop: &StopToken,
    ) -> Option<Permit> {
        let waiting = interactive.then(|| {
            self.interactive.fetch_add(1, Ordering::AcqRel);
            Waiting(self.clone())
        });
        let lane = loop {
            let lock = self.lane.clone().lock_owned();
            tokio::pin!(lock);
            let lane = loop {
                if stop.stopped() {
                    return None;
                }
                tokio::select! {
                    permit = &mut lock => break permit,
                    () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
                }
            };
            if !interactive && self.interactive.load(Ordering::Acquire) > 0 {
                drop(lane);
                tokio::task::yield_now().await;
                continue;
            }
            break lane;
        };
        drop(waiting);
        (!stop.stopped()).then(|| Permit {
            admission: self.clone(),
            lane: Some(lane),
            interactive,
        })
    }
}

impl Permit {
    pub(crate) fn should_yield(&self) -> bool {
        !self.interactive && self.admission.interactive.load(Ordering::Acquire) > 0
    }

    pub(crate) async fn yield_to_interactive(&mut self, stop: &StopToken) -> bool {
        self.lane.take();
        let Some(next) = self.admission.acquire(false, stop).await else {
            return false;
        };
        *self = next;
        true
    }
}
