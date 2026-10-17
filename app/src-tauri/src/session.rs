// SPDX-FileCopyrightText: JR Lanteigne <root@dnim.dev>
// SPDX-License-Identifier: GPL-3.0-or-later
//! What a connected board lets the user do, and the shapes both frontends
//! receive.
//!
//! These are decisions, not conversation: which controls a board offers,
//! what an owner has allowed. Both backends answer the same questions, and
//! answering them separately is how the browser build came to allow a
//! magnetic write the desktop build refused.

use serde::{Deserialize, Serialize};

use crate::protocol::{hall, KbOptions, LedParam, SledParam, SleepTimes};
use crate::registry::DeviceSpec;

/// Rules live on `DeviceSpec`. Both frontends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SwitchAccess {
    None,
    /// Columns can be read, not written.
    Read,
    /// Columns can be read and written.
    Write,
    /// No columns: one board-wide record can be written, nothing read back.
    Global,
}

/// `owner_hall`: the owner's felt round trip stands in for firmware
/// evidence on a board whose columns read.
pub fn switch_access(
    spec: &DeviceSpec,
    revision: Option<u16>,
    read_only: bool,
    owner_hall: bool,
) -> SwitchAccess {
    if spec.hall_reads(revision) {
        if (spec.hall_writes(revision) || owner_hall) && !read_only {
            SwitchAccess::Write
        } else {
            SwitchAccess::Read
        }
    } else if spec.hall_global(revision) && !read_only {
        SwitchAccess::Global
    } else {
        SwitchAccess::None
    }
}

/// Owner answers from the check. Spec overlays apply only to an unregistered
/// board, so a registry entry is never contradicted by a click. `switch_writes`
/// is a felt round trip standing in for unread firmware, on any board whose
/// columns read.
#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OwnerRecord {
    pub allowed: bool,
    pub magnetic: bool,
    pub side_light: Option<bool>,
    pub switch_writes: bool,
    pub profiles: Option<u8>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceSettings {
    pub debounce: u8,
    pub sleep: SleepTimes,
    /// Absent on families whose option bitfield is not decoded.
    pub options: Option<KbOptions>,
    /// Firmware revision, e.g. 0x0102 -> "1.02". From 0x80, or on gen2 the
    /// identify reply.
    pub revision: String,
    /// Board auto-detects the host OS and switches its Win/Mac layer.
    pub auto_os: bool,
    /// Present only when the firmware answers 0x88.
    pub side_light: Option<SledParam>,
}

/// Everything sharkfin can read back from a board, as one restorable file.
/// Per-key colour is absent by necessity: the firmware never reports it.
///
/// Version 1 carried the base keymaps, Fn layers, lighting and settings.
/// Version 2 adds what magnetic gen2 boards keep beside the base keymap:
/// the mod-tap, dynamic-keystroke and toggle sub-layers, and the per-key
/// switch settings. Both new fields are optional so version 1 files still
/// import.
#[derive(Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedConfig {
    pub version: u8,
    pub device_id: u32,
    pub family: String,
    pub board: String,
    pub profiles: Vec<Vec<u8>>,
    pub fn_layers: Vec<Vec<u8>>,
    pub led: LedParam,
    pub side_light: Option<SledParam>,
    pub debounce: u8,
    pub sleep: SleepTimes,
    pub options: Option<KbOptions>,
    /// Per profile, keymap sub-layers 1..3 (dynamic keystroke, mod-tap,
    /// toggle), each 512 bytes. gen2 magnetic boards only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_layers: Option<Vec<Vec<Vec<u8>>>>,
    /// Per profile, the hall-effect switch settings. Captured per profile,
    /// which needs a write (the active profile must move), so read-only
    /// links leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub switch_settings: Option<Vec<hall::SwitchSettings>>,
}

/// The version this build writes.
pub const SAVED_CONFIG_VERSION: u8 = 2;

