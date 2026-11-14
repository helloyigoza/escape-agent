use std::time::Duration;

use oc_store::{NewSession, NewSessionTree};
use oc_subagents::{BudgetError, Policy, SpawnError, resolve_model};
use oc_tools::{ToolContext, ToolError};
use oc_types::SessionId;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::runtime::Runtime;

use super::super::settings::{
    SubagentSettings, available_agents, available_models, model_defaults, model_value,
    requested_role,
};
use super::super::tree::{
    budget_error, load_info, load_node, optional_str, runtime_error, spawn_error, store_error,
};
use super::super::wait::enqueue_prompt;
use super::{PartitionMeta, SpawnContext, SpawnedPartitionChild};

pub(super) fn parse_optional_usize(input: &Value, key: &str) -> Result<Option<usize>, ToolError> {
    let Some(val) = input.get(key) else {
        return Ok(None);
    };
    if val.is_null() {
        return Ok(None);
    }
    let Some(n) = val.as_u64() else {
        return Err(ToolError::new(format!(
            "{key} must be a non-negative integer"
        )));
    };
    usize::try_from(n)
        .map(Some)
        .map_err(|_| ToolError::new(format!("{key} is too large")))
}

pub(super) async fn resolve_manual_partition(
    ctx: &ToolContext,
    prompt: &str,
    partition_index: Option<usize>,
    partition_count: Option<usize>,
) -> Result<Option<(String, PartitionMeta)>, ToolError> {
    match (partition_index, partition_count) {
        (None, None) => Ok(None),
        (Some(_), None) | (None, Some(_)) => Err(ToolError::new(
            "partition_index and partition_count must both be provided together",
        )),
        (Some(idx), Some(count)) => {
            if !(2..=16).contains(&count) {
                return Err(ToolError::new("partition_count must be between 2 and 16"));
            }
            if idx >= count {
                return Err(ToolError::new(format!(
                    "partition_index ({idx}) must be less than partition_count ({count})"
                )));
            }
            let partitioned =
                oc_tools::code_map::partition_task(ctx, &ctx.directory, prompt, count, 2000)
                    .await?;
            let section = partitioned.prompt_section_for(idx).unwrap_or_default();
            let files = partitioned
                .slices
                .iter()
                .find(|s| s.index == idx)
                .map(|s| s.files.clone())
                .unwrap_or_default();
            Ok(Some((
                section,
                PartitionMeta {
                    index: idx,
                    count: partitioned.slices.len().max(1),
                    files,
                },
            )))
        }
    }
}

pub(super) async fn prepare_spawn_context(
    runtime: &Runtime,
    caller: &str,
    agent: &str,
    input: &Value,
    settings: &SubagentSettings,
) -> Result<SpawnContext, ToolError> {
    let mut parent = load_node(runtime, caller).await?;
    let mut limits = settings.limits.clone();
    if let Some(over) = settings.overrides.get(&parent.agent) {
        if over.allow_spawn == Some(false) {
            return Err(spawn_error(&SpawnError::SpawnNotAllowed {
                id: caller.to_owned(),
            }));
        }
        if let Some(max_children) = over.max_children {
            limits.max_children = max_children;
        }
    }
    let active = runtime
        .inner
        .config
        .store
        .active_child_count(caller)
        .await
        .unwrap_or(parent.child_count);
    if active < parent.child_count {
        parent.child_count = active;
    }
    let available = available_agents(&runtime.inner.config.config);
    let policy = Policy::new(limits.clone());
    let requested_depth = input
        .get("spawn")
        .and_then(|spawn| spawn.get("max_depth"))
        .and_then(Value::as_u64)
        .map(|depth| u32::try_from(depth).unwrap_or(u32::MAX));
    let mut plan = policy
        .spawn(&parent, agent, requested_depth, &available)
        .map_err(|error| spawn_error(&error))?;

    if let Some(over) = settings.overrides.get(agent)
        && over.allow_spawn == Some(false)
    {
        plan.spawn_allowed = false;
    }
    if input
        .get("spawn")
        .and_then(|spawn| spawn.get("allow"))
        .and_then(Value::as_bool)
        == Some(false)
    {
        plan.spawn_allowed = false;
    }

    let parent_model = parent
        .model
        .clone()
        .or_else(|| runtime.inner.config.config.model())
        .unwrap_or_default();
    let defaults = model_defaults(&runtime.inner.config.config, settings);
    let choice = resolve_model(
        optional_str(input, "model").as_deref(),
        Some(agent),
        defaults,
        &parent_model,
        &available_models(&runtime.inner.config.catalog, &runtime.inner.config.client),
    )
    .map_err(|error| ToolError::new(format!("ModelUnavailableError: {error}")))?;
    let model_value = model_value(
        &choice.reference,
        &runtime.inner.config.catalog,
        &runtime.inner.config.client,
    )?;
    let created_model = serde_json::from_value(model_value.clone()).ok();
    let parent_info = load_info(runtime, caller).await?;
    let role = requested_role(input, settings, agent);
    let lane = optional_str(input, "lane");
    let group = optional_str(input, "group").and_then(|g| {
        let trimmed = g.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_owned())
        }
    });

    Ok(SpawnContext {
        runtime: runtime.clone(),
        caller: caller.to_owned(),
        agent: agent.to_owned(),
        settings: settings.clone(),
        limits,
        active_children: parent.child_count,
        plan,
        parent_root_id: parent.root_id,
        parent_info,
        model_value,
        created_model,
        role,
        lane,
        group,
    })
}

