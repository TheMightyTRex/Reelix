use tokio::sync::broadcast;

use crate::DomainEvent;

/// In-process publish/subscribe for [`DomainEvent`]s.
///
/// Publishing never blocks; slow subscribers lag and receive `RecvError::Lagged`.
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<DomainEvent>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        let (tx, _) = broadcast::channel(capacity);
        Self { tx }
    }

    pub fn publish(&self, event: DomainEvent) {
        // An error only means nobody is subscribed right now.
        let _ = self.tx.send(event);
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DomainEvent> {
        self.tx.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(1024)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_published_events() {
        let bus = EventBus::default();
        let mut rx = bus.subscribe();
        bus.publish(DomainEvent::TaskUpdated { kind: "x".into() });
        assert_eq!(
            rx.recv().await.unwrap(),
            DomainEvent::TaskUpdated { kind: "x".into() }
        );
    }
}
