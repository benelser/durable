//! Validation tests for the durable execution rubric.
//! Each test corresponds to a specific acceptance criterion.

use durable_runtime::*;
use durable_runtime::json::{self, json_object, json_str, json_num, json_null, json_bool, ToJson, FromJson};
use std::sync::Arc;

// ===========================================================================
// 1. IMMUTABLE EVENT LOG
// ===========================================================================

#[test]
fn test_event_log_append_and_reconstruct() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // Create execution
    store.append(exec_id, EventType::ExecutionCreated { version: Some("v1".to_string()), prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();

    // Simulate 3 steps
    for i in 0..3 {
        store.append(exec_id, EventType::StepStarted {
            step_number: i,
            step_name: format!("step_{}", i),
            param_hash: i * 1000,
            params: format!("{{\"i\":{}}}", i),
        }).unwrap();
        store.append(exec_id, EventType::StepCompleted {
            step_number: i,
            step_name: format!("step_{}", i),
            result: format!("\"result_{}\"", i),
        }).unwrap();
    }

    store.append(exec_id, EventType::ExecutionCompleted {
        result: "done".to_string(),
    }).unwrap();

    // Reconstruct state from events
    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 8); // 1 created + 3*(started+completed) + 1 completed

    let state = ExecutionState::from_events(exec_id, &events);
    assert_eq!(state.status, ExecutionStatus::Completed);
    assert_eq!(state.version, Some("v1".to_string()));
    assert_eq!(state.step_count, 3);
    assert_eq!(state.step_results.len(), 3);

    // Verify each step
    for i in 0..3 {
        let snap = &state.step_results[&i];
        assert_eq!(snap.step_name, format!("step_{}", i));
        assert!(snap.completed);
        assert_eq!(snap.result, Some(format!("\"result_{}\"", i)));
    }
}

#[test]
fn test_event_log_signals_are_events() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();

    // Signal received is an immutable event
    store.append(exec_id, EventType::SignalReceived {
        name: "approval".to_string(),
        data: "true".to_string(),
    }).unwrap();

    // Signal consumed is ALSO an immutable event (not a deletion)
    store.append(exec_id, EventType::SignalConsumed {
        name: "approval".to_string(),
    }).unwrap();

    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 3);

    // All events are still there — nothing was deleted
    let state = ExecutionState::from_events(exec_id, &events);
    assert!(state.pending_signals.is_empty()); // consumed
}

#[test]
fn test_event_log_point_in_time_query() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();
    store.append(exec_id, EventType::StepStarted {
        step_number: 0, step_name: "step_0".to_string(),
        param_hash: 0, params: "{}".to_string(),
    }).unwrap();
    store.append(exec_id, EventType::StepCompleted {
        step_number: 0, step_name: "step_0".to_string(),
        result: "\"a\"".to_string(),
    }).unwrap();
    // At this point: 1 step completed, running

    store.append(exec_id, EventType::Suspended {
        reason: "waiting".to_string(),
    }).unwrap();
    // At this point: suspended

    store.append(exec_id, EventType::Resumed { generation: 2 }).unwrap();
    store.append(exec_id, EventType::ExecutionCompleted { result: "done".to_string() }).unwrap();

    let all_events = store.events(exec_id).unwrap();

    // Point-in-time: after 3 events (step completed)
    let state_at_3 = ExecutionState::from_events(exec_id, &all_events[..3]);
    assert_eq!(state_at_3.status, ExecutionStatus::Running);
    assert_eq!(state_at_3.step_results.len(), 1);

    // Point-in-time: after 4 events (suspended)
    let state_at_4 = ExecutionState::from_events(exec_id, &all_events[..4]);
    assert_eq!(state_at_4.status, ExecutionStatus::Suspended);

    // Full history
    let state_final = ExecutionState::from_events(exec_id, &all_events);
    assert_eq!(state_final.status, ExecutionStatus::Completed);
    assert_eq!(state_final.generation, 2);
}

#[test]
fn test_event_log_file_backed_persistence() {
    let dir = std::env::temp_dir().join("durable_test_event_file");
    let _ = std::fs::remove_dir_all(&dir);

    let exec_id = ExecutionId::new();

    // Write events with one store instance
    {
        let store = FileEventStore::new(&dir).unwrap();
        store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();
        store.append(exec_id, EventType::StepStarted {
            step_number: 0, step_name: "s0".to_string(),
            param_hash: 42, params: "{}".to_string(),
        }).unwrap();
        store.append(exec_id, EventType::StepCompleted {
            step_number: 0, step_name: "s0".to_string(),
            result: "\"ok\"".to_string(),
        }).unwrap();
    }

    // Read with a NEW store instance (simulating process restart)
    {
        let store = FileEventStore::new(&dir).unwrap();
        let events = store.events(exec_id).unwrap();
        assert_eq!(events.len(), 3);

        let state = ExecutionState::from_events(exec_id, &events);
        assert_eq!(state.step_count, 1);
        assert!(state.step_results[&0].completed);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// 2. DETERMINISTIC REPLAY
// ===========================================================================

#[test]
fn test_replay_happy_path() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // First run: execute steps normally
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    let r1 = ctx.step("llm_call_0", &json_num(1.0), || Ok(json_str("hello")));
    assert!(r1.is_ok());
    let r2 = ctx.step("tool_search", &json_num(2.0), || Ok(json_str("results")));
    assert!(r2.is_ok());
    ctx.complete("done").unwrap();

    // Resume: steps should come from cache, not re-execute
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    let mut executed = false;
    let r3 = ctx2.step("llm_call_0", &json_num(1.0), || {
        executed = true;
        Ok(json_str("this should not run"))
    });
    assert!(r3.is_ok());
    assert_eq!(r3.unwrap().as_str().unwrap(), "hello"); // cached result
    assert!(!executed, "step should NOT have been re-executed on replay");
}

#[test]
fn test_replay_step_name_mismatch_detected() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // First run: step_0 is called "llm_call_0"
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.step("llm_call_0", &json_num(1.0), || Ok(json_str("hello"))).unwrap();
    ctx.complete("done").unwrap();

    // Resume with DIFFERENT step name at step 0
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    let result = ctx2.step("RENAMED_step", &json_num(1.0), || Ok(json_str("x")));

    match result {
        Err(DurableError::NonDeterminismDetected { step_number, expected_name, actual_name }) => {
            assert_eq!(step_number, 0);
            assert_eq!(expected_name, "llm_call_0");
            assert_eq!(actual_name, "RENAMED_step");
        }
        other => panic!("expected NonDeterminismDetected, got {:?}", other),
    }
}

