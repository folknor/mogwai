# mogwai architecture

How the venue is built and why. Four subjects in order: the venue's runtime
shape and its account model; accounts, risk, instruments and valuation; the
boatyard, clocks, delivery and history; and the generator, the tape identity and
the fingerprint. The workspace map is last. Each lives in its own file now,
mapped below; this page is the entry point and the reading order.

The prose was written to be read straight through, so a section is an entry
point rather than a self-contained unit: several state a rule and then spend
paragraphs on the case that made the rule necessary, and that second half is
usually the part worth reading. The headings were added on 2026-08-26 and the
split into files followed on 2026-08-27; neither pass moved, cut or reworded
anything.

Mogwai is a fake venue. A direct launcher starts one foreground process and
receives a versioned readiness record as one JSON line on the child's stdout.
The process binds one endpoint and owns an open set of resolved instruments,
generated rivers, and one ledger per account.

## The map

Step two of the split, 2026-08-27: the four subjects and the workspace
map now live in five files, each a contiguous slice of the one long
document in its original reading order, verified byte-for-byte at the
split. The sections each file carries:

- `architecture-runtime.md` - the venue's runtime shape and account model:
  - Two topologies
  - Rivers, accounts and passengers
  - Delivery is attributed, not broadcast
  - Callsigns and eviction
  - Close codes carry no meaning; close reasons do
  - What order an upgrade does its work in
  - Freeze, resume, retirement and the TTL
  - The account id on a snapshot is a label
- `architecture-accounts.md` - accounts, risk, instruments and the order path:
  - Risk policy, and why the venue enforces it
  - The order-type surface
  - Order lists, linkage, and atomic admission
  - Conditionals, trails, and tick resolution without per-tick evaluation
  - Time in force, and why an expiry is not a cancel
  - The six instrument classes
  - Equity accounts, margin, and holds
  - Funding, valuation and the policy currency
  - Symbol resolution, the river cap, and what `/instruments` advertises
  - The route surface
  - The two output-byte admissions
  - The upgrade query string is the whole binding
  - Declared feed gaps, and what is not one
  - How an order rests, triggers and fills
- `architecture-delivery.md` - boats, clocks, delivery and history:
  - The boatyard: rivers, boats, tickets and placement
  - Placement on demand, and who pays for a cold river
  - Clocks: the venue's reference against a boat's
  - Generator havoc forks the river
  - History is read over the socket
  - The fill sweeper
- `architecture-generator.md` - the generator, the tape identity and the fingerprint:
  - The tape identity, and what each bump changed
  - The synthetic top of book
  - The generator's volatility process and its three rails
  - Two structural fidelity limits no parameter can remove
  - Fingerprint ranges diagnose; mechanism validation gates
  - A future's ledger, calendars and fees
  - Seeding, the tape origin, and the two durations
  - History refusals, and what bounds a page
  - The adapter's side of the wire
  - Raw fills, and how the fill band was calibrated
  - The three tiers of a market reading
- `architecture-workspace.md` - the workspace and the offline evidence toolbox:
  - The workspace and the offline evidence toolbox
