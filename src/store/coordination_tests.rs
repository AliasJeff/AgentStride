use crate::domain::{Actor, IssueDraft, IssueId, Priority, Status};

use super::{Store, StoreError};

fn id(value: &str) -> IssueId {
    IssueId::new(value).expect("valid issue id")
}

fn store() -> Store {
    let mut store = Store::open_in_memory().expect("open");
    store.migrate().expect("migrate");
    store
        .insert_draft(
            id("ag-aaaaaa"),
            IssueDraft {
                title: "lease task".to_owned(),
                description: String::new(),
                priority: Priority::new(1).expect("priority"),
                kind: crate::domain::IssueKind::Task,
                created_at: "2026-09-26T00:00:00Z".to_owned(),
            },
        )
        .expect("insert");
    store
}

#[test]
fn lease_owner_can_heartbeat_but_foreign_actor_cannot() {
    let mut store = store();
    let owner = Actor::new("agent-a").expect("actor");
    let foreign = Actor::new("agent-b").expect("actor");
    store
        .claim_issue_with_lease(
            &id("ag-aaaaaa"),
            owner.clone(),
            "2026-09-26T00:01:00Z",
            "2026-09-26T00:02:00Z",
        )
        .expect("claim");
    let lease = store
        .heartbeat(
            &id("ag-aaaaaa"),
            &owner,
            "2026-09-26T00:01:30Z",
            "2026-09-26T00:03:00Z",
        )
        .expect("heartbeat");
    assert_eq!(lease.until, "2026-09-26T00:03:00Z");
    assert!(matches!(
        store.heartbeat(
            &id("ag-aaaaaa"),
            &foreign,
            "2026-09-26T00:01:40Z",
            "2026-09-26T00:03:00Z",
        ),
        Err(StoreError::LeaseNotOwned { .. })
    ));
}

#[test]
fn expired_lease_can_be_reclaimed_and_returns_to_ready() {
    let mut store = store();
    let owner = Actor::new("agent-a").expect("actor");
    let supervisor = Actor::new("supervisor").expect("actor");
    store
        .claim_issue_with_lease(
            &id("ag-aaaaaa"),
            owner,
            "2026-09-26T00:01:00Z",
            "2026-09-26T00:02:00Z",
        )
        .expect("claim");
    let issue = store
        .reclaim_lease(&id("ag-aaaaaa"), &supervisor, "2026-09-26T00:03:00Z")
        .expect("reclaim");
    assert_eq!(issue.status, Status::Open);
    assert!(
        store
            .list_ready("2026-09-26T00:03:00Z")
            .expect("ready")
            .iter()
            .any(|candidate| candidate.id == id("ag-aaaaaa"))
    );
}
