//! Audio Unit effects installed on this Mac: Apple's own (`AULowpass`,
//! `AUNBandEQ`, `AUPeakLimiter`…) and others' (`FabFilter`, Waves, `iZotope`),
//! as a bus's insert will offer them (design: research/2026-10-30-overlays/
//! DESIGN-au-inserts.md). Listed on the device-listing thread, never the
//! frame's: macOS may scan its plug-in folders the first time.

/// An installed effect, as offered.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub struct AudioEffect {
    pub name: String,
    pub manufacturer: String,
    pub version: String,
    /// Its component, as `type:subtype:manufacturer` four-character codes
    /// ("aufx:lpas:appl"): what a show saves to find it again.
    pub component: String,
    /// It has a window of its own (else a plain list of its controls).
    pub has_view: bool,
}

/// A four-character code as text: 0x6c706173 is "lpas". A byte that is not
/// printable ASCII is written as `?`.
#[cfg(any(target_os = "macos", test))]
pub fn four_cc(code: u32) -> String {
    code.to_be_bytes()
        .iter()
        .map(|&b| {
            if b.is_ascii_graphic() || b == b' ' {
                b as char
            } else {
                '?'
            }
        })
        .collect()
}

/// A component's `type:subtype:manufacturer` codes, each four characters.
pub fn component_codes(component: &str) -> Option<[u32; 3]> {
    let mut parts = component.split(':').map(|p| {
        let bytes: [u8; 4] = p.as_bytes().try_into().ok()?;
        Some(u32::from_be_bytes(bytes))
    });
    let codes = [parts.next()??, parts.next()??, parts.next()??];
    parts.next().is_none().then_some(codes)
}

/// The installed effects (Audio Units of type `aufx`), by manufacturer then
/// name.
#[cfg(target_os = "macos")]
pub fn installed_effects() -> Vec<AudioEffect> {
    use objc2_audio_toolbox::{kAudioUnitType_Effect, AudioComponentDescription};
    use objc2_avf_audio::AVAudioUnitComponentManager;

    let wanted = AudioComponentDescription {
        componentType: kAudioUnitType_Effect,
        componentSubType: 0,
        componentManufacturer: 0,
        componentFlags: 0,
        componentFlagsMask: 0,
    };
    let mut effects: Vec<AudioEffect> = objc2::rc::autoreleasepool(|_| {
        // SAFETY: the shared manager and its components are plain queries,
        // made on a thread of our own inside an autorelease pool.
        let found = unsafe {
            AVAudioUnitComponentManager::sharedAudioUnitComponentManager()
                .componentsMatchingDescription(wanted)
        };
        found
            .iter()
            .map(|c| {
                // SAFETY: as above; each getter returns an owned value.
                let desc = unsafe { c.audioComponentDescription() };
                AudioEffect {
                    name: unsafe { c.name() }.to_string(),
                    manufacturer: unsafe { c.manufacturerName() }.to_string(),
                    version: unsafe { c.versionString() }.to_string(),
                    component: format!(
                        "{}:{}:{}",
                        four_cc(desc.componentType),
                        four_cc(desc.componentSubType),
                        four_cc(desc.componentManufacturer)
                    ),
                    has_view: unsafe { c.hasCustomView() },
                }
            })
            .collect()
    });
    effects.sort_by(|a, b| {
        (a.manufacturer.to_lowercase(), a.name.to_lowercase())
            .cmp(&(b.manufacturer.to_lowercase(), b.name.to_lowercase()))
    });
    effects
}

#[cfg(not(target_os = "macos"))]
pub fn installed_effects() -> Vec<AudioEffect> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::four_cc;

    #[test]
    fn a_four_character_code_reads_as_its_letters() {
        assert_eq!(four_cc(0x6175_6678), "aufx");
        assert_eq!(four_cc(0x6170_706c), "appl");
        assert_eq!(four_cc(0x0000_0041), "???A");
    }

    /// Apple's own effects are on every Mac: the low-pass is listed, by
    /// its component, with Apple as its maker.
    #[cfg(target_os = "macos")]
    #[test]
    fn apples_own_effects_are_listed() {
        let effects = super::installed_effects();
        let lowpass = effects
            .iter()
            .find(|e| e.component == "aufx:lpas:appl")
            .unwrap_or_else(|| panic!("AULowpass in {effects:?}"));
        assert_eq!(lowpass.manufacturer, "Apple");
        assert!(effects.iter().all(|e| e.component.starts_with("aufx:")));
    }
}
