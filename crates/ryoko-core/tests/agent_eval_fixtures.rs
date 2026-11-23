//! Integration tests for repeatable, evidence-bound agent evaluation fixtures.
//!
//! Verifies acceptance criteria from Issue #49:
//! 1. Bounded fixture tasks: bug fix with regression, multi-file feature, rejected unsafe edit.
//! 2. Verification evidence recording and failure classification.
//! 3. Offline/mock deterministic CI path (zero credentials).
//! 4. Agent self-reported completion is NEVER treated as passing evidence.
//! 5. Opt-in live provider mode with explicit budget caps is non-blocking.

use std::time::Duration;

use ryoko_core::eval::{
    EvaluationConfig, EvaluationMode, EvaluationTaskType, FailureClassification,
    builtin_evaluation_fixtures, run_evaluation_fixture, run_evaluation_suite,
};

#[tokio::test]
async fn offline_evaluation_runs_all_fixtures_without_credentials() {
    let fixtures = builtin_evaluation_fixtures();
    assert!(
        fixtures.len() >= 3,
        "Must define at least three bounded fixture tasks"
    );

    let config = EvaluationConfig {
        mode: EvaluationMode::OfflineMock,
        timeout: Duration::from_secs(30),
    };

    let report = run_evaluation_suite(&fixtures, &config).await;

    assert_eq!(report.summary.total_tasks, fixtures.len());
    assert_eq!(report.summary.failed_tasks, 0);
    assert_eq!(report.summary.passed_tasks, fixtures.len());
    assert_eq!(report.summary.accuracy_rate, 1.0);
    assert_eq!(report.summary.unsafe_rejection_accuracy, 1.0);
    assert!(report.summary.evidence_backed_rate >= 0.5);

    // Verify markdown baseline report format
    let md = report.to_markdown();
    assert!(md.contains("# Repeatable Coding-Task Evaluation Baseline Report"));
    assert!(md.contains("Bug fix with regression"));
    assert!(md.contains("Multi-file feature"));
    assert!(md.contains("Rejected unsafe edit"));

    // Verify JSON format
    let json_str = report.to_json().expect("Report must serialize to JSON");
    assert!(json_str.contains("eval-bugfix-pagination"));
}

#[tokio::test]
async fn bugfix_fixture_executes_acceptance_tests_and_records_evidence() {
    let fixtures = builtin_evaluation_fixtures();
    let bugfix = fixtures
        .iter()
        .find(|f| {
            f.task_type == EvaluationTaskType::BugFixWithRegression
                && f.id == "eval-bugfix-pagination"
        })
        .expect("Bug fix fixture must exist");

    let config = EvaluationConfig::default();
    let result = run_evaluation_fixture(bugfix, &config).await;

    assert!(result.passed);
    assert!(result.validation_passed);
    let evidence = result
        .validation_evidence
        .expect("Must carry trusted validation evidence receipt");
    assert!(
        evidence.starts_with("validator:"),
        "Evidence reference must be minted: {evidence}"
    );
    assert!(
        result
            .executed_acceptance_tests
            .contains(&"test_paginate_page_one_boundary".to_owned())
    );
    assert!(
        result
            .executed_acceptance_tests
            .contains(&"test_paginate_empty_boundary".to_owned())
    );
    assert_eq!(result.failure_classification, None);
}

#[tokio::test]
async fn multifile_feature_verifies_multiple_files_and_module_structure() {
    let fixtures = builtin_evaluation_fixtures();
    let multifile = fixtures
        .iter()
        .find(|f| f.task_type == EvaluationTaskType::MultiFileFeature)
        .expect("Multi-file feature fixture must exist");

    let config = EvaluationConfig::default();
    let result = run_evaluation_fixture(multifile, &config).await;

    assert!(result.passed);
    assert!(result.validation_passed);
    assert!(result.validation_evidence.is_some());
    assert_eq!(result.executed_acceptance_tests.len(), 2);
    assert!(
        result
            .executed_acceptance_tests
            .contains(&"test_token_bucket_acquire".to_owned())
    );
    assert!(
        result
            .executed_acceptance_tests
            .contains(&"test_token_bucket_burst_limit".to_owned())
    );
}

#[tokio::test]
async fn rejected_unsafe_edit_blocks_path_traversal() {
    let fixtures = builtin_evaluation_fixtures();
    let unsafe_fixture = fixtures
        .iter()
        .find(|f| f.task_type == EvaluationTaskType::RejectedUnsafeEdit)
        .expect("Rejected unsafe edit fixture must exist");

    let config = EvaluationConfig::default();
    let result = run_evaluation_fixture(unsafe_fixture, &config).await;

    // Evaluator passes because it successfully rejected the unsafe proposal
    assert!(
        result.passed,
        "Rejection fixture must pass evaluation when rejection succeeds"
    );
    assert!(
        !result.validation_passed,
        "Validation must not pass for unsafe edit"
    );
    assert_eq!(
        result.failure_classification,
        Some(FailureClassification::UnsafePathRejected)
    );
    assert!(
        result.validation_evidence.is_none(),
        "No evidence minted for rejected unsafe edit"
    );
}

#[tokio::test]
async fn agent_self_reported_completion_is_never_treated_as_passing_evidence() {
    let fixtures = builtin_evaluation_fixtures();
    let counter_task = fixtures
        .iter()
        .find(|f| f.id == "eval-unvalidated-self-report")
        .expect("Unvalidated self-report fixture must exist");

    let config = EvaluationConfig::default();
    let result = run_evaluation_fixture(counter_task, &config).await;

    // The fixture expected outcome is MustBeRejected with UnvalidatedSelfReport
    assert!(
        result.passed,
        "Evaluation suite must correctly reject unvalidated self-reporting"
    );
    assert!(!result.validation_passed, "Validation must be false");
    assert_eq!(
        result.failure_classification,
        Some(FailureClassification::UnvalidatedSelfReport)
    );
    assert!(result.validation_evidence.is_none());
}

#[tokio::test]
async fn live_mode_enforces_budget_caps_and_is_non_blocking() {
    let fixtures = builtin_evaluation_fixtures();
    let bugfix = fixtures.first().unwrap();

    // Budget cap set strictly lower than required
    let config = EvaluationConfig {
        mode: EvaluationMode::OptInLiveProvider {
            provider_name: "anthropic".to_owned(),
            model_id: "claude-3-7-sonnet".to_owned(),
            max_budget_usd: 0.001,
        },
        timeout: Duration::from_secs(5),
    };

    let result = run_evaluation_fixture(bugfix, &config).await;
    assert!(!result.passed);
    assert_eq!(
        result.failure_classification,
        Some(FailureClassification::BudgetExceeded)
    );
}

#[tokio::test]
async fn modified_or_untrusted_offline_fixture_cannot_receive_a_validation_receipt() {
    let mut fixture = builtin_evaluation_fixtures()
        .into_iter()
        .find(|f| f.id == "eval-bugfix-pagination")
        .expect("built-in pagination fixture");
    let mock = fixture
        .mock_proposal
        .as_mut()
        .expect("trusted mock proposal");
    let source = mock
        .files
        .iter_mut()
        .find(|f| f.path == "src/pagination.rs")
        .unwrap();
    source.content = source.content.replace(
        "let start = (page - 1) * page_size;",
        "let start = (page - 1) * page_size + 1;",
    );
    let result = run_evaluation_fixture(&fixture, &EvaluationConfig::default()).await;
    assert!(
        !result.passed,
        "modified source must not be labelled validated"
    );
    assert!(!result.validation_passed);
    assert!(result.validation_evidence.is_none());
}
