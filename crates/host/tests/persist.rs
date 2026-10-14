//! Crash-safe persistence: durable atomic saves, the backup ring, the collapse guard, opening
//! a project with a corrupt canvas or project.json, and the format version gate.

use std::path::Path;
use std::time::Duration;

use falos_doc::{Actor, Canvas, Command, NodeKind, Project};
use falos_host::{Host, HostOptions, persist};
use serde_json::json;

fn opts() -> HostOptions {
    HostOptions { fal_key: None, fal_session: false, home: None, ..Default::default() }
}

fn add_notes(host: &Host, n: usize) -> Vec<String> {
    let me = Actor::human("you", "you");
    let cmds: Vec<Command> = (0..n)
        .map(|i| Command::CreateNode {
            id: None,
            kind: NodeKind::Note,
            parent: None,
            index: None,
            name: format!("n{i}"),
            props: [("x".to_string(), json!(i * 10)), ("y".to_string(), json!(0))].into(),
            text: Some(format!("note {i}")),
        })
        .collect();
    host.mutate("main", "you", None, |c| c.apply(&me, None, &cmds)).unwrap().created
}

fn main_file(dir: &Path) -> std::path::PathBuf {
    dir.join("canvases").join("main.loro")
}

fn notice_keys(host: &Host) -> Vec<String> {
    host.notices().iter().filter_map(|n| n["key"].as_str().map(str::to_string)).collect()
}

#[test]
fn atomic_durable_save_leaves_no_temp_files_and_stamps_the_format() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    let ids = add_notes(&host, 3);
    host.flush().unwrap();
    let names: Vec<String> = std::fs::read_dir(dir.path().join("canvases"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.contains(".tmp")), "{names:?}");
    let p = Project::load(dir.path()).unwrap();
    assert_eq!(p.format, falos_doc::FORMAT);
    let c = p.open_canvas(dir.path(), "main").unwrap();
    assert_eq!(c.format(), falos_doc::FORMAT);
    assert!(ids.iter().all(|i| c.node(i).is_some()));
}

#[cfg(unix)]
#[test]
fn a_failed_write_keeps_the_old_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.json");
    falos_doc::write_durable(&file, b"old").unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let r = falos_doc::write_durable(&file, b"new");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(r.is_err());
    assert_eq!(std::fs::read(&file).unwrap(), b"old");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1, "no temp file left behind");
}

#[test]
fn backup_ring_is_small_and_newest_first() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    add_notes(&host, 1);
    host.flush().unwrap();
    // Loading a canvas makes a first backup.
    assert_eq!(persist::backups(dir.path(), "main").len(), 1);
    for _ in 0..8 {
        assert!(persist::maybe_backup(dir.path(), "main", Duration::ZERO).unwrap());
    }
    let b = persist::backups(dir.path(), "main");
    assert_eq!(b.len(), persist::BACKUPS_KEPT);
    assert!(b.windows(2).all(|w| w[0].0 > w[1].0), "newest first");
    // Rotated by time: a recent backup means no new one.
    assert!(!persist::maybe_backup(dir.path(), "main", Duration::from_secs(600)).unwrap());
    // Every backup is a loadable canvas.
    for (_, p) in b {
        Canvas::from_snapshot(&std::fs::read(p).unwrap()).unwrap();
    }
}

