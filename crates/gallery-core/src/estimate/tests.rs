use super::*;
use crate::ai::usage_log::UsageRow;
use crate::apply::OutputResult;
use crate::config::{PhotoPreset, VideoPreset};
use crate::plan::PlannedOutput;
use crate::route::Via;
use serde_json::json;

const MB: u64 = 1_000_000;

fn plan_of(items: Vec<PlanItem>) -> Plan {
    let mut p: Plan = serde_json::from_value(json!({
        "version": 1, "created": "2026-11-05T10:00:00+02:00", "side": "local",
        "content_presets": {}, "items": [], "review": [],
        "unmatched": [], "warnings": [], "unchanged": []
    }))
    .unwrap();
    p.content_presets.insert(
        "photo-max".into(),
        ContentPreset::Photo(PhotoPreset::png_avif()),
    );
    p.content_presets
        .insert("video-max".into(), ContentPreset::Video(VideoPreset::max()));
    p.items = items;
    p
}

fn item(src: &str, target: &str, size: u64, ops: &[(OutputOp, &[&str])]) -> PlanItem {
    PlanItem {
        ai_meta: None,
        source: src.into(),
        source_root: "/s".into(),
        size,
        mtime_ns: 1,
        target: target.into(),
        via: Via::Mask,
        outputs: ops
            .iter()
            .enumerate()
            .map(|(i, (op, steps))| PlannedOutput {
                dest: format!("/o/{i}{src}").into(),
                op: *op,
                steps: steps.iter().map(|s| s.to_string()).collect(),
                trace: vec![],
            })
            .collect(),
        kind: Default::default(),
    }
}

fn photo(src: &str, size: u64) -> PlanItem {
    item(src, "photos", size, &[(OutputOp::Write, &["photo-max"])])
}

fn sample(class: &str, ext: &str, input: u64, output: u64, ms: Option<u64>) -> Sample {
    Sample {
        class: class.into(),
        kind: Kind::Photo,
        ext: ext.into(),
        input,
        output,
        elapsed_ms: ms,
    }
}

/// `n` photo samples: output is `pct`% of the input, `ms` per item.
fn history(n: usize, pct: u64, ms: u64) -> History {
    History {
        samples: (0..n)
            .map(|_| sample("photo-max", "jpg", 10 * MB, 10 * MB * pct / 100, Some(ms)))
            .collect(),
    }
}

#[test]
fn defaults_when_history_is_empty() {
    let p = plan_of(vec![photo("/s/a.jpg", 10 * MB)]);
    let e = estimate_with(&p, &History::default(), 1);
    assert_eq!(e.source, EstimateSource::Defaults);
    assert_eq!(e.source_label, "rough estimate");
    let t = &e.total;
    assert_eq!((t.files, t.input_bytes, t.trash), (1, 10 * MB, 1));
    // Conservative photo defaults: 40..85% of the input remains.
    assert_eq!(t.output_bytes.low, 4 * MB);
    assert_eq!(t.output_bytes.high, 8_500_000);
    assert!(t.saving_bytes.low < t.saving_bytes.mid && t.saving_bytes.mid < t.saving_bytes.high);
    assert_eq!(t.saving_bytes.mid, 4 * MB as i64);
    assert!(t.seconds.low < t.seconds.mid && t.seconds.mid < t.seconds.high);
    // 10 MB at 1 MB/s.
    assert!((t.seconds.mid - 10.0).abs() < 1e-6);
}

#[test]
fn learns_ratio_and_range_from_history() {
    // Ratios 30, 40, 50, 60, 70 %: p25 40, p50 50, p75 60.
    let samples = [30, 40, 50, 60, 70]
        .iter()
        .map(|&pct| sample("photo-max", "jpg", 100, pct, Some(1000)))
        .collect();
    let e = estimate_with(
        &plan_of(vec![photo("/s/a.jpg", 1000)]),
        &History { samples },
        1,
    );
    assert_eq!(
        e.source,
        EstimateSource::History {
            samples: 5,
            guessed_files: 0
        }
    );
    assert_eq!(e.source_label, "from past runs (5 files)");
    let o = e.total.output_bytes;
    assert_eq!((o.low, o.mid, o.high), (400, 500, 600));
    let s = e.total.saving_bytes;
    assert_eq!((s.low, s.mid, s.high), (400, 500, 600));
}

