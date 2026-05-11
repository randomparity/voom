use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ast::{ExtendsSource, MetadataNode, PhaseNode, PolicyAst};
use crate::bundled_policy;
use crate::errors::{DslError, DslPipelineError};
use crate::parse_policy;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) enum PolicySourceId {
    Inline,
    Bundled(String),
    File(PathBuf),
}

#[derive(Debug, Clone)]
pub(crate) struct ResolvedPolicyAst {
    pub(crate) ast: PolicyAst,
    pub(crate) source_id: PolicySourceId,
    pub(crate) extends_chain: Vec<String>,
    pub(crate) phase_sources: HashMap<String, PhaseComposition>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) enum PhaseComposition {
    Local,
    Inherited {
        source: String,
    },
    Extended {
        source: String,
        added_operations: usize,
    },
    Overridden {
        source: String,
    },
}

pub(crate) fn resolve_policy_file(path: &Path) -> Result<ResolvedPolicyAst, DslPipelineError> {
    let canonical = canonicalize_existing_path(path)?;
    let mut stack = Vec::new();
    resolve_file_path(&canonical, &mut stack)
}

pub(crate) fn resolve_policy_with_bundled(
    source: &str,
) -> Result<ResolvedPolicyAst, DslPipelineError> {
    let ast = parse_policy(source).map_err(DslPipelineError::Parse)?;
    resolve_ast(ast, PolicySourceId::Inline, None, &mut Vec::new())
}

fn resolve_file_path(
    path: &Path,
    stack: &mut Vec<PolicySourceId>,
) -> Result<ResolvedPolicyAst, DslPipelineError> {
    let source_id = PolicySourceId::File(path.to_path_buf());
    push_source(source_id.clone(), stack)?;
    let source = std::fs::read_to_string(path).map_err(|err| {
        DslPipelineError::Compile(DslError::compile(format!(
            "failed to read policy file {}: {err}",
            path.display()
        )))
    })?;
    let ast = parse_policy(&source).map_err(DslPipelineError::Parse)?;
    let base_dir = path.parent().map(Path::to_path_buf);
    let resolved = resolve_ast(ast, source_id, base_dir.as_deref(), stack);
    stack.pop();
    resolved
}

fn resolve_bundled_policy(
    name: &str,
    stack: &mut Vec<PolicySourceId>,
) -> Result<ResolvedPolicyAst, DslPipelineError> {
    let source_id = PolicySourceId::Bundled(name.to_owned());
    push_source(source_id.clone(), stack)?;
    let source = bundled_policy(name).ok_or_else(|| {
        DslPipelineError::Compile(DslError::compile(format!(
            "unknown bundled policy \"{name}\""
        )))
    })?;
    let ast = parse_policy(source).map_err(DslPipelineError::Parse)?;
    let resolved = resolve_ast(ast, source_id, None, stack);
    stack.pop();
    resolved
}

fn resolve_ast(
    mut ast: PolicyAst,
    source_id: PolicySourceId,
    base_dir: Option<&Path>,
    stack: &mut Vec<PolicySourceId>,
) -> Result<ResolvedPolicyAst, DslPipelineError> {
    let Some(extends) = ast.extends.take() else {
        reject_unresolved_phase_extends(&ast)?;
        return Ok(ResolvedPolicyAst {
            ast,
            source_id,
            extends_chain: Vec::new(),
            phase_sources: HashMap::new(),
        });
    };

    let parent = match extends {
        ExtendsSource::Bundled(name) => {
            if name.contains("://") {
                return Err(DslPipelineError::Compile(DslError::compile(format!(
                    "unsupported policy extends URI \"{name}\"; only file:// URIs and bundled policy names are supported"
                ))));
            }
            resolve_bundled_policy(&name, stack)?
        }
        ExtendsSource::File(uri) => {
            if source_id == PolicySourceId::Inline {
                return Err(DslPipelineError::Compile(DslError::compile(
                    "file:// policy extends requires compile_policy_file(path)",
                )));
            }
            let path = resolve_file_uri(&uri, base_dir)?;
            resolve_file_path(&path, stack)?
        }
    };

    merge_policy(parent, ast, source_id)
}

