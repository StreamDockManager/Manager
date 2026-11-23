//! Saved workspace membership is independent of device appearance. This first
//! consumer is the official Randomizer; other plugins receive no inventory.
use super::*;
use std::collections::BTreeSet;

pub const CONFIGURED_ACTIONS_OWNER: &str = "app.streamdockmanager.plugins.randomizer-dice";
const EVENT: &str = "streamDockManagerConfiguredActions";

#[derive(Default)]
pub(super) struct ConfiguredActions {
    revision: Option<u64>,
    actions: BTreeSet<String>,
}

impl Hub {
    pub(super) fn configured_actions_event(&self, owner: &str) -> Option<Value> {
        if owner != CONFIGURED_ACTIONS_OWNER || self.configured_actions.revision.is_none() {
            return None;
        }
        let declared = &self.capabilities.get(owner)?.actions;
        let actions: Vec<_> = self
            .configured_actions
            .actions
            .iter()
            .filter(|id| declared.contains_key(id.as_str()))
            .collect();
        Some(json!({"event":EVENT,"payload":{"actions":actions}}))
    }
}

impl PluginHost {
    pub fn needs_configured_actions(&self, revision: u64) -> bool {
        self.hub
            .lock()
            .unwrap()
            .configured_actions
            .revision
            .is_none_or(|known| revision > known)
    }

    pub fn set_configured_actions(&self, revision: u64, actions: BTreeSet<String>) {
        self.hub
            .lock()
            .unwrap()
            .set_configured_actions(revision, actions);
    }
}

impl Hub {
    fn set_configured_actions(&mut self, revision: u64, actions: BTreeSet<String>) {
        if self
            .configured_actions
            .revision
            .is_some_and(|known| revision <= known)
        {
            return;
        }
        let previous = self.configured_actions_event(CONFIGURED_ACTIONS_OWNER);
        self.configured_actions = ConfiguredActions {
            revision: Some(revision),
            actions,
        };
        let event = self.configured_actions_event(CONFIGURED_ACTIONS_OWNER);
        if event != previous {
            if let (Some(event), Some(queue)) = (
                event,
                self.sessions.queues.get_mut(CONFIGURED_ACTIONS_OWNER),
            ) {
                // An unregistered process will receive the newest state on
                // registration. Avoid retaining outdated states on its queue.
                if queue.wake.is_some() {
                    queue.push_back(event);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_actions_only_reach_the_owner_and_precede_pending_appearance() {
        let mut hub = Hub::default();
        let owner = CONFIGURED_ACTIONS_OWNER.to_owned();
        let action = format!("{owner}.dice-roll-counter");
        let cap = capabilities::Capabilities {
            actions: [(action.clone(), Default::default())].into(),
            ..Default::default()
        };
        hub.capabilities.insert(owner.clone(), cap.clone());
        hub.sessions.tokens.insert("owner".into(), owner.clone());
        hub.sessions.tokens.insert("other".into(), "other".into());
        hub.set_configured_actions(1, [action.clone(), "foreign.action".into()].into());
        let poll = mio::Poll::new().unwrap();
        let wake = Arc::new(mio::Waker::new(poll.registry(), mio::Token(1)).unwrap());
        assert!(transport::register(
            &mut hub,
            &json!({"event":"registerPlugin","uuid":"other"}),
            wake.clone()
        )
        .is_some());
        assert!(hub
            .sessions
            .queues
            .get_mut("other")
            .unwrap()
            .take_batch()
            .unwrap()
            .iter()
            .all(|entry| entry.value["event"] != EVENT));
        hub.sessions
            .queues
            .entry(owner.clone())
            .or_default()
            .push_back(json!({"event":"willAppear"}));
        assert!(transport::register(
            &mut hub,
            &json!({"event":"registerPlugin","uuid":"owner","payload":{"capabilities":cap}}),
            wake
        )
        .is_some());
        let first = hub
            .sessions
            .queues
            .get_mut(&owner)
            .unwrap()
            .take_batch()
            .unwrap();
        assert_eq!(
            first[0].value,
            json!({"event":EVENT,"payload":{"actions":[action]}})
        );
        assert!(first
            .iter()
            .any(|entry| entry.value["event"] == "willAppear"));
        hub.set_configured_actions(2, [action.clone()].into());
        hub.set_configured_actions(1, BTreeSet::new());
        assert!(hub
            .sessions
            .queues
            .get_mut(&owner)
            .unwrap()
            .take_batch()
            .unwrap()
            .is_empty());
        hub.set_configured_actions(3, BTreeSet::new());
        let removed = hub
            .sessions
            .queues
            .get_mut(&owner)
            .unwrap()
            .take_batch()
            .unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(
            removed[0].value,
            json!({"event":EVENT,"payload":{"actions":[]}})
        );
    }
}
