//! The notification bus (§4.6).
//!
//! Every adapter's [`AgentSignal`] stream converges here, and the in-app
//! consumers — sidebar pill, status bar, and the OS notifier — each take their
//! own subscription. This is fan-out, not work-stealing: a signal must reach
//! *all* subscribers, so a bare multi-consumer channel would be wrong.
//!
//! No new IPC: this is the same channel discipline the forge poller and status
//! watcher already use.

use std::sync::{Arc, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::AgentSignal;

/// Fan-out publisher for [`AgentSignal`]s. Cheap to clone; clones share
/// subscribers.
#[derive(Clone, Default)]
pub struct Bus {
    subscribers: Arc<Mutex<Vec<Sender<AgentSignal>>>>,
}

impl Bus {
    pub fn new() -> Self {
        Bus::default()
    }

    /// Take a new subscription. Dropping the receiver unsubscribes lazily — the
    /// dead sender is pruned on the next publish.
    pub fn subscribe(&self) -> Receiver<AgentSignal> {
        let (tx, rx) = unbounded();
        self.subscribers.lock().unwrap().push(tx);
        rx
    }

    /// Deliver to every live subscriber, dropping those whose receiver is gone.
    pub fn publish(&self, signal: AgentSignal) {
        self.subscribers
            .lock()
            .unwrap()
            .retain(|tx| tx.send(signal.clone()).is_ok());
    }

    /// Number of live subscriptions (test/diagnostic helper).
    pub fn subscriber_count(&self) -> usize {
        self.subscribers.lock().unwrap().len()
    }
}
