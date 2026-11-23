//! Authenticated picker and credentialed summary use the same lifecycle as live input.
use super::*;
use crate::vault_references::{EntryName, EntryPage};
const ENTRY: &str = "12345678-1234-4234-8234-123456789abc.aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
struct Fixture {
    selected: AtomicBool,
    epoch: uuid::Uuid,
    calls: std::sync::atomic::AtomicUsize,
}
impl References for Fixture {
    fn version(&self) -> Result<ReviewVersion, &'static str> {
        Ok(ReviewVersion {
            epoch: self.epoch,
            revision: 1,
        })
    }
    fn entries(
        &self,
        _: &ReferenceScope,
        approved: bool,
        _: usize,
        _: Option<ReviewVersion>,
    ) -> Result<EntryPage, &'static str> {
        Ok(EntryPage {
            entries: if !approved || self.selected.load(Ordering::Acquire) {
                vec![EntryName {
                    id: ENTRY.into(),
                    name: "Synthetic key".into(),
                }]
            } else {
                vec![]
            },
            next: None,
        })
    }
    fn select(
        &self,
        _: &ReferenceScope,
        id: &str,
        _: ReviewVersion,
        live: &dyn Fn() -> bool,
    ) -> Result<EntryName, &'static str> {
        assert_eq!(id, ENTRY);
        assert!(live());
        self.selected.store(true, Ordering::Release);
        Ok(EntryName {
            id: id.into(),
            name: "Synthetic key".into(),
        })
    }
    fn resolve(&self, _: &RecipeEntry) -> Result<RecipeValue, &'static str> {
        self.selected
            .load(Ordering::Acquire)
            .then_some(RecipeValue::Authorization(Zeroizing::new(
                "test-key".into(),
            )))
            .ok_or("permission_denied")
    }
}
impl Services for Fixture {
    fn references(&self) -> Option<&dyn References> {
        Some(self)
    }
    fn clipboard_available(&self) -> bool {
        true
    }
    fn copy_available(&self) -> bool {
        false
    }
    fn capture(&self, _: &dyn Fn() -> bool) -> Result<Zeroizing<String>, &'static str> {
        Ok(Zeroizing::new("sample".into()))
    }
    fn copy(&self, _: &str) -> Result<(), &'static str> {
        unreachable!()
    }
    fn summarize(&self, _: &str, _: &dyn Fn() -> bool) -> Result<Zeroizing<String>, &'static str> {
        unreachable!()
    }
    fn credentialed_summary(
        &self,
        text: &str,
        key: &str,
        cancel: &dyn Fn() -> bool,
    ) -> Result<Zeroizing<String>, &'static str> {
        assert!(!cancel());
        assert_eq!(text, "reviewed");
        assert_eq!(key, "test-key");
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Zeroizing::new("done".into()))
    }
    fn open(&self, _: &str) -> Result<(), &'static str> {
        Ok(())
    }
    fn close(&self, _: &str) {}
}
#[test]
fn selected_entry_and_transfer_consent_are_required() {
    let root = tempfile::tempdir().unwrap();
    let store = Storage::open(root.path().join("store")).unwrap();
    let owner = PermissionOwner {
        plugin: "fixture".into(),
        installation: "install".into(),
        inventory: "hash".into(),
        generation: store.stats().unwrap().storage_generation,
    };
    let scope = json!({"id":"vault.access","mode":"use-entry","field":"authorization","operation":"authorize-request","origin":"https://api.example.test","recipe":"clipboard-summary"});
    let scopes = json!([scope,{"id":"clipboard.read-text","mode":"manager-recipe"},{"id":"ui.popup","mode":"text-result"},{"id":"network.https","recipe":"clipboard-summary","origin":"https://api.example.test","data_class":"clipboard.text"}]);
    let session=Session{owner:owner.clone(),declaration:serde_json::from_value(json!({"version":1,"actions":{"fixture.press":{"purpose":"Summary","optional":scopes}}})).unwrap()};
    let rev = store
        .set_permission_decisions(
            owner,
            0,
            session.declaration.actions["fixture.press"]
                .optional
                .iter()
                .map(|s| ("fixture.press".into(), scope_key(s), true))
                .collect(),
        )
        .unwrap();
    let fixture = Arc::new(Fixture {
        epoch: uuid::Uuid::new_v4(),
        calls: Default::default(),
        selected: AtomicBool::new(false),
    });
    let flow = Workflow::default();
    flow.attach(fixture.clone());
    let lease = Arc::new(AtomicBool::new(true));
    let token = || {
        flow.start(
            session.clone(),
            lease.clone(),
            Arc::new(AtomicBool::new(false)),
            "context".into(),
            "fixture.press",
            rev,
        )
        .unwrap()
    };
    let call = |proto: &str, method: &str, payload: Value| {
        let message = json!({"event":"streamDockManagerRpcRequest","request_id":uuid::Uuid::new_v4().to_string(),"protocol_id":proto,"method":method,"payload":payload});
        let mut value = Value::Null;
        assert!(dispatch(
            &flow,
            &store,
            Some(&session),
            Some(&lease),
            (&message, 500),
            Ok(()),
            |reply| {
                value = reply;
                true
            }
        ));
        value
    };
    let summary = || {
        call(
            BROKER,
            "clipboard-summary",
            json!({"scope":scope,"entry_ref":ENTRY,"activation_token":token()}),
        )
    };
    assert_eq!(summary()["error"]["code"], "permission_denied");
    assert_eq!(
        call(
            vault_references::PROTOCOL,
            "list-vault-entries",
            json!({"scope":scope,"action":"fixture.press"})
        )["error"]["code"],
        "permission_denied"
    );
    let select = || {
        call(
            vault_references::PROTOCOL,
            "select-vault-entry",
            json!({"scope":scope,"action":"fixture.press","activation_token":token()}),
        )
    };
    let ack = select();
    let id = ack["result"]["review_id"].as_str().unwrap();
    assert!(!ack.to_string().contains("Synthetic key"));
    assert_eq!(
        flow.view(&store, id).unwrap()["page"]["entries"][0]["id"],
        ENTRY
    );
    flow.respond(&store, id, "deny", None).unwrap();
    assert_eq!(flow.notices(&lease)[0]["outcome"], "denied");
    let ack = select();
    let id = ack["result"]["review_id"].as_str().unwrap();
    flow.respond(&store, id, "select", Some(Zeroizing::new(ENTRY.into())))
        .unwrap();
    assert_eq!(flow.notices(&lease)[0]["entry"]["id"], ENTRY);
    let ack = summary();
    let id = ack["result"]["result_handle"].as_str().unwrap();
    assert!(!ack.to_string().contains("test-key"));
    assert_eq!(flow.view(&store, id).unwrap()["text"], "sample");
    assert_eq!(fixture.calls.load(Ordering::Acquire), 0);
    flow.respond(&store, id, "send", Some(Zeroizing::new("reviewed".into())))
        .unwrap();
    assert_eq!(fixture.calls.load(Ordering::Acquire), 1);
    assert_eq!(flow.view(&store, id).unwrap()["text"], "done");
    assert!(flow.respond(&store, id, "send", None).is_err());
}
