//! Several stacks on one machine (ADR-0082 points 3 to 7): before the main
//! tree's daemon starts, `devstack up` compares the stack's ports, after
//! its shift on this machine, with what the other registered stacks claim.
//! A running stack's claim refuses the start, naming it, and offers a
//! shift; a stopped stack's only warns. `--shift auto|<n>|none` sets the
//! shift in the stack's registry entry, where it sticks; the daemon moves
//! the stack by it when it starts.

use anyhow::{Result, anyhow, bail};
use devstack_daemon::registry::{self, Entry, Other, SHIFT_STEP};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What `--shift` asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Asked {
    /// The lowest free multiple of [`SHIFT_STEP`].
    Auto,
    /// This shift; 0 is `none`, the stack file's own ports.
    To(u16),
}

/// `auto`, `none` (or `0`), or a multiple of 1000.
pub fn parse(text: &str) -> Result<Asked> {
    let t = text.trim().trim_start_matches('+');
    match t {
        "auto" => Ok(Asked::Auto),
        "none" | "0" => Ok(Asked::To(0)),
        _ => {
            let n: u16 = t.parse().map_err(|_| {
                anyhow!(
                    "--shift {text}: `auto`, `none` or a multiple of {SHIFT_STEP} (1000, 2000, ...)"
                )
            })?;
            if !n.is_multiple_of(SHIFT_STEP) {
                bail!("--shift {text}: a shift is a multiple of {SHIFT_STEP} (1000, 2000, ...)");
            }
            Ok(Asked::To(n))
        }
    }
}

/// The ports of the main tree at shift 0: what its stack file declares
/// (services, announced ports, the ports its `urls` name) and what its
/// daemon claimed for the main slot last time, moved back by the shift it
/// had (its containers' host ports, which only Compose knows).
pub fn base_ports(stack: &devstack_config::Stack, own: Option<&Entry>) -> BTreeSet<u16> {
    let mut out: BTreeSet<u16> = devstack_daemon::ports::declared(stack)
        .iter()
        .map(|e| e.port)
        .collect();
    if let Some(e) = own {
        out.extend(e.claims.main.iter().filter_map(|p| p.checked_sub(e.shift)));
    }
    out
}

/// `base` moved by `shift`; `None` beyond 65535.
fn moved(base: &BTreeSet<u16>, shift: u16) -> Option<BTreeSet<u16>> {
    base.iter().map(|p| p.checked_add(shift)).collect()
}

/// Another stack that claims some of the ports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collision {
    pub name: String,
    pub repo: String,
    pub running: bool,
    /// Each port, with who of it claims it (`main` or a lane).
    pub ports: Vec<(u16, String)>,
}

