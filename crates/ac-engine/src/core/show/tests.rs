//! Tests of the show export: what a core exports after runtime changes,
//! and that a core built from that show laid over its studio exports the
//! same show again (a save and a restart, without the files).

use ac_core::{Preset, Query, QueryId, ShotSize, SourceId};
use ac_director::{ContentProfile, DirectorMode};
use ac_media::SourceInfo;
use ac_sources::SourceConfig;
use ac_switcher::{Transition, TransitionKind};

use super::*;
use crate::command::{CommandReply, EngineCommand};
use crate::config::{DetectorSettings, MonitorSettings};

/// A studio with one synthetic source and the queries `queries` (name,
/// prompt, priority), no director and no monitors.
fn studio(queries: &[(&str, &str, Option<u8>)]) -> StudioConfig {
    let mut c = StudioConfig::with_source(
        "cam",
        SourceConfig::Synthetic {
            width: 320,
            height: 180,
            fps: 30.0,
            subjects: 2,
        },
    );
    c.detector = DetectorSettings::Synthetic { score: 0.9 };
    for (name, prompt, priority) in queries {
        c.queries.push(QuerySettings {
            priority: *priority,
            ..QuerySettings::new(name, prompt)
        });
    }
    c.output.width = 320;
    c.output.height = 180;
    c.output.record_path = Some("recordings/show.mp4".into());
    c.director.mode = DirectorMode::Off;
    c.monitors = MonitorSettings::disabled();
    c
}

/// A core for `config`, its one source described as configured.
fn core(config: &StudioConfig) -> EngineCore {
    let info = SourceInfo {
        id: SourceId("cam".into()),
        width: 320,
        height: 180,
        fps: 30.0,
        audio: None,
    };
    EngineCore::new(config, vec![info], &[true]).unwrap()
}

/// Applies `cmd`, which must be accepted.
fn apply(core: &mut EngineCore, cmd: EngineCommand) -> CommandReply {
    core.apply(&cmd)
        .unwrap_or_else(|e| panic!("{}: {e}", cmd.name()))
}

/// The core a restart builds: `show` written out, read back and laid over
/// `base`, validated as a loaded file would be.
fn restart(base: &StudioConfig, show: &Show) -> EngineCore {
    let text = show.to_text(false).unwrap();
    let (read, warnings) = Show::parse_with_warnings(&text, false).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut config = base.clone();
    read.apply_to(&mut config);
    config.validate().unwrap();
    core(&config)
}

/// The priority of each live query, in order.
fn priorities(core: &EngineCore) -> Vec<u8> {
    core.queries()
        .iter()
        .map(|q| core.priorities[&q.id])
        .collect()
}

/// The priority each query of `show` gives, in order.
fn given(show: &Show) -> Vec<Option<u8>> {
    show.queries.iter().flatten().map(|q| q.priority).collect()
}

#[test]
fn a_query_added_at_runtime_is_back_after_a_save_and_restart() {
    let base = studio(&[("Pastor", "speaker upper body", None)]);
    let mut live = core(&base);
    let reply = apply(
        &mut live,
        EngineCommand::AddQuery {
            name: "Guitarist".into(),
            prompt: "guitarist close-up".into(),
            presets: None,
            literal: false,
        },
    );
    assert_eq!(
        reply,
        CommandReply::QueryAdded {
            id: QueryId("guitarist".into())
        }
    );
    apply(
        &mut live,
        EngineCommand::SetDirectorMode {
            mode: DirectorMode::Suggest,
        },
    );
    apply(
        &mut live,
        EngineCommand::SetDirectorProfile {
            profile: ContentProfile::Music,
        },
    );
    let dip = Transition {
        kind: TransitionKind::Dip,
        duration: 1.5,
    };
    apply(&mut live, EngineCommand::SetTransition { transition: dip });
    let saved = live.export_show();
    let back = restart(&base, &saved);
    let ids: Vec<&str> = back.queries().iter().map(|q| q.id.0.as_str()).collect();
    assert_eq!(ids, ["pastor", "guitarist"], "the added query is back");
    assert_eq!(
        back.queries(),
        live.queries(),
        "prompts, parses and presets"
    );
    assert_eq!(priorities(&back), priorities(&live));
    assert_eq!(priorities(&back), [5, 4]);
    let director = back.director.config();
    assert_eq!(
        (director.mode, director.profile),
        (DirectorMode::Suggest, ContentProfile::Music)
    );
    assert_eq!(back.switcher().transition(), dip);
    assert_eq!(
        back.export_show(),
        saved,
        "a second save writes the same show"
    );
}

#[test]
fn the_export_holds_the_studio_parts_no_command_changes() {
    let base = studio(&[("Pastor", "person", None)]);
    let show = core(&base).export_show();
    assert_eq!(show.show, SHOW_VERSION);
    assert_eq!(show.studio, None, "the caller names the studio file");
    assert_eq!(show.sources.as_ref(), Some(&base.sources));
    assert_eq!(
        show.output,
        ShowOutput {
            rtmp_url: None,
            record_path: Some("recordings/show.mp4".into()),
        }
    );
    assert_eq!(show.director.mode, Some(DirectorMode::Off));
    assert_eq!(show.switcher.transition, Some(TransitionKind::Mix));
}

