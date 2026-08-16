# Order lists: OCO, OTO and OUO

An ORDER LIST is several orders submitted together whose fates are linked. The
venue serves all three linkage rules, so a bracket - an entry with a take-profit
and a stop that reap each other - is a real primitive here rather than two
independent legs a strategy has to reconcile itself.

## The model

A linkage is a GROUP ID plus a RULE, carried by each member. The venue holds no
list object: it holds what each order says about the orders it names, and it
acts on that at the instant a member fills.

Every order may carry a `link`:

```json
{
  "order_list_id": "OL-7",
  "contingency": "Oco",
  "linked_order_ids": ["EXIT-SL"],
  "parent_order_id": null
}
```

- **`order_list_id`** - the list's identity, shared by every member.
- **`contingency`** - what a fill of THIS order does to the orders it names.
- **`linked_order_ids`** - the siblings the rule acts on. Capped at 8.
- **`parent_order_id`** - the order this one WAITS FOR, if any.

An order with no `link` is a standalone order and behaves exactly as it always
did.

## The three rules

| `contingency` | A fill of this order... |
|---|---|
| `NoContingency` | does nothing to the orders it names. |
| `Oco` | CANCELS every named sibling still resting. |
| `Oto` | releases its children (which name it as their parent). |
| `Ouo` | SHRINKS every named sibling by the filled quantity, cancelling one the shrink takes to zero. |

`Oco` cancels on ANY fill, not only a full one: a venue that let a partially
filled take-profit leave its stop live would leave a bracket holding two live
exits for one position. `Ouo` is the variant that survives partial fills - the
surviving leg tracks how much of the position is actually left.

## When the reap happens, and why it matters

**At the instant the fill is committed**, in the same batch, never on a later
sweep. That timing is the whole reason the primitive is worth having: a tape
span can cross both legs' prices at once, so a stop reaped after the batch would
already have filled against the same prints that filled its take-profit. Both
legs of an OCO pair swept together produce exactly one fill and one cancel.

## Children: what `parent_order_id` buys

A child is ACCEPTED at submit and then HELD: on the book, answerable to
`QueryOrders`, scanned by nothing, and **holding no reservation**. An order that
cannot execute must not tie up funds the parent's own fill needs.

Its parent's first fill RELEASES it: it takes the resting state it would have
been given at submit, draws a fresh fill-band trigger, starts its scan from the
release instant, and takes its reservation then. Release emits no wire frame -
the child was already accepted and its status has not changed.

A child of a parent that has ALREADY filled is live at once. That is the
fast-market bracket: a market entry that filled on arrival leaves its exits
nothing to wait for.

A parent that goes terminal WITHOUT filling - cancelled, or expired - takes its
held children with it, in the same batch. A child left waiting for a release
that can never come would rest for the life of the run.

## What the venue refuses, and why

- A child that is a `Market` order, or `Ioc`/`Fok`. A released child RESTS, and
  a now-or-never child would be gone before its parent ever filled.
- `Oco` or `Ouo` naming nothing. It would silently behave like a standalone
  order, which a client discovers only by watching a stop it thought was reaped
  go on to fill.
- An order linking or parenting ITSELF.
- A child whose parent the venue has not seen. Submit a list in its own order,
  parent first - which is the order nautilus's `OrderList` already puts them in.
- A child whose parent is terminal and never filled.
- **A child of a child.** One generation only. This is not tidiness: cancelling
  one order reaps its children in the same batch, and the byte budget that
  cancel reserves has to be computable before it runs. A chain of children would
  make that unbounded, and it costs nothing real - a bracket is one entry and
  its exits.
- More than 8 children on one parent, for the same reason.

## From nautilus

A host submits an `OrderList` and the adapter sends its legs as ordinary submits
carrying their linkage, in the list's own order. A leg that fails conversion
aborts the whole list before anything is dispatched: half a bracket is worse
than none, and a strategy that gets a rejection for its entry can retry, while
one whose stop silently never reached the venue cannot.

Nautilus's `ContingencyType` maps across unchanged, and a linked order must name
its `order_list_id` - the adapter refuses a contingency, a link or a parent
without one rather than passing an unkeyable linkage to the venue.
