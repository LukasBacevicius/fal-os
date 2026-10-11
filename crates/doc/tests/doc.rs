use falos_doc::*;
use serde_json::json;

fn lukas() -> Actor {
    Actor::human("lukas", "Lukas")
}
fn claude() -> Actor {
    Actor::agent("claude", "Claude Code")
}
fn codex() -> Actor {
    Actor::agent("codex", "Codex")
}

fn props(v: serde_json::Value) -> Props {
    serde_json::from_value(v).unwrap()
}

fn create(kind: NodeKind, name: &str, p: serde_json::Value) -> Command {
    Command::CreateNode {
        id: None,
        kind,
        parent: None,
        index: None,
        name: name.into(),
        props: props(p),
        text: None,
    }
}

fn read(c: &Canvas) -> String {
    c.read(&ReadOpts::default()).unwrap()
}

#[test]
fn commands_create_update_move_delete() {
    let mut c = Canvas::new("c1", "Moodboard");
    let r = c
        .apply(
            &lukas(),
            None,
            &[
                create(NodeKind::Frame, "board", json!({"w": 2000, "h": 1000})),
                create(
                    NodeKind::Image,
                    "red car",
                    json!({"x": 40, "y": 80, "asset": "sha:9f"}),
                ),
            ],
        )
        .unwrap();
    let [frame, img] = [r.created[0].clone(), r.created[1].clone()];
    assert_eq!(frame.len(), 6);

    c.apply(
        &claude(),
        None,
        &[
            Command::UpdateNode {
                id: img.clone(),
                props: props(json!({"name": "red car v3", "w": 1024, "h": 1024})),
            },
            Command::MoveNode {
                id: img.clone(),
                parent: Some(frame.clone()),
                index: None,
            },
        ],
    )
    .unwrap();
    let n = c.node(&img).unwrap();
    assert_eq!((n.name.as_str(), n.x, n.w), ("red car v3", 40.0, 1024.0));
    assert_eq!(n.parent.as_deref(), Some(frame.as_str()));
    assert_eq!(
        (n.created_by.as_str(), n.updated_by.as_str()),
        ("lukas", "claude")
    );
    assert_eq!(n.props["asset"], json!("sha:9f"));
    assert_eq!(c.node(&frame).unwrap().children, vec![img.clone()]);

    // Type-checked props.
    let err = c
        .apply(
            &claude(),
            None,
            &[Command::UpdateNode {
                id: img.clone(),
                props: props(json!({"crop": [1, 2]})),
            }],
        )
        .unwrap_err();
    assert!(err.to_string().contains("crop"), "{err}");
    // Unknown props are kept but warned about.
    let r = c
        .apply(
            &claude(),
            None,
            &[Command::UpdateNode {
                id: img.clone(),
                props: props(json!({"mood": "noir"})),
            }],
        )
        .unwrap();
    assert_eq!(r.warnings, vec!["unknown prop `mood` on image"]);

    c.apply(&lukas(), None, &[Command::DeleteNode { id: frame.clone() }])
        .unwrap();
    assert!(c.node(&frame).is_none() && c.node(&img).is_none());
    assert!(c.nodes().is_empty());
}

#[test]
fn failed_transaction_is_atomic() {
    let mut c = Canvas::new("c1", "x");
    c.apply(&lukas(), None, &[create(NodeKind::Note, "n", json!({}))])
        .unwrap();
    let before = read(&c);
    let v = c.version();
    let err = c
        .apply(
            &lukas(),
            None,
            &[
                create(NodeKind::Image, "ok", json!({})),
                Command::UpdateNode {
                    id: "nope42".into(),
                    props: Props::new(),
                },
            ],
        )
        .unwrap_err();
    assert!(matches!(err, Error::UnknownNode(_)));
    assert_eq!(read(&c), before);
    assert_eq!(c.version(), v);
    // The actor's session (and undo stack) survives a validation failure.
    assert!(c.can_undo("lukas"));
}