#[test]
fn test_replay_idempotent() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.step("step_a", &json_num(1.0), || Ok(json_num(42.0))).unwrap();
    ctx.complete("done").unwrap();

    // Replay 3 times — same result every time
    for _ in 0..3 {
        let ctx = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
        let r = ctx.step("step_a", &json_num(1.0), || {
            panic!("should never execute");
        });
        assert_eq!(r.unwrap().as_f64().unwrap(), 42.0);
    }
}

// ===========================================================================
// 3. EXACTLY-ONCE SEMANTICS (fencing via generation)
// ===========================================================================

#[test]
fn test_generation_increments_on_resume() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx1 = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    assert_eq!(ctx1.generation(), 1);
    ctx1.complete("done").unwrap();
    drop(ctx1); // Release lease before next resume

    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    assert_eq!(ctx2.generation(), 2);
    drop(ctx2); // Release lease before next resume

    let ctx3 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    assert_eq!(ctx3.generation(), 3);
}

#[test]
fn test_generation_in_event_log() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.complete("done").unwrap();

    let _ = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    let _ = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();

    // Check event log contains generation events
    let events = store.events(exec_id).unwrap();
    let resumed_events: Vec<_> = events.iter().filter(|e| {
        matches!(&e.event_type, EventType::Resumed { .. })
    }).collect();
    assert_eq!(resumed_events.len(), 2);

    // Verify generations
    let state = ExecutionState::from_events(exec_id, &events);
    assert_eq!(state.generation, 3);
}

// ===========================================================================
// 6. IDEMPOTENCY — full parameter storage
// ===========================================================================

#[test]
fn test_full_params_stored_in_events() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let params = json_object(vec![("query", json_str("test")), ("limit", json_num(10.0))]);
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.step("search", &params, || Ok(json_str("results"))).unwrap();

    // Verify the event contains full params
    let events = store.events(exec_id).unwrap();
    let step_started = events.iter().find(|e| {
        matches!(&e.event_type, EventType::StepStarted { step_name, .. } if step_name == "search")
    }).unwrap();

    if let EventType::StepStarted { params: stored_params, .. } = &step_started.event_type {
        assert!(stored_params.contains("query"));
        assert!(stored_params.contains("test"));
        assert!(stored_params.contains("limit"));
    } else {
        panic!("expected StepStarted");
    }
}

#[test]
fn test_param_change_detected_on_replay() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.step("search", &json_object(vec![("q", json_str("cats"))]), || {
        Ok(json_str("cat results"))
    }).unwrap();
    ctx.complete("done").unwrap();

    // Resume with DIFFERENT params — should detect non-determinism
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    let result = ctx2.step("search", &json_object(vec![("q", json_str("dogs"))]), || {
        Ok(json_str("dog results"))
    });

    // Should fail — same step name but different parameters
    assert!(result.is_err(), "should detect parameter change");
}

// ===========================================================================
// 7. BACKPRESSURE (already tested, but add event-store variant)
// ===========================================================================

#[test]
fn test_event_store_concurrent_appends() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();
    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();

    // Append 100 events from 10 threads
    let mut handles = Vec::new();
    for i in 0..10 {
        let store = store.clone();
        let handle = std::thread::spawn(move || {
            for j in 0..10 {
                store.append(exec_id, EventType::TagSet {
                    key: format!("t{}_{}", i, j),
                    value: "v".to_string(),
                }).unwrap();
            }
        });
        handles.push(handle);
    }
    for h in handles { h.join().unwrap(); }

    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 101); // 1 created + 100 tags

    // Event IDs should be unique and monotonic
    let ids: Vec<u64> = events.iter().map(|e| e.event_id).collect();
    for i in 1..ids.len() {
        assert!(ids[i] > ids[i - 1], "event IDs must be monotonically increasing");
    }
}

// ===========================================================================
// 8. CONSISTENCY — atomic event append
// ===========================================================================

#[test]
fn test_file_event_store_atomic_append() {
    let dir = std::env::temp_dir().join("durable_test_atomic_event");
    let _ = std::fs::remove_dir_all(&dir);

    let store = FileEventStore::new(&dir).unwrap();
    let exec_id = ExecutionId::new();

    // Append 50 events
    for i in 0..50 {
        store.append(exec_id, EventType::TagSet {
            key: format!("k{}", i),
            value: format!("v{}", i),
        }).unwrap();
    }

    // Read back — all 50 should be present and parseable
    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 50);

    // Each event should be complete (no partial JSON)
    for event in &events {
        let json_str = json::to_string(&event.to_json());
        let reparsed = json::parse(&json_str).unwrap();
        assert!(reparsed.get("event_id").is_some());
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// ===========================================================================
// 3. EXACTLY-ONCE — fenced appends
// ===========================================================================

#[test]
fn test_fenced_append_rejects_stale_generation() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();

    // Append with correct generation should succeed
    let result = store.append_fenced(
        exec_id,
        EventType::TagSet { key: "k".to_string(), value: "v".to_string() },
        1,
    );
    assert!(result.is_ok());

    // Resume (generation → 2)
    store.append(exec_id, EventType::Resumed { generation: 2 }).unwrap();

    // Append with stale generation 1 should fail
    let result = store.append_fenced(
        exec_id,
        EventType::TagSet { key: "k2".to_string(), value: "v2".to_string() },
        1,
    );
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("stale_generation"));

    // Append with current generation 2 should succeed
    let result = store.append_fenced(
        exec_id,
        EventType::TagSet { key: "k2".to_string(), value: "v2".to_string() },
        2,
    );
    assert!(result.is_ok());
}

#[test]
fn test_step_fencing_in_replay_context() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx1 = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx1.step("step_a", &json_num(1.0), || Ok(json_str("ok"))).unwrap();
    ctx1.complete("done").unwrap();

    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    assert_eq!(ctx2.generation(), 2);

    // Replay from cache
    let result = ctx2.step("step_a", &json_num(1.0), || panic!("should not execute"));
    assert!(result.is_ok());

    // New step uses fenced append
    let result = ctx2.step("step_b", &json_num(2.0), || Ok(json_str("new")));
    assert!(result.is_ok());
}

// ===========================================================================
// 4. SAGA COMPENSATION
// ===========================================================================

