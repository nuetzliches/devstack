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
            enrol,
        } => {
            if telemetry {
                poolcmd::telemetry(&pool, Some("on"))?;
            }
            poolcmd::join(
                &pool,
                &coordinator,
                &fingerprint,
                name.as_deref(),
                !no_wait,
                enrol.as_deref(),
            )
        }
        Cmd::Pool { pool, action } => match action {
            None => poolcmd::pool(pool.as_deref()),
            Some(PoolAction::Shared) => hubcmd::shared(pool.as_deref()),
            Some(PoolAction::Provisioned { state }) => poolcmd::provisioned(
                pool.as_deref()
                    .unwrap_or(devstack_node::member::DEFAULT_POOL),
                state.as_deref(),
            ),
            Some(PoolAction::Connect { lane }) => hubcmd::connect_shared(&lane, pool.as_deref()),
            Some(PoolAction::Disconnect { lane }) => {
                hubcmd::disconnect_shared(&lane, pool.as_deref())
            }
            Some(PoolAction::DefaultOn { on }) => poolcmd::default_on(on.as_deref()),
            Some(PoolAction::Placement { lane }) => crate::client::print_placement(file, &lane),
            Some(PoolAction::Skills { action }) => poolcmd::skills(
                pool.as_deref()
                    .unwrap_or(devstack_node::member::DEFAULT_POOL),
                action,
            ),
            Some(PoolAction::Upstreams { action }) => poolcmd::upstreams(
                pool.as_deref()
                    .unwrap_or(devstack_node::member::DEFAULT_POOL),
                action,
            ),
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
        NodeAction::Serve {
            listen,
            dir,
            provisioner,
        } => poolcmd::serve(dir, &listen, provisioner.as_deref()),
        NodeAction::Provisioning(a) => poolcmd::provisioning(
            a.dir,
            a.per.as_deref(),
            a.confirm.as_deref(),
            a.idle_minutes,
            a.max_nodes,
            (a.max_cpus, a.max_memory_gib),
        ),
        NodeAction::Confirm { node, dir } => poolcmd::confirm(dir, &node),
        NodeAction::GiveBack { node, dir } => poolcmd::give_back(dir, &node),
        NodeAction::MockProvisioner(a) => poolcmd::mock_provisioner(
            &a.listen,
            &a.address,
            (a.cpus, a.memory_gib),
            a.estimate,
            a.refuse,
            a.fail_deletes,
            a.token,
            a.exec,
        ),
        NodeAction::Fingerprint { dir } => poolcmd::print_fingerprint(dir),
        NodeAction::RunLane { on, lane, keep } => poolcmd::run_lane(file, &on, &lane, keep),
        NodeAction::Agent {
            listen,
            api,
            max_cpus,
            max_memory_gib,
            dedicated,
            pool,
            print_unit: true,
        } => {
            print!(
                "{}",
                poolcmd::agent_unit(
                    &pool,
                    &listen,
                    api.as_deref(),
                    max_cpus,
                    max_memory_gib,
                    dedicated
                )?
            );
            Ok(())
        }
        NodeAction::Agent {
            listen,
            api,
            max_cpus,
            max_memory_gib,
            dedicated,
            pool,
            print_unit: false,
        } => poolcmd::agent(&pool, &listen, api, max_cpus, max_memory_gib, dedicated),
        NodeAction::Members { action, dir } => match action.unwrap_or(MembersAction::List) {
            MembersAction::List => poolcmd::members_list(dir),
            MembersAction::Approve { member, admin } => {
                poolcmd::members_approve(dir, &member, admin)
            }
            MembersAction::Role { member, role } => poolcmd::members_role(dir, &member, &role),
            MembersAction::Tags { member, tags } => poolcmd::members_tags(dir, &member, &tags),
            MembersAction::Node { member, state } => poolcmd::members_node(dir, &member, &state),
            MembersAction::Provision { member, state } => {
                poolcmd::members_provision(dir, &member, &state)
            }
            MembersAction::Policy {
                node,
                allow,
                repo,
                memory_headroom,
                max_cpus,
                share_percent,
                network,
                clear,
            } => poolcmd::members_policy(
                dir,
                &node,
                allow,
                repo,
                (memory_headroom, max_cpus, share_percent),
                network,
                clear,
            ),
            MembersAction::Remove { member } => poolcmd::members_remove(dir, &member),
        },
    }
}