#[test]
fn text_fields_are_collaborative() {
    let mut c = Canvas::new("c1", "x");
    let r = c
        .apply(
            &lukas(),
            None,
            &[Command::CreateNode {
                id: Some("script".into()),
                kind: NodeKind::Text,
                parent: None,
                index: None,
                name: "Script".into(),
                props: Props::new(),
                text: Some("INT. GARAGE - NIGHT".into()),
            }],
        )
        .unwrap();
    assert_eq!(r.created, vec!["script"]);
    let mut other = Canvas::from_snapshot(&c.export_snapshot().unwrap()).unwrap();
    c.apply(
        &lukas(),
        None,
        &[Command::SetText {
            id: "script".into(),
            text: "INT. GARAGE - NIGHT\nA red car.".into(),
        }],
    )
    .unwrap();
    other
        .apply(
            &claude(),
            None,
            &[Command::SetText {
                id: "script".into(),
                text: "EXT. GARAGE - NIGHT".into(),
            }],
        )
        .unwrap();
    sync(&mut c, &mut other);
    let t = c.node("script").unwrap().text.unwrap();
    assert_eq!(t, other.node("script").unwrap().text.unwrap());
    // Character-level merge keeps both edits.
    assert!(t.starts_with("EXT.") && t.ends_with("A red car."), "{t}");
}

fn sync(a: &mut Canvas, b: &mut Canvas) {
    let to_b = a.export_updates(&b.vv()).unwrap();
    let to_a = b.export_updates(&a.vv()).unwrap();
    b.import(&to_b).unwrap();
    a.import(&to_a).unwrap();
}

#[test]
fn concurrent_edits_from_two_actors_merge() {
    let mut a = Canvas::new("c1", "x");
    let r = a
        .apply(
            &lukas(),
            None,
            &[create(NodeKind::Image, "car", json!({"x": 0, "y": 0}))],
        )
        .unwrap();
    let img = r.created[0].clone();
    let mut b = Canvas::from_snapshot(&a.export_snapshot().unwrap()).unwrap();

    // Concurrently: human moves the image, agent renames it and adds a generation.
    a.apply(
        &lukas(),
        None,
        &[Command::UpdateNode {
            id: img.clone(),
            props: props(json!({"x": 500})),
        }],
    )
    .unwrap();
    b.apply(
        &claude(),
        None,
        &[
            Command::UpdateNode {
                id: img.clone(),
                props: props(json!({"name": "car v2"})),
            },
            create(
                NodeKind::Generation,
                "variation",
                json!({"status": "running", "model": "fal-ai/flux/dev", "progress": 0.25}),
            ),
        ],
    )
    .unwrap();
    sync(&mut a, &mut b);
    assert_eq!(read(&a), read(&b));
    let n = a.node(&img).unwrap();
    assert_eq!((n.x, n.name.as_str()), (500.0, "car v2"));
    assert_eq!(a.nodes().len(), 2);
    // Attribution of both actors survives sync.
    assert_eq!(b.actor_name("lukas").as_deref(), Some("Lukas"));
    assert_eq!(a.actor_name("claude").as_deref(), Some("Claude Code"));
}

#[test]
fn per_actor_undo_redo() {
    let mut c = Canvas::new("c1", "x");
    let r = c
        .apply(
            &lukas(),
            None,
            &[create(NodeKind::Image, "car", json!({"x": 1}))],
        )
        .unwrap();
    let img = r.created[0].clone();
    c.apply(
        &claude(),
        None,
        &[Command::UpdateNode {
            id: img.clone(),
            props: props(json!({"x": 100})),
        }],
    )
    .unwrap();
    let r = c
        .apply(
            &claude(),
            None,
            &[create(NodeKind::Note, "todo", json!({}))],
        )
        .unwrap();
    let note = r.created[0].clone();
    c.apply(
        &lukas(),
        None,
        &[Command::UpdateNode {
            id: img.clone(),
            props: props(json!({"name": "car (human)"})),
        }],
    )
    .unwrap();

    // Undo Claude's last change: the note goes away, everything else stays.
    assert!(c.undo("claude").unwrap());
    assert!(c.node(&note).is_none());
    assert_eq!(c.node(&img).unwrap().x, 100.0);
    // Undo Claude's previous change: x restored, the human's later rename survives.
    assert!(c.undo("claude").unwrap());
    let n = c.node(&img).unwrap();
    assert_eq!((n.x, n.name.as_str()), (1.0, "car (human)"));
    assert!(!c.undo("claude").unwrap());
    assert!(!c.undo("codex").unwrap());

    // Redo brings the note back with the same id.
    assert!(c.redo("claude").unwrap());
    assert_eq!(c.node(&img).unwrap().x, 100.0);
    assert!(c.redo("claude").unwrap());
    assert_eq!(c.node(&note).unwrap().name, "todo");

    // The human's own undo only touches the human's ops.
    assert!(c.undo("lukas").unwrap());
    let n = c.node(&img).unwrap();
    assert_eq!((n.x, n.name.as_str()), (100.0, "car"));
}