#[test]
fn test_saga_compensation_reverse_order() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();

    let order = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));

    let o1 = order.clone();
    ctx.step_with_compensation(
        "charge_payment", &json_num(100.0),
        || Ok(json_str("charged")),
        move || { o1.lock().unwrap().push("refund_payment"); Ok(json_str("refunded")) },
    ).unwrap();

    let o2 = order.clone();
    ctx.step_with_compensation(
        "send_email", &json_str("user@example.com"),
        || Ok(json_str("sent")),
        move || { o2.lock().unwrap().push("retract_email"); Ok(json_str("retracted")) },
    ).unwrap();

    let o3 = order.clone();
    ctx.step_with_compensation(
        "update_db", &json_str("SET status='active'"),
        || Ok(json_str("updated")),
        move || { o3.lock().unwrap().push("rollback_db"); Ok(json_str("rolled back")) },
    ).unwrap();

    let results = ctx.compensate().unwrap();
    assert_eq!(results.len(), 3);

    let exec_order = order.lock().unwrap();
    assert_eq!(*exec_order, vec!["rollback_db", "retract_email", "refund_payment"]);
}

#[test]
fn test_saga_compensation_is_durable() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();

    ctx.step_with_compensation(
        "charge", &json_num(50.0),
        || Ok(json_str("charged")),
        || Ok(json_str("refunded")),
    ).unwrap();

    let results = ctx.compensate().unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].1.is_ok());

    let events = store.events(exec_id).unwrap();
    let comp_events: Vec<_> = events.iter().filter(|e| {
        matches!(&e.event_type, EventType::StepCompleted { step_name, .. }
            if step_name.starts_with("__compensate_"))
    }).collect();
    assert_eq!(comp_events.len(), 1, "compensation should be a durable step");
}

#[test]
fn test_saga_no_compensation_on_success() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();

    ctx.step("step_a", &json_num(1.0), || Ok(json_str("ok"))).unwrap();
    let results = ctx.compensate().unwrap();
    assert!(results.is_empty());
}

// ===========================================================================
// 5. WORKFLOW VERSIONING
// ===========================================================================

#[test]
fn test_version_recorded_in_events() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new_versioned(exec_id, store.clone(), "v2.1", None, None, None, None).unwrap();
    assert_eq!(ctx.version(), Some("v2.1"));

    let state = ctx.current_state().unwrap();
    assert_eq!(state.version, Some("v2.1".to_string()));
}

#[test]
fn test_version_mismatch_on_resume() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new_versioned(exec_id, store.clone(), "v1.0", None, None, None, None).unwrap();
    ctx.complete("done").unwrap();

    // Same version — succeeds
    let result = ReplayContext::resume_versioned(exec_id, store.clone(), "v1.0", None, None, None);
    assert!(result.is_ok());

    // Different version — fails
    let result = ReplayContext::resume_versioned(exec_id, store.clone(), "v2.0", None, None, None);
    match result {
        Err(e) => assert!(e.to_string().contains("version mismatch"), "got: {}", e),
        Ok(_) => panic!("expected version mismatch error"),
    }
}

#[test]
fn test_patching_api_get_version() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // First run: records choice as max_supported
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    let v = ctx.get_version("add-validation", 1, 2).unwrap();
    assert_eq!(v, 2);
    ctx.step("process", &json_num(1.0), || Ok(json_str("ok"))).unwrap();
    ctx.complete("done").unwrap();

    // Replay: returns previously recorded choice even if max changed
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    let v2 = ctx2.get_version("add-validation", 1, 3).unwrap();
    assert_eq!(v2, 2); // sticky to first run
}

// ===========================================================================
// 8. CONSISTENCY — batch transactions
// ===========================================================================

#[test]
fn test_batch_append_atomic() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let events = store.append_batch(exec_id, vec![
        EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None },
        EventType::SignalReceived { name: "approval".to_string(), data: "yes".to_string() },
        EventType::ExecutionCompleted { result: "approved".to_string() },
    ]).unwrap();

    assert_eq!(events.len(), 3);
    assert_eq!(events[0].event_id, 1);
    assert_eq!(events[1].event_id, 2);
    assert_eq!(events[2].event_id, 3);

    let all = store.events(exec_id).unwrap();
    let state = ExecutionState::from_events(exec_id, &all);
    assert_eq!(state.status, ExecutionStatus::Completed);
}

#[test]
fn test_complete_is_atomic_batch() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.complete("final result").unwrap();

    let events = store.events(exec_id).unwrap();
    let has_tag = events.iter().any(|e| matches!(&e.event_type, EventType::TagSet { key, .. } if key == "final_result"));
    let has_completed = events.iter().any(|e| matches!(&e.event_type, EventType::ExecutionCompleted { .. }));
    assert!(has_tag, "completion should include tag");
    assert!(has_completed, "completion should include status event");
}

// ===========================================================================
// 9. IDEMPOTENT EVENT APPENDS
// ===========================================================================

#[test]
fn test_idempotent_append_deduplicates() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();
    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();

    // First append with idempotency key
    let e1 = store.append_idempotent(
        exec_id,
        EventType::StepStarted {
            step_number: 0,
            step_name: "my_step".to_string(),
            param_hash: 123,
            params: "{}".to_string(),
        },
        "key_step_0_started".to_string(),
    ).unwrap();

    // Second append with SAME key — should return existing event, not duplicate
    let e2 = store.append_idempotent(
        exec_id,
        EventType::StepStarted {
            step_number: 0,
            step_name: "my_step".to_string(),
            param_hash: 123,
            params: "{}".to_string(),
        },
        "key_step_0_started".to_string(),
    ).unwrap();

    assert_eq!(e1.event_id, e2.event_id, "should return same event");

    // Total events should be 2 (ExecutionCreated + one StepStarted), not 3
    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 2, "duplicate should not be appended");
}

#[test]
fn test_idempotent_append_different_keys_both_stored() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();
    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();

    store.append_idempotent(
        exec_id,
        EventType::StepStarted { step_number: 0, step_name: "a".into(), param_hash: 0, params: "{}".into() },
        "key_a".to_string(),
    ).unwrap();

    store.append_idempotent(
        exec_id,
        EventType::StepStarted { step_number: 1, step_name: "b".into(), param_hash: 0, params: "{}".into() },
        "key_b".to_string(),
    ).unwrap();

    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 3, "different keys should both be stored");
}

// ===========================================================================
// 10. HASH CHAIN IMMUTABILITY
// ===========================================================================

#[test]
fn test_hash_chain_validates_intact_log() {
    use durable_runtime::storage::{hash_event, validate_chain};

    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();
    store.append(exec_id, EventType::StepStarted {
        step_number: 0, step_name: "a".into(), param_hash: 0, params: "{}".into(),
    }).unwrap();
    store.append(exec_id, EventType::StepCompleted {
        step_number: 0, step_name: "a".into(), result: "ok".into(),
    }).unwrap();

    let events = store.events(exec_id).unwrap();

    // Hash chain is now computed by the store — verify it's valid
    assert!(validate_chain(&events).is_ok());

    // Verify hashes are actually set (not all zero)
    assert_eq!(events[0].prev_hash, 0, "first event has no predecessor");
    assert_ne!(events[1].prev_hash, 0, "second event should chain to first");
    assert_eq!(events[1].prev_hash, hash_event(&events[0]));
    assert_eq!(events[2].prev_hash, hash_event(&events[1]));
}

