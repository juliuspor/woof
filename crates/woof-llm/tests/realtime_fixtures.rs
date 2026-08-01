use serde_json::Value;
use woof_llm::TranscriptReconciler;

#[test]
fn official_delta_and_completed_shapes_reconcile() {
    let events: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/realtime-events.json")).unwrap();
    let mut reconciler = TranscriptReconciler::default();
    for event in events {
        assert!(event["event_id"].as_str().is_some_and(|id| !id.is_empty()));
        assert_eq!(event["content_index"].as_u64(), Some(0));
        let item_id = event["item_id"].as_str().unwrap();
        match event["type"].as_str().unwrap() {
            "conversation.item.input_audio_transcription.delta" => {
                reconciler
                    .apply_delta(item_id, event["delta"].as_str().unwrap())
                    .unwrap();
            }
            "conversation.item.input_audio_transcription.completed" => {
                assert_eq!(event["usage"]["type"], "tokens");
                reconciler
                    .complete(item_id, event["transcript"].as_str().unwrap())
                    .unwrap();
            }
            _ => unreachable!(),
        }
    }
    assert_eq!(reconciler.final_transcript(), "Hello, how are you?");
}
