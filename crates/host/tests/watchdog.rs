//! The LED tile watchdog: generating tiles whose job is gone turn `failed` after a grace
//! period; tiles with a live job (or a queued batch item) are left alone.

use falos_doc::{Actor, Command, NodeKind};
use falos_host::context::NoRenderer;
use falos_host::{Event, Host, HostOptions};
use serde_json::json;

fn tile(id: &str) -> Command {
    Command::CreateNode {
        id: Some(id.into()),
        kind: NodeKind::Generation,
        parent: None,
        index: None,
        name: id.into(),
        props: serde_json::from_value(json!({"x": 0, "y": 0, "w": 100, "h": 100, "status": "running", "model": "fal-ai/flux"})).unwrap(),
        text: None,
    }
}

#[tokio::test]
async fn orphaned_generating_tiles_fail_after_the_grace_period() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(
        &dir.path().join("proj"),
        HostOptions {
            fal_queue_url: "http://127.0.0.1:9".into(),
            fal_key: None,
            fal_session: false,
            renderer: Box::new(NoRenderer),
            home: Some(dir.path().join("home")),
        },
    )
    .unwrap();
    let canvas = host.project().canvases[0].id.clone();
    let me = Actor::agent("a", "agent");
    host.mutate(&canvas, &me.id, None, |c| c.apply(&me, None, &[tile("orphan"), tile("alive")])).unwrap();
    // "alive" has a job in flight.
    host.emit(Event::Job {
        canvas: canvas.clone(),
        node: "alive".into(),
        model: "fal-ai/flux".into(),
        status: "running".into(),
        queue_position: None,
        error: None,
        cost: None,
    });
    let mut rx = host.subscribe();
    assert!(host.sweep_generations(0, 60_000).is_empty(), "first seen: within the grace period");
    assert!(host.sweep_generations(59_999, 60_000).is_empty());
    assert_eq!(host.sweep_generations(60_000, 60_000), vec!["orphan".to_string()]);
    let status = |id: &str| host.read(&canvas, |c| c.node(id).unwrap().props.get("status").cloned()).unwrap();
    assert_eq!(status("orphan"), Some(json!("failed")));
    assert_eq!(status("alive"), Some(json!("running")));
    // The UI hears about it as a failed job.
    let mut saw = false;
    while let Ok(e) = rx.try_recv() {
        if let Event::Job { node, status, .. } = e {
            saw |= node == "orphan" && status == "failed";
        }
    }
    assert!(saw);
    // Failed tiles are never swept again; the job finishing makes "alive" an orphan only
    // after a fresh grace period.
    host.emit(Event::Job {
        canvas: canvas.clone(),
        node: "alive".into(),
        model: "fal-ai/flux".into(),
        status: "done".into(),
        queue_position: None,
        error: None,
        cost: None,
    });
    assert!(host.sweep_generations(70_000, 60_000).is_empty());
    assert_eq!(host.sweep_generations(130_000, 60_000), vec!["alive".to_string()]);
    // The reconnect snapshot lists no jobs any more.
    assert_eq!(host.jobs_snapshot(), json!([]));
}