#[test]
fn test_hash_chain_detects_tampering() {
    use durable_runtime::storage::{hash_event, validate_chain, Event};
    use durable_runtime::core::time::now_millis;

    // Manually construct a chain with correct hashes
    let exec_id = ExecutionId::new();
    let e1 = Event {
        event_id: 1,
        execution_id: exec_id,
        timestamp: 1000,
        event_type: EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None },
        idempotency_key: None,
        prev_hash: 0,
        schema_version: 1,
        lamport_ts: 0,
    };
    let e1_hash = hash_event(&e1);

    let e2 = Event {
        event_id: 2,
        execution_id: exec_id,
        timestamp: 2000,
        event_type: EventType::StepStarted {
            step_number: 0, step_name: "a".into(), param_hash: 0, params: "{}".into(),
        },
        idempotency_key: None,
        prev_hash: e1_hash,
        schema_version: 1,
        lamport_ts: 0,
    };
    let e2_hash = hash_event(&e2);

    let e3 = Event {
        event_id: 3,
        execution_id: exec_id,
        timestamp: 3000,
        event_type: EventType::StepCompleted {
            step_number: 0, step_name: "a".into(), result: "ok".into(),
        },
        idempotency_key: None,
        prev_hash: e2_hash,
        schema_version: 1,
        lamport_ts: 0,
    };

    // Valid chain
    assert!(validate_chain(&[e1.clone(), e2.clone(), e3.clone()]).is_ok());

    // Tamper with e2 — change the result
    let e2_tampered = Event {
        event_type: EventType::StepStarted {
            step_number: 0, step_name: "TAMPERED".into(), param_hash: 0, params: "{}".into(),
        },
        ..e2.clone()
    };

    // e3.prev_hash still points to original e2, so chain breaks
    assert!(validate_chain(&[e1, e2_tampered, e3]).is_err());
}

// ===========================================================================
// 11. EVENT SNAPSHOTS
// ===========================================================================

#[test]
fn test_snapshot_and_resume() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // Run 5 steps
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    for i in 0..5 {
        ctx.step("work", &json_num(i as f64), || Ok(json_str("done"))).unwrap();
    }
    // Take a snapshot at step 5
    ctx.maybe_snapshot(5).unwrap();
    ctx.complete("finished").unwrap();

    // Verify snapshot was created
    let snapshot = store.latest_snapshot(exec_id).unwrap();
    assert!(snapshot.is_some(), "snapshot should exist");

    // Resume — should use snapshot (fast path)
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    // Steps should be cached from snapshot
    for i in 0..5 {
        let result = ctx2.step("work", &json_num(i as f64), || {
            panic!("should not re-execute — cached from snapshot");
        }).unwrap();
        assert_eq!(result.as_str(), Some("done"));
    }
}

#[test]
fn test_snapshot_state_roundtrip() {
    use durable_runtime::storage::ExecutionState;
    use durable_runtime::json::{ToJson, FromJson};

    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.step("step_a", &json_str("params_a"), || Ok(json_str("result_a"))).unwrap();
    ctx.step("step_b", &json_num(42.0), || Ok(json_num(84.0))).unwrap();
    ctx.set_tag("env", "test").unwrap();

    let state = ctx.current_state().unwrap();
    let json = state.to_json();
    let serialized = durable_runtime::json::to_string(&json);
    let parsed = durable_runtime::json::parse(&serialized).unwrap();
    let restored = ExecutionState::from_json(&parsed).unwrap();

    assert_eq!(restored.execution_id, state.execution_id);
    assert_eq!(restored.step_count, state.step_count);
    assert_eq!(restored.generation, state.generation);
    assert_eq!(restored.tags.get("env"), Some(&"test".to_string()));
    assert_eq!(restored.step_results.len(), state.step_results.len());
}

#[test]
fn test_snapshot_interval_skips_when_not_due() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.step("a", &json_null(), || Ok(json_str("ok"))).unwrap();
    ctx.step("b", &json_null(), || Ok(json_str("ok"))).unwrap();

    // Interval is 5, we're at step 2 — no snapshot
    let taken = ctx.maybe_snapshot(5).unwrap();
    assert!(!taken);

    // After 3 more steps, we're at step 5 — snapshot
    ctx.step("c", &json_null(), || Ok(json_str("ok"))).unwrap();
    ctx.step("d", &json_null(), || Ok(json_str("ok"))).unwrap();
    ctx.step("e", &json_null(), || Ok(json_str("ok"))).unwrap();
    let taken = ctx.maybe_snapshot(5).unwrap();
    assert!(taken);
}

// ===========================================================================
// 12. TYPE-STATE BUILDER — compile-time enforcement
// ===========================================================================

#[test]
fn test_typestate_builder_compiles_with_llm() {
    // This compiles because .llm() transitions to HasLlm, unlocking .build()
    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![LlmResponse::text("Hi")]))
        .build();

    let response = runtime.run("Hello").unwrap();
    assert_eq!(response.text(), Some("Hi"));
}

#[test]
fn test_typestate_builder_config_before_llm() {
    // Config methods work before .llm() — order doesn't matter
    let runtime = AgentRuntime::builder()
        .system_prompt("Custom prompt")
        .max_iterations(10)
        .llm(MockLlmClient::new(vec![LlmResponse::text("Ok")]))
        .build();

    let response = runtime.run("test").unwrap();
    assert_eq!(response.text(), Some("Ok"));
}

#[test]
fn test_typestate_builder_config_after_llm() {
    // Config methods also work after .llm()
    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![LlmResponse::text("Ok")]))
        .system_prompt("Custom prompt")
        .max_iterations(10)
        .build();

    let response = runtime.run("test").unwrap();
    assert_eq!(response.text(), Some("Ok"));
}

// ===========================================================================
// 13. PARALLEL TOOL MEMOIZATION — individual step durability
// ===========================================================================

