# ADR-0070: A review card comes before the code; an ADR records what was built

- **Status:** Proposed
- **Date:** 2026-10-27
- **Deciders:** project owner
- **Related:** [ADR-0001](0001-development-is-adr-driven.md) (superseded once this is accepted), [README](README.md), [plan](../plan.md), `AGENTS.md`

## Context

ADR-0001 made every architectural decision an ADR, `Proposed` before code
depends on it and `Accepted` before that code merges. An accepted ADR reads as
a reviewed one. In practice it was not:

- 69 ADRs were written in 13 days (2026-10-14 to 2026-10-27), most of them
  by AI agents, and many moved from `Proposed` to `Accepted` on the day they
  were written (0025 to 0029, 0031, 0033). One owner cannot review five
  pages a day of prose and still build.
- 33 of them were amended, noted or partly replaced since. Sorted by cause:
  - **Corrections** a review beforehand could have caught, in three areas.
    *Trust boundaries:* 0033 was accepted on 2026-10-16 and amended the next
    day because a lane's Compose file was root on the node; 0027's pinned
    certificate pairs were replaced by 0031 the same day; 0019 described a
    command that never existed. *Core concepts:* what a lane is (0024 amended
    twice on its first day, its point 4 replaced by 0030 a day later, its
    "devstack never creates a worktree" reversed by 0047). *User-visible
    terms:* 0040's separate `view` grant lived one day, 0031's share switch
    five; these two are the only breaking (`!`) commits in the history.
  - **Details found while building** (a 202 once leased, how long a hold
    lasts, a window brought to the front on Windows). They fail the README's
    own admission test and lengthen the text a reviewer has to read.
  - **Growth** (the tray moving into the hub, sharing extended to viewing
    and connecting, the store under `prepare`). Nothing was decided wrongly.

So the owner approved more than they had checked, and the prose made it hard
to see which few sentences carried the risk.

## Decision

1. **Before the code, a review card, not an ADR.** Work that touches an
   architectural point starts with a card in the issue or pull request that
   carries it: three to seven points, each one sentence, each tagged with one
   of the four categories below, with the alternative it beats in one
   sentence and what reversing it would cost. A point on a trust boundary
   also says what an attacker gains if it is wrong. A last line lists what
   the agent decides on its own, so that the owner sees it was not asked.
2. **What goes on a card.** A point that is expensive to reverse *and*
   - **trust:** moves a trust boundary (who may do what on which machine,
     what leaves the machine, credentials);
   - **concept:** fixes what a core term *is* (a lane, a node, a share), who
     owns it, who creates or removes it;
   - **contract:** fixes an outside contract (the stack file, the pool
     protocol, CLI flags and output that scripts read, state on disk);
   - **term:** adds a user-visible level, mode, switch or grant.
   Everything else (timeouts, status codes, ordering, window handling) the
   agent decides and records in a code comment or the changelog.
3. **The owner approves points, not prose.** A card point is approved when
   the owner says so for that point; a point they strike or change goes back
   to the agent. Code may be spiked before, and merges after.
4. **The ADR follows the code.** When the work merges, an ADR records the
   decision as built, under the README's admission test, about a page. Its
   status is `Accepted` with a link to the approved card when its points went
   through one, and `Recorded` when they did not (a decision that turned out
   to be architectural only while building). `Recorded` is a fact, not a
   verdict: it says nobody approved it beforehand.
5. **Amendments only for the four categories.** A later change to a card
   category of an ADR goes through a card and is named in the ADR's status.
   Details found while building are not amendments; `*Paths as built*`
   notes stay as they are.
6. **`Open` keeps its meaning.** An open question is still written down with
   its options, and nothing builds on either side until a card decides it.
7. **ADR-0001 to ADR-0069 stay as written.** Their status lines are not
   rewritten and none is demoted to a plan: they are the record of what was
   decided, with the alternatives, and the code follows them. The index says
   that `Accepted` before ADR-0070 means accepted under ADR-0001, often
   without a review beforehand. When work touches a card-category point of
   one of them, the card names that point and the owner reviews it then;
   the ADR is amended or superseded as usual.

## Alternatives considered

- **Keep ADR-0001 and review harder.** Fails on volume: the owner cannot read
  every ADR in full, and the risky sentences look like the safe ones.
- **ADRs only after the fact, no review beforehand.** Cheapest, but the
  corrections above came from exactly the points nobody checked, and two of
  them broke users.
- **Rewrite the existing ADRs as plan entries, or mark them all `Recorded`.**
  Erases the record of the alternatives, and rewrites history in a public
  repository; a reader would no longer know what the code was built against.
  A dated note in the index says the same honestly.
- **The card in `docs/plan.md`.** The plan is the overview and is edited
  continuously; a card belongs to the change it gates and stays with it.

## Consequences

- The owner reads a few sentences per change, and sees which category each
  one is in. What the agent decided alone is listed, so silence is visible.
- An ADR is written later and may say less about options considered while
  building; the card keeps the ones that mattered.
- A new status, `Recorded`, and a note on the old `Accepted` in the index.
- Agents must sort points into categories; a mis-sorted point is the new
  failure mode. The owner can move a point from the "decides on its own"
  line onto the card at any time.

## Enforced by

- `docs/adr/README.md` (status values with `Recorded`, the note on ADRs
  before 0070, amendments)
- `docs/adr/0000-template.md` (status values)
- `AGENTS.md` (core expectation 3: card before code, ADR after)
- `.github/pull_request_template.md` (a section for the card), to be added

## What this ADR does not say

- That every change needs a card: only one with a point in the four
  categories.
- How a card is presented in a chat with an agent; it ends up in the issue
  or pull request either way.
