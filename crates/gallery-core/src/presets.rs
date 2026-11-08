//! Built-in (system) presets: always available, never stored in a config file.
//!
//! A user preset may not reuse a built-in name (see `Config::validate`), so a
//! name always means the same thing on every machine.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use crate::config::PipeStep;
use crate::config::{
    AudioPreset, BundlePreset, ContentPreset, DateSource, MetadataPreset, NamePreset, PhotoPreset,
    PngPreset, VideoPreset,
};
use crate::mask::{MaskExpr, MaskItem};

/// Already named by date (`YYYYMMDD_HHMMSS…`): leave the name as it is.
fn dated_skip() -> Option<MaskExpr> {
    Some(MaskExpr::parse("@dated").expect("built-in skip_if is a valid mask"))
}

fn name_preset(
    template: &str,
    date: DateSource,
    skip_if: Option<MaskExpr>,
    normalize_ext: bool,
) -> NamePreset {
    NamePreset {
        mask: None,
        template: template.into(),
        date,
        skip_if,
        normalize_ext,
    }
}

static NAME: LazyLock<BTreeMap<String, NamePreset>> = LazyLock::new(|| {
    let by_date = "{date:YYYYMMDD_HHMMSS}.{ext}";
    [
        (
            "capture-date",
            name_preset(by_date, DateSource::Capture, dated_skip(), true),
        ),
        (
            "file-date",
            name_preset(by_date, DateSource::FileCreated, dated_skip(), true),
        ),
        (
            // No skip_if: a file already named by date still goes to its folder.
            "capture-date-dirs",
            name_preset(
                "{date:YYYY}/{date:MM}/{date:YYYYMMDD_HHMMSS}.{ext}",
                DateSource::Capture,
                None,
                true,
            ),
        ),
        (
            "capture-date-name",
            name_preset(
                "{date:YYYYMMDD_HHMMSS}_{stem}.{ext}",
                DateSource::Capture,
                dated_skip(),
                true,
            ),
        ),
        (
            "ext-lower",
            name_preset("{stem}.{ext}", DateSource::None, None, true),
        ),
    ]
    .into_iter()
    .map(|(n, p)| (n.to_string(), p))
    .collect()
});

static CONTENT: LazyLock<BTreeMap<String, ContentPreset>> = LazyLock::new(|| {
    use ContentPreset::{Audio, Metadata, Photo, Png, Video};
    let bundle = |steps: &[&str]| {
        ContentPreset::Bundle(BundlePreset {
            mask: None,
            steps: steps.iter().map(|s| PipeStep::named(s)).collect(),
        })
    };
    let list = [
        ("meta-full", Metadata(MetadataPreset::full())),
        ("meta-dates", Metadata(MetadataPreset::dates_only())),
        ("avif-low", Photo(PhotoPreset::avif_low())),
        ("avif-medium", Photo(PhotoPreset::avif_medium())),
        ("avif-high", Photo(PhotoPreset::avif_high())),
        ("avif-max", Photo(PhotoPreset::avif_max())),
        ("avif-fast", Photo(PhotoPreset::avif_fast())),
        ("webp-low", Photo(PhotoPreset::webp_low())),
        ("webp-medium", Photo(PhotoPreset::webp_medium())),
        ("webp-high", Photo(PhotoPreset::webp_high())),
        ("webp-max", Photo(PhotoPreset::webp_max())),
        ("av1-low", Video(VideoPreset::low())),
        ("av1-medium", Video(VideoPreset::medium())),
        ("av1-high", Video(VideoPreset::high())),
        ("av1-max", Video(VideoPreset::max())),
        ("av1-fast", Video(VideoPreset::fast())),
        ("av1-action-cam", Video(VideoPreset::action_cam())),
        ("opus-64", Audio(AudioPreset::opus(64))),
        ("opus-96", Audio(AudioPreset::opus(96))),
        ("opus-128", Audio(AudioPreset::opus(128))),
        ("opus-192", Audio(AudioPreset::opus(192))),
        ("mp3-128", Audio(AudioPreset::mp3(128))),
        ("mp3-192", Audio(AudioPreset::mp3(192))),
        ("mp3-256", Audio(AudioPreset::mp3(256))),
        ("mp3-320", Audio(AudioPreset::mp3(320))),
        ("aac-96", Audio(AudioPreset::aac(96))),
        ("aac-128", Audio(AudioPreset::aac(128))),
        ("aac-192", Audio(AudioPreset::aac(192))),
        ("aac-256", Audio(AudioPreset::aac(256))),
        ("flac", Audio(AudioPreset::flac())),
        ("png-optipng", Png(PngPreset::default())),
        ("png-avif", Photo(PhotoPreset::png_avif())),
        ("webp-avif", Photo(PhotoPreset::webp_avif())),
        // One encoder per file type: photos, PNG, WebP, video.
        (
            "compress-all",
            bundle(&["avif-high", "png-avif", "webp-avif", "av1-high"]),
        ),
        (
            "compress-photos",
            bundle(&["avif-high", "png-avif", "webp-avif"]),
        ),
        (
            "compress-fast",
            bundle(&["avif-fast", "png-avif", "webp-avif", "av1-fast"]),
        ),
    ];
    list.into_iter().map(|(n, p)| (n.to_string(), p)).collect()
});