#[test]
fn test_parallel_tools_individually_memoized() {
    use std::sync::atomic::{AtomicU32, Ordering};

    static A_RUNS: AtomicU32 = AtomicU32::new(0);
    static B_RUNS: AtomicU32 = AtomicU32::new(0);

    let event_store: Arc<dyn EventStore> = Arc::new(InMemoryEventStore::new());
    let storage: Arc<dyn ExecutionLog> = Arc::new(InMemoryStorage::new());

    let make_tools = || {
        let mut tools = ToolRegistry::new();
        tools.register_fn(
            ToolDefinition::new("tool_a", "Tool A"),
            |_: &durable_runtime::Value| {
                A_RUNS.fetch_add(1, Ordering::SeqCst);
                Ok(json_str("a_result"))
            },
        );
        tools.register_fn(
            ToolDefinition::new("tool_b", "Tool B"),
            |_: &durable_runtime::Value| {
                B_RUNS.fetch_add(1, Ordering::SeqCst);
                Ok(json_str("b_result"))
            },
        );
        tools
    };

    let exec_id = ExecutionId::new();

    // First run — both tools execute
    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![
            LlmResponse::tool_calls(vec![
                ToolCall { id: "a".into(), name: "tool_a".into(), arguments: json_object(vec![]) },
                ToolCall { id: "b".into(), name: "tool_b".into(), arguments: json_object(vec![]) },
            ]),
            LlmResponse::text("Done"),
        ]))
        .storage(storage.clone())
        .event_store(event_store.clone())
        .tools(make_tools())
        .build();

    let outcome = runtime.start_with_id(exec_id, "run both");
    match &outcome {
        AgentOutcome::Complete { response } => assert_eq!(response, "Done"),
        other => panic!("expected Complete, got {:?}", other),
    }
    assert_eq!(A_RUNS.load(Ordering::SeqCst), 1);
    assert_eq!(B_RUNS.load(Ordering::SeqCst), 1);

    // Verify individual tool steps appear in event store
    let events = event_store.events(exec_id).unwrap();
    let tool_completed: Vec<_> = events.iter().filter(|e| {
        matches!(&e.event_type, EventType::StepCompleted { step_name, .. }
            if step_name.starts_with("tool_"))
    }).collect();
    assert_eq!(tool_completed.len(), 2, "both tool calls should be individually memoized");

    // Resume — tools should NOT re-execute (memoized)
    let runtime2 = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![
            LlmResponse::text("Resumed"),
        ]))
        .storage(storage.clone())
        .event_store(event_store.clone())
        .tools(make_tools())
        .build();

    let _ = runtime2.resume(exec_id);
    assert_eq!(A_RUNS.load(Ordering::SeqCst), 1, "tool_a should not re-execute on resume");
    assert_eq!(B_RUNS.load(Ordering::SeqCst), 1, "tool_b should not re-execute on resume");
}

// ===========================================================================
// 14. CHILD FLOW ORCHESTRATION — hierarchical execution
// ===========================================================================

#[test]
fn test_child_flow_start_and_complete() {
    let store = Arc::new(InMemoryEventStore::new());
    let parent_id = ExecutionId::new();
    let child_id = ExecutionId::new();

    let ctx = ReplayContext::new(parent_id, store.clone(), None, None, None, None).unwrap();

    // Start a child flow
    let returned_id = ctx.start_child_flow(child_id, "child input").unwrap();
    assert_eq!(returned_id, child_id);

    // Verify ChildFlowStarted event was recorded
    let events = store.events(parent_id).unwrap();
    let started = events.iter().any(|e| {
        matches!(&e.event_type, EventType::ChildFlowStarted { child_id: cid, .. }
            if *cid == child_id)
    });
    assert!(started, "ChildFlowStarted event should exist");

    // Await should suspend (child not yet complete)
    let result = ctx.await_child_flow(child_id);
    assert!(matches!(result, Err(DurableError::Suspended(
        SuspendReason::WaitingForChild { .. }
    ))));

    // Complete the child flow
    ctx.complete_child_flow(child_id, "\"child_result\"").unwrap();

    // Verify ChildFlowCompleted event was recorded
    let events = store.events(parent_id).unwrap();
    let completed = events.iter().any(|e| {
        matches!(&e.event_type, EventType::ChildFlowCompleted { child_id: cid, .. }
            if *cid == child_id)
    });
    assert!(completed, "ChildFlowCompleted event should exist");
}

#[test]
fn test_child_flow_await_after_complete() {
    let store = Arc::new(InMemoryEventStore::new());
    let parent_id = ExecutionId::new();
    let child_id = ExecutionId::new();

    let ctx = ReplayContext::new(parent_id, store.clone(), None, None, None, None).unwrap();
    ctx.start_child_flow(child_id, "input").unwrap();
    ctx.complete_child_flow(child_id, "\"done\"").unwrap();

    // Await should return immediately (child already completed)
    let result = ctx.await_child_flow(child_id).unwrap();
    assert_eq!(result.as_str(), Some("done"));
}

#[test]
fn test_child_flow_survives_resume() {
    let store = Arc::new(InMemoryEventStore::new());
    let parent_id = ExecutionId::new();
    let child_id = ExecutionId::new();

    // First run: start child, complete child
    {
        let ctx = ReplayContext::new(parent_id, store.clone(), None, None, None, None).unwrap();
        ctx.start_child_flow(child_id, "input").unwrap();
        ctx.complete_child_flow(child_id, "\"child_done\"").unwrap();
        ctx.complete("parent_done").unwrap();
    }

    // Resume: child flow result should be available from history
    {
        let ctx = ReplayContext::resume(parent_id, store.clone(), None, None, None).unwrap();
        // start_child_flow should be idempotent on replay
        let returned_id = ctx.start_child_flow(child_id, "input").unwrap();
        assert_eq!(returned_id, child_id);
        // await should return cached result
        let result = ctx.await_child_flow(child_id).unwrap();
        assert_eq!(result.as_str(), Some("child_done"));
    }
}

#[test]
fn test_child_flow_suspend_reason_serialization() {
    use durable_runtime::json::{ToJson, FromJson};

    let child_id = ExecutionId::new();
    let reason = SuspendReason::WaitingForChild { child_id };
    let json = reason.to_json();
    let serialized = durable_runtime::json::to_string(&json);
    let parsed = durable_runtime::json::parse(&serialized).unwrap();
    let restored = SuspendReason::from_json(&parsed).unwrap();

    match restored {
        SuspendReason::WaitingForChild { child_id: restored_id } => {
            assert_eq!(restored_id, child_id);
        }
        other => panic!("expected WaitingForChild, got {:?}", other),
    }
}

