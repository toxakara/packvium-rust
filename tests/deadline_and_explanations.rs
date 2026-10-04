//! Two small public surfaces a caller reads directly: a `Deadline` driven by an injected
//! clock, and the human-readable explanation of why an item was left unpacked.

mod support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use packvium_core::{
    Clock, Deadline, ItemInstance, ReasonProof, UnpackedItem, explain_reason, explain_unpacked_item,
};

/// A clock that stands still until a test moves it.
#[derive(Debug, Default)]
struct ManualClock(AtomicU64);

impl Clock for ManualClock {
    fn now_ns(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
fn a_deadline_reports_elapsed_and_remaining_time_from_its_clock() {
    let clock = Arc::new(ManualClock::default());
    let deadline = Deadline::with_clock(10, clock.clone());
    assert_eq!(deadline.remaining_ns(), 10_000_000);
    clock.0.store(4_000_000, Ordering::SeqCst);
    assert_eq!(deadline.elapsed_ms(), 4);
    assert_eq!(deadline.remaining_ns(), 6_000_000);
    clock.0.store(25_000_000, Ordering::SeqCst);
    assert_eq!(deadline.remaining_ns(), 0);
    assert!(deadline.expired());
    let rendered = format!("{deadline:?}");
    assert!(rendered.starts_with("Deadline {"), "{rendered}");
    assert!(rendered.contains("limit_ns: 10000000"), "{rendered}");
}

fn unpacked(reason: &str, details: Vec<String>) -> UnpackedItem {
    UnpackedItem::new(
        ItemInstance {
            item: support::item("vase"),
            sequence: 1,
        },
        reason.into(),
        details,
    )
}

#[test]
fn an_unknown_reason_is_refused_with_its_code() {
    let error = explain_reason("gremlins").expect_err("no such reason");
    assert_eq!(
        error.to_string(),
        "no explanation registered for reason code \"gremlins\""
    );
}

#[test]
fn an_explanation_carries_its_proof_level_and_details() {
    let observed =
        explain_unpacked_item(&unpacked("search_exhausted", Vec::new())).expect("a known reason");
    assert!(observed.contains("Observed: "), "{observed}");
    assert!(!observed.contains('('), "{observed}");

    let mut unlabelled = unpacked("payload_exceeded", vec!["over by 2 kg".into()]);
    unlabelled.proof = ReasonProof {
        level: "hearsay".into(),
        observations: Vec::new(),
    };
    let rendered = explain_unpacked_item(&unlabelled).expect("a known reason");
    assert!(!rendered.contains("Proven"), "{rendered}");
    assert!(rendered.ends_with(" (over by 2 kg)"), "{rendered}");
}