#[test]
fn undo_after_remote_edits_and_delete() {
    let mut c = Canvas::new("c1", "x");
    let r = c
        .apply_edit(
            "frame \"board\" s(100,100)\n  image \"a\" p(1,1)\n  image \"b\" p(2,2)\n",
            &lukas(),
            None,
        )
        .unwrap();
    let frame = r.created[0].clone();
    let before = read(&c);
    c.apply_edit(&format!("delete({frame})"), &codex(), None)
        .unwrap();
    assert_eq!(read(&c), "");
    assert!(c.undo("codex").unwrap());
    assert_eq!(
        read(&c).replace("by(@lukas)", ""),
        before.replace("by(@lukas)", "")
    );
    assert!(c.node(&frame).is_some());
}

#[test]
fn revert_all_changes_of_an_actor_since_version() {
    let mut c = Canvas::new("c1", "x");
    c.apply(
        &claude(),
        None,
        &[create(NodeKind::Note, "before", json!({}))],
    )
    .unwrap();
    let r = c
        .apply(&lukas(), None, &[create(NodeKind::Image, "car", json!({}))])
        .unwrap();
    let img = r.created[0].clone();
    let v = c.version();
    for i in 0..3 {
        c.apply(
            &claude(),
            None,
            &[create(NodeKind::Image, &format!("var {i}"), json!({}))],
        )
        .unwrap();
        c.apply(
            &lukas(),
            None,
            &[Command::UpdateNode {
                id: img.clone(),
                props: props(json!({"x": i})),
            }],
        )
        .unwrap();
    }
    c.apply(
        &claude(),
        None,
        &[Command::UpdateNode {
            id: img.clone(),
            props: props(json!({"locked": true})),
        }],
    )
    .unwrap();
    assert_eq!(c.revert_since("claude", &v).unwrap(), 4);
    let names: Vec<String> = c.nodes().into_iter().map(|n| n.name).collect();
    assert_eq!(names, vec!["before", "car"]);
    let n = c.node(&img).unwrap();
    assert_eq!((n.x, n.locked), (2.0, false));
    assert_eq!(c.revert_since("claude", &v).unwrap(), 0);
}

#[test]
fn staleness_report_instead_of_failure() {
    let mut c = Canvas::new("c1", "x");
    let r = c
        .apply(
            &lukas(),
            None,
            &[
                create(NodeKind::Image, "a", json!({})),
                create(NodeKind::Image, "b", json!({})),
            ],
        )
        .unwrap();
    let (a, b) = (r.created[0].clone(), r.created[1].clone());
    let r = c
        .apply(
            &lukas(),
            None,
            &[Command::AddAnnotation {
                id: None,
                node_id: a.clone(),
                anchor: Anchor::Box {
                    x: 0.0,
                    y: 0.0,
                    w: 10.0,
                    h: 10.0,
                },
                text: Some("warmer".into()),
                assignee: Some("claude".into()),
            }],
        )
        .unwrap();
    let ann = r.created[0].clone();
    let claude_read = c.version();

    // Others change things after Claude's read.
    c.apply(
        &lukas(),
        None,
        &[Command::UpdateNode {
            id: a.clone(),
            props: props(json!({"x": 5})),
        }],
    )
    .unwrap();
    c.apply(
        &codex(),
        None,
        &[Command::MoveNode {
            id: b.clone(),
            parent: Some(a.clone()),
            index: None,
        }],
    )
    .unwrap();
    c.apply(
        &codex(),
        None,
        &[Command::Reply {
            id: ann.clone(),
            text: "on it?".into(),
        }],
    )
    .unwrap();

    // Claude's write still applies, but reports what changed and by whom.
    let r = c
        .apply(
            &claude(),
            Some(&claude_read),
            &[Command::UpdateNode {
                id: a.clone(),
                props: props(json!({"y": 9})),
            }],
        )
        .unwrap();
    assert_eq!(c.node(&a).unwrap().y, 9.0);
    let s = r.stale.expect("stale report");
    assert_eq!(s.since, claude_read);
    assert_eq!(
        s.changes,
        vec![
            ActorChanges {
                actor: "codex".into(),
                nodes: vec![b.clone()],
                annotations: vec![ann.clone()]
            },
            ActorChanges {
                actor: "lukas".into(),
                nodes: vec![a.clone()],
                annotations: vec![]
            },
        ]
    );
    assert_eq!(s.conflicts, vec![a.clone()]);
    let msg = s.to_string();
    assert!(
        msg.contains("by @codex") && msg.contains(&format!("you also edited {a}")),
        "{msg}"
    );

    // Own changes are not reported; an up-to-date version yields no report.
    let r2 = c
        .apply(
            &claude(),
            Some(&r.version),
            &[Command::UpdateNode {
                id: a.clone(),
                props: props(json!({"y": 1})),
            }],
        )
        .unwrap();
    assert!(r2.stale.is_none());
    let mine = c.apply(&claude(), Some(&r.version), &[]).unwrap();
    assert!(mine.stale.is_none());

    // Versions round-trip through strings (what agents hold on to).
    let v: Version = claude_read.to_string().parse().unwrap();
    assert_eq!(v, claude_read);
    assert!(matches!(
        "x@y".parse::<Version>(),
        Err(Error::UnknownVersion(_))
    ));
}

