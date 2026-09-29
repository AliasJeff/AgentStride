use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

use agent_stride::{
    domain::{Edge, EdgeType, IssueDraft, IssueId, IssueKind, Priority},
    graph::would_create_cycle,
    store::Store,
};

fn id(index: usize) -> IssueId {
    IssueId::new(format!("ag-{index:06x}")).expect("benchmark issue id")
}

fn populated_store(count: usize) -> Store {
    let mut store = Store::open_in_memory().expect("open benchmark database");
    store.migrate().expect("migrate benchmark database");
    for index in 0..count {
        store
            .insert_draft(
                id(index),
                IssueDraft {
                    title: format!("Task {index}"),
                    description: String::new(),
                    priority: Priority::new((index % 5) as u8).expect("benchmark priority"),
                    kind: IssueKind::Task,
                    created_at: format!("2026-09-26T00:00:{:02}Z", index % 60),
                },
            )
            .expect("insert benchmark issue");
    }
    store
}

fn ready_query(c: &mut Criterion) {
    let mut group = c.benchmark_group("ready_query");
    for count in [1_000usize, 10_000] {
        let store = populated_store(count);
        group.bench_with_input(
            BenchmarkId::from_parameter(count),
            &store,
            |bench, store| {
                bench.iter(|| black_box(store.list_ready("2026-09-27T00:00:00Z")));
            },
        );
    }
    group.finish();
}

fn cycle_check(c: &mut Criterion) {
    let mut group = c.benchmark_group("cycle_check");
    for depth in [100usize, 1_000] {
        let edges = (1..depth)
            .map(|index| {
                Edge::new(
                    id(index),
                    id(index - 1),
                    EdgeType::Blocks,
                    "2026-09-26T00:00:00Z".to_owned(),
                )
                .expect("benchmark edge")
            })
            .collect::<Vec<_>>();
        group.bench_with_input(
            BenchmarkId::from_parameter(depth),
            &edges,
            |bench, edges| {
                bench.iter(|| black_box(would_create_cycle(&id(0), &id(depth - 1), edges.clone())));
            },
        );
    }
    group.finish();
}

fn journal_page(c: &mut Criterion) {
    let store = populated_store(10_000);
    c.bench_function("journal_page_10000", |bench| {
        bench.iter(|| black_box(store.list_events(0, 10_000)));
    });
}

criterion_group!(benches, ready_query, cycle_check, journal_page);
criterion_main!(benches);
