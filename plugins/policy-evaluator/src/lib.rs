//! Policy evaluation library.
//!
//! Evaluates compiled policies against introspected media files to produce
//! [`Plan`](voom_domain::plan::Plan) structs describing the operations needed.
//! This crate is called directly by the CLI and does not implement
//! `voom_kernel::Plugin`.

pub mod condition;
pub mod container_compat;
pub mod evaluator;
pub mod field;
pub mod filter;

pub use evaluator::{
    evaluate, evaluate_with_evaluation_context, EvaluationContext, EvaluationOutcome,
    SinglePhaseEvaluationContext,
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use voom_domain::MediaFile;

    #[test]
    fn test_evaluate_returns_result_with_plans() {
        let policy =
            voom_dsl::compile_policy(r#"policy "test" { phase init { container mkv } }"#).unwrap();
        let file = MediaFile::new(PathBuf::from("/test/video.mkv"));
        let result = evaluate(&policy, &file);
        assert_eq!(result.plans.len(), 1);
    }
}