#[test]
fn annotation_crud() {
    let mut c = Canvas::new("c1", "x");
    let r = c
        .apply(
            &lukas(),
            None,
            &[create(NodeKind::Video, "clip", json!({"asset": "sha:aa"}))],
        )
        .unwrap();
    let vid = r.created[0].clone();
    let anchor = Anchor::Frame {
        t: 4.6,
        region: Some(Region::Box {
            x: 10.0,
            y: 0.0,
            w: 300.0,
            h: 120.0,
        }),
    };
    let r = c
        .apply(
            &lukas(),
            None,
            &[Command::AddAnnotation {
                id: None,
                node_id: vid.clone(),
                anchor: anchor.clone(),
                text: Some("make the sky stormier".into()),
                assignee: Some("codex".into()),
            }],
        )
        .unwrap();
    let id = r.created[0].clone();
    let a = c.annotation(&id).unwrap();
    assert_eq!(
        (a.node_id.as_str(), a.author.as_str(), a.status),
        (vid.as_str(), "lukas", Status::Open)
    );
    assert_eq!(a.anchor, anchor);
    assert_eq!(a.assignee.as_deref(), Some("codex"));
    assert_eq!(a.thread[0].text, "make the sky stormier");

    let r = c
        .apply(
            &codex(),
            None,
            &[create(NodeKind::Video, "clip v2", json!({}))],
        )
        .unwrap();
    let v2 = r.created[0].clone();
    c.apply(
        &codex(),
        None,
        &[
            Command::UpdateAnnotation {
                id: id.clone(),
                status: Some(Status::Resolved),
                anchor: None,
                assignee: None,
                outcome: Some(vec![v2.clone()]),
            },
            Command::Reply {
                id: id.clone(),
                text: "v2 here".into(),
            },
        ],
    )
    .unwrap();
    let a = c.annotation(&id).unwrap();
    assert_eq!(a.status, Status::Resolved);
    assert_eq!(a.outcome, vec![v2.clone()]);
    assert_eq!(
        a.thread
            .iter()
            .map(|m| (m.author.as_str(), m.text.as_str()))
            .collect::<Vec<_>>(),
        vec![("lukas", "make the sky stormier"), ("codex", "v2 here")]
    );

    // Reopen + unassign, then delete.
    c.apply(
        &lukas(),
        None,
        &[Command::UpdateAnnotation {
            id: id.clone(),
            status: Some(Status::Open),
            anchor: None,
            assignee: Some(None),
            outcome: None,
        }],
    )
    .unwrap();
    assert_eq!(c.annotation(&id).unwrap().assignee, None);
    assert!(
        c.apply(
            &lukas(),
            None,
            &[Command::AddAnnotation {
                id: None,
                node_id: "nope".into(),
                anchor: Anchor::Time { t: 1.0 },
                text: None,
                assignee: None
            }]
        )
        .is_err()
    );
    c.apply(
        &lukas(),
        None,
        &[Command::DeleteAnnotation { id: id.clone() }],
    )
    .unwrap();
    assert!(c.annotation(&id).is_none());
    assert!(c.undo("lukas").unwrap());
    assert_eq!(c.annotation(&id).unwrap().thread.len(), 2);
}