impl SavedConfig {
    /// Shape checks for the v2 fields, before anything is written: a
    /// malformed file must not leave half a restore on the board. `profiles`
    /// is the connected board's count, `slots` its switch count when the
    /// board has magnetic switches.
    pub fn check_v2_shapes(&self, profiles: u8, slots: Option<usize>) -> Result<(), String> {
        if let Some(sub_layers) = &self.sub_layers {
            for (p, layers) in sub_layers.iter().enumerate() {
                if layers.len() != 3 {
                    return Err(format!(
                        "profile {p} in the file holds {} sub-layers, expected 3",
                        layers.len()
                    ));
                }
                for (s, layer) in layers.iter().enumerate() {
                    if layer.len() != 512 {
                        return Err(format!(
                            "profile {p} sub-layer {} in the file is {} bytes, expected 512",
                            s + 1,
                            layer.len()
                        ));
                    }
                }
            }
        }
        if let Some(settings) = &self.switch_settings {
            if settings.len() != usize::from(profiles) {
                return Err(format!(
                    "the file holds switch settings for {} profiles, but {} has {}",
                    settings.len(),
                    self.board,
                    profiles
                ));
            }
            let slots = slots.ok_or_else(|| {
                format!(
                    "the file holds switch settings, but {} has no magnetic switches",
                    self.board
                )
            })?;
            if let Some(bad) = settings.iter().find(|s| s.keys.len() != slots) {
                return Err(format!(
                    "the file's switch settings hold {} keys, {} has {}",
                    bad.keys.len(),
                    self.board,
                    slots
                ));
            }
        }
        Ok(())
    }
}

/// Which optional blocks an export captures, by board capability. The two
/// conditions the export path gates on, in one place so the code and the
/// tests cannot drift apart. The switch capture is what writes during a
/// backup: the active profile must move to read each profile's hall block.
pub fn capture_gates(
    spec: &DeviceSpec,
    revision: Option<u16>,
    read_only: bool,
    owner_switch_writes: bool,
) -> (bool, bool) {
    let sub_layers = spec.family == "gen2" && spec.hall_reads(revision);
    let switch_settings =
        switch_access(spec, revision, read_only, owner_switch_writes) == SwitchAccess::Write;
    (sub_layers, switch_settings)
}

/// Refuse backups from a newer sharkfin rather than silently dropping the
/// fields that version added.
pub fn check_saved_version(version: u8) -> Result<(), String> {
    if version > SAVED_CONFIG_VERSION {
        return Err(format!(
            "this backup is version {version}, but this sharkfin restores up to version \
             {SAVED_CONFIG_VERSION}. Update sharkfin and try again."
        ));
    }
    Ok(())
}

/// Whether the switch columns may be written, and why.
///
/// `owner_hall` is the owner's felt round trip standing in for firmware
/// evidence; `trial` is the single-key trial. Both backends ask this one
/// question rather than each deciding for itself, which is how the browser
/// build once allowed a write the desktop build refused.
pub fn hall_write_allowed(
    spec: &DeviceSpec,
    revision: Option<u16>,
    owner_switch_writes: bool,
    trial_slot: Option<u8>,
    slots: Option<&[u8]>,
) -> bool {
    let reads = spec.hall_reads(revision);
    let owner_hall = owner_switch_writes && reads;
    let trial = reads
        && match (trial_slot, slots) {
            (Some(t), Some(k)) => !k.is_empty() && k.iter().all(|&x| x == t),
            _ => false,
        };
    spec.hall_writes(revision) || owner_hall || trial
}

