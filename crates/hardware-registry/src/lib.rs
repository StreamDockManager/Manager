use hardware_core::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
// Enumerate the registered hardware modules
pub fn modules() -> Vec<Box<dyn HardwareModule>> {
    vec![
        Box::new(stream_dock_manager_hardware_n4_pro::N4Pro),
        Box::new(stream_dock_manager_hardware_293::Model),
        Box::new(stream_dock_manager_hardware_293_v3::Model),
        Box::new(stream_dock_manager_hardware_293s::Model),
        Box::new(stream_dock_manager_hardware_293s_v3::Model),
        Box::new(stream_dock_manager_hardware_m18::Model),
        Box::new(stream_dock_manager_hardware_m3::Model),
        Box::new(stream_dock_manager_hardware_n3::Model),
        Box::new(stream_dock_manager_hardware_n3_vsd::Model),
        Box::new(stream_dock_manager_hardware_n1::Model),
        Box::new(stream_dock_manager_hardware_n1_vsd::Model),
        Box::new(stream_dock_manager_hardware_n4::Model),
        Box::new(stream_dock_manager_hardware_k1_pro::Model),
        Box::new(stream_dock_manager_hardware_mini::Model),
        Box::new(stream_dock_manager_hardware_xl::Model),
        Box::new(stream_dock_manager_hardware_h1_pro::Model),
    ]
}
/// Editor/simulator layouts. Only `modules()` grants physical USB support
pub fn manifests() -> Vec<ModelManifest> {
    let layouts: Vec<_> = modules()
        .iter()
        .map(|module| {
            module
                .representative_contract()
                .expect("reviewed representative contract")
                .manifest
        })
        .collect();
    layouts
}
// Find the requested entry in this registry
pub fn find(identity: &HidIdentity) -> Option<Box<dyn HardwareModule>> {
    modules().into_iter().find(|m| m.matches(identity))
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Catalogue {
    pub models: Vec<KnownModel>,
    #[serde(default)]
    pub unimplemented_variants: Vec<KnownModel>,
}
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct KnownModel {
    pub model_id: String,
    pub name: String,
    pub usb_ids: Vec<String>,
}
// Build the registered hardware catalog
pub fn catalogue() -> Catalogue {
    toml::from_str(include_str!("../../../hardware/catalogue.toml")).expect("embedded catalogue")
}
// Reviewed sources outside the pinned manufacturer SDK remain separate from
// that SDK's reproducible inventory. Never invent a Python registration.
fn additional_registrations() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../../../hardware/additional-registrations.v1.json"
    ))
    .expect("reviewed additional hardware registrations")
}
// Validate for the current operation
pub fn validate() -> Result<()> {
    let inventory: serde_json::Value = serde_json::from_str(include_str!(
        "../../../hardware/vendor-registrations.v1.json"
    ))
    .map_err(|e| Error::Descriptor(e.to_string()))?;
    let additional = additional_registrations();
    let mut matches: Vec<(HidSelector, String)> = vec![];
    let mut ids = HashMap::new();
    for module in modules() {
        let manifest = module.manifest();
        manifest.validate()?;
        if ids.insert(manifest.model_id.clone(), ()).is_some() {
            return Err(Error::Descriptor("duplicate model".into()));
        }
        for v in &manifest.variants {
            let expected_unavailable =
                inventory["unavailable_variants"]
                    .as_array()
                    .is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row["model_id"] == manifest.model_id
                                && row["usb_id"]
                                    == format!(
                                        "{:04x}:{:04x}",
                                        v.identity.vendor_id, v.identity.product_id
                                    )
                        })
                    });
            if v.available == expected_unavailable
                || !v.available
                    && v.unavailable_reason
                        .as_ref()
                        .is_none_or(|reason| reason.trim().is_empty())
            {
                return Err(Error::Descriptor(format!(
                    "variant source gate disagrees with audit: {} {}",
                    manifest.model_id, v.id
                )));
            }
            let usb_id = format!("{:04x}:{:04x}", v.identity.vendor_id, v.identity.product_id);
            let vendor_registered =
                inventory["python_registrations"]
                    .as_array()
                    .is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row["model_id"] == manifest.model_id && row["usb_id"] == usb_id
                        })
                    });
            let additionally_reviewed =
                additional["registrations"].as_array().is_some_and(|rows| {
                    rows.iter().any(|row| {
                        row["model_id"] == manifest.model_id
                            && row["usb_id"] == usb_id
                            && row["identity"] == serde_json::to_value(&v.identity).unwrap()
                            && row["evidence"].as_str().is_some_and(|s| !s.is_empty())
                    })
                });
            if !vendor_registered && !additionally_reviewed {
                return Err(Error::Descriptor(format!(
                    "unreviewed hardware registration: {} {usb_id}",
                    manifest.model_id
                )));
            }
            if let Some((_, other)) = matches
                .iter()
                .find(|(selector, _)| selector.overlaps(&v.identity))
            {
                return Err(Error::Descriptor(format!(
                    "USB/interface collision: {other} and {}",
                    manifest.model_id
                )));
            }
            matches.push((v.identity.clone(), manifest.model_id.clone()));
        }
    }
    validate_profile_families(&manifests())?;
    for model in catalogue().models {
        if ids.insert(model.model_id, ()).is_some() {
            return Err(Error::Descriptor(
                "catalogue duplicates registered model".into(),
            ));
        }
    }
    Ok(())
}
// Validate profile families
fn validate_profile_families(models: &[ModelManifest]) -> Result<()> {
    let inputs = |model: &ModelManifest| {
        model
            .controls
            .iter()
            .filter(|control| matches!(control.kind.as_str(), "button" | "encoder" | "touch"))
            .map(|control| (control.id.clone(), control.kind.clone()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    for model in models {
        let Some(family) = model.profile_family.as_deref() else {
            continue;
        };
        let canonical = models
            .iter()
            .find(|candidate| candidate.model_id == family)
            .ok_or_else(|| {
                Error::Descriptor(format!(
                    "{}: unknown profile family {family}",
                    model.model_id
                ))
            })?;
        if canonical.profile_family.is_some() || inputs(model) != inputs(canonical) {
            return Err(Error::Descriptor(format!(
                "{}: profile family {family} has incompatible control IDs or kinds",
                model.model_id
            )));
        }
    }
    Ok(())
}
// Generate Linux udev rules for the supported devices
pub fn udev_rules() -> String {
    let mut output="# Generated by cargo run -p xtask -- hardware generate.\n# Ordered before 73-seat-late.rules; access is restricted to the active local session.\n".to_string();
    let mut pairs: std::collections::BTreeMap<(u16, u16), std::collections::BTreeSet<i32>> =
        std::collections::BTreeMap::new();
    let mut interfaces = std::collections::BTreeSet::new();
    for module in modules() {
        for v in &module.manifest().variants {
            if !v.available {
                continue;
            }
            // A source may establish an interface-independent HID predicate
            // Do not turn that into a broader udev grant for keyboard interfaces
            let Some(interface) = v.identity.interface else {
                output.push_str(&format!(
                    "# {} {}: interface selector unmeasured; no automatic permission grant.\n",
                    module.manifest().model_id,
                    v.id
                ));
                continue;
            };
            pairs
                .entry((v.identity.vendor_id, v.identity.product_id))
                .or_default()
                .insert(interface);
            interfaces.insert(interface);
        }
    }
    for ((vendor, product), selected) in pairs {
        let selected = selected
            .into_iter()
            .map(|i| format!("{i:02x}:"))
            .collect::<String>();
        output.push_str(&format!("SUBSYSTEM==\"hidraw\", ATTRS{{idVendor}}==\"{vendor:04x}\", ATTRS{{idProduct}}==\"{product:04x}\", ENV{{STREAM_DOCK_MANAGER_INTERFACES}}=\":{selected}\"\n"));
    }
    for interface in interfaces {
        output.push_str(&format!("SUBSYSTEM==\"hidraw\", ENV{{STREAM_DOCK_MANAGER_INTERFACES}}==\"*:{interface:02x}:*\", ATTRS{{bInterfaceNumber}}==\"{interface:02x}\", TAG+=\"uaccess\"\n"));
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    // Verify owned crt codecs declare report wake and input policy
    fn owned_crt_codecs_declare_report_wake_and_input_policy() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hardware");
        for entry in std::fs::read_dir(root).unwrap() {
            let path = entry.unwrap().path().join("codec.toml");
            if !path.is_file() {
                continue;
            }
            let source = std::fs::read_to_string(&path).unwrap();
            let codec: toml::Value = toml::from_str(&source).unwrap();
            if codec.get("report_size").is_none() {
                continue;
            }
            for field in [
                "report_id",
                "mode_before_wake",
                "reinitialize_on_wake",
                "require_input_ok",
                "button_states",
                "rotate_states",
                "swipe_states",
            ] {
                assert!(
                    codec.get(field).is_some(),
                    "{} must explicitly declare {field}",
                    path.display()
                );
            }
        }
    }

    #[test]
    // Verify treaslin retail IDs select the reviewed driver and exclude keyboards
    fn treaslin_retail_ids_select_the_reviewed_driver_and_exclude_keyboards() {
        for (product_id, model) in [
            (0x1001, "n3-vsd"),
            (0x1002, "n1-vsd"),
            (0x1003, "293-v3"),
            (0x1034, "xl"),
        ] {
            let identity = HidIdentity {
                vendor_id: 0x5548,
                product_id,
                interface: 0,
                usage_page: 0xffa0,
                usage: 1,
            };
            assert_eq!(find(&identity).unwrap().manifest().model_id, model);
            assert!(find(&HidIdentity {
                interface: 1,
                usage_page: 1,
                usage: 6,
                ..identity
            })
            .is_none());
        }
        let original = stream_dock_manager_hardware_n1::Model;
        let retail = stream_dock_manager_hardware_n1_vsd::Model;
        assert_eq!(
            original
                .manifest()
                .control("display-1")
                .unwrap()
                .image_width,
            Some(80)
        );
        assert_eq!(
            retail.manifest().control("display-1").unwrap().image_width,
            Some(64)
        );
        assert_ne!(original.manifest().model_id, retail.manifest().model_id);
        for model in [original.manifest(), retail.manifest()] {
            assert!(model.control("key-16").unwrap().image_slot.is_none());
            assert_eq!(model.control("display-1").unwrap().image_slot, Some(16));
        }
    }
    #[test]
    // Verify profile families share inputs without aliasing native models
    fn profile_families_share_inputs_without_aliasing_native_models() {
        let models = manifests();
        validate_profile_families(&models).unwrap();
        let families: std::collections::BTreeMap<_, _> = models
            .iter()
            .filter_map(|model| {
                model
                    .profile_family
                    .as_deref()
                    .map(|family| (model.model_id.as_str(), family))
            })
            .collect();
        assert_eq!(
            families,
            [
                ("293-v3", "293"),
                ("293s-v3", "293s"),
                ("n1-vsd", "n1"),
                ("n3-vsd", "n3")
            ]
            .into()
        );
        let canonical = models
            .iter()
            .find(|model| model.model_id == "n3")
            .unwrap()
            .clone();
        let variant = models
            .iter()
            .find(|model| model.model_id == "n3-vsd")
            .unwrap()
            .clone();
        assert!(!canonical.variants[0]
            .identity
            .overlaps(&variant.variants[0].identity));
        for invalid in ["missing", "n3-vsd"] {
            let mut changed = variant.clone();
            changed.profile_family = Some(invalid.into());
            assert!(validate_profile_families(&[canonical.clone(), changed]).is_err());
        }
        for change_kind in [false, true] {
            let mut changed = variant.clone();
            if change_kind {
                changed.controls[0].kind = "touch".into();
            } else {
                changed.controls[0].id = "different-key".into();
            }
            assert!(validate_profile_families(&[canonical.clone(), changed]).is_err());
        }
        let mut chained = canonical;
        chained.profile_family = Some("293".into());
        assert!(validate_profile_families(&[chained, variant]).is_err());
    }
    #[test]
    // Verify all registered descriptors and matches are unique
    fn all_registered_descriptors_and_matches_are_unique() {
        validate().unwrap();
        let source: serde_json::Value = serde_json::from_str(include_str!(
            "../../../hardware/vendor-registrations.v1.json"
        ))
        .unwrap();
        let additional = additional_registrations();
        let expected: std::collections::BTreeSet<_> = source["python_registrations"]
            .as_array()
            .unwrap()
            .iter()
            .chain(additional["registrations"].as_array().unwrap().iter())
            .map(|row| {
                (
                    row["model_id"].as_str().unwrap().to_string(),
                    row["usb_id"].as_str().unwrap().to_string(),
                )
            })
            .collect();
        let actual: std::collections::BTreeSet<_> = modules()
            .iter()
            .flat_map(|module| {
                module
                    .manifest()
                    .variants
                    .iter()
                    .map(|variant| {
                        (
                            module.manifest().model_id.clone(),
                            format!(
                                "{:04x}:{:04x}",
                                variant.identity.vendor_id, variant.identity.product_id
                            ),
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        assert_eq!(actual, expected);
    }
    #[test]
    // Verify directional groups require three distinct edges
    fn directional_groups_require_three_distinct_edges() {
        let mut manifest = modules()[0].manifest().clone();
        for position in ["left", "center", "right"] {
            let mut control = manifest.controls[0].clone();
            control.id = format!("switch-1-{position}");
            control.image_slot = None;
            control.image_width = None;
            control.image_height = None;
            control.directional_switch = Some(DirectionalSwitch {
                group: "switch-1".into(),
                position: position.into(),
            });
            manifest.controls.push(control);
        }
        manifest.validate().unwrap();
        manifest
            .controls
            .last_mut()
            .unwrap()
            .directional_switch
            .as_mut()
            .unwrap()
            .position = "left".into();
        assert!(manifest.validate().is_err());
        manifest.controls.pop();
        assert!(manifest.validate().is_err());
    }
    #[test]
    // Verify reconciled k1 variants match without keyboard permission grants
    fn reconciled_k1_variants_match_without_keyboard_permission_grants() {
        validate().unwrap();
        let rules = udev_rules();
        for (vendor_id, product_id) in [(0x6603, 0x1015), (0x5548, 0x1025), (0x6603, 0x1019)] {
            let identity = HidIdentity {
                vendor_id,
                product_id,
                interface: 0,
                usage_page: 0xffa0,
                usage: 1,
            };
            let module = find(&identity).expect("reviewed K1 variant should match");
            assert_eq!(module.manifest().model_id, "k1-pro");
            assert!(find(&HidIdentity {
                usage_page: 1,
                ..identity
            })
            .is_none());
            let selector = format!(
                "ATTRS{{idVendor}}==\"{vendor_id:04x}\", ATTRS{{idProduct}}==\"{product_id:04x}\""
            );
            assert!(!rules
                .lines()
                .any(|line| !line.starts_with('#') && line.contains(&selector)));
        }
    }

    #[test]
    // Verify unsupported catalogue aliases do not grant support
    fn unsupported_catalogue_aliases_do_not_grant_support() {
        for (vendor_id, product_id) in [(0x5548, 0x1004), (0x5548, 0x1000), (0xeeef, 0x2929)] {
            assert!(find(&HidIdentity {
                vendor_id,
                product_id,
                interface: 0,
                usage_page: 0xffa0,
                usage: 1
            })
            .is_none());
        }
        assert!(catalogue().models.is_empty());
        for model in catalogue().unimplemented_variants {
            for pair in model.usb_ids {
                let (vendor, product) = pair.split_once(':').unwrap();
                assert!(find(&HidIdentity {
                    vendor_id: u16::from_str_radix(vendor, 16).unwrap(),
                    product_id: u16::from_str_radix(product, 16).unwrap(),
                    interface: 0,
                    usage_page: 0xffa0,
                    usage: 1
                })
                .is_none());
            }
        }
        let registered: std::collections::BTreeSet<_> = modules()
            .iter()
            .map(|m| m.manifest().model_id.clone())
            .collect();
        assert_eq!(
            registered,
            [
                "n4-pro", "293", "293-v3", "293s", "293s-v3", "m18", "m3", "n3", "n3-vsd", "n1",
                "n1-vsd", "n4", "k1-pro", "mini", "xl", "h1-pro"
            ]
            .into_iter()
            .map(String::from)
            .collect()
        );
        assert_eq!(manifests().len(), registered.len());
        assert!(
            udev_rules().contains("interface selector unmeasured; no automatic permission grant")
        );
        assert_eq!(
            udev_rules().lines().filter(|l| l.contains("TAG+=")).count(),
            1
        );
    }
}