#[test]
fn ratio_is_per_extension_then_per_class() {
    let mut h = history(3, 50, 1000);
    h.samples
        .extend((0..3).map(|_| sample("photo-max", "heic", 10 * MB, 2 * MB, Some(1000))));
    let p = plan_of(vec![
        photo("/s/a.heic", 10 * MB),
        photo("/s/b.jpg", 10 * MB),
    ]);
    let e = estimate_with(&p, &h, 1);
    // heic cell: 20%, jpg cell: 50%.
    assert_eq!(e.total.output_bytes.mid, 7 * MB);
    // An extension without a cell of its own uses the whole class.
    let p = plan_of(vec![photo("/s/c.png", 10 * MB)]);
    let e = estimate_with(&p, &h, 1);
    assert_eq!(e.total.output_bytes.mid, 3_500_000);
}

#[test]
fn too_few_samples_fall_back_to_defaults_and_say_so() {
    let h = history(2, 10, 1000);
    let e = estimate_with(&plan_of(vec![photo("/s/a.jpg", 10 * MB)]), &h, 1);
    assert_eq!(e.source, EstimateSource::Defaults);
    // Mixed: one file known, one guessed.
    let mut h = history(3, 50, 1000);
    h.samples.push(sample("video-max", "mp4", 1, 1, None));
    let p = plan_of(vec![
        photo("/s/a.jpg", MB),
        item(
            "/s/v.mp4",
            "videos",
            MB,
            &[(OutputOp::Write, &["video-max"])],
        ),
    ]);
    let e = estimate_with(&p, &h, 1);
    assert_eq!(
        e.source,
        EstimateSource::History {
            samples: 3,
            guessed_files: 1
        }
    );
    assert!(e.source_label.contains("for 1 files — rough estimate"));
}

#[test]
fn time_comes_from_item_elapsed_and_scales_with_jobs() {
    // 10 MB in 2 s: 5 MB/s.
    let h = history(4, 50, 2000);
    let p = plan_of(vec![
        photo("/s/a.jpg", 100 * MB),
        photo("/s/b.jpg", 100 * MB),
    ]);
    let one = estimate_with(&p, &h, 1).total.seconds;
    assert!((one.mid - 40.0).abs() < 1e-6);
    let four = estimate_with(&p, &h, 4).total.seconds;
    assert!((four.mid - 10.0).abs() < 1e-6);
    // jobs of 0 counts as 1.
    assert_eq!(estimate_with(&p, &h, 0).jobs, 1);
}

#[test]
fn time_range_uses_slow_and_fast_quartiles() {
    let samples = [1000, 2000, 3000, 4000, 5000]
        .iter()
        .map(|&ms| sample("photo-max", "jpg", 10 * MB, 5 * MB, Some(ms)))
        .collect();
    let e = estimate_with(
        &plan_of(vec![photo("/s/a.jpg", 10 * MB)]),
        &History { samples },
        1,
    );
    let s = e.total.seconds;
    // Throughputs 10, 5, 3.33, 2.5, 2 MB/s: p75 = 5, p25 = 2.5 MB/s.
    assert!((s.low - 2.0).abs() < 1e-6, "{s:?}");
    assert!((s.high - 4.0).abs() < 1e-6, "{s:?}");
    assert!(s.low <= s.mid && s.mid <= s.high);
}

#[test]
fn fates_and_totals_per_target() {
    use OutputOp::*;
    let p = plan_of(vec![
        photo("/s/a.jpg", 10),
        item("/s/b.jpg", "other", 10, &[(Rename, &[])]),
        item("/s/c.jpg", "other", 10, &[(Keep, &[])]),
        item("/s/d.jpg", "other", 10, &[(Write, &[]), (Rename, &[])]),
    ]);
    let e = estimate_with(&p, &History::default(), 1);
    assert_eq!(e.targets.len(), 2);
    let other = e.targets.iter().find(|t| t.name == "other").unwrap();
    assert_eq!(
        (other.files, other.trash, other.renamed, other.kept),
        (3, 0, 2, 1)
    );
    // Renamed/kept sources stay; a copy next to a rename adds its bytes.
    assert_eq!(other.saving_bytes.mid, -10);
    let t = &e.total;
    assert_eq!((t.files, t.trash, t.renamed, t.kept), (4, 1, 2, 1));
    assert_eq!(t.input_bytes, 40);
    assert_eq!(e.targets.iter().map(|t| t.files).sum::<usize>(), 4);
}