/// The message shown when it is not.
pub fn hall_write_refusal(spec: &DeviceSpec) -> String {
    format!(
        "sharkfin has not read {}'s firmware for its switch settings, so it will not write them",
        spec.label()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file shaped exactly like a version 1 export: no sub-layers, no
    /// switch settings. Both must come back None, not an error.
    #[test]
    fn v1_file_imports_with_new_fields_none() {
        let json = r#"{
            "version": 1,
            "deviceId": 2836,
            "family": "gen2",
            "board": "Veekos Shine60",
            "profiles": [[0, 0], [1, 0]],
            "fnLayers": [[2, 0], [3, 0]],
            "led": {
                "mode": 1, "speed": 2, "brightness": 3, "option": 0,
                "dazzle": false, "r": 10, "g": 20, "b": 30
            },
            "sideLight": null,
            "debounce": 5,
            "sleep": {"sleepBt": 300, "sleep24": 300, "deepBt": 600, "deep24": 600},
            "options": null
        }"#;
        let cfg: SavedConfig = serde_json::from_str(json).unwrap();
        assert_eq!(cfg.version, 1);
        assert!(cfg.sub_layers.is_none());
        assert!(cfg.switch_settings.is_none());
    }

    fn v2_config() -> SavedConfig {
        let f = hall::Columns::gen2(Some(0x0309));
        let keys = vec![
            hall::KeySwitch {
                slot: 0,
                kind: hall::KIND_MOD_TAP,
                rapid_trigger: true,
                travel: 2.0,
                lift: 1.0,
                rt_press: 0.1,
                rt_lift: 0.1,
                dead_bottom: 0.3,
                dks_start: 0.4,
                dks_actions: [0, 1, 2, 3],
                mt_time_ms: 300,
                snap_partner: 0xFF,
                switch_type: 7,
            },
            hall::KeySwitch {
                slot: 1,
                kind: hall::KIND_SNAP,
                rapid_trigger: false,
                travel: 1.5,
                lift: 1.2,
                rt_press: 0.2,
                rt_lift: 0.2,
                dead_bottom: 0.3,
                dks_start: 0.4,
                dks_actions: [3, 2, 1, 0],
                mt_time_ms: 200,
                snap_partner: 0,
                switch_type: 0,
            },
        ];
        SavedConfig {
            version: SAVED_CONFIG_VERSION,
            device_id: 2836,
            family: "gen2".into(),
            board: "Veekos Shine60".into(),
            profiles: vec![vec![0; 512], vec![1; 512]],
            fn_layers: vec![vec![2; 512], vec![3; 512]],
            led: LedParam {
                mode: 1,
                speed: 2,
                brightness: 3,
                option: 0,
                dazzle: false,
                r: 10,
                g: 20,
                b: 30,
            },
            side_light: None,
            debounce: 5,
            sleep: SleepTimes {
                sleep_bt: 300,
                sleep_24: 300,
                deep_bt: 600,
                deep_24: 600,
            },
            options: None,
            sub_layers: Some(vec![
                vec![vec![9; 512], vec![8; 512], vec![7; 512]],
                vec![vec![6; 512], vec![5; 512], vec![4; 512]],
            ]),
            switch_settings: Some(vec![
                hall::SwitchSettings {
                    format: f.format,
                    unit_mm: f.unit_mm,
                    keys: keys.clone(),
                },
                hall::SwitchSettings {
                    format: f.format,
                    unit_mm: f.unit_mm,
                    keys,
                },
            ]),
        }
    }

    /// Serialize then deserialize, and compare the JSON both ways: the new
    /// fields survive, and nothing about the shape changed in the trip.
    #[test]
    fn v2_config_round_trips() {
        let cfg = v2_config();
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        assert!(json.contains("\"subLayers\""));
        assert!(json.contains("\"switchSettings\""));
        let back: SavedConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sub_layers, cfg.sub_layers);
        assert_eq!(
            back.switch_settings.as_ref().unwrap()[1].keys[0].dks_actions,
            [0, 1, 2, 3]
        );
        assert_eq!(
            back.switch_settings.as_ref().unwrap()[0].keys[0].kind,
            hall::KIND_MOD_TAP
        );
        let again = serde_json::to_string_pretty(&back).unwrap();
        assert_eq!(again, json);
    }

    /// Fields absent from the JSON default to None even at version 2, so a
    /// partial file still opens.
    #[test]
    fn v2_file_without_new_fields_imports() {
        let mut cfg = v2_config();
        cfg.sub_layers = None;
        cfg.switch_settings = None;
        let json = serde_json::to_string_pretty(&cfg).unwrap();
        let back: SavedConfig = serde_json::from_str(&json).unwrap();
        assert!(back.sub_layers.is_none() && back.switch_settings.is_none());
    }

    #[test]
    fn version_guard_refuses_newer_backups() {
        assert!(check_saved_version(1).is_ok());
        assert!(check_saved_version(SAVED_CONFIG_VERSION).is_ok());
        let err = check_saved_version(SAVED_CONFIG_VERSION + 1).unwrap_err();
        assert!(err.contains("version 3"), "names the file version: {err}");
    }

    /// A good v2 file passes, a v1 file passes, and each corruption is
    /// named. These run before the first write of a restore.
    #[test]
    fn v2_shapes_accept_good_files_and_name_bad_ones() {
        v2_config().check_v2_shapes(2, Some(2)).unwrap();
        let mut v1 = v2_config();
        v1.sub_layers = None;
        v1.switch_settings = None;
        v1.check_v2_shapes(2, None).unwrap();

        let mut cfg = v2_config();
        cfg.sub_layers.as_mut().unwrap()[0].pop();
        let err = cfg.check_v2_shapes(2, Some(2)).unwrap_err();
        assert!(err.contains("2 sub-layers"), "names the count: {err}");

        let mut cfg = v2_config();
        cfg.sub_layers.as_mut().unwrap()[1][2] = vec![0; 511];
        let err = cfg.check_v2_shapes(2, Some(2)).unwrap_err();
        assert!(err.contains("511 bytes"), "names the size: {err}");

        let mut cfg = v2_config();
        cfg.switch_settings.as_mut().unwrap().pop();
        let err = cfg.check_v2_shapes(2, Some(2)).unwrap_err();
        assert!(err.contains("1 profiles, but"), "names the count: {err}");

        let mut cfg = v2_config();
        cfg.switch_settings.as_mut().unwrap()[0].keys.pop();
        let err = cfg.check_v2_shapes(2, Some(2)).unwrap_err();
        assert!(err.contains("hold 1 keys"), "names the count: {err}");

        let err = v2_config().check_v2_shapes(2, None).unwrap_err();
        assert!(
            err.contains("no magnetic switches"),
            "settings on a board without them: {err}"
        );
    }

    /// The export capture decision, across the board shapes that exist:
    /// plain boards never gain the new blocks, the switch capture always
    /// needs write evidence, and a read-only link never switches profiles.
    #[test]
    fn capture_gates_track_board_capability() {
        use crate::registry;
        let find = |pred: &dyn Fn(&registry::DeviceSpec) -> bool| {
            registry::all()
                .into_iter()
                .find(|d| pred(d))
                .expect("a matching registry entry")
        };
        let plain = find(&|d| !d.magnetic);
        assert_eq!(capture_gates(&plain, None, false, false), (false, false));

        let ry5088 =
            find(&|d| d.family == "gen2" && d.magnetic && d.internal_name.starts_with("ry5088_"));
        assert_eq!(capture_gates(&ry5088, None, false, false), (true, true));

        let other_gen2 =
            find(&|d| d.family == "gen2" && d.magnetic && !d.internal_name.starts_with("ry5088_"));
        assert_eq!(
            capture_gates(&other_gen2, None, false, false),
            (true, false)
        );
        assert_eq!(capture_gates(&other_gen2, None, false, true), (true, true));
        assert_eq!(capture_gates(&other_gen2, None, true, true), (true, false));

        // The yc3121 yc500 lineages carry their own firmware evidence, so
        // 2.00+ writes without the owner's round trip; below 2.00 there are
        // no columns to read at all.
        let yc3121 =
            find(&|d| d.family == "yc500" && d.magnetic && d.internal_name.starts_with("yc3121_"));
        assert_eq!(
            capture_gates(&yc3121, Some(0x0200), false, false),
            (false, true)
        );
        assert_eq!(
            capture_gates(&yc3121, Some(0x0200), false, true),
            (false, true)
        );
        assert_eq!(
            capture_gates(&yc3121, Some(0x0107), false, true),
            (false, false)
        );
    }

    /// Every non-magnetic board in the registry, at any firmware revision:
    /// a backup of it is exactly what version 1 already captured.
    #[test]
    fn non_magnetic_boards_capture_nothing() {
        for d in crate::registry::all() {
            if d.magnetic {
                continue;
            }
            for revision in [None, Some(0x0107), Some(0x0200)] {
                assert_eq!(
                    capture_gates(&d, revision, false, true),
                    (false, false),
                    "{} at {revision:?}",
                    d.label()
                );
            }
        }
    }
}
