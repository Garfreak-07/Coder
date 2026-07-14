use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use coder_events::{OutputEnvelope, OutputEvent, OutputPriority};
use tokio::sync::broadcast;

const DEFAULT_OUTPUT_CHANNEL_CAPACITY: usize = 1_024;

#[derive(Debug)]
struct OutputChannel {
    sender: broadcast::Sender<OutputEnvelope>,
    next_sequence: u64,
}

#[derive(Debug, Clone)]
pub(crate) struct OutputHub {
    capacity: usize,
    channels: Arc<Mutex<BTreeMap<String, OutputChannel>>>,
}

impl Default for OutputHub {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_OUTPUT_CHANNEL_CAPACITY)
    }
}

impl OutputHub {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            channels: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }

    pub(crate) fn register_session(&self, session_id: &str) {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        channels.entry(session_id.to_owned()).or_insert_with(|| {
            let (sender, _) = broadcast::channel(self.capacity);
            OutputChannel {
                sender,
                next_sequence: 1,
            }
        });
    }

    pub(crate) fn remove_session(&self, session_id: &str) {
        self.channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(session_id);
    }

    pub(crate) fn subscribe(
        &self,
        session_id: &str,
    ) -> Option<broadcast::Receiver<OutputEnvelope>> {
        self.channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .map(|channel| channel.sender.subscribe())
    }

    pub(crate) fn publish(
        &self,
        session_id: &str,
        turn_id: Option<String>,
        source: impl Into<String>,
        priority: OutputPriority,
        output: OutputEvent,
    ) -> Option<OutputEnvelope> {
        let mut channels = self
            .channels
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let channel = channels.get_mut(session_id)?;
        let sequence = channel.next_sequence;
        channel.next_sequence = channel.next_sequence.saturating_add(1);
        let envelope = OutputEnvelope::new(session_id, turn_id, sequence, source, priority, output);
        let _ = channel.sender.send(envelope.clone());
        Some(envelope)
    }
}

#[cfg(test)]
mod tests {
    use coder_events::{OutputEvent, OutputPriority};
    use tokio::sync::broadcast::error::TryRecvError;

    use super::*;

    #[test]
    fn output_sequences_are_monotonic_and_session_scoped() {
        let hub = OutputHub::default();
        hub.register_session("session-a");
        hub.register_session("session-b");

        let first = hub
            .publish(
                "session-a",
                None,
                "conversation",
                OutputPriority::Normal,
                OutputEvent::TextStarted,
            )
            .unwrap();
        let second = hub
            .publish(
                "session-a",
                None,
                "conversation",
                OutputPriority::Normal,
                OutputEvent::TextCompleted {
                    text: "done".to_owned(),
                },
            )
            .unwrap();
        let other = hub
            .publish(
                "session-b",
                None,
                "runtime",
                OutputPriority::Low,
                OutputEvent::TurnStarted,
            )
            .unwrap();

        assert_eq!((first.sequence, second.sequence), (1, 2));
        assert_eq!(other.sequence, 1);
    }

    #[test]
    fn subscribers_receive_only_their_session_output() {
        let hub = OutputHub::default();
        hub.register_session("session-a");
        hub.register_session("session-b");
        let mut a = hub.subscribe("session-a").unwrap();
        let mut b = hub.subscribe("session-b").unwrap();

        hub.publish(
            "session-a",
            Some("turn-a".to_owned()),
            "conversation",
            OutputPriority::Normal,
            OutputEvent::TextDelta {
                delta: "hello".to_owned(),
            },
        );

        assert_eq!(a.try_recv().unwrap().session_id, "session-a");
        assert!(matches!(b.try_recv(), Err(TryRecvError::Empty)));
    }

    #[test]
    fn bounded_channel_reports_lag_instead_of_growing_without_limit() {
        let hub = OutputHub::with_capacity(2);
        hub.register_session("session-a");
        let mut receiver = hub.subscribe("session-a").unwrap();

        for _ in 0..3 {
            hub.publish(
                "session-a",
                None,
                "conversation",
                OutputPriority::Normal,
                OutputEvent::TextStarted,
            );
        }

        assert!(matches!(receiver.try_recv(), Err(TryRecvError::Lagged(1))));
    }
}