#[test]
fn assets_with_lineage() {
    let mut c = Canvas::new("c1", "x");
    let asset = Asset {
        id: "sha256:9f86d0".into(),
        mime: Some("image/png".into()),
        uri: Some("https://v3.fal.media/files/x.png".into()),
        lineage: Some(Lineage {
            model: "fal-ai/flux/dev".into(),
            params: json!({"prompt": "red car", "steps": 28}),
            seed: Some(42),
            parents: vec!["sha256:0000".into()],
            fal_request_id: Some("req_123".into()),
            cost: Some(0.04),
            actor: "claude".into(),
        }),
    };
    c.apply(&claude(), None, &[Command::PutAsset(asset.clone())])
        .unwrap();
    assert_eq!(c.asset("sha256:9f86d0"), Some(asset));
}

const SAMPLE: &str = r##"frame "Moodboard" s(2000,1200)
  image "red car v3" p(40,80) s(1024,1024) asset(sha:9f86) crop(0,0,512,512)
    ! box(10,20,200,100) to(@codex) "make the sky stormier"
      > "and warmer"
  video "drive" p(1100,80) s(1920,1080) asset(sha:aa) in(1.5) out(4) loop muted
    ! frame(4.2) poly(0,0,10,0,10,10) "flicker here"
  audio "score" asset(sha:bb) out(30)
    ! range(3,7.5) resolved
  text "Script" s(400,600) text("INT. GARAGE - NIGHT\nA \"red\" car.")
    ! chars(0,3)
  model3d "car.glb" asset(sha:cc)
    ! pt3(1,0.5,-2) cam(0,1,5,0,0,0,45) in_progress
  document "brief.pdf" asset(sha:dd) page(2)
    ! page(2) box(0,0,100,50)
  vector "logo" asset(sha:ee)
    ! pt(5,5)
  embed "prototype" url("https://example.com/a b")
    ! mask(sha:mask1)
  generation "variation" p(0,1200) status(running) model(fal-ai/flux/dev) job("123") progress(0.4) params({"prompt":"red car","seed":7})
  style:palette "warm" data(["#ff0000","#ffaa00"])
  style:lora "my-face" asset(sha:ff)
  timeline "cut" duration(30)
    stream "cam" muted
  note "todo" color(yellow) text("fix the sky")
  connector from(abc) to(def) kind(lineage)
group "stuff" r(15) locked
  frame "inner"
"##;

#[test]
fn text_round_trip_is_deterministic() {
    let mut a = Canvas::new("c1", "x");
    let r = a.apply_edit(SAMPLE, &lukas(), None).unwrap();
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    let text = read(&a);
    println!("{text}");
    // read → apply on an empty doc → read gives the same text.
    let mut b = Canvas::new("c2", "y");
    b.apply_edit(&text, &lukas(), None).unwrap();
    assert_eq!(read(&b), text);
    // ...and re-applying the read output to the same doc is a no-op edit.
    let v = a.version();
    let r = a.apply_edit(&text, &lukas(), None).unwrap();
    assert_eq!(read(&a), text);
    assert_eq!(a.version(), v);
    assert_eq!(r.warnings, vec!["no changes"]);
    // Echoing it back as another actor is also a no-op (attribution is display-only).
    let r = a.apply_edit(&text, &claude(), None).unwrap();
    assert_eq!(
        (read(&a), r.warnings),
        (text.clone(), vec!["no changes".to_string()])
    );
    // Without attribution the projection carries no actor names.
    let plain = a
        .read(&ReadOpts {
            attribution: false,
            ..Default::default()
        })
        .unwrap();
    assert!(!plain.contains('@') || plain.contains("to(@codex)"));
    let mut c = Canvas::new("c3", "z");
    c.apply_edit(&plain, &codex(), None).unwrap();
    assert_eq!(
        c.read(&ReadOpts {
            attribution: false,
            ..Default::default()
        })
        .unwrap(),
        plain
    );
}

