// apps/conary/src/commands/packaging_mcp/server/verification_tests.rs

use super::*;
use conary_agent_contract::{
    CcsVerificationOutcome, CcsVerificationReport, CcsVerificationRequest,
};

#[tokio::test]
async fn verification_adapter_preserves_contract_schema_and_failure_status() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("missing.ccs").to_str().unwrap().to_owned();
    let policy = temp.path().join("policy.toml").to_str().unwrap().to_owned();
    let server = PackagingMcpServer::new(PackagingAgentService::with_operations_dir(
        temp.path().join("operations"),
    ));
    let tool = server.get_tool("conary.packaging.verify_artifact").unwrap();
    assert_eq!(
        tool.output_schema,
        Some(rmcp::handler::server::tool::schema_for_output::<
            CcsVerificationReport,
        >())
    );
    let required = tool
        .input_schema
        .get("required")
        .unwrap()
        .as_array()
        .unwrap();
    assert!(required.contains(&serde_json::json!("policy")));
    let expected = crate::commands::ccs::verification::verification_report(&package, Some(&policy));
    let output = server
        .verify_artifact(Parameters(CcsVerificationRequest { package, policy }))
        .await
        .unwrap();
    assert_eq!(output.is_error, Some(true));
    let decoded: CcsVerificationReport =
        serde_json::from_value(output.structured_content.unwrap()).unwrap();
    assert_eq!(decoded, expected);
    assert!(matches!(
        decoded.outcome,
        CcsVerificationOutcome::Failed { .. }
    ));
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 0);
}