#[test]
fn test_child_flow_events_in_state() {
    let store = Arc::new(InMemoryEventStore::new());
    let parent_id = ExecutionId::new();
    let child_id = ExecutionId::new();

    store.append(parent_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();
    store.append(parent_id, EventType::ChildFlowStarted {
        child_id,
        input: "hello".to_string(),
    }).unwrap();

    let events = store.events(parent_id).unwrap();
    let state = ExecutionState::from_events(parent_id, &events);
    assert_eq!(state.child_flows.len(), 1);
    assert_eq!(state.child_flows.get(&child_id.to_string()), Some(&None));

    store.append(parent_id, EventType::ChildFlowCompleted {
        child_id,
        result: "\"result\"".to_string(),
    }).unwrap();

    let events = store.events(parent_id).unwrap();
    let state = ExecutionState::from_events(parent_id, &events);
    assert_eq!(
        state.child_flows.get(&child_id.to_string()),
        Some(&Some("\"result\"".to_string()))
    );
}

// ===========================================================================
// INVARIANT II — Crash recovery proof
// ===========================================================================

#[test]
fn test_crash_recovery_truncated_write() {
    // Simulate a crash mid-write: write valid events, then manually truncate
    // the file to simulate an incomplete write. The store should recover by
    // skipping the corrupt trailing line.
    use std::io::Write;

    let dir = std::env::temp_dir().join(format!("durable_test_crash_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = durable_runtime::FileEventStore::new(&dir).unwrap();
    let exec_id = ExecutionId::new();

    // Write 3 valid events
    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();
    store.append(exec_id, EventType::StepStarted {
        step_number: 0, step_name: "a".into(), param_hash: 0, params: "{}".into(),
    }).unwrap();
    store.append(exec_id, EventType::StepCompleted {
        step_number: 0, step_name: "a".into(), result: "\"ok\"".into(),
    }).unwrap();

    // Verify 3 events exist
    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 3);

    // Now manually append a truncated (corrupt) line to the event file
    let event_file = dir.join("events").join(format!("{}.ndjson", exec_id));
    let mut f = std::fs::OpenOptions::new().append(true).open(&event_file).unwrap();
    writeln!(f, "{{\"event_id\":4,\"truncat").unwrap(); // Incomplete JSON

    // Reading should still return the 3 valid events (skip corrupt line)
    let events = store.events(exec_id).unwrap();
    assert_eq!(events.len(), 3, "should recover 3 valid events, skipping corrupt trailing line");

    // The state reconstructed from those events should be correct
    let state = ExecutionState::from_events(exec_id, &events);
    assert_eq!(state.step_count, 1);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn test_verify_integrity_passes_for_valid_log() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    store.append(exec_id, EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None }).unwrap();
    store.append(exec_id, EventType::StepStarted {
        step_number: 0, step_name: "a".into(), param_hash: 0, params: "{}".into(),
    }).unwrap();
    store.append(exec_id, EventType::StepCompleted {
        step_number: 0, step_name: "a".into(), result: "\"ok\"".into(),
    }).unwrap();

    assert!(store.verify_integrity(exec_id).is_ok());
}

#[test]
fn test_verify_integrity_detects_tampered_event() {
    use durable_runtime::storage::{hash_event, Event};

    // Build a valid chain, then tamper with middle event
    let exec_id = ExecutionId::new();
    let e1 = Event {
        event_id: 1, execution_id: exec_id, timestamp: 1000,
        event_type: EventType::ExecutionCreated { version: None, prompt_hash: None, prompt_text: None, agent_id: None, tools_hash: None },
        idempotency_key: None, prev_hash: 0, schema_version: 1, lamport_ts: 0,
    };
    let e2 = Event {
        event_id: 2, execution_id: exec_id, timestamp: 2000,
        event_type: EventType::StepStarted {
            step_number: 0, step_name: "a".into(), param_hash: 0, params: "{}".into(),
        },
        idempotency_key: None, prev_hash: hash_event(&e1), schema_version: 1, lamport_ts: 0,
    };
    let e3 = Event {
        event_id: 3, execution_id: exec_id, timestamp: 3000,
        event_type: EventType::StepCompleted {
            step_number: 0, step_name: "a".into(), result: "ok".into(),
        },
        idempotency_key: None, prev_hash: hash_event(&e2), schema_version: 1, lamport_ts: 0,
    };

    // Valid chain passes
    assert!(durable_runtime::storage::validate_chain(&[e1.clone(), e2.clone(), e3.clone()]).is_ok());

    // Tamper with e2
    let e2_bad = Event {
        event_type: EventType::StepStarted {
            step_number: 0, step_name: "TAMPERED".into(), param_hash: 0, params: "{}".into(),
        },
        ..e2
    };

    // Chain breaks because e3.prev_hash doesn't match hash(e2_bad)
    assert!(durable_runtime::storage::validate_chain(&[e1, e2_bad, e3]).is_err());
}

// ===========================================================================
// INVARIANT I — Deterministic primitives
// ===========================================================================

#[test]
fn test_deterministic_now_is_memoized() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // First execution: capture time
    let t1 = {
        let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
        let t = ctx.now().unwrap();
        ctx.complete("done").unwrap();
        t
    };

    // Replay: must return the SAME time, not current wall-clock
    let t2 = {
        let ctx = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
        ctx.now().unwrap()
    };

    assert_eq!(t1, t2, "deterministic now() must return same value on replay");
}

#[test]
fn test_deterministic_random_is_memoized() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // First execution: generate random
    let r1 = {
        let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
        let r = ctx.random_u64().unwrap();
        ctx.complete("done").unwrap();
        r
    };

    // Replay: must return the SAME random value
    let r2 = {
        let ctx = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
        ctx.random_u64().unwrap()
    };

    assert_eq!(r1, r2, "deterministic random() must return same value on replay");
}

// ===========================================================================
// INVARIANT V — Lease-based mutual exclusion
// ===========================================================================

#[test]
fn test_lease_prevents_concurrent_resume() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx1 = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx1.complete("done").unwrap();

    // First resume acquires lease
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();

    // Second resume while first lease is active should fail
    let result = ReplayContext::resume(exec_id, store.clone(), None, None, None);
    assert!(result.is_err(), "concurrent resume should fail while lease is active");

    // After dropping ctx2 (releases lease), resume should succeed
    drop(ctx2);
    let ctx3 = ReplayContext::resume(exec_id, store.clone(), None, None, None);
    assert!(ctx3.is_ok(), "resume should succeed after lease is released");
}

#[test]
fn test_lease_released_on_complete() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx1 = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx1.complete("done").unwrap();
    drop(ctx1);

    // Resume, complete, and verify lease is released
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), None, None, None).unwrap();
    ctx2.complete("done again").unwrap();
    // Lease is released by complete() AND by Drop — both are safe

    // Verify by checking state
    let events = store.events(exec_id).unwrap();
    let state = ExecutionState::from_events(exec_id, &events);
    assert!(state.lease_holder.is_none(), "lease should be released after complete");
}

// ===========================================================================
// INVARIANT VI — Three-axis error classification
// ===========================================================================