/// The new name of a built-in preset that was renamed to English kebab-case.
pub fn renamed_builtin(old: &str) -> Option<&'static str> {
    // The old names are Russian on purpose: they are what existing configs hold.
    const OLD: [(&str, &str); 6] = [
        ("Дата съёмки", "capture-date"),
        ("Дата создания файла", "file-date"),
        ("Дата с папками", "capture-date-dirs"),
        ("Дата и имя", "capture-date-name"),
        ("Расширение", "ext-lower"),
        ("Полный", "meta-full"),
    ];
    if let Some((_, new)) = OLD.iter().find(|(o, _)| *o == old) {
        return Some(new);
    }
    match old {
        "Только даты" => Some("meta-dates"),
        "AVIF быстро" => Some("avif-fast"),
        "AV1 быстро" => Some("av1-fast"),
        "Экшн-камера" => Some("av1-action-cam"),
        "PNG optipng" => Some("png-optipng"),
        _ => {
            // "AVIF high", "Opus 128": kind and level joined by a dash, in lower case.
            let (kind, level) = old.split_once(' ')?;
            let known = ["AVIF", "WebP", "AV1", "Opus", "MP3", "AAC"].contains(&kind);
            let new = format!("{}-{}", kind.to_lowercase(), level);
            (known && builtin_content_preset(&new).is_some())
                .then(|| builtin_content_preset_name(&new))
                .flatten()
        }
    }
}

fn builtin_content_preset_name(name: &str) -> Option<&'static str> {
    CONTENT.keys().find(|k| *k == name).map(String::as_str)
}

/// A built-in mask: alternatives (OR), each a pattern or `@name`.
fn mask_expr(items: &[&str]) -> MaskExpr {
    MaskExpr(
        items
            .iter()
            .map(|s| MaskItem::parse(s).expect("built-in mask is valid"))
            .collect(),
    )
}

