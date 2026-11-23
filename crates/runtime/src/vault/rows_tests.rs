use super::*;
use serde_json::json;
use stream_dock_manager_storage::vault::rows::RowKind;

#[test]
fn personal_metadata_and_single_value_operations_ignore_unrelated_value_rows() {
    let root = tempfile::tempdir().unwrap();
    let store = Storage::open(root.path().join("manager-store")).unwrap();
    let vault = Vault::new(
        root.path().into(),
        Arc::new(tests::TestKeys::default()),
        store,
    );
    vault.initialize().unwrap();
    let create = |label: &str, text: &str| {
        vault
            .create_entry(VaultEntryInput {
                label: label.into(),
                kind: VaultKind::Text,
                protected: false,
                value: json!(text),
            })
            .unwrap()
    };
    let selected = create("Selected fixture", "selected-value");
    let unrelated = create("Unrelated fixture", "other-value");
    {
        let state = vault.current_state();
        let session = state.session.as_ref().unwrap();
        let (_, other_key) = unrelated.id.split_once('.').unwrap();
        session
            .vault
            .rows(|rows| rows.delete(RowKind::EntryValue, other_key))
            .unwrap();
    }

    let snapshot = vault.personal_entries().unwrap();
    assert_eq!(snapshot.entries.len(), 2);
    assert!(snapshot
        .entries
        .iter()
        .any(|entry| entry.id == unrelated.id));
    let renamed = vault
        .rename_entry(&selected.id, &selected.revision, "New selected name".into())
        .unwrap();
    assert_eq!(renamed.id, selected.id);
    assert_ne!(renamed.revision, selected.revision);

    let lease = vault.begin_entry_reveal().unwrap();
    assert!(vault
        .reveal_entry(lease, &selected.id, &selected.revision, None)
        .is_err());
    assert_eq!(
        vault
            .reveal_entry(lease, &renamed.id, &renamed.revision, None)
            .unwrap()
            .value,
        json!("selected-value")
    );
    assert!(vault
        .reveal_entry(lease, &unrelated.id, &unrelated.revision, None)
        .is_err());
    let replaced = vault
        .replace_entry(&renamed.id, &renamed.revision, json!("replacement"))
        .unwrap();
    assert_eq!(replaced.id, selected.id);
    assert_eq!(
        vault
            .reveal_entry(lease, &replaced.id, &replaced.revision, None)
            .unwrap()
            .value,
        json!("replacement")
    );
}