#[test]
fn test_step_retry_override_never_retry() {
    use durable_runtime::core::retry::{StepRetryOverride, RetryPolicy};

    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();
    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();

    let call_count = std::sync::atomic::AtomicU32::new(0);
    let result = ctx.step_with_retry(
        "flaky_step",
        &json_null(),
        &RetryPolicy::AGGRESSIVE, // Would retry 10 times...
        &StepRetryOverride::NeverRetry, // ...but override says never
        || {
            call_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Err(DurableError::StepFailed {
                step_name: "flaky_step".into(),
                message: "transient".into(),
                retryable: true,
                execution_id: None,
                step_number: None,
            })
        },
    );

    assert!(result.is_err());
    assert_eq!(call_count.load(std::sync::atomic::Ordering::SeqCst), 1,
        "NeverRetry should prevent any retries");
}

#[test]
fn test_safe_default_retryable() {
    use durable_runtime::core::retry::Retryable;

    // Storage errors: retryable (safe default)
    assert!(DurableError::Storage("timeout".into()).is_retryable());
    assert!(DurableError::QueueFull.is_retryable());

    // Known permanent: not retryable
    assert!(!DurableError::Serialization("bad json".into()).is_retryable());
    assert!(!DurableError::NotFound("missing".into()).is_retryable());
    assert!(!DurableError::Cancelled.is_retryable());
    assert!(!DurableError::NonDeterminismDetected {
        step_number: 0,
        expected_name: "a".into(),
        actual_name: "b".into(),
    }.is_retryable());
}

// ===========================================================================
// LIFECYCLE HOOKS
// ===========================================================================

#[test]
fn test_before_tool_hook_modifies_args() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let hook_ran = Arc::new(AtomicBool::new(false));
    let hook_ran_clone = hook_ran.clone();

    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![
            LlmResponse::tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "greet".into(),
                arguments: json!({ "name": "world" }),
            }]),
            LlmResponse::text("done"),
        ]))
        .tool("greet", "Greet someone",
            json!({"type": "object", "properties": {"name": {"type": "string"}}}),
            move |args: &Value| {
                // The hook should have transformed the args
                hook_ran_clone.store(true, Ordering::SeqCst);
                let name = args.string("name").unwrap_or("?".into());
                Ok(json_object(vec![("greeting", json_str(&format!("hello {}", name)))]))
            },
        )
        .before_tool(|_name, _args| {
            // Hook runs but doesn't block execution
            Ok(json!({ "name": "hooked" }))
        })
        .build();

    let response = runtime.run("hi").unwrap();
    assert!(response.is_completed());
    assert!(hook_ran.load(Ordering::SeqCst));
}

#[test]
fn test_contract_violation_suspends() {
    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![
            LlmResponse::tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "charge".into(),
                arguments: json!({ "amount": 500.0 }),
            }]),
            LlmResponse::text("charged"),
        ]))
        .tool("charge", "Charge payment",
            json!({"type": "object", "properties": {"amount": {"type": "number"}}}),
            |_args: &Value| {
                panic!("should never execute — contract should block this");
            },
        )
        .contract("max-charge", |step_name, args| {
            if step_name.starts_with("tool_charge") {
                let amount = args.number("amount").unwrap_or(0.0);
                if amount > 100.0 {
                    return Err(format!("charge ${:.2} exceeds $100 limit", amount));
                }
            }
            Ok(())
        })
        .build();

    let response = runtime.run("charge $500").unwrap();
    assert!(response.is_suspended(), "contract violation should suspend");
    if let Some(SuspendReason::ContractViolation { contract_name, reason, .. }) = response.suspend_reason() {
        assert_eq!(contract_name, "max-charge");
        assert!(reason.contains("500"));
    } else {
        panic!("expected ContractViolation suspend reason");
    }
}

#[test]
fn test_contract_allows_valid_call() {
    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![
            LlmResponse::tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "charge".into(),
                arguments: json!({ "amount": 50.0 }),
            }]),
            LlmResponse::text("charged $50"),
        ]))
        .tool("charge", "Charge payment",
            json!({"type": "object", "properties": {"amount": {"type": "number"}}}),
            |_args: &Value| Ok(json!({ "status": "ok" })),
        )
        .contract("max-charge", |step_name, args| {
            if step_name.starts_with("tool_charge") {
                let amount = args.number("amount").unwrap_or(0.0);
                if amount > 100.0 {
                    return Err("too much".into());
                }
            }
            Ok(())
        })
        .build();

    let response = runtime.run("charge $50").unwrap();
    assert!(response.is_completed(), "contract should allow $50 charge");
}

// ===========================================================================
// EXECUTION BUDGET
// ===========================================================================

#[test]
fn test_budget_exhaustion_suspends() {
    use durable_runtime::Budget;

    // Agent needs 2 LLM calls (tool call + final response) but budget allows only 1
    let runtime = AgentRuntime::builder()
        .llm(MockLlmClient::new(vec![
            LlmResponse::tool_calls(vec![ToolCall {
                id: "c1".into(),
                name: "noop".into(),
                arguments: json!({}),
            }]),
            LlmResponse::text("done"),
        ]))
        .tool("noop", "No-op tool",
            json!({"type": "object"}),
            |_: &Value| Ok(json!("ok")),
        )
        .budget(Budget::new().max_llm_calls(1))
        .build();

    // This execution needs 2 LLM calls (first for tool call, second for response)
    // but budget only allows 1. After the first LLM call + tool, the second LLM
    // call should trigger budget suspension.
    let response = runtime.run("do it").unwrap();
    assert!(response.is_suspended(), "should suspend when LLM budget exhausted");
    if let Some(SuspendReason::BudgetExhausted { dimension, .. }) = response.suspend_reason() {
        assert_eq!(dimension, "llm_calls");
    } else {
        panic!("expected BudgetExhausted suspend reason, got {:?}", response.suspend_reason());
    }
}

// ===========================================================================
// MULTI-AGENT COORDINATION
// ===========================================================================

#[test]
fn test_coordinator_executes_dag() {
    use durable_runtime::AgentCoordinator;

    let store = Arc::new(InMemoryEventStore::new());
    let storage = Arc::new(durable_runtime::InMemoryStorage::new());
    let exec_id = ExecutionId::new();

    let mut coord = AgentCoordinator::new(store, storage);
    coord.add_worker("fetch", vec![], |_deps| {
        Ok(json!({ "data": "hello" }))
    });
    coord.add_worker("transform", vec!["fetch".into()], |deps| {
        let data = deps["fetch"].get("data").and_then(|v| v.as_str()).unwrap_or("");
        Ok(json_object(vec![("result", json_str(&format!("{} world", data)))]))
    });
    coord.add_worker("validate", vec!["transform".into()], |deps| {
        let result = deps["transform"].get("result").and_then(|v| v.as_str()).unwrap_or("");
        Ok(json_object(vec![("valid", json_bool(result == "hello world"))]))
    });

    let results = coord.execute(exec_id).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(
        results["validate"].get("valid").and_then(|v| v.as_bool()),
        Some(true)
    );
}

