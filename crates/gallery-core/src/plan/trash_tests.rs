//! Plan version 2: `trash` and `keep` targets, series review items, and the
//! last-copy guard.

use super::*;
use crate::config::{Source, Target};
use crate::filter::groups::Series;

/// Source `cam` at `root/cam`; targets `Junk` (trash), `Keep` (keep) and
/// `Album` (folders, into `root/out`).
fn cfg(root: &Path) -> Config {
    let mut c = Config::default();
    c.sources.push(Source {
        id: "cam".into(),
        path: root.join("cam"),
        server_path: Some(PathBuf::from("/srv/cam")),
        recursive: false,
        skip: vec![],
        ai_prefix: None,
        ai_prefix_file: None,
        ai_suffix: None,
        ai_suffix_file: None,
    });
    for (name, kind) in [("Junk", TargetKind::Trash), ("Keep", TargetKind::Keep)] {
        c.targets.push(Target {
            name: name.into(),
            kind,
            filters: vec![],
            outputs: vec![],
        });
    }
    c.targets.push(Target {
        name: "Album".into(),
        kind: TargetKind::Folders,
        filters: vec![],
        outputs: vec![Output {
            dir: root.join("out").to_str().unwrap().into(),
            server_dir: Some("/srv/out".into()),
            content: vec![],
            rename: vec![],
        }],
    });
    c
}

fn entry(root: &Path, name: &str) -> FileEntry {
    FileEntry {
        source_id: "cam".into(),
        path: root.join("cam").join(name),
        rel: PathBuf::from(name),
        size: 7,
        mtime_ns: 42,
    }
}

fn to(target: &str) -> Decision {
    Decision::Target {
        name: target.into(),
        via: Via::Mask,
    }
}

fn names(paths: impl IntoIterator<Item = PathBuf>) -> Vec<String> {
    paths
        .into_iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn trash_and_keep_targets_become_plan_decisions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let series = SeriesReview {
        members: vec![root.join("cam/s1.jpg"), root.join("cam/s2.jpg")],
        best: root.join("cam/s2.jpg"),
    };
    let routed = vec![
        (entry(root, "junk.jpg"), to("Junk")),
        (entry(root, "keep.jpg"), to("Keep")),
        (entry(root, "a.jpg"), to("Album")),
        (
            entry(root, "s1.jpg"),
            Decision::Review {
                suggested: "Junk".into(),
                reason: "series".into(),
                series: Some(series.clone()),
            },
        ),
    ];
    let p = build_plan(&cfg(root), routed, &HashMap::new(), Side::Local).unwrap();
    assert_eq!(p.version, 2);
    let junk = &p.items[0];
    assert_eq!(
        (junk.target.as_str(), junk.kind, junk.outputs.len()),
        ("Junk", TargetKind::Trash, 0)
    );
    assert_eq!(p.items[1].kind, TargetKind::Folders);
    assert_eq!(p.items.len(), 2);
    assert_eq!(
        p.kept,
        vec![KeptItem {
            source: root.join("cam/keep.jpg"),
            target: "Keep".into()
        }]
    );
    assert!(p.unchanged.is_empty());
    assert_eq!(p.review[0].series.as_ref(), Some(&series));

    // Round trip; a version-1 item (no `kind`) is a folders item.
    let path = root.join("plan.json");
    p.save(&path).unwrap();
    let q = Plan::load(&path).unwrap();
    assert_eq!(
        (&q.items, &q.kept, &q.review),
        (&p.items, &p.kept, &p.review)
    );
    let mut v = serde_json::to_value(&p).unwrap();
    v["version"] = 1.into();
    v.as_object_mut().unwrap().remove("kept");
    for it in v["items"].as_array_mut().unwrap() {
        it.as_object_mut().unwrap().remove("kind");
    }
    std::fs::write(&path, v.to_string()).unwrap();
    let old = Plan::load(&path).unwrap();
    assert!(old.items.iter().all(|it| it.kind == TargetKind::Folders));
    assert!(old.kept.is_empty() && old.review[0].series.is_some());
}

fn groups_with(dups: &[(usize, usize)], series: Vec<Series>) -> Groups {
    let mut g = Groups {
        dup_original: dups.iter().copied().collect(),
        ..Default::default()
    };
    for (k, s) in series.iter().enumerate() {
        for &m in &s.members {
            g.series_of.insert(m, k);
        }
    }
    g.series = series;
    g
}