fn reject_unresolved_phase_extends(ast: &PolicyAst) -> Result<(), DslPipelineError> {
    if let Some(phase) = ast.phases.iter().find(|phase| phase.extend) {
        return Err(DslPipelineError::Compile(DslError::compile(format!(
            "phase \"{}\" uses extend but policy composition was not resolved",
            phase.name
        ))));
    }
    Ok(())
}

fn merge_policy(
    parent: ResolvedPolicyAst,
    mut child: PolicyAst,
    source_id: PolicySourceId,
) -> Result<ResolvedPolicyAst, DslPipelineError> {
    let child_source = source_label(&source_id);
    child.extends = None;
    let parent_ast = parent.ast;
    let parent_name = parent_ast.name.clone();
    let parent_phase_sources = parent.phase_sources;
    let mut child_phases = child.phases;
    let mut consumed_child_phases = vec![false; child_phases.len()];

    let mut phase_sources = HashMap::new();
    let mut phases = Vec::new();

    for parent_phase in parent_ast.phases {
        let parent_phase_name = parent_phase.name.clone();
        let parent_source = parent_phase_sources
            .get(&parent_phase_name)
            .map(phase_source_label)
            .unwrap_or_else(|| parent_name.clone());

        let child_phase_index = child_phases
            .iter()
            .enumerate()
            .find(|(index, phase)| {
                !consumed_child_phases[*index] && phase.name == parent_phase_name
            })
            .map(|(index, _phase)| index);

        if let Some(index) = child_phase_index {
            consumed_child_phases[index] = true;
            let child_phase = child_phases[index].clone();
            if child_phase.extend {
                let added_operations = child_phase.operations.len();
                phases.push(extend_phase(parent_phase, child_phase));
                phase_sources.insert(
                    parent_phase_name,
                    PhaseComposition::Extended {
                        source: parent_source,
                        added_operations,
                    },
                );
            } else {
                phases.push(clear_extend(child_phase));
                phase_sources.insert(
                    parent_phase_name,
                    PhaseComposition::Overridden {
                        source: child_source.clone(),
                    },
                );
            }
        } else {
            phases.push(clear_extend(parent_phase));
            phase_sources.insert(
                parent_phase_name,
                PhaseComposition::Inherited {
                    source: parent_source,
                },
            );
        }
    }

    for (index, phase) in child_phases.drain(..).enumerate() {
        if consumed_child_phases[index] {
            continue;
        }
        let name = phase.name.clone();
        if phase.extend {
            return Err(DslPipelineError::Compile(DslError::compile(format!(
                "phase \"{name}\" uses extend but no parent phase exists"
            ))));
        }
        phases.push(clear_extend(phase));
        phase_sources.insert(name, PhaseComposition::Local);
    }

    let ast = PolicyAst {
        name: child.name,
        extends: None,
        metadata: merge_metadata(parent_ast.metadata, child.metadata),
        config: child.config.or(parent_ast.config),
        phases,
        span: child.span,
    };

    let mut extends_chain = parent.extends_chain;
    extends_chain.push(source_label(&parent.source_id));

    Ok(ResolvedPolicyAst {
        ast,
        source_id,
        extends_chain,
        phase_sources,
    })
}

fn extend_phase(mut parent: PhaseNode, child: PhaseNode) -> PhaseNode {
    parent.extend = false;
    if child.skip_when.is_some() {
        parent.skip_when = child.skip_when;
    }
    if child.depends_on.is_some() {
        parent.depends_on = child.depends_on;
    }
    if child.run_if.is_some() {
        parent.run_if = child.run_if;
    }
    if child.on_error.is_some() {
        parent.on_error = child.on_error;
    }
    parent.operations.extend(child.operations);
    parent
}

fn clear_extend(mut phase: PhaseNode) -> PhaseNode {
    phase.extend = false;
    phase
}