#[test]
fn a_collapsing_save_is_refused_unless_deletes_explain_it() {
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    let ids = add_notes(&host, 20);
    host.flush().unwrap();
    let good = std::fs::read(main_file(dir.path())).unwrap();

    // The doc loses 95 % of its nodes behind the host's back: refused, file kept.
    let mut broken = Canvas::new("main", "Main");
    let me = Actor::human("you", "you");
    broken
        .apply(&me, None, &[Command::CreateNode { id: None, kind: NodeKind::Note, parent: None, index: None, name: "lone".into(), props: Default::default(), text: None }])
        .unwrap();
    host.replace_canvas_unchecked("main", broken).unwrap();
    host.flush().unwrap();
    assert_eq!(std::fs::read(main_file(dir.path())).unwrap(), good, "the last good file is kept");
    assert!(notice_keys(&host).contains(&"refused:main".to_string()), "{:?}", host.notices());

    // "save anyway" goes through.
    host.notice_action("refused:main", "save-anyway").unwrap();
    host.flush().unwrap();
    assert_ne!(std::fs::read(main_file(dir.path())).unwrap(), good);
    assert!(!notice_keys(&host).contains(&"refused:main".to_string()));

    // Real deletes of most nodes are saved without a fuss.
    let dir2 = tempfile::tempdir().unwrap();
    let host2 = Host::open(dir2.path(), opts()).unwrap();
    let ids2 = add_notes(&host2, 20);
    host2.flush().unwrap();
    let dels: Vec<Command> = ids2.iter().skip(1).map(|id| Command::DeleteNode { id: id.clone() }).collect();
    host2.mutate("main", "you", None, |c| c.apply(&me, None, &dels)).unwrap();
    host2.flush().unwrap();
    let saved = Project::load(dir2.path()).unwrap().open_canvas(dir2.path(), "main").unwrap();
    assert_eq!(saved.node_count(), 1);
    assert!(host2.notices().is_empty(), "{:?}", host2.notices());
    let _ = ids;
}

#[test]
fn a_corrupt_canvas_opens_the_project_and_is_never_saved_over() {
    let dir = tempfile::tempdir().unwrap();
    {
        let host = Host::open(dir.path(), opts()).unwrap();
        add_notes(&host, 2);
        host.flush().unwrap();
    }
    let garbage = b"\x00\x01 this is not a loro snapshot";
    std::fs::write(main_file(dir.path()), garbage).unwrap();

    let host = Host::open(dir.path(), opts()).unwrap();
    assert!(host.read("main", |_| ()).is_err());
    assert!(host.load_failed("main"));
    let n = host.notices().into_iter().find(|n| n["key"] == "load:main").expect("load notice");
    assert!(n["text"].as_str().unwrap().contains("the file is untouched"), "{n}");
    assert_eq!(n["actions"][0]["id"], "restore");
    // Writes are refused; flushing never touches the file.
    assert!(host.write_block("main").unwrap().contains("retry shortly"));
    assert!(host.mutate("main", "you", None, |c| c.set_format(1)).is_err());
    host.flush().unwrap();
    assert_eq!(std::fs::read(main_file(dir.path())).unwrap(), garbage);

    // [restore backup]: the newest good backup comes back, the broken file is kept aside.
    host.restore_backup("main").unwrap();
    assert!(!host.load_failed("main"));
    assert_eq!(host.read("main", |c| c.node_count()).unwrap(), 2);
    let aside: Vec<_> = std::fs::read_dir(dir.path().join("canvases"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("main.loro.broken-"))
        .collect();
    assert_eq!(aside.len(), 1);
    assert_eq!(std::fs::read(aside[0].path()).unwrap(), garbage);
}

#[test]
fn a_damaged_project_json_is_rebuilt_from_the_canvas_files() {
    let dir = tempfile::tempdir().unwrap();
    {
        let host = Host::open(dir.path(), opts()).unwrap();
        add_notes(&host, 1);
        host.flush().unwrap();
    }
    std::fs::write(dir.path().join("project.json"), b"{ \"name\": \"Launch\", \"canv").unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    assert_eq!(host.project().canvases.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["main"]);
    assert_eq!(host.read("main", |c| c.node_count()).unwrap(), 1);
    assert!(notice_keys(&host).contains(&"project.json".to_string()));
    let kept = std::fs::read_dir(dir.path())
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().starts_with("project.json.broken-"));
    assert!(kept, "the damaged original is kept");
    Project::load(dir.path()).unwrap();
}