#[test]
fn guard_keeps_the_original_of_a_duplicate_pair_out_of_the_trash() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let files = vec![
        entry(root, "orig.jpg"),
        entry(root, "copy.jpg"),
        entry(root, "other.jpg"),
        entry(root, "placed.jpg"),
    ];
    // other.jpg is a duplicate of placed.jpg, which the album takes: fine.
    let groups = groups_with(&[(1, 0), (2, 3)], vec![]);
    let routed = vec![
        (files[0].clone(), to("Junk")),
        (files[1].clone(), to("Junk")),
        (files[2].clone(), to("Junk")),
        (files[3].clone(), to("Album")),
    ];
    let mut p = build_plan(&cfg(root), routed, &HashMap::new(), Side::Local).unwrap();
    guard_last_copies(&mut p, &groups, &files);
    let trashed: Vec<PathBuf> = p
        .items
        .iter()
        .filter(|it| it.kind == TargetKind::Trash)
        .map(|it| it.source.clone())
        .collect();
    assert_eq!(names(trashed), ["copy.jpg", "other.jpg"]);
    assert_eq!(p.review.len(), 1);
    let r = &p.review[0];
    assert_eq!(r.source, root.join("cam/orig.jpg"));
    assert_eq!(r.suggested, "Junk");
    assert_eq!(r.reason, crate::t!("plan.last_copy_kept"));
    assert_eq!(r.source_root, Some(root.join("cam")));
}

#[test]
fn guard_keeps_the_best_shot_of_a_series_trashed_by_manual_decisions() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let files: Vec<FileEntry> = ["s1.jpg", "s2.jpg", "s3.jpg"]
        .iter()
        .map(|n| entry(root, n))
        .collect();
    let groups = groups_with(
        &[],
        vec![Series {
            members: vec![0, 1, 2],
            best: 1,
        }],
    );
    // All three sent to the trash by manual decisions.
    let manual = |f: &FileEntry| {
        (
            f.clone(),
            Decision::Target {
                name: "Junk".into(),
                via: Via::Manual,
            },
        )
    };
    let routed = files.iter().map(manual).collect();
    let mut p = build_plan(&cfg(root), routed, &HashMap::new(), Side::Local).unwrap();
    guard_last_copies(&mut p, &groups, &files);
    assert_eq!(
        names(p.items.iter().map(|i| i.source.clone())),
        ["s1.jpg", "s3.jpg"]
    );
    assert_eq!(names(p.review.iter().map(|r| r.source.clone())), ["s2.jpg"]);

    // One shot kept elsewhere: the series is not lost, nothing is held.
    let mut routed: Vec<_> = files.iter().map(manual).collect();
    routed[0].1 = to("Keep");
    let mut p = build_plan(&cfg(root), routed, &HashMap::new(), Side::Local).unwrap();
    guard_last_copies(&mut p, &groups, &files);
    assert_eq!(p.items.len(), 2);
    assert!(p.review.is_empty());
}

#[test]
fn guard_finds_server_side_items_by_their_path_below_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let files = vec![entry(root, "orig.jpg"), entry(root, "copy.jpg")];
    let groups = groups_with(&[(1, 0)], vec![]);
    let routed = files.iter().map(|f| (f.clone(), to("Junk"))).collect();
    let mut p = build_plan(&cfg(root), routed, &HashMap::new(), Side::Server).unwrap();
    assert_eq!(p.items[0].source, PathBuf::from("/srv/cam/orig.jpg"));
    guard_last_copies(&mut p, &groups, &files);
    assert_eq!(
        names(p.items.iter().map(|i| i.source.clone())),
        ["copy.jpg"]
    );
    assert_eq!(p.review[0].source, PathBuf::from("/srv/cam/orig.jpg"));
}

#[test]
fn pilot_takes_trash_items_per_target_like_any_other() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let mut routed = Vec::new();
    for i in 0..4 {
        routed.push((entry(root, &format!("j{i}.jpg")), to("Junk")));
        routed.push((entry(root, &format!("a{i}.jpg")), to("Album")));
    }
    routed.push((entry(root, "k.jpg"), to("Keep")));
    let p = build_plan(&cfg(root), routed, &HashMap::new(), Side::Local).unwrap();
    let (pilot, rest) = pilot(&p, 2);
    let count = |p: &Plan, kind| p.items.iter().filter(|i| i.kind == kind).count();
    assert_eq!(count(&pilot, TargetKind::Trash), 2);
    assert_eq!(count(&pilot, TargetKind::Folders), 2);
    assert_eq!(count(&rest, TargetKind::Trash), 2);
    assert_eq!(count(&rest, TargetKind::Folders), 2);
    // The kept list stays with the rest, like the other lists outside the run.
    assert!(pilot.kept.is_empty());
    assert_eq!(rest.kept.len(), 1);
}
