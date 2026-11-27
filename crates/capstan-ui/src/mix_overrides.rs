//! 按曲目对保存的混音计划覆盖，持久化至 `mix_overrides.json`。

use std::collections::HashMap;

use capstan_audio::MixPlan;
use capstan_core::TrackId;
use serde::{Deserialize, Serialize};

use crate::persist;
use crate::platform::*;

const FILE: &str = "mix_overrides.json";

/// 按 `(前曲, 后曲)` 索引的手动混音覆盖表。
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MixOverrides {
    entries: HashMap<String, MixPlan>,
}

impl MixOverrides {
    pub fn load() -> Self {
        persist::load_or_default(FILE)
    }

    pub fn save(&self) {
        persist::save_lossy(FILE, self.clone());
    }

    fn key(a: TrackId, b: TrackId) -> String {
        format!("{a}-{b}")
    }

    pub fn get(&self, a: TrackId, b: TrackId) -> Option<&MixPlan> {
        self.entries.get(&Self::key(a, b))
    }

    pub fn contains(&self, a: TrackId, b: TrackId) -> bool {
        self.entries.contains_key(&Self::key(a, b))
    }

    pub fn set(&mut self, a: TrackId, b: TrackId, plan: MixPlan) {
        self.entries.insert(Self::key(a, b), plan);
    }

    pub fn remove(&mut self, a: TrackId, b: TrackId) -> bool {
        self.entries.remove(&Self::key(a, b)).is_some()
    }
}

/// 混音覆盖表的实体封装，修改自动落盘并通知订阅者刷新预载。
pub struct MixOverridesEntity {
    store: MixOverrides,
}

impl MixOverridesEntity {
    pub fn new(store: MixOverrides) -> Self {
        Self { store }
    }

    pub fn get(&self, a: TrackId, b: TrackId) -> Option<&MixPlan> {
        self.store.get(a, b)
    }

    pub fn contains(&self, a: TrackId, b: TrackId) -> bool {
        self.store.contains(a, b)
    }

    pub fn set(&mut self, a: TrackId, b: TrackId, plan: MixPlan, cx: &mut Context<Self>) {
        self.store.set(a, b, plan);
        self.store.save();
        cx.notify();
    }

    pub fn remove(&mut self, a: TrackId, b: TrackId, cx: &mut Context<Self>) -> bool {
        if self.store.remove(a, b) {
            self.store.save();
            cx.notify();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::MixOverrides;
    use capstan_audio::{Automation, CurvePreset, MixKind, MixPlan};
    use capstan_core::TrackId;
    use std::time::Duration;

    #[test]
    fn overrides_round_trip_by_ordered_track_pair() {
        let mut store = MixOverrides::default();
        let a = TrackId(1);
        let b = TrackId(2);
        let plan = MixPlan {
            kind: MixKind::BeatMatched,
            out_start: Duration::from_secs(120),
            out_end: Duration::from_secs(136),
            in_start: Duration::from_secs(4),
            a_gain: Automation::preset_out(CurvePreset::EqualPower),
            b_gain: Automation::preset_in(CurvePreset::EqualPower),
            ..MixPlan::default()
        };

        store.set(a, b, plan.clone());
        assert_eq!(store.get(a, b), Some(&plan));
        assert_eq!(store.get(b, a), None, "歌曲对覆盖必须区分前后顺序");

        let json = serde_json::to_string(&store).unwrap();
        let decoded: MixOverrides = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.get(a, b), Some(&plan));
    }

    #[test]
    fn legacy_override_json_without_eq_or_fx_fields_loads_cleanly() {
        let key = MixOverrides::key(TrackId(1), TrackId(2));
        let raw = format!(
            r#"{{
            "entries": {{
                "{key}": {{
                    "kind": "Crossfade",
                    "out_start": {{ "secs": 100, "nanos": 0 }},
                    "out_end": {{ "secs": 106, "nanos": 0 }},
                    "in_start": {{ "secs": 0, "nanos": 0 }},
                    "a_gain": {{ "points": [{{ "t": 0.0, "value": 1.0 }}, {{ "t": 1.0, "value": 0.0 }}] }},
                    "b_gain": {{ "points": [{{ "t": 0.0, "value": 0.0 }}, {{ "t": 1.0, "value": 1.0 }}] }}
                }}
            }}
        }}"#
        );
        let decoded: MixOverrides = serde_json::from_str(&raw).unwrap();
        let plan = decoded.get(TrackId(1), TrackId(2)).unwrap();
        assert_eq!(plan.kind, MixKind::Crossfade);
        assert!(plan.a_mid_db.is_empty());
        assert!(plan.a_filter.is_empty());
        assert!(plan.a_echo_send.is_empty());
    }
}