#[test]
fn text_projection_format() {
    let mut c = Canvas::new("c1", "x");
    c.apply_edit(
        "f1 frame \"board\" s(100,100)\n  car image \"red car\" p(40,80) s(1024,1024) asset(sha:9f)\n    ! a1 box(1,2,3,4) to(@codex) \"stormier\"\n",
        &lukas(),
        None,
    )
    .unwrap();
    c.apply_edit("car p(41,80)", &claude(), None).unwrap();
    assert_eq!(
        read(&c),
        "f1 frame \"board\" s(100,100) by(@lukas)\n  car image \"red car\" p(41,80) s(1024,1024) asset(sha:9f) by(@lukas) upd(@claude)\n    ! a1 open box(1,2,3,4) to(@codex) by(@lukas)\n      > @lukas \"stormier\"\n"
    );
    // Depth limit summarizes hidden children; root selects a subtree; filter keeps ancestors.
    let o = ReadOpts {
        depth: Some(0),
        annotations: false,
        attribution: false,
        ..Default::default()
    };
    assert_eq!(
        c.read(&o).unwrap(),
        "f1 frame \"board\" s(100,100) more(1)\n"
    );
    let o = ReadOpts {
        root: Some("car".into()),
        annotations: false,
        attribution: false,
        ..Default::default()
    };
    assert_eq!(
        c.read(&o).unwrap(),
        "car image \"red car\" p(41,80) s(1024,1024) asset(sha:9f)\n"
    );
    let o = ReadOpts {
        filter: Some("RED".into()),
        annotations: false,
        attribution: false,
        ..Default::default()
    };
    assert_eq!(c.read(&o).unwrap().lines().count(), 2);
    let o = ReadOpts {
        filter: Some("video".into()),
        ..Default::default()
    };
    assert_eq!(c.read(&o).unwrap(), "");
}

#[test]
fn text_edit_semantics() {
    let mut c = Canvas::new("c1", "x");
    let r = c
        .apply_edit("frame \"board\" s(500,500)\n  image \"a\" p(1,2) s(10,10) asset(sha:1)\n  note \"n\" text(\"hi\")\n", &lukas(), None)
        .unwrap();
    let [board, a, n] = [
        r.created[0].clone(),
        r.created[1].clone(),
        r.created[2].clone(),
    ];
    assert_eq!(c.node(&a).unwrap().parent.as_deref(), Some(board.as_str()));

    // A row with an id changes only the named props.
    c.apply_edit(&format!("{a} p(5,6)"), &claude(), None)
        .unwrap();
    let node = c.node(&a).unwrap();
    assert_eq!(
        (node.x, node.y, node.w, node.name.as_str()),
        (5.0, 6.0, 10.0, "a")
    );
    assert_eq!(node.props["asset"], json!("sha:1"));

    // Rename, remove a prop with null, set flags, edit text.
    c.apply_edit(
        &format!("{a} image \"a2\" asset(null) locked\n{n} text(\"hello\")"),
        &claude(),
        None,
    )
    .unwrap();
    let node = c.node(&a).unwrap();
    assert_eq!(
        (node.name.as_str(), node.locked, node.props.get("asset")),
        ("a2", true, None)
    );
    assert_eq!(c.node(&n).unwrap().text.as_deref(), Some("hello"));

    // New rows nested under an existing id row become its children; parent() moves.
    let r = c
        .apply_edit(
            &format!("{board}\n  image \"b\"\n{n} parent(root)\n{a} parent({board},0)"),
            &claude(),
            None,
        )
        .unwrap();
    let b = r.created[0].clone();
    assert_eq!(c.node(&board).unwrap().children, vec![a.clone(), b.clone()]);
    assert_eq!(c.node(&n).unwrap().parent, None);

    // Annotations: create with shorthand, then update status / reply / outcome via text.
    let r = c
        .apply_edit(&format!("{b}\n  ! pt(3,4) \"too dark\""), &lukas(), None)
        .unwrap();
    let ann = r.created[0].clone();
    c.apply_edit(
        &format!("! {ann} resolved out({b}) on({b})\n  > \"fixed\"\n  > @lukas \"too dark\""),
        &claude(),
        None,
    )
    .unwrap();
    let an = c.annotation(&ann).unwrap();
    assert_eq!(an.status, Status::Resolved);
    assert_eq!(an.outcome, vec![b.clone()]);
    // Echoed `> @author` rows are ignored; bare `>` rows are new replies.
    assert_eq!(an.thread.len(), 2);
    assert_eq!(an.thread[1].author, "claude");

    // Deletes (nodes and annotations).
    c.apply_edit(&format!("delete({ann},{n})"), &lukas(), None)
        .unwrap();
    assert!(c.annotation(&ann).is_none() && c.node(&n).is_none());

    // Errors carry line numbers and leave the canvas untouched.
    let before = read(&c);
    for (bad, want) in [
        ("image \"ok\"\nzzzz p(1,1)", "line 2: unknown node `zzzz`"),
        (&format!("{a} video") as &str, "line 1: "),
        ("image p(1)", "line 1: `p` expects 2 numbers"),
        ("image \"x\" \"y\"", "line 1: unexpected string"),
        (
            "note\n  ! \"no anchor\"",
            "line 2: new annotation needs an anchor",
        ),
        (&format!("{board} parent({a})"), "line 1: can't move"),
        ("generation status(nope)", "line 1: prop `status` expects"),
        ("image \"unterminated", "line 1: unterminated string"),
    ] {
        let err = c.apply_edit(bad, &claude(), None).unwrap_err().to_string();
        assert!(err.starts_with(want), "{bad:?}: {err}");
    }
    assert_eq!(read(&c), before);
}

