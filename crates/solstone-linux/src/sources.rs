// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceDescriptor {
    pub index: u32,
    pub name: Option<String>,
    pub monitor_of_sink: Option<u32>,
    pub monitor_of_sink_name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceSelection {
    pub microphone: SourceDescriptor,
    pub monitor: SourceDescriptor,
    pub monitor_matches_default_sink: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceSelectionError {
    MissingMicrophone,
    MissingMonitor,
    MissingBoth,
    OverrideNotFound(String),
    OverrideIsMonitor(String),
}

impl std::fmt::Display for SourceSelectionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingMicrophone => formatter.write_str("missing microphone source"),
            Self::MissingMonitor => formatter.write_str("missing system monitor source"),
            Self::MissingBoth => {
                formatter.write_str("missing microphone and system monitor sources")
            }
            Self::OverrideNotFound(name) => {
                write!(formatter, "microphone override not found: {name}")
            }
            Self::OverrideIsMonitor(name) => {
                write!(formatter, "microphone override is a monitor source: {name}")
            }
        }
    }
}

pub fn classify_sources(
    sources: &[SourceDescriptor],
    default_sink_name: Option<&str>,
    microphone_override: Option<&str>,
) -> Result<SourceSelection, SourceSelectionError> {
    let monitor = sources
        .iter()
        .find(|source| source.monitor_of_sink.is_some())
        .cloned();
    let microphone = if let Some(name) = microphone_override {
        match sources
            .iter()
            .find(|source| source.name.as_deref() == Some(name))
        {
            None => return Err(SourceSelectionError::OverrideNotFound(name.into())),
            Some(source) if source.monitor_of_sink.is_some() => {
                return Err(SourceSelectionError::OverrideIsMonitor(name.into()));
            }
            Some(source) => Some(source.clone()),
        }
    } else {
        sources
            .iter()
            .find(|source| source.monitor_of_sink.is_none())
            .cloned()
    };
    let (microphone, monitor) = match (microphone, monitor) {
        (Some(microphone), Some(monitor)) => (microphone, monitor),
        (None, Some(_)) => return Err(SourceSelectionError::MissingMicrophone),
        (Some(_), None) => return Err(SourceSelectionError::MissingMonitor),
        (None, None) => return Err(SourceSelectionError::MissingBoth),
    };
    let monitor_matches_default_sink = default_sink_name.map(|default| {
        monitor
            .monitor_of_sink_name
            .as_deref()
            .is_some_and(|name| name == default)
    });
    let monitor_matches_default_sink = if monitor.monitor_of_sink_name.is_none() {
        None
    } else {
        monitor_matches_default_sink
    };
    Ok(SourceSelection {
        microphone,
        monitor,
        monitor_matches_default_sink,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn microphone(index: u32, name: &str) -> SourceDescriptor {
        SourceDescriptor {
            index,
            name: Some(name.into()),
            monitor_of_sink: None,
            monitor_of_sink_name: None,
        }
    }

    fn monitor(index: u32, name: &str, sink: Option<&str>) -> SourceDescriptor {
        SourceDescriptor {
            index,
            name: Some(name.into()),
            monitor_of_sink: Some(42),
            monitor_of_sink_name: sink.map(Into::into),
        }
    }

    #[test]
    fn selects_first_mic_and_first_monitor_structurally() {
        let sources = vec![
            microphone(1, "mic-1"),
            monitor(2, "monitor-1", Some("sink-1")),
            monitor(3, "monitor-2", Some("sink-2")),
        ];
        let selected = classify_sources(&sources, Some("sink-1"), None).unwrap();
        assert_eq!(selected.microphone.index, 1);
        assert_eq!(selected.monitor.index, 2);
        assert_eq!(selected.monitor_matches_default_sink, Some(true));
    }

    #[test]
    fn monitor_first_order_does_not_swap_roles() {
        let sources = vec![monitor(2, "monitor", Some("other")), microphone(1, "mic")];
        let selected = classify_sources(&sources, Some("default"), None).unwrap();
        assert_eq!(selected.microphone.index, 1);
        assert_eq!(selected.monitor.index, 2);
        assert_eq!(selected.monitor_matches_default_sink, Some(false));
    }

    #[test]
    fn missing_legs_are_explicit() {
        assert_eq!(
            classify_sources(&[monitor(2, "monitor", None)], None, None),
            Err(SourceSelectionError::MissingMicrophone)
        );
        assert_eq!(
            classify_sources(&[microphone(1, "mic")], None, None),
            Err(SourceSelectionError::MissingMonitor)
        );
        assert_eq!(
            classify_sources(&[], None, None),
            Err(SourceSelectionError::MissingBoth)
        );
    }

    #[test]
    fn override_selects_exact_non_monitor_name() {
        let sources = vec![
            microphone(1, "mic-1"),
            microphone(4, "mic-2"),
            monitor(2, "monitor", Some("sink")),
        ];
        assert_eq!(
            classify_sources(&sources, Some("sink"), Some("mic-2"))
                .unwrap()
                .microphone
                .index,
            4
        );
        assert_eq!(
            classify_sources(&sources, None, Some("absent")),
            Err(SourceSelectionError::OverrideNotFound("absent".into()))
        );
        assert_eq!(
            classify_sources(&sources, None, Some("monitor")),
            Err(SourceSelectionError::OverrideIsMonitor("monitor".into()))
        );
    }

    #[test]
    fn absent_monitor_sink_name_makes_comparison_unknown() {
        let selected = classify_sources(
            &[microphone(1, "mic"), monitor(2, "monitor", None)],
            Some("sink"),
            None,
        )
        .unwrap();
        assert_eq!(selected.monitor_matches_default_sink, None);
    }
}