fn report_of(plan: &Plan, ms: u64, size_pct: u64) -> Report {
    Report {
        run_id: "r".into(),
        plan: "/none".into(),
        started: String::new(),
        finished: "f".into(),
        items: plan
            .items
            .iter()
            .map(|it| ItemResult {
                source: it.source.clone(),
                status: ItemStatus::Done { trashed: None },
                outputs: it
                    .outputs
                    .iter()
                    .map(|o| OutputResult {
                        planned: o.dest.clone(),
                        actual: Some(o.dest.clone()),
                        error: None,
                        warnings: vec![],
                        fell_back: false,
                        size: Some(it.size * size_pct / 100),
                        hash: None,
                        renamed_from: None,
                        anomalies: vec![],
                    })
                    .collect(),
                error: None,
                elapsed_ms: Some(ms),
            })
            .collect(),
        tool_versions: Default::default(),
        tool_warnings: vec![],
        undone: false,
        undone_sources: vec![],
        cancelled: false,
        stopped_by_anomalies: false,
        kept: 0,
    }
}

#[test]
fn samples_come_from_a_report_and_its_plan() {
    let p = plan_of(vec![
        photo("/s/a.JPG", 1000),
        photo("/s/b.jpg", 2000),
        item("/s/c.jpg", "x", 5, &[(OutputOp::Rename, &[])]),
    ]);
    let mut r = report_of(&p, 600, 40);
    // A failed item and an item without elapsed time.
    r.items[1].status = ItemStatus::Failed { trashed: None };
    r.items.push(ItemResult {
        source: "/s/unknown.jpg".into(),
        ..r.items[0].clone()
    });
    let mut h = History::default();
    h.add_report(&p, &r);
    assert_eq!(h.samples.len(), 1);
    let s = &h.samples[0];
    assert_eq!(
        (
            s.class.as_str(),
            s.kind,
            s.ext.as_str(),
            s.input,
            s.output,
            s.elapsed_ms
        ),
        ("photo-max", Kind::Photo, "jpg", 1000, 400, Some(600))
    );
    // An old report without elapsed_ms still gives size samples.
    r.items[0].elapsed_ms = None;
    let mut h = History::default();
    h.add_report(&p, &r);
    assert_eq!(h.samples[0].elapsed_ms, None);
}

#[test]
fn load_dir_reads_reports_next_to_plans_and_skips_junk() {
    let d = tempfile::tempdir().unwrap();
    let p = plan_of(vec![photo("/s/a.jpg", 1000)]);
    p.save(&d.path().join("20260101-000000.plan.json")).unwrap();
    let r = report_of(&p, 500, 50);
    std::fs::write(
        d.path().join("20260101-000000.report.json"),
        serde_json::to_string(&r).unwrap(),
    )
    .unwrap();
    std::fs::write(
        d.path().join("20260101-000000.report.old.json"),
        serde_json::to_string(&r).unwrap(),
    )
    .unwrap();
    std::fs::write(d.path().join("20260102-000000.report.json"), "{broken").unwrap();
    // A report whose plan is gone is skipped.
    std::fs::write(
        d.path().join("20260103-000000.report.json"),
        serde_json::to_string(&r).unwrap(),
    )
    .unwrap();
    let h = History::load_dir(d.path());
    assert_eq!(h.samples.len(), 2);
    assert!(
        History::load_dir(&d.path().join("missing"))
            .samples
            .is_empty()
    );
}

#[test]
fn estimate_is_read_only() {
    let p = plan_of(vec![photo("/s/a.jpg", 10)]);
    let before = serde_json::to_string(&p).unwrap();
    let _ = estimate(&p, &History::default());
    assert_eq!(serde_json::to_string(&p).unwrap(), before);
}

