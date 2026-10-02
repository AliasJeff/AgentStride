use assert_cmd::Command;
use serde_json::Value;

#[test]
fn runtime_evaluation_is_successful_and_seed_stable() {
    let run = || {
        let output = Command::cargo_bin("as-eval")
            .expect("binary should build")
            .args([
                "--variant",
                "runtime",
                "--scenario",
                "chain",
                "--agents",
                "2",
                "--seed",
                "7",
            ])
            .output()
            .expect("evaluation should run");
        assert!(
            output.status.success(),
            "evaluation failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).expect("evaluation JSON")
    };

    let first = run();
    let second = run();
    assert_eq!(first, second);
    assert_eq!(first["variant"], "runtime");
    assert_eq!(first["success"], true);
    assert_eq!(first["completed"], 8);
    assert_eq!(first["recovered"], 8);
    assert_eq!(first["dependency_violations"], 0);
    assert_eq!(first["duplicate_claims"], 0);
}
