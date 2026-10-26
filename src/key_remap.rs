//! Per-client key remapping applied to input before it is sent to a client,
//! e.g. swapping Cmd and Ctrl or turning F18 into Hangul for a Windows peer.

use std::collections::HashMap;

use input_event::{Event, KeyboardEvent, scancode::Linux};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct KeyRemap {
    keys: HashMap<u32, u32>,
    /// (from, to) XKB modifier masks derived from remapped modifier keys
    mods: Vec<(u32, u32)>,
}

impl KeyRemap {
    pub fn new(map: &HashMap<Linux, Linux>) -> Self {
        let keys = map.iter().map(|(&f, &t)| (f as u32, t as u32)).collect();
        let mods = map
            .iter()
            .filter_map(|(&f, &t)| Some((modifier_mask(f)?, modifier_mask(t)?)))
            .collect();
        Self { keys, mods }
    }

    pub fn key(&self, key: u32) -> u32 {
        self.keys.get(&key).copied().unwrap_or(key)
    }

    fn mask(&self, mask: u32) -> u32 {
        let from_bits = self.mods.iter().fold(0, |acc, &(f, _)| acc | f);
        self.mods
            .iter()
            .filter(|&&(f, _)| mask & f != 0)
            .fold(mask & !from_bits, |acc, &(_, t)| acc | t)
    }

    pub fn apply(&self, event: Event) -> Event {
        match event {
            Event::Keyboard(KeyboardEvent::Key { time, key, state }) => {
                Event::Keyboard(KeyboardEvent::Key {
                    time,
                    key: self.key(key),
                    state,
                })
            }
            Event::Keyboard(KeyboardEvent::Modifiers {
                depressed,
                latched,
                locked,
                group,
            }) => Event::Keyboard(KeyboardEvent::Modifiers {
                depressed: self.mask(depressed),
                latched: self.mask(latched),
                locked,
                group,
            }),
            e => e,
        }
    }
}

/// XKB modifier mask a modifier key contributes to
fn modifier_mask(key: Linux) -> Option<u32> {
    match key {
        Linux::KeyLeftShift | Linux::KeyRightShift => Some(1 << 0),
        Linux::KeyLeftCtrl | Linux::KeyRightCtrl => Some(1 << 2),
        Linux::KeyLeftAlt | Linux::KeyRightalt => Some(1 << 3),
        Linux::KeyLeftMeta | Linux::KeyRightmeta => Some(1 << 6),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL: u32 = 1 << 2;
    const META: u32 = 1 << 6;
    const SHIFT: u32 = 1 << 0;

    fn mac_to_windows() -> KeyRemap {
        KeyRemap::new(&HashMap::from([
            (Linux::KeyLeftMeta, Linux::KeyLeftCtrl),
            (Linux::KeyLeftCtrl, Linux::KeyLeftMeta),
            (Linux::KeyF18, Linux::KeyHanguel),
        ]))
    }

    fn key(key: Linux) -> Event {
        Event::Keyboard(KeyboardEvent::Key {
            time: 0,
            key: key as u32,
            state: 1,
        })
    }

    fn mods(depressed: u32) -> Event {
        Event::Keyboard(KeyboardEvent::Modifiers {
            depressed,
            latched: 0,
            locked: 0,
            group: 0,
        })
    }

    #[test]
    fn swaps_keys() {
        let r = mac_to_windows();
        assert_eq!(r.apply(key(Linux::KeyLeftMeta)), key(Linux::KeyLeftCtrl));
        assert_eq!(r.apply(key(Linux::KeyLeftCtrl)), key(Linux::KeyLeftMeta));
        assert_eq!(r.apply(key(Linux::KeyF18)), key(Linux::KeyHanguel));
        assert_eq!(r.apply(key(Linux::KeyC)), key(Linux::KeyC));
    }

    #[test]
    fn swaps_modifier_masks() {
        let r = mac_to_windows();
        assert_eq!(r.apply(mods(META)), mods(CTRL));
        assert_eq!(r.apply(mods(CTRL)), mods(META));
        assert_eq!(r.apply(mods(CTRL | META)), mods(CTRL | META));
        assert_eq!(r.apply(mods(META | SHIFT)), mods(CTRL | SHIFT));
    }

    #[test]
    fn empty_remap_is_identity() {
        let r = KeyRemap::default();
        assert_eq!(r.apply(key(Linux::KeyLeftMeta)), key(Linux::KeyLeftMeta));
        assert_eq!(r.apply(mods(META)), mods(META));
    }
}
