//! Event-local macOS modifier identity. Aggregate CG flags alone cannot tell
//! whether the left or right key changed while the other side remains held
//! Side masks are public NX_DEVICE*KEYMASK values in IOKit/hidsystem/IOLLEvent.h;
//! aggregate masks are CGEventFlags values in CoreGraphics/CGEventTypes.h

// Map a physical modifier to its family and device-side native flags
pub(crate) fn modifier_masks(code: u16) -> Option<(u64, u64)> {
    Some(match code {
        56 => (1 << 17, 0x0002), // Left Shift
        60 => (1 << 17, 0x0004), // Right Shift
        59 => (1 << 18, 0x0001), // Left Control
        62 => (1 << 18, 0x2000), // Right Control
        58 => (1 << 19, 0x0020), // Left Option
        61 => (1 << 19, 0x0040), // Right Option
        55 => (1 << 20, 0x0008), // Left Command
        54 => (1 << 20, 0x0010), // Right Command
        // Caps Lock is a toggle, not a physical modifier edge. Fn has no
        // supported portable identity. Neither is inferred from aggregate flags
        _ => return None,
    })
}

/// Decode the changed key from this event, not the current HID state, which can
/// already include later queued events. Inconsistent/side-less flags cannot
/// establish a physical edge and must not silently discard a modifier press
pub(crate) fn event_down(code: u16, flags: u64) -> Option<bool> {
    let (aggregate, side) = modifier_masks(code)?;
    let family = match aggregate {
        value if value == 1 << 17 => 0x0006,
        value if value == 1 << 18 => 0x2001,
        value if value == 1 << 19 => 0x0060,
        value if value == 1 << 20 => 0x0018,
        _ => return None,
    };
    if (flags & aggregate != 0) != (flags & family != 0) {
        return None;
    }
    Some(flags & side != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    // Verify that each modifier side uses its event local flag
    fn each_modifier_side_uses_its_event_local_flag() {
        for (left, right) in [(56, 60), (59, 62), (58, 61), (55, 54)] {
            let (aggregate, left_mask) = modifier_masks(left).unwrap();
            let (_, right_mask) = modifier_masks(right).unwrap();
            assert_eq!(event_down(left, aggregate | left_mask), Some(true));
            assert_eq!(event_down(right, aggregate | right_mask), Some(true));
            assert_eq!(
                event_down(left, aggregate | left_mask | right_mask),
                Some(true)
            );
            assert_eq!(
                event_down(right, aggregate | left_mask | right_mask),
                Some(true)
            );
            assert_eq!(event_down(left, aggregate | right_mask), Some(false));
            assert_eq!(event_down(right, aggregate | left_mask), Some(false));
            assert_eq!(event_down(left, 0), Some(false));
            assert_eq!(event_down(right, 0), Some(false));
            assert_eq!(event_down(left, aggregate), None);
            assert_eq!(event_down(left, left_mask), None);
        }
    }

    #[test]
    // Verify that toggle and unmapped keys are not modifier edges
    fn toggle_and_unmapped_keys_are_not_modifier_edges() {
        for code in [0, 18, 57, 63, u16::MAX] {
            assert_eq!(modifier_masks(code), None);
            assert_eq!(event_down(code, u64::MAX), None);
        }
    }
}
