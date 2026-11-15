# OpenAction manifest conformance fixture

`starterpack/manifest.json` is an unchanged source fixture from
[OpenDeck](https://github.com/nekename/OpenDeck/blob/3ddecc1ce8952a423916cf066724356267b97410/plugins/com.amansprojects.starterpack.sdPlugin/assets/manifest.json),
commit `3ddecc1ce8952a423916cf066724356267b97410`. The upstream license is preserved in `LICENSE.md`.
Only the manifest is included; tests create disposable entry-point and inspector files.
This fixture is not an executable distribution or full OpenDeck compatibility claim.

## Getting started

This folder contains conformance inputs, not an installable plugin. For a runnable probe, follow the [OpenAction SDK fixture guide](../../openaction-sdk-executable/README.md). For normal plugin installation, use [Manager's plugin guide](../../../../../README.md#add-plugins-and-nodejs).

| Reference                                               | Use it for                                   |
| ------------------------------------------------------- | -------------------------------------------- |
| [`../vendor-sources.json`](../../vendor-sources.json)   | Pinned source and hashes                     |
| [API inventory below](#pinned-documented-api-inventory) | Implementation and evidence per event family |
| [User samples](user-samples/README.md)                  | Bounded real-world import regressions        |

The documented API shape is at https://openaction.amankhanna.me/manifest.html.

## Pinned documented API inventory

The complete documentation source was reviewed at
[OpenActionAPI/docs `ffe2564bd42f54b19443cf1dac821adf6a85a848`](https://github.com/OpenActionAPI/docs/tree/ffe2564bd42f54b19443cf1dac821adf6a85a848/src)
(October 4, 2026). The table maps every documented event family to implementation
and executable evidence. Test presence is not a claim that every platform, browser,
plugin or physical device has been qualified. Paths below are relative to this repository;
`host` means `crates/plugin-host/src`.

| Documentation / events                                                                        | Implementation                                                                  | Executable evidence                                                                                                                                                                         |
| --------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `manifest.md`: identity, OS/architecture entrypoints, flags, state defaults, image resolution | `host/manifest.rs`, `manifest_schema.rs`, `vendor_presentation.rs`, `visual.rs` | Manifest schema tests and source/deployed fixtures; `openaction_sdk_null_visual_reset_uses_action_icon_and_defaults`                                                                        |
| `api/registration.md`: native arguments, HTML/PI callback, registration info                  | `host/browser.rs`, `lib.rs`, `transport.rs`                                     | `openaction_browser_callback_is_available_without_elgato_alias`; `crates/plugin-host/tests/openaction_sdk.rs`                                                                               |
| `willAppear`, `willDisappear`                                                                 | `host/lib.rs` synchronization, dispatch and context retirement                  | Genuine SDK integration; `openaction_fresh_pulse_announces_initial_state_before_release_toggle`                                                                                             |
| `keyDown`, `keyUp`                                                                            | `host/lib.rs` dispatch                                                          | `openaction_toggle_occurs_on_release_and_pulse_preserves_both_states`; `openaction_multi_action_release_toggles_after_event_without_affecting_press`                                        |
| `dialRotate`, `dialDown`, `dialUp`, `touchTap`                                                | `host/lib.rs` dispatch                                                          | Shared `vendor_dials_have_signed_ticks_pressed_state_coordinates_and_touch_arrays`; genuine SDK integration                                                                                 |
| `deviceDidConnect`, `deviceDidDisconnect`                                                     | `host/lib.rs::synchronize`                                                      | Shared `device_changes_emit_change_without_fabricating_reconnect`; genuine SDK integration                                                                                                  |
| `applicationDidLaunch`, `applicationDidTerminate`                                             | `host/vendor_system.rs`; `crates/runtime/src/vendor_os_events.rs`               | Owner/manifest filtering test `system_and_deep_link_events_are_scoped_to_enabled_registered_owners`; process snapshot unit tests; genuine SDK integration                                   |
| `systemDidWakeUp`                                                                             | Same OS event modules                                                           | Owner filtering and genuine SDK integration; these do not exercise a real OS suspend/resume                                                                                                 |
| `didReceiveDeepLink`                                                                          | `host/vendor_system.rs`; `src-tauri/src/vendor_links.rs`                        | `deep_links_have_bounded_exact_owner_and_opaque_message`; owner filtering and genuine SDK integration                                                                                       |
| `propertyInspectorDidAppear`, `propertyInspectorDidDisappear`                                 | `host/settings.rs`, `lib.rs`                                                    | `inspector_lifecycle_payload_preserves_live_action_and_device`; genuine SDK integration                                                                                                     |
| `getSettings`, `setSettings`, `didReceiveSettings`                                            | `host/lib.rs`, `saved_settings.rs`, `saved_settings_reads.rs`                   | `inspector_settings_event_keeps_token_and_resolves_live_device_metadata`; saved-settings persistence/routing tests; genuine SDK integration                                                 |
| `getGlobalSettings`, `setGlobalSettings`, `didReceiveGlobalSettings`                          | `host/lib.rs`, `settings.rs`, `persistence.rs`                                  | `openaction_opaque_global_settings_survive_snapshot_validation_and_restart`; `openaction_opaque_defaults_commit_and_configuration_roundtrip`; genuine SDK integration                       |
| `sendToPlugin`, `sendToPropertyInspector`                                                     | `host/lib.rs`, `saved_settings.rs`, `settings.rs`                               | Genuine SDK integration with disposable PI WebSocket roundtrip                                                                                                                              |
| `titleParametersDidChange`                                                                    | `host/vendor_system.rs`; runtime appearance synchronization                     | Shared `authored_title_changes_are_deduplicated_and_removed_overlays_are_hidden`; genuine SDK integration                                                                                   |
| `setTitle`, `setImage`, `setState`                                                            | `host/visual.rs`                                                                | `host/visual_tests.rs` target/state/default tests; OpenAction null-reset and stale encoder-title regressions; genuine SDK title/image integration                                           |
| `setFeedback`, `setFeedbackLayout`                                                            | `host/visual.rs` and feedback renderer                                          | Shared builtin/custom layout tests; `openaction_socket_misc_and_encoder_feedback_routes_are_scoped`                                                                                         |
| `openUrl`, `logMessage`, `showAlert`, `showOk`                                                | `host/lib.rs`, `effects.rs`, `visual.rs`                                        | `openaction_socket_misc_and_encoder_feedback_routes_are_scoped` checks transport admission/effect routing, log isolation, and cached feedback. It never opens a browser or drives hardware. |
| Manifest `HasSettingsInterface` / SDK `showSettingsInterface`                                 | `host/vendor_system.rs` and explicit UI command                                 | `openaction_settings_interface_requires_connected_enabled_opt_in`; genuine SDK integration                                                                                                  |

The genuine SDK test is opt-in: build the pinned fixture in
`tests/fixtures/plugins/openaction-sdk-executable`, provide its absolute executable path
through `STREAM_DOCK_MANAGER_OPENACTION_FIXTURE`, then run the integration test with
`--ignored`. It starts only disposable processes and loopback transports. Its PI peer is
a WebSocket test client, not an actual browser-rendered Svelte inspector.

### Interpretation and verification limits

- The pinned `api/serverbound/states.md` section headed `setState` mistakenly shows
  `event: "logMessage"` and omits `context`. Manager implements the actual `setState`
  event with owned context; the typo is not treated as a second logging protocol.
- The official documentation tree contains no device-provider protocol.
  OpenDeck's `DeviceNamespace` and provider messages are separate, undocumented
  reference-implementation extensions, not covered by this inventory.
- OpenDeck's pinned `src-tauri/src/events/outbound/keypad.rs` toggles ordinary
  two-state keypad actions before emitting `keyUp`, but toggles multi-action
  children after their old-state `keyUp` events. Tests distinguish these cases.
- Numeric `FontSize`, four-component version strings and ignored `Software`
  metadata have reference-implementation precedent in OpenDeck's pinned
  `src-tauri/src/shared.rs` and `plugins/manifest.rs`. They are compatibility
  inputs, not the literal types promised by the current documentation.
- OS URL-scheme ownership is opt-in and platform-qualified separately from parsing
  and plugin delivery. Transport tests do not prove installation or OS dispatch.
- Windows/Linux process snapshots can miss applications that start and terminate
  between polls. Linux uses executable basenames where available, avoiding the
  kernel's truncated process-name field. Linux resume relies on logind's real
  `PrepareForSleep(false)` signal; unavailable services produce diagnostics.
- Extracting the Linux observer into a macOS compile harness verifies Rust API
  compatibility, not actual Linux process observation, D-Bus delivery, desktop
  behavior or packaging. Native Linux/Windows and hardware qualification remains
  separate from disposable host tests.
- Shared visual tests cover rendering contracts but do not establish that every
  external plugin asset or arbitrary encoder layout renders identically.
