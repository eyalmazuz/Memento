//! Audio source entries stored under `[audio-srcs]` and `[audio-srcs/<n>]`.

use memento_config::config::Config;
use memento_config::Value;
use memento_setting::keys as raw;

use super::schema::{effective_i64, effective_value, value_text};

#[derive(Clone, Debug)]
pub struct AudioSourceEntry {
    pub name: String,
    pub url: String,
    pub kind: i64,
    pub md5: String,
}

pub(crate) fn audio_source_count(config: &Config) -> usize {
    let stored = match config.get_at(raw::audio_source::GROUP, "size") {
        Some(Value::Int(size)) if size > 0 => size as usize,
        _ => 0,
    };
    let prefix = format!("{}/", raw::audio_source::GROUP);
    let highest_group = config
        .groups_with_prefix(&prefix)
        .iter()
        .filter_map(|group| group.strip_prefix(&prefix)?.parse::<usize>().ok())
        .max()
        .unwrap_or(0);
    stored.max(highest_group)
}

pub(crate) fn audio_sources(config: &Config) -> Vec<AudioSourceEntry> {
    (1..=audio_source_count(config))
        .map(|index| {
            let group = format!("{}/{}", raw::audio_source::GROUP, index);
            AudioSourceEntry {
                name: value_text(&effective_value(config, &group, raw::audio_source::NAME)),
                url: value_text(&effective_value(config, &group, raw::audio_source::URL)),
                kind: effective_i64(config, &group, raw::audio_source::TYPE),
                md5: value_text(&effective_value(config, &group, raw::audio_source::MD5)),
            }
        })
        .collect()
}

pub(crate) fn default_audio_source() -> AudioSourceEntry {
    AudioSourceEntry {
        name: raw::audio_source::NAME_DEFAULT.to_owned(),
        url: raw::audio_source::URL_DEFAULT.to_owned(),
        kind: raw::audio_source::TYPE_DEFAULT as i64,
        md5: raw::audio_source::MD5_DEFAULT.to_owned(),
    }
}

pub(crate) fn write_audio_sources(
    config: &mut Config,
    sources: &[AudioSourceEntry],
) -> Result<(), String> {
    let groups = config.groups_with_prefix(&format!("{}/", raw::audio_source::GROUP));
    for group in groups {
        config.remove_group(&group);
    }
    config
        .put_at(
            raw::audio_source::GROUP,
            "size",
            &Value::Int(sources.len() as i64),
        )
        .map_err(|error| format!("{error:?}"))?;
    for (index, source) in sources.iter().enumerate() {
        let group = format!("{}/{}", raw::audio_source::GROUP, index + 1);
        for (key, value) in [
            (raw::audio_source::NAME, Value::Text(source.name.clone())),
            (raw::audio_source::URL, Value::Text(source.url.clone())),
            (raw::audio_source::TYPE, Value::Int(source.kind)),
            (raw::audio_source::MD5, Value::Text(source.md5.clone())),
        ] {
            config
                .put_at(&group, key, &value)
                .map_err(|error| format!("{error:?}"))?;
        }
    }
    Ok(())
}

pub(crate) fn ensure_default_audio_source(config: &mut Config) {
    if audio_source_count(config) == 0 {
        let _ = write_audio_sources(config, &[default_audio_source()]);
    }
}