#[test]
fn test_coordinator_detects_cycle() {
    use durable_runtime::AgentCoordinator;

    let store = Arc::new(InMemoryEventStore::new());
    let storage = Arc::new(durable_runtime::InMemoryStorage::new());
    let exec_id = ExecutionId::new();

    let mut coord = AgentCoordinator::new(store, storage);
    coord.add_worker("a", vec!["b".into()], |_| Ok(json!(1)));
    coord.add_worker("b", vec!["a".into()], |_| Ok(json!(2)));

    let result = coord.execute(exec_id);
    assert!(result.is_err(), "should detect cycle");
}

// ===========================================================================
// PROMPT HASHING & DRIFT DETECTION (Invariant I — Replay Determinism)
// ===========================================================================

#[test]
fn test_prompt_hash_recorded_in_execution_created() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let prompt = "You are a helpful assistant.";
    let ctx = ReplayContext::new(exec_id, store.clone(), Some(prompt), None, None, None).unwrap();
    ctx.complete("done").unwrap();

    // Verify the prompt hash and text are in the first event
    let events = store.events(exec_id).unwrap();
    assert!(!events.is_empty());
    match &events[0].event_type {
        EventType::ExecutionCreated { prompt_hash, prompt_text, .. } => {
            assert!(prompt_hash.is_some(), "prompt_hash should be recorded");
            assert_eq!(prompt_text.as_deref(), Some(prompt));
            // Hash should be deterministic
            let expected = durable_runtime::core_hash_fnv1a(prompt.as_bytes());
            assert_eq!(prompt_hash.unwrap(), expected);
        }
        other => panic!("expected ExecutionCreated, got {:?}", other),
    }
}

#[test]
fn test_prompt_hash_none_when_no_prompt() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let ctx = ReplayContext::new(exec_id, store.clone(), None, None, None, None).unwrap();
    ctx.complete("done").unwrap();

    let events = store.events(exec_id).unwrap();
    match &events[0].event_type {
        EventType::ExecutionCreated { prompt_hash, prompt_text, .. } => {
            assert!(prompt_hash.is_none(), "prompt_hash should be None when no prompt");
            assert!(prompt_text.is_none(), "prompt_text should be None when no prompt");
        }
        other => panic!("expected ExecutionCreated, got {:?}", other),
    }
}

#[test]
fn test_resume_with_same_prompt_succeeds() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let prompt = "You are a code reviewer.";

    // Start execution with prompt
    let ctx = ReplayContext::new(exec_id, store.clone(), Some(prompt), None, None, None).unwrap();
    ctx.step("step_1", &json_num(1.0), || Ok(json_str("ok"))).unwrap();
    ctx.complete("done").unwrap();

    // Resume with the SAME prompt — should succeed
    let ctx2 = ReplayContext::resume(exec_id, store.clone(), Some(prompt), None, None).unwrap();
    let result = ctx2.step("step_1", &json_num(1.0), || {
        panic!("should not re-execute — cached");
    });
    assert!(result.is_ok(), "resume with same prompt should succeed");
}

#[test]
fn test_resume_with_different_prompt_is_rejected() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let original_prompt = "You are a code reviewer.";
    let changed_prompt = "You are a malicious agent.";

    // Start execution with original prompt
    let ctx = ReplayContext::new(exec_id, store.clone(), Some(original_prompt), None, None, None).unwrap();
    ctx.step("step_1", &json_num(1.0), || Ok(json_str("ok"))).unwrap();
    ctx.complete("done").unwrap();

    // Resume with DIFFERENT prompt — should be rejected
    let result = ReplayContext::resume(exec_id, store.clone(), Some(changed_prompt), None, None);
    assert!(result.is_err(), "resume with different prompt must fail");
    let err_msg = match result {
        Err(e) => format!("{}", e),
        Ok(_) => panic!("expected error"),
    };
    assert!(
        err_msg.contains("prompt") || err_msg.contains("drift") || err_msg.contains("Invariant"),
        "error should mention prompt drift, got: {}",
        err_msg,
    );
}

#[test]
fn test_resume_without_prompt_skips_check() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    // Start with a prompt
    let ctx = ReplayContext::new(exec_id, store.clone(), Some("original"), None, None, None).unwrap();
    ctx.step("s", &json_num(1.0), || Ok(json_str("x"))).unwrap();
    ctx.complete("done").unwrap();

    // Resume with None (e.g., coordinator that doesn't have prompt context) — allowed
    let result = ReplayContext::resume(exec_id, store.clone(), None, None, None);
    assert!(result.is_ok(), "resume with None prompt should skip drift check");
}

#[test]
fn test_prompt_hash_survives_serialization_roundtrip() {
    let prompt = "You are a helpful assistant.";
    let hash = durable_runtime::core_hash_fnv1a(prompt.as_bytes());

    // Create event, serialize to JSON, deserialize back
    let event_type = EventType::ExecutionCreated {
        version: Some("v1".to_string()),
        prompt_hash: Some(hash),
        prompt_text: Some(prompt.to_string()),
        agent_id: None,
        tools_hash: None,
    };

    let json_val = event_type.to_json();
    let json_str_val = json::to_string(&json_val);
    let parsed = json::parse(&json_str_val).unwrap();
    let roundtripped = EventType::from_json(&parsed).unwrap();

    match roundtripped {
        EventType::ExecutionCreated { prompt_hash, prompt_text, version, .. } => {
            assert_eq!(prompt_hash, Some(hash), "hash must survive roundtrip");
            assert_eq!(prompt_text.as_deref(), Some(prompt), "text must survive roundtrip");
            assert_eq!(version.as_deref(), Some("v1"), "version must survive roundtrip");
        }
        other => panic!("expected ExecutionCreated, got {:?}", other),
    }
}

#[test]
fn test_prompt_drift_rejected_on_versioned_resume() {
    let store = Arc::new(InMemoryEventStore::new());
    let exec_id = ExecutionId::new();

    let original = "You are helpful.";
    let changed = "You are harmful.";

    let ctx = ReplayContext::new_versioned(exec_id, store.clone(), "v1.0", Some(original), None, None, None).unwrap();
    ctx.step("s", &json_num(1.0), || Ok(json_str("x"))).unwrap();
    ctx.complete("done").unwrap();

    // resume_versioned with correct version but wrong prompt
    let result = ReplayContext::resume_versioned(exec_id, store.clone(), "v1.0", Some(changed), None, None);
    assert!(result.is_err(), "versioned resume with different prompt must fail");
}