const SONNET: &str = "claude-sonnet-5-5";

fn dry(cached: usize, to_send: usize) -> AiDry {
    AiDry {
        candidates: cached + to_send,
        cached,
        to_send,
    }
}

#[test]
fn ai_cost_from_defaults() {
    let e = ai_estimate(&dry(30, 100), SONNET, &[]);
    assert_eq!(e.source_label, "rough estimate");
    // 1500 in, 80 out, 700 cache read per request at Sonnet prices.
    let per = (1500.0 * 2.0 + 80.0 * 10.0 + 700.0 * 0.2) / 1e6;
    let r = e.usd.unwrap();
    assert!((r.mid - per * 100.0).abs() < 1e-9);
    assert!(r.low < r.mid && r.mid < r.high);
    assert_eq!(e.label, format!("≈ ${:.2} (100 files, 30 cached)", r.mid));
}

#[test]
fn ai_cost_from_past_usage_averages_per_request() {
    let rows = vec![UsageRow {
        model: SONNET.into(),
        requests: 10,
        input: 20_000,
        output: 1_000,
        cache_write: 0,
        cache_read: 0,
    }];
    let e = ai_estimate(&dry(0, 50), SONNET, &rows);
    assert_eq!(e.source_label, "from past scans (10 requests)");
    // 2000 in + 100 out per request = $0.005.
    let r = e.usd.unwrap();
    assert!((r.mid - 0.25).abs() < 1e-9, "{r:?}");
    assert!((r.low - 0.2).abs() < 1e-9 && (r.high - 0.3125).abs() < 1e-9);
    // Rows of another model do not count.
    let e = ai_estimate(&dry(0, 50), "claude-haiku-5-5", &rows);
    assert_eq!(e.source_label, "rough estimate");
}

#[test]
fn ai_nothing_to_send_costs_nothing_and_unknown_model_has_no_price() {
    let e = ai_estimate(&dry(5, 0), SONNET, &[]);
    assert_eq!(e.usd.unwrap().mid, 0.0);
    assert_eq!(e.label, "≈ $0.00 (0 files, 5 cached)");
    let e = ai_estimate(&dry(0, 3), "mystery", &[]);
    assert!(e.usd.is_none());
    assert!(e.label.contains("model price unknown"));
}

#[test]
fn ai_scan_over_a_dollar_asks_for_confirmation() {
    // About $0.0039 per request by default: 200 files stay under $1, 300 pass.
    assert!(!ai_estimate(&dry(0, 100), SONNET, &[]).confirm);
    assert!(!ai_estimate(&dry(0, 200), SONNET, &[]).confirm);
    assert!(ai_estimate(&dry(0, 300), SONNET, &[]).confirm);
    // An unknown price never blocks.
    assert!(!ai_estimate(&dry(0, 10_000), "mystery", &[]).confirm);
}

#[test]
fn trash_items_count_as_to_trash_and_keep_items_as_kept() {
    let mut junk = item("/s/junk.jpg", "Junk", 3 * MB, &[]);
    junk.kind = crate::config::TargetKind::Trash;
    let mut p = plan_of(vec![junk, photo("/s/a.jpg", 10 * MB)]);
    p.kept = vec![crate::plan::KeptItem {
        source: "/s/k.jpg".into(),
        target: "Keep".into(),
    }];
    let e = estimate_with(&p, &History::default(), 1);
    let t = |n: &str| e.targets.iter().find(|t| t.name == n).unwrap();
    let junk = t("Junk");
    assert_eq!((junk.files, junk.trash, junk.kept), (1, 1, 0));
    // Nothing is written for it: all of it is freed, in no time.
    assert_eq!(junk.output_bytes.high, 0);
    assert_eq!(junk.saving_bytes.low, 3 * MB as i64);
    assert_eq!(junk.seconds.high, 0.0);
    let keep = t("Keep");
    assert_eq!((keep.files, keep.trash, keep.kept), (0, 0, 1));
    assert_eq!((e.total.trash, e.total.kept), (2, 1));
}