static MASKS: LazyLock<BTreeMap<String, MaskExpr>> = LazyLock::new(|| {
    let list: [(&str, &[&str]); 17] = [
        (
            "photo",
            &["*.{jpg,jpeg,heic,heif,webp,avif,tif,tiff,bmp,gif}"],
        ),
        ("photo-original", &["*.{jpg,jpeg,heic,heif}"]),
        ("video", &["*.{mp4,m4v,mov,mkv,webm,avi,3gp}"]),
        // av1-* presets write `*.av1.mp4`; never take their own output again.
        ("video-original", &["*.{mp4,m4v,mov,mkv}", "!*.av1.mp4"]),
        ("raw", &["*.{cr2,cr3,nef,arw,dng,raf,orf,rw2}"]),
        ("audio", &["*.{mp3,m4a,aac,flac,wav,ogg,opus,wma,ape,alac}"]),
        ("png", &["*.png"]),
        ("media", &["@photo", "@video", "@png"]),
        (
            "screenshot",
            &[
                "*screenshot*.{png,jpg,jpeg,webp}",
                "{Знімок екрана*,Снимок экрана*}.{png,jpg,jpeg,webp}",
            ],
        ),
        (
            "screen-recording",
            &["{screen-*,*screenrecord*,*screen_record*,*screencast*}.{mp4,webm,mkv,mov}"],
        ),
        (
            "tiktok",
            &[r"^[0-9a-f]{32}(_[0-9]+)?\.(mp4|png|webp|jpg|jpeg)$"],
        ),
        // Already named by capture date.
        ("dated", &["^[0-9]{8}_[0-9]{6}"]),
        ("whatsapp", &["^(IMG|VID|AUD|PTT)-[0-9]{8}-WA[0-9]+"]),
        // Messenger and app names made of an epoch timestamp.
        ("epoch-name", &[r"^([0-9]{13}|[0-9]{10})([-_][0-9]+)?\."]),
        ("camera-phone", &["^(IMG|VID|PXL|MVIMG)_[0-9]{8}_[0-9]{6}"]),
        ("edited", &["*{-edited,_edited}*"]),
        // Duplicate and download-copy markers: not edits.
        ("copy", &["*{(1),(2),(3),_copy,-copy,- copy}*"]),
    ];
    list.into_iter()
        .map(|(n, items)| (n.to_string(), mask_expr(items)))
        .collect()
});

/// The built-in named masks, by name.
pub fn builtin_masks() -> &'static BTreeMap<String, MaskExpr> {
    &MASKS
}

/// One built-in named mask.
pub fn builtin_mask(name: &str) -> Option<&'static MaskExpr> {
    MASKS.get(name)
}

/// The built-in name presets, by name.
pub fn builtin_name_presets() -> BTreeMap<String, NamePreset> {
    NAME.clone()
}

/// The built-in content presets, by name.
pub fn builtin_content_presets() -> BTreeMap<String, ContentPreset> {
    CONTENT.clone()
}

/// One built-in name preset, without copying the whole set.
pub fn builtin_name_preset(name: &str) -> Option<&'static NamePreset> {
    NAME.get(name)
}

/// One built-in content preset, without copying the whole set.
pub fn builtin_content_preset(name: &str) -> Option<&'static ContentPreset> {
    CONTENT.get(name)
}

/// One line of key parameters of a content preset, for `gallery presets`.
pub fn describe_content(p: &ContentPreset) -> String {
    match p {
        ContentPreset::Metadata(m) => {
            let on = [
                (m.write_capture_date, crate::t!("preset.m_capture_date")),
                (m.fix_gcam_make_model, "Make/Model GCam".to_string()),
                (m.strip_gcamera_xmp, "XMP-GCamera".to_string()),
                (
                    m.strip_gain_map_xmp_for_avif,
                    crate::t!("preset.m_gain_map"),
                ),
                (m.fill_offset_time, "OffsetTime".to_string()),
                (m.video_make_model, crate::t!("preset.m_video_make_model")),
                (m.video_restore_gps, crate::t!("preset.m_video_gps")),
            ];
            let names: Vec<_> = on
                .iter()
                .filter(|(f, _)| *f)
                .map(|(_, n)| n.as_str())
                .collect();
            crate::t!("preset.metadata", names = names.join(", "))
        }
        ContentPreset::Photo(p) => {
            let (fmt, speed) = match p.format {
                crate::config::PhotoFormat::Avif => ("AVIF", crate::t!("preset.speed")),
                crate::config::PhotoFormat::Webp => ("WebP", crate::t!("preset.method")),
            };
            crate::t!(
                "preset.photo",
                fmt = fmt,
                quality = p.quality,
                speed = speed,
                level = p.speed
            )
        }
        ContentPreset::Video(v) => {
            let mut s = crate::t!("preset.video", crf = v.crf, preset = v.preset);
            if v.stabilize {
                s.push_str(&crate::t!("preset.stabilization"));
            }
            if let Some(d) = v.denoise {
                s.push_str(&crate::t!(
                    "preset.denoise",
                    level = format!("{d:?}").to_lowercase()
                ));
            }
            s
        }
        ContentPreset::Audio(a) => match a.kbps {
            Some(k) => crate::t!(
                "preset.kbps",
                codec = format!("{:?}", a.codec).to_lowercase(),
                kbps = k
            ),
            None => crate::t!(
                "preset.lossless",
                codec = format!("{:?}", a.codec).to_lowercase()
            ),
        },
        ContentPreset::Png(p) => format!("optipng -o{}", p.level),
        ContentPreset::Bundle(b) => {
            let names: Vec<_> = b.steps.iter().map(|s| s.preset.as_str()).collect();
            crate::t!("preset.bundle", names = names.join(" + "))
        }
    }
}