pub(super) async fn reserve_spawn_slots(
    runtime: &Runtime,
    caller: &str,
    limits: &oc_subagents::Limits,
    active_children: u32,
    count: u32,
    cancel: Option<&CancellationToken>,
) -> Result<(), ToolError> {
    loop {
        let result = {
            let mut state = runtime.inner.subagents.lock().expect("subagent state");
            state
                .spawn_budget(caller, limits, active_children)
                .try_spawn_batch(count, oc_types::now_millis())
        };
        match result {
            Ok(()) => return Ok(()),
            Err(
                ref error @ BudgetError::RateLimited {
                    retry_after_ms,
                    max_per_minute,
                    ..
                },
            ) if max_per_minute >= count && retry_after_ms > 0 && retry_after_ms <= 25_000 => {
                let wait_ms = u64::try_from(retry_after_ms.saturating_add(25)).unwrap_or(25);
                let wait = Duration::from_millis(wait_ms);
                if let Some(cancel) = cancel {
                    tokio::select! {
                        () = cancel.cancelled() => return Err(budget_error(error)),
                        () = tokio::time::sleep(wait) => {}
                    }
                } else {
                    tokio::time::sleep(wait).await;
                }
            }
            Err(error) => return Err(budget_error(&error)),
        }
    }
}

pub(super) async fn create_and_notify_child(
    ctx_spawn: &SpawnContext,
    description: &str,
    prompt: &str,
    partition: Option<&PartitionMeta>,
) -> Result<SpawnedPartitionChild, ToolError> {
    let runtime = &ctx_spawn.runtime;
    let child = SessionId::new().into_inner();
    let mut meta = serde_json::Map::new();
    if let Some(lane) = &ctx_spawn.lane {
        meta.insert("lane".to_owned(), Value::String(lane.clone()));
    }
    if let Some(group) = &ctx_spawn.group {
        meta.insert("group".to_owned(), Value::String(group.clone()));
    }
    if let Some(part) = partition {
        meta.insert("partition_index".to_owned(), json!(part.index));
        meta.insert("partition_count".to_owned(), json!(part.count));
        meta.insert("partition_files".to_owned(), json!(part.files));
    }
    let metadata = if meta.is_empty() {
        None
    } else {
        Some(Value::Object(meta))
    };
    let created_metadata = metadata.as_ref().and_then(Value::as_object).cloned();

    runtime
        .inner
        .config
        .store
        .create_session_with_tree(
            NewSession {
                id: child.clone(),
                parent_id: Some(ctx_spawn.caller.clone()),
                project_id: ctx_spawn.parent_info.project_id.to_string(),
                directory: ctx_spawn.parent_info.location.directory.clone(),
                title: Some(description.to_owned()),
                agent: Some(ctx_spawn.agent.clone()),
                model: Some(ctx_spawn.model_value.clone()),
                metadata,
                permissions: None,
                time_created: oc_types::now_millis(),
            },
            NewSessionTree {
                root_id: ctx_spawn.parent_root_id.clone(),
                depth: ctx_spawn.plan.depth,
                role: ctx_spawn.role.as_str().to_owned(),
                spawn_allowed: ctx_spawn.plan.spawn_allowed,
            },
        )
        .await
        .map_err(|error| store_error(&error))?;

    runtime
        .emit(
            &child,
            &ctx_spawn.parent_info.location.directory,
            oc_types::payload::SessionCreated {
                session_id: SessionId::from_wire(child.clone()),
                project_id: ctx_spawn.parent_info.project_id.clone(),
                location: ctx_spawn.parent_info.location.clone(),
                subpath: ctx_spawn.parent_info.subpath.clone(),
                parent_id: Some(SessionId::from_wire(ctx_spawn.caller.clone())),
                slug: format!("subagent-{}", &child[child.len().saturating_sub(6)..]),
                title: Some(description.to_owned()),
                agent: Some(ctx_spawn.agent.clone()),
                model: ctx_spawn.created_model.clone(),
                metadata: created_metadata,
                permissions: None,
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
        )
        .await
        .map_err(|error| runtime_error(&error))?;

    let prompt_id = enqueue_prompt(runtime, &child, prompt, &ctx_spawn.caller).await?;
    runtime
        .notify(&child)
        .await
        .map_err(|error| runtime_error(&error))?;

    Ok(SpawnedPartitionChild {
        session_id: child,
        prompt_id,
        partition_index: partition.map_or(0, |p| p.index),
        partition_count: partition.map_or(1, |p| p.count),
        files: partition.map(|p| p.files.clone()).unwrap_or_default(),
    })
}