impl Collision {
    fn ports_text(&self) -> String {
        let mut by: BTreeMap<&str, Vec<String>> = BTreeMap::new();
        for (p, who) in &self.ports {
            by.entry(who).or_default().push(p.to_string());
        }
        by.iter()
            .map(|(who, ps)| {
                if *who == "main" {
                    ps.join(", ")
                } else {
                    format!("{} (its lane {who})", ps.join(", "))
                }
            })
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// The other stacks that claim one of `ports`.
pub fn collisions(ports: &BTreeSet<u16>, others: &[Other]) -> Vec<Collision> {
    others
        .iter()
        .filter_map(|o| {
            let hit: Vec<(u16, String)> = ports
                .iter()
                .filter_map(|p| Some((*p, o.entry.claims.holder(*p)?.to_string())))
                .collect();
            (!hit.is_empty()).then(|| Collision {
                name: o.entry.name.clone(),
                repo: o.entry.repo.display().to_string(),
                running: o.running,
                ports: hit,
            })
        })
        .collect()
}

/// How many lanes `auto` keeps room for above the main ports.
const AUTO_LANES: u16 = 3;

/// The lowest shift, a multiple of [`SHIFT_STEP`] above 0, whose ports
/// (lanes 1 to 3 included, `step` apart) are free of every claim in
/// `claimed` and of what `in_use` (the OS) reports.
pub fn auto_shift(
    base: &BTreeSet<u16>,
    step: u16,
    claimed: &BTreeSet<u16>,
    in_use: impl Fn(u16) -> bool,
) -> Option<u16> {
    (1..=65u16).map(|k| k * SHIFT_STEP).find(|shift| {
        (0..=AUTO_LANES).all(|n| {
            let Some(off) = n.checked_mul(step).and_then(|o| o.checked_add(*shift)) else {
                return false;
            };
            base.iter().all(|p| {
                p.checked_add(off)
                    .is_some_and(|q| !claimed.contains(&q) && !in_use(q))
            })
        })
    })
}

/// What `up` does about the collisions at the shift it starts with.
#[derive(Debug, PartialEq, Eq)]
pub enum Decision {
    /// Start; say these warnings first (stopped stacks' claims).
    Go(Vec<String>),
    /// Start nothing: a running stack holds some of the ports.
    Refuse(String),
}

/// Refuse when a running stack claims one of the ports, naming it, and
/// offer the shifts (`suggested`: what `auto` would take); a stopped
/// stack's claim is a warning (ADR-0082 point 6).
pub fn decide(collisions: &[Collision], shift: u16, suggested: Option<u16>) -> Decision {
    let running: Vec<&Collision> = collisions.iter().filter(|c| c.running).collect();
    if !running.is_empty() {
        let who: Vec<String> = running
            .iter()
            .map(|c| {
                format!(
                    "the stack `{}` ({}) runs on {}",
                    c.name,
                    c.repo,
                    c.ports_text()
                )
            })
            .collect();
        let at = if shift == 0 {
            "its ports".to_string()
        } else {
            format!("its ports shifted by {shift}")
        };
        let auto = suggested
            .map(|s| format!(" (the lowest free one now: {s})"))
            .unwrap_or_default();
        return Decision::Refuse(format!(
            "this stack would start on {at}, and {}. Nothing started; whoever was first keeps its ports.\n\
             Start this stack moved by a port shift on this machine (its services, containers and links move; its data stays):\n  \
             devstack up --shift auto{auto}\n  \
             devstack up --shift <n>    (a multiple of {SHIFT_STEP})\n\
             The shift sticks to this stack on this machine until `devstack up --shift none`.",
            who.join("; ")
        ));
    }
    Decision::Go(
        collisions
            .iter()
            .map(|c| {
                format!(
                    "warning: the stopped stack `{}` ({}) claims {} too; whichever starts second cannot have them",
                    c.name,
                    c.repo,
                    c.ports_text()
                )
            })
            .collect(),
    )
}

/// The registry entry of the main tree's `stack_file` as it would be
/// registered, and the one registered now.
fn entries(stack_file: &Path, name: &str) -> Result<(Entry, Option<Entry>)> {
    let abs = devstack_proc::long_path(&std::path::absolute(stack_file)?);
    let repo = abs.parent().map(Path::to_path_buf).unwrap_or(abs.clone());
    let id = registry::id_for(&abs);
    let base = Entry {
        id: id.clone(),
        name: name.to_string(),
        file: abs.clone(),
        repo,
        ..Entry::default()
    };
    Ok((base, registry::read(&id)))
}

fn shift_flag(shift: u16) -> String {
    if shift == 0 {
        "none".into()
    } else {
        shift.to_string()
    }
}

/// Before `up` starts the main tree's daemon (`running`: it runs
/// already): check the ports at the shift it will run with, set an asked
/// shift, and say what changed. Errors refuse the start.
pub fn before_up(stack_file: &Path, running: bool, asked: Option<Asked>) -> Result<Vec<String>> {
    let stack = devstack_config::load(stack_file)?;
    let (base_entry, own) = entries(stack_file, &stack.stack.name)?;
    let now = own.as_ref().map_or(0, |e| e.shift);
    if running {
        // Its ports are its own already; another shift needs a new daemon.
        return match asked {
            None => Ok(Vec::new()),
            Some(Asked::Auto) if now > 0 => Ok(vec![format!(
                "the main working tree runs shifted by {now} already"
            )]),
            Some(Asked::To(n)) if n == now => Ok(Vec::new()),
            Some(a) => bail!(
                "the main working tree runs {}: `devstack down` first, then `devstack up --shift {}`",
                if now == 0 {
                    "on its own ports".to_string()
                } else {
                    format!("shifted by {now}")
                },
                match a {
                    Asked::Auto => "auto".to_string(),
                    Asked::To(n) => shift_flag(n),
                }
            ),
        };
    }
    let base = base_ports(&stack, own.as_ref());
    let others = registry::others(&base_entry.id);
    let claimed: BTreeSet<u16> = others.iter().flat_map(|o| o.entry.claims.ports()).collect();
    let step = stack.lanes.port_step;
    let auto = || {
        auto_shift(
            &base,
            step,
            &claimed,
            devstack_daemon::supervisor::port_in_use,
        )
    };
    let shift = match asked {
        None => now,
        Some(Asked::To(n)) => n,
        Some(Asked::Auto) => auto().ok_or_else(|| {
            anyhow!(
                "no shift up to 65000 moves this stack's ports, and three lanes' above them, to ports free of every other stack's and of what runs on this machine"
            )
        })?,
    };
    let ports = moved(&base, shift).ok_or_else(|| {
        anyhow!(
            "the shift {shift} moves a port of this stack beyond 65535: `devstack up --shift none`, or a smaller one"
        )
    })?;
    let found = collisions(&ports, &others);
    let mut notes = match decide(&found, shift, None) {
        Decision::Go(w) => w,
        Decision::Refuse(_) => {
            let suggested = if asked == Some(Asked::Auto) {
                None
            } else {
                auto()
            };
            match decide(&found, shift, suggested) {
                Decision::Refuse(why) => bail!("{why}"),
                Decision::Go(w) => w,
            }
        }
    };
    if shift != now {
        registry::set_shift(&base_entry, shift)
            .map_err(|e| anyhow!("could not keep the shift in the registry of stacks: {e}"))?;
    }
    if shift != now || asked.is_some() {
        notes.push(if shift == 0 {
            "the main working tree runs on the ports its stack file gives".to_string()
        } else {
            format!(
                "the main working tree runs shifted by {shift} on this machine (`devstack up --shift none` takes it back)"
            )
        });
    }
    Ok(notes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use devstack_daemon::registry::Claims;

    fn other(name: &str, running: bool, main: &[u16], lane: &[u16]) -> Other {
        Other {
            entry: Entry {
                id: format!("{name}-id"),
                name: name.into(),
                repo: format!("/work/{name}").into(),
                claims: Claims {
                    main: main.to_vec(),
                    lanes: if lane.is_empty() {
                        BTreeMap::new()
                    } else {
                        BTreeMap::from([("fix".to_string(), lane.to_vec())])
                    },
                },
                ..Entry::default()
            },
            running,
        }
    }

    #[test]
    fn shifts_parse_as_auto_none_or_a_multiple_of_1000() {
        assert_eq!(parse("auto").unwrap(), Asked::Auto);
        assert_eq!(parse("none").unwrap(), Asked::To(0));
        assert_eq!(parse("0").unwrap(), Asked::To(0));
        assert_eq!(parse("+2000").unwrap(), Asked::To(2000));
        assert!(parse("1500").is_err());
        assert!(parse("lots").is_err());
        assert!(parse("70000").is_err());
    }

    #[test]
    fn auto_takes_the_lowest_free_multiple_of_1000_with_room_for_three_lanes() {
        let base = BTreeSet::from([5000, 5432]);
        let none = BTreeSet::new();
        assert_eq!(auto_shift(&base, 100, &none, |_| false), Some(1000));
        // Another stack's lane 2 of +1000 is claimed: +2000.
        let claimed = BTreeSet::from([6200]);
        assert_eq!(auto_shift(&base, 100, &claimed, |_| false), Some(2000));
        // A port the OS reports in use counts the same.
        assert_eq!(auto_shift(&base, 100, &none, |p| p == 7732), Some(1000));
        assert_eq!(auto_shift(&base, 100, &none, |p| p == 6732), Some(2000));
        // Nothing fits below 65536.
        assert_eq!(
            auto_shift(&BTreeSet::from([64600]), 100, &none, |_| false),
            None
        );
    }

    #[test]
    fn a_running_stack_refuses_the_start_and_a_stopped_one_warns() {
        let ports = BTreeSet::from([5000, 5432]);
        let others = vec![
            other("shop", true, &[5000], &[]),
            other("docs", false, &[], &[5432]),
            other("blog", true, &[4000], &[]),
        ];
        let found = collisions(&ports, &others);
        assert_eq!(found.len(), 2, "{found:?}");
        match decide(&found, 0, Some(1000)) {
            Decision::Refuse(why) => {
                assert!(why.contains("`shop`") && why.contains("5000"), "{why}");
                assert!(
                    why.contains("--shift auto (the lowest free one now: 1000)"),
                    "{why}"
                );
                assert!(
                    why.contains("--shift <n>") && why.contains("--shift none"),
                    "{why}"
                );
                assert!(!why.contains("docs"), "only who runs: {why}");
            }
            d => panic!("{d:?}"),
        }
        // Only the stopped one: a warning, then go on.
        let found = collisions(&ports, &others[1..]);
        match decide(&found, 0, None) {
            Decision::Go(w) => {
                assert_eq!(w.len(), 1);
                assert!(
                    w[0].contains("`docs`") && w[0].contains("its lane fix"),
                    "{w:?}"
                );
            }
            d => panic!("{d:?}"),
        }
        // Nobody else: go on, nothing to say.
        assert_eq!(decide(&[], 1000, None), Decision::Go(Vec::new()));
    }

    #[test]
    fn the_base_ports_add_what_the_daemon_claimed_moved_back() {
        let stack = devstack_config::from_str(
            r#"version = 1
[stack]
name = "shop"
[services.api]
run = "x"
port = 5000
"#,
            Path::new("devstack.toml"),
        )
        .unwrap();
        let own = Entry {
            shift: 1000,
            claims: Claims {
                main: vec![6000, 6432],
                lanes: BTreeMap::new(),
            },
            ..Entry::default()
        };
        assert_eq!(base_ports(&stack, None), BTreeSet::from([5000]));
        assert_eq!(base_ports(&stack, Some(&own)), BTreeSet::from([5000, 5432]));
        assert_eq!(moved(&BTreeSet::from([65000]), 1000), None);
    }
}