/// One line of key parameters of a name preset, for `gallery presets`.
pub fn describe_name(p: &NamePreset) -> String {
    let date = match p.date {
        DateSource::Capture => crate::t!("preset.date_capture"),
        DateSource::FileCreated => crate::t!("preset.date_created"),
        DateSource::None => crate::t!("preset.date_none"),
    };
    let mut s = format!("{} ({date}", p.template);
    if p.skip_if.is_some() {
        s.push_str(&crate::t!("preset.keep_names"));
    }
    if p.normalize_ext {
        s.push_str(&crate::t!("preset.lower_ext"));
    }
    s.push(')');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_set_is_exact() {
        let names: Vec<_> = builtin_name_presets().into_keys().collect();
        let mut want = vec![
            "capture-date",
            "file-date",
            "capture-date-dirs",
            "capture-date-name",
            "ext-lower",
        ];
        want.sort();
        assert_eq!(names, want);
    }

    #[test]
    fn content_set_is_exact() {
        let names: Vec<_> = builtin_content_presets().into_keys().collect();
        let mut want = vec![
            "meta-full",
            "meta-dates",
            "avif-low",
            "avif-medium",
            "avif-high",
            "avif-max",
            "avif-fast",
            "webp-low",
            "webp-medium",
            "webp-high",
            "webp-max",
            "av1-low",
            "av1-medium",
            "av1-high",
            "av1-max",
            "av1-fast",
            "av1-action-cam",
            "opus-64",
            "opus-96",
            "opus-128",
            "opus-192",
            "mp3-128",
            "mp3-192",
            "mp3-256",
            "mp3-320",
            "aac-96",
            "aac-128",
            "aac-192",
            "aac-256",
            "flac",
            "png-optipng",
            "png-avif",
            "webp-avif",
            "compress-all",
            "compress-photos",
            "compress-fast",
        ];
        want.sort();
        assert_eq!(names, want);
    }

    #[test]
    fn png_and_webp_to_avif_presets_take_only_their_format() {
        let c = builtin_content_presets();
        for (name, yes, no) in [
            ("png-avif", "Screenshot 1.PNG", "a.jpg"),
            ("webp-avif", "a.WebP", "a.png"),
        ] {
            let ContentPreset::Photo(p) = &c[name] else {
                panic!("{name} is a photo preset");
            };
            assert_eq!(p.format, crate::config::PhotoFormat::Avif);
            let m = c[name].mask_or_default();
            assert!(
                m.matches(yes, &crate::config::Config::default()),
                "{name} takes {yes}"
            );
            assert!(
                !m.matches(no, &crate::config::Config::default()),
                "{name} skips {no}"
            );
        }
    }

    #[test]
    fn pattern_masks_match_and_reject() {
        use crate::mask::MaskExpr;
        let cases: [(&str, &[&str], &[&str]); 9] = [
            (
                "screenshot",
                &[
                    "Screenshot_20260101-120000.png",
                    "Знімок екрана 2026-11-03 182219.png",
                    "Снимок экрана 2026-11-03.JPG",
                ],
                &["IMG_1.jpg", "Screenshot_1.mp4"],
            ),
            (
                "screen-recording",
                &[
                    "screen-20260101.mp4",
                    "Screenrecord_1.MP4",
                    "my_screencast.webm",
                ],
                &["IMG_1.mp4", "screen-1.jpg"],
            ),
            (
                "tiktok",
                &[
                    "0123456789abcdef0123456789abcdef.mp4",
                    "0123456789ABCDEF0123456789abcdef_2.jpeg",
                ],
                &[
                    "0123456789abcdef.mp4",
                    "0123456789abcdef0123456789abcdef.gif",
                ],
            ),
            (
                "dated",
                &["20260412_103314.jpg", "20260412_103314_x.mp4"],
                &["IMG_20260412_103314.jpg", "2026-04-12.jpg"],
            ),
            (
                "whatsapp",
                &["IMG-20260412-WA0003.jpg", "PTT-20260412-WA0001.opus"],
                &["IMG_20260412_103314.jpg", "IMG-20260412-X0003.jpg"],
            ),
            (
                "epoch-name",
                &["1481122477728.mp4", "1234567890.jpg", "1481122477728_2.jpg"],
                &["123456789.jpg", "12345678901234.jpg"],
            ),
            (
                "camera-phone",
                &[
                    "IMG_20260412_103314.jpg",
                    "PXL_20260412_103314123.mp4",
                    "mvimg_20260412_103314.jpg",
                ],
                &["IMG-20260412-WA0003.jpg", "20260412_103314.jpg"],
            ),
            (
                "edited",
                &["a-edited.jpg", "a_edited.png"],
                &["a.jpg", "a (1).jpg", "a~2.jpg"],
            ),
            (
                "copy",
                &[
                    "a (1).jpg",
                    "a(2).jpg",
                    "a_copy.png",
                    "a - Copy.jpg",
                    "a-copy.mp4",
                ],
                &["a.jpg", "a-edited.jpg", "a~2.jpg"],
            ),
        ];
        let scope = crate::config::Config::default();
        for (name, yes, no) in cases {
            let e = MaskExpr::parse(&format!("@{name}")).unwrap();
            for n in yes {
                assert!(e.matches(n, &scope), "@{name} should match {n}");
            }
            for n in no {
                assert!(!e.matches(n, &scope), "@{name} should reject {n}");
            }
        }
    }

    #[test]
    fn video_original_skips_av1_output() {
        let scope = crate::config::Config::default();
        let e = MaskExpr::parse("@video-original").unwrap();
        for n in ["a.mp4", "A.MOV", "a.mkv", "a.m4v", "a.av1.mkv"] {
            assert!(e.matches(n, &scope), "{n}");
        }
        for n in ["a.av1.mp4", "a.AV1.MP4", "a.webm"] {
            assert!(!e.matches(n, &scope), "{n}");
        }
    }

    #[test]
    fn old_builtin_names_point_to_the_new_ones() {
        for (old, new) in [
            ("AVIF high", "avif-high"),
            ("Opus 128", "opus-128"),
            ("AV1 быстро", "av1-fast"),
            ("Полный", "meta-full"),
            ("Дата съёмки", "capture-date"),
            ("Экшн-камера", "av1-action-cam"),
        ] {
            assert_eq!(renamed_builtin(old), Some(new), "{old}");
        }
        assert_eq!(renamed_builtin("AVIF turbo"), None);
        assert_eq!(renamed_builtin("my preset"), None);
    }

    #[test]
    fn spot_values() {
        let n = builtin_name_presets();
        let f = &n["capture-date-dirs"];
        assert_eq!(
            f.template,
            "{date:YYYY}/{date:MM}/{date:YYYYMMDD_HHMMSS}.{ext}"
        );
        assert!(f.skip_if.is_none());
        let s = &n["capture-date"];
        assert_eq!(s.date, DateSource::Capture);
        assert!(s.skip_if.is_some() && s.normalize_ext);
        assert_eq!(n["file-date"].date, DateSource::FileCreated);
        assert_eq!(n["ext-lower"].date, DateSource::None);
        assert!(n["ext-lower"].skip_if.is_none());

        let c = builtin_content_presets();
        let ContentPreset::Photo(p) = &c["avif-high"] else {
            panic!("avif-high is a photo preset")
        };
        assert_eq!((p.quality, p.speed), (72, 6));
        let ContentPreset::Video(v) = &c["av1-high"] else {
            panic!("av1-high is a video preset")
        };
        assert_eq!((v.crf, v.preset), (30, 3));
    }
}