#[test]
fn project_save_and_load() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::new("p1", "Car spot");
    let mut c = Canvas::new("main", "Main");
    c.apply_edit(
        "image \"car\" p(1,2) asset(sha:1)\n  ! pt(1,1) \"hi\"",
        &lukas(),
        None,
    )
    .unwrap();
    p.save_canvas(dir.path(), &c).unwrap();
    assert!(dir.path().join("canvases/main.loro").exists());

    let p2 = Project::load(dir.path()).unwrap();
    assert_eq!(p2, p);
    assert_eq!(
        p2.canvases,
        vec![CanvasEntry {
            id: "main".into(),
            name: "Main".into()
        }]
    );
    let mut c2 = p2.open_canvas(dir.path(), "main").unwrap();
    assert_eq!(read(&c2), read(&c));
    assert_eq!((c2.id().as_str(), c2.name().as_str()), ("main", "Main"));
    // History survives: staleness works across the reload.
    let v = c2.version();
    let id = c2.nodes()[0].id.clone();
    c2.apply_edit(&format!("{id} p(9,9)"), &lukas(), None)
        .unwrap();
    assert_eq!(c2.changes_since(&v).unwrap()[0].nodes, vec![id]);
    assert!(p2.open_canvas(dir.path(), "../evil").is_err());
}

#[test]
fn canvas_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Canvas>();
}

#[test]
fn untracked_fill_ins_ride_along_with_the_creating_transaction() {
    let mut c = Canvas::new("c1", "x");
    // A paste: frame + image placeholder in one transaction.
    let r = c
        .apply_edit("frame \"board\" s(100,100)\n  image \"photo\" p(1,1) s(50,50)\n", &lukas(), None)
        .unwrap();
    let (frame, image) = (r.created[0].clone(), r.created[1].clone());
    // The download completes later: not an undo step of its own.
    c.apply_untracked(
        &lukas(),
        &[Command::UpdateNode { id: image.clone(), props: props(json!({"asset": "sha256:ab"})) }],
    )
    .unwrap();
    assert_eq!(c.node(&image).unwrap().props["asset"], "sha256:ab");
    // One undo removes the whole paste...
    assert!(c.undo("lukas").unwrap());
    assert!(c.node(&frame).is_none() && c.node(&image).is_none());
    assert!(!c.can_undo("lukas"));
    // ...and redo restores it with the filled-in asset.
    assert!(c.redo("lukas").unwrap());
    assert_eq!(c.node(&image).unwrap().props["asset"], "sha256:ab");
    assert_eq!(c.node(&image).unwrap().parent.as_deref(), Some(frame.as_str()));
}