#[test]
fn a_newer_format_opens_read_only_and_an_older_one_is_archived_then_migrated() {
    // Newer project.json: read-only, nothing written.
    let dir = tempfile::tempdir().unwrap();
    {
        let host = Host::open(dir.path(), opts()).unwrap();
        add_notes(&host, 1);
        host.flush().unwrap();
    }
    let pj = dir.path().join("project.json");
    let mut v: serde_json::Value = serde_json::from_slice(&std::fs::read(&pj).unwrap()).unwrap();
    v["format"] = json!(99);
    std::fs::write(&pj, serde_json::to_vec_pretty(&v).unwrap()).unwrap();
    let before = std::fs::read(main_file(dir.path())).unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    assert!(host.write_block("main").unwrap().contains("newer fal OS"));
    assert!(add_notes_err(&host));
    host.flush().unwrap();
    assert_eq!(std::fs::read(main_file(dir.path())).unwrap(), before);
    assert!(notice_keys(&host).contains(&"readonly".to_string()));
    assert!(host.update_project(|p| p.name = "x".into()).is_err());

    // Newer canvas only: that canvas is read-only.
    let dir = tempfile::tempdir().unwrap();
    {
        let host = Host::open(dir.path(), opts()).unwrap();
        add_notes(&host, 1);
        host.flush().unwrap();
    }
    let mut c = Canvas::from_snapshot(&std::fs::read(main_file(dir.path())).unwrap()).unwrap();
    c.set_format(99).unwrap();
    std::fs::write(main_file(dir.path()), c.export_snapshot().unwrap()).unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    assert_eq!(host.read("main", |c| c.format()).unwrap(), 99);
    assert!(host.write_block("main").is_some());
    assert!(notice_keys(&host).contains(&"readonly:main".to_string()));

    // Older (format 0): the originals are archived, then migrated on the next save.
    let dir = tempfile::tempdir().unwrap();
    let mut p = Project::new("old", "Old");
    p.format = 0;
    let mut c = Canvas::new("main", "Main");
    c.set_format(0).unwrap();
    p.save_canvas(dir.path(), &c).unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    assert_eq!(host.read("main", |c| c.format()).unwrap(), falos_doc::FORMAT);
    host.flush().unwrap();
    let archived: Vec<String> = std::fs::read_dir(dir.path().join(".archive"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(archived.iter().any(|n| n.starts_with("project.json.v0.")), "{archived:?}");
    assert!(archived.iter().any(|n| n.starts_with("main.loro.v0.")), "{archived:?}");
    assert_eq!(Project::load(dir.path()).unwrap().format, falos_doc::FORMAT);
    let saved = Canvas::from_snapshot(&std::fs::read(main_file(dir.path())).unwrap()).unwrap();
    assert_eq!(saved.format(), falos_doc::FORMAT);
}

fn add_notes_err(host: &Host) -> bool {
    let me = Actor::human("you", "you");
    host.mutate("main", "you", None, |c| {
        c.apply(&me, None, &[Command::CreateNode { id: None, kind: NodeKind::Note, parent: None, index: None, name: "x".into(), props: Default::default(), text: None }])
    })
    .is_err()
}

/// A save that fails (the folder became read-only) keeps the canvas dirty, raises the held
/// "not saved to disk yet · retrying" notice and recovers by itself once writing works again.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_save_retries_under_a_held_notice_until_it_works() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let host = Host::open(dir.path(), opts()).unwrap();
    host.start_tasks();
    add_notes(&host, 1);
    host.flush().unwrap();
    let canvases = dir.path().join("canvases");
    std::fs::set_permissions(&canvases, std::fs::Permissions::from_mode(0o555)).unwrap();
    let mut rx = host.subscribe();
    let ids = add_notes(&host, 1);
    let mut held = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !held {
        if let Ok(Ok(falos_host::Event::Notice { notice })) = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            held = notice["key"] == "save" && notice["level"] == "ongoing";
            if held {
                assert!(notice["text"].as_str().unwrap().starts_with("not saved to disk yet · retrying"), "{notice}");
            }
        }
    }
    std::fs::set_permissions(&canvases, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(held, "a held notice while saving fails");
    let mut recovered = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline && !recovered {
        if let Ok(Ok(falos_host::Event::Notice { notice })) = tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
            recovered = notice["key"] == "save" && notice["level"] == "ok";
        }
    }
    assert!(recovered, "back to saved once the folder is writable");
    let saved = Project::load(dir.path()).unwrap().open_canvas(dir.path(), "main").unwrap();
    assert!(saved.node(&ids[0]).is_some(), "the edit made while saving failed reached disk");
    assert!(host.notices().is_empty());
}