fn merge_metadata(
    parent: Option<MetadataNode>,
    child: Option<MetadataNode>,
) -> Option<MetadataNode> {
    match (parent, child) {
        (None, None) => None,
        (Some(metadata), None) | (None, Some(metadata)) => Some(metadata),
        (Some(parent), Some(child)) => Some(MetadataNode {
            version: child.version.or(parent.version),
            author: child.author.or(parent.author),
            description: child.description.or(parent.description),
            requires_voom: child.requires_voom.or(parent.requires_voom),
            requires_tools: child.requires_tools.or(parent.requires_tools),
            test_fixtures: child.test_fixtures.or(parent.test_fixtures),
            span: child.span,
        }),
    }
}

fn resolve_file_uri(uri: &str, base_dir: Option<&Path>) -> Result<PathBuf, DslPipelineError> {
    let path = uri.strip_prefix("file://").ok_or_else(|| {
        DslPipelineError::Compile(DslError::compile(format!(
            "unsupported policy extends URI \"{uri}\"; only file:// URIs and bundled policy names are supported"
        )))
    })?;
    if path.is_empty() {
        return Err(DslPipelineError::Compile(DslError::compile(
            "file:// policy extends URI must include a path",
        )));
    }

    let path = Path::new(path);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else if let Some(base_dir) = base_dir {
        base_dir.join(path)
    } else {
        return Err(DslPipelineError::Compile(DslError::compile(
            "file:// policy extends requires compile_policy_file(path)",
        )));
    };
    canonicalize_existing_path(&path)
}

fn canonicalize_existing_path(path: &Path) -> Result<PathBuf, DslPipelineError> {
    std::fs::canonicalize(path).map_err(|err| {
        DslPipelineError::Compile(DslError::compile(format!(
            "failed to resolve policy file {}: {err}",
            path.display()
        )))
    })
}

fn push_source(
    source_id: PolicySourceId,
    stack: &mut Vec<PolicySourceId>,
) -> Result<(), DslPipelineError> {
    if let Some(index) = stack.iter().position(|source| source == &source_id) {
        let mut chain: Vec<String> = stack[index..].iter().map(source_label).collect();
        chain.push(source_label(&source_id));
        return Err(DslPipelineError::Compile(DslError::compile(format!(
            "cyclic policy extends: {}",
            chain.join(" -> ")
        ))));
    }
    stack.push(source_id);
    Ok(())
}

fn phase_source_label(composition: &PhaseComposition) -> String {
    match composition {
        PhaseComposition::Local => "local".to_owned(),
        PhaseComposition::Inherited { source }
        | PhaseComposition::Extended { source, .. }
        | PhaseComposition::Overridden { source } => source.clone(),
    }
}

fn source_label(source_id: &PolicySourceId) -> String {
    match source_id {
        PolicySourceId::Inline => "inline".to_owned(),
        PolicySourceId::Bundled(name) => name.clone(),
        PolicySourceId::File(path) => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use crate::composition::resolve_policy_with_bundled;

    #[test]
    fn explicit_empty_metadata_list_clears_parent_value() {
        let resolved = resolve_policy_with_bundled(
            r#"policy "child" extends "anime-base" {
                metadata {
                    requires_tools: []
                }

                phase subtitles {
                    keep subtitles
                }
            }"#,
        )
        .unwrap();

        let metadata = resolved.ast.metadata.unwrap();
        assert_eq!(metadata.requires_tools, Some(Vec::new()));
    }

    #[test]
    fn omitted_metadata_list_inherits_parent_value() {
        let resolved = resolve_policy_with_bundled(
            r#"policy "child" extends "anime-base" {
                metadata {
                    version: "1.1.0"
                }

                phase subtitles {
                    keep subtitles
                }
            }"#,
        )
        .unwrap();

        let metadata = resolved.ast.metadata.unwrap();
        assert_eq!(
            metadata.requires_tools,
            Some(vec![
                "ffmpeg".to_owned(),
                "mkvmerge".to_owned(),
                "mkvextract".to_owned()
            ])
        );
    }
}
