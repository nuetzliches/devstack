//! The pool (ADR-0031): the coordinator and the node agent (`devstack
//! node`), joining, the pools of this machine, and lanes others share.

use crate::{Cmd, MembersAction, NodeAction, PoolAction, hubcmd, poolcmd};
use anyhow::Result;
use std::path::Path;

pub(crate) fn run(file: &Path, cmd: Cmd) -> Result<()> {
    match cmd {
        Cmd::Node { action } => node(file, action),
        Cmd::Join {
            coordinator,
            fingerprint,
            name,
            no_wait,
            pool,
            telemetry,
        } => {
            if telemetry {
                poolcmd::telemetry(&pool, Some("on"))?;
            }
            poolcmd::join(&pool, &coordinator, &fingerprint, name.as_deref(), !no_wait)
        }
        Cmd::Pool { pool, action } => match action {
            None => poolcmd::pool(pool.as_deref()),
            Some(PoolAction::Shared) => hubcmd::shared(pool.as_deref()),
            Some(PoolAction::Telemetry { state }) => poolcmd::telemetry(
                pool.as_deref()
                    .unwrap_or(devstack_node::member::DEFAULT_POOL),
                state.as_deref(),
            ),
        },
        Cmd::Open {
            lane,
            service,
            pool,
        } => hubcmd::open_shared(&lane, service.as_deref(), pool.as_deref()),
        _ => super::misrouted(),
    }
}

/// `devstack node`: the coordinator, and a node's agent.
fn node(file: &Path, action: NodeAction) -> Result<()> {
    match action {
        NodeAction::Init { addresses, dir } => poolcmd::init(dir, &addresses),
        NodeAction::Serve { listen, dir } => poolcmd::serve(dir, &listen),
        NodeAction::Fingerprint { dir } => poolcmd::print_fingerprint(dir),
        NodeAction::RunLane { on, lane, keep } => poolcmd::run_lane(file, &on, &lane, keep),
        NodeAction::Agent {
            listen,
            api,
            max_cpus,
            max_memory_gib,
            dedicated,
            pool,
        } => poolcmd::agent(&pool, &listen, api, max_cpus, max_memory_gib, dedicated),
        NodeAction::Members { action, dir } => match action.unwrap_or(MembersAction::List) {
            MembersAction::List => poolcmd::members_list(dir),
            MembersAction::Approve { member, admin } => {
                poolcmd::members_approve(dir, &member, admin)
            }
            MembersAction::Role { member, role } => poolcmd::members_role(dir, &member, &role),
            MembersAction::Tags { member, tags } => poolcmd::members_tags(dir, &member, &tags),
            MembersAction::Node { member, state } => poolcmd::members_node(dir, &member, &state),
            MembersAction::Policy {
                node,
                allow,
                repo,
                memory_headroom,
                max_cpus,
                network,
                clear,
            } => poolcmd::members_policy(
                dir,
                &node,
                allow,
                repo,
                (memory_headroom, max_cpus),
                network,
                clear,
            ),
            MembersAction::Remove { member } => poolcmd::members_remove(dir, &member),
        },
    }
}