#[test]
fn exporting_the_show_is_a_command_that_changes_nothing() {
    let base = studio(&[("Pastor", "person", None)]);
    let mut live = core(&base);
    let before = format!("{:?}", live.queries());
    let reply = apply(&mut live, EngineCommand::ExportShow);
    assert_eq!(reply, CommandReply::Show(Box::new(live.export_show())));
    assert_eq!(format!("{:?}", live.queries()), before);
    assert_eq!(live.switcher().preview(), None);
}

#[test]
fn queries_are_written_as_a_config_would_ask_for_them() {
    let mut base = studio(&[]);
    let mut tall = Preset::new("tall", "Tall", ShotSize::MediumClose);
    tall.headroom = 0.2;
    base.queries.push(QuerySettings {
        id: Some("lead".into()),
        presets: Some(vec![
            PresetSpec::Size(ShotSize::Medium),
            PresetSpec::Full(tall.clone()),
        ]),
        ..QuerySettings::new("Pastor", "person")
    });
    base.queries.push(QuerySettings {
        literal: true,
        ..QuerySettings::new("Drums", "drummer wide")
    });
    let show = core(&base).export_show();
    let queries = show.queries.clone().unwrap();
    assert_eq!(
        queries[0].id.as_deref(),
        Some("lead"),
        "not the slug of its name"
    );
    assert_eq!(queries[1].id, None, "the slug of its name");
    assert_eq!(
        queries[0].presets,
        Some(vec![
            PresetSpec::Size(ShotSize::Medium),
            PresetSpec::Full(tall)
        ])
    );
    assert!(queries[1].literal && !queries[0].literal);
    assert_eq!(
        queries[1].presets,
        Some(vec![
            PresetSpec::Size(ShotSize::Medium),
            PresetSpec::Size(ShotSize::Close)
        ]),
        "a literal prompt has no shot words: medium and close"
    );
    let back = restart(&base, &show);
    assert_eq!(back.queries(), core(&base).queries());
}

#[test]
fn shot_word_presets_are_kept_whatever_the_lexicon_says_later() {
    let base = studio(&[("Drummer", "drummer wide", None)]);
    let show = core(&base).export_show();
    let presets = show.queries.as_ref().unwrap()[0].presets.clone();
    assert_eq!(presets, Some(vec![PresetSpec::Size(ShotSize::Full)]));
    // A studio whose lexicon reads "wide" as close-up still frames Full.
    let mut changed = base.clone();
    changed.lexicon =
        toml::from_str("[[shots]]\nphrase = \"wide\"\nsizes = [\"close\"]\n").unwrap();
    let back = restart(&changed, &show);
    let sizes: Vec<ShotSize> = back.queries()[0].presets.iter().map(|p| p.size).collect();
    assert_eq!(sizes, [ShotSize::Full]);
}

#[test]
fn priorities_stay_implicit_while_list_order_gives_them_back() {
    let base = studio(&[
        ("Pastor", "person", None),
        ("Leader", "person", None),
        ("Band", "person", None),
    ]);
    let mut live = core(&base);
    assert_eq!(given(&live.export_show()), [None, None, None]);
    apply(
        &mut live,
        EngineCommand::AddQuery {
            name: "Choir".into(),
            prompt: "person".into(),
            presets: None,
            literal: false,
        },
    );
    assert_eq!(priorities(&live), [5, 4, 3, 2]);
    assert_eq!(
        given(&live.export_show()),
        [None; 4],
        "appended in list order"
    );
    // Removing the last query keeps list order.
    let choir = QueryId("choir".into());
    apply(&mut live, EngineCommand::RemoveQuery { query: choir });
    assert_eq!(given(&live.export_show()), [None; 3]);
    // Removing the first does not: each query keeps its own.
    let pastor = QueryId("pastor".into());
    apply(&mut live, EngineCommand::RemoveQuery { query: pastor });
    let show = live.export_show();
    assert_eq!(given(&show), [Some(4), Some(3)]);
    let back = restart(&base, &show);
    assert_eq!(priorities(&back), [4, 3]);
    assert_eq!(back.export_show(), show);
}

#[test]
fn given_priorities_are_written_for_every_query() {
    let base = studio(&[("Pastor", "person", Some(5)), ("Band", "person", None)]);
    let mut live = core(&base);
    apply(
        &mut live,
        EngineCommand::AddQuery {
            name: "Choir".into(),
            prompt: "person".into(),
            presets: None,
            literal: false,
        },
    );
    let show = live.export_show();
    assert_eq!(given(&show), [Some(5), Some(3), Some(3)]);
    let back = restart(&base, &show);
    assert_eq!(priorities(&back), priorities(&live));
    let q: &Query = &back.queries()[2];
    assert_eq!(q.name, "Choir");
}
