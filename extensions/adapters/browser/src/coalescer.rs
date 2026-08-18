use crate::{BrowserEvent, PointerKind};

#[derive(Debug, Default)]
pub(crate) struct ContinuousCoalescer {
    pending: Vec<BrowserEvent>,
}

impl ContinuousCoalescer {
    pub(crate) fn ingest(&mut self, event: BrowserEvent) -> Vec<BrowserEvent> {
        if is_continuous(&event) {
            if self
                .pending
                .last()
                .is_some_and(|pending| same_continuous_stream(pending, &event))
            {
                *self.pending.last_mut().expect("pending event exists") = event;
            } else {
                self.pending.push(event);
            }
            Vec::new()
        } else {
            let mut ready = self.flush();
            ready.push(event);
            ready
        }
    }

    pub(crate) fn flush(&mut self) -> Vec<BrowserEvent> {
        std::mem::take(&mut self.pending)
    }
}

fn is_continuous(event: &BrowserEvent) -> bool {
    matches!(
        event,
        BrowserEvent::Pointer {
            kind: PointerKind::PointerMove,
            ..
        } | BrowserEvent::Scroll { .. }
    )
}

fn same_continuous_stream(left: &BrowserEvent, right: &BrowserEvent) -> bool {
    match (left, right) {
        (
            BrowserEvent::Pointer {
                kind: PointerKind::PointerMove,
                pointer_id: left,
                ..
            },
            BrowserEvent::Pointer {
                kind: PointerKind::PointerMove,
                pointer_id: right,
                ..
            },
        ) => left == right,
        (BrowserEvent::Scroll { .. }, BrowserEvent::Scroll { .. }) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use crate::{KeyboardKind, Modifiers, PointerType};

    use super::*;

    fn pointer(kind: PointerKind, x: f64) -> BrowserEvent {
        BrowserEvent::Pointer {
            kind,
            pointer_id: 1,
            pointer_type: PointerType::Mouse,
            x_normalized: x,
            y_normalized: 0.5,
            button: 0,
            buttons: 0,
        }
    }

    fn key(kind: KeyboardKind) -> BrowserEvent {
        BrowserEvent::Keyboard {
            kind,
            code: "ArrowRight".to_owned(),
            modifiers: Modifiers::default(),
        }
    }

    fn click() -> BrowserEvent {
        BrowserEvent::Click {
            x_normalized: 0.5,
            y_normalized: 0.5,
            button: 0,
        }
    }

    fn scroll(y: f64) -> BrowserEvent {
        BrowserEvent::Scroll { x: 0.0, y }
    }

    #[test]
    fn pending_continuous_events_flush_before_every_later_discrete_event() {
        let cases = [
            (
                pointer(PointerKind::PointerMove, 0.1),
                key(KeyboardKind::KeyDown),
            ),
            (scroll(10.0), click()),
            (
                pointer(PointerKind::PointerMove, 0.1),
                pointer(PointerKind::PointerUp, 0.2),
            ),
            (scroll(10.0), key(KeyboardKind::KeyUp)),
        ];
        for (continuous, discrete) in cases {
            let mut coalescer = ContinuousCoalescer::default();
            assert!(coalescer.ingest(continuous.clone()).is_empty());
            assert_eq!(
                coalescer.ingest(discrete.clone()),
                vec![continuous, discrete]
            );
        }
    }

    #[test]
    fn pointer_move_and_scroll_keep_observation_order_before_pointer_down() {
        let mut coalescer = ContinuousCoalescer::default();
        let move_event = pointer(PointerKind::PointerMove, 0.1);
        let scroll_event = scroll(10.0);
        assert!(coalescer.ingest(move_event.clone()).is_empty());
        assert!(coalescer.ingest(scroll_event.clone()).is_empty());
        let down = pointer(PointerKind::PointerDown, 0.2);
        assert_eq!(
            coalescer.ingest(down.clone()),
            vec![move_event, scroll_event, down]
        );
    }

    #[test]
    fn only_adjacent_events_from_the_same_continuous_stream_are_coalesced() {
        let mut coalescer = ContinuousCoalescer::default();
        assert!(
            coalescer
                .ingest(pointer(PointerKind::PointerMove, 0.1))
                .is_empty()
        );
        assert!(
            coalescer
                .ingest(pointer(PointerKind::PointerMove, 0.2))
                .is_empty()
        );
        assert!(coalescer.ingest(scroll(10.0)).is_empty());
        assert!(
            coalescer
                .ingest(pointer(PointerKind::PointerMove, 0.3))
                .is_empty()
        );
        assert_eq!(
            coalescer.flush(),
            vec![
                pointer(PointerKind::PointerMove, 0.2),
                scroll(10.0),
                pointer(PointerKind::PointerMove, 0.3),
            ]
        );
    }
}
