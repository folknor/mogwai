# Order lists: OCO, OTO and OUO

An ORDER LIST is several orders submitted together whose fates are linked. The
venue serves all three linkage rules, so a bracket - an entry with a take-profit
and a stop that reap each other - is a real primitive here rather than two
independent legs a strategy has to reconcile itself.

## Admission is atomic, and a linked order may not travel alone

**Every member of a group is accepted, or the whole group is rejected and
nothing reaches the book.** A group is submitted as one `SubmitOrderGroup`
frame, and the venue validates every member - against the book and against the
rest of the group - before it accepts any of them. A refusal answers each member
with its own `OrderRejected` and emits no `OrderAccepted` at all.

Three things are guaranteed, and they are meant to be cited:

1. **Atomic admission**, as above.
2. **No tape advance between members.** The group is one engine call at one
   instant against one market reading, so no member meets a market a sibling did
   not, and a market member takes the same synthesized price as its siblings.
3. **Fill-atomic linkage.** A member that fills during the group has its rule
   applied to every sibling **including the ones admitted after it**, before the
   call returns and therefore before any sweep can look at them.

The third is the one that costs something to implement and is the reason the
frame exists at all. Sent leg by leg, a two-leg `Ouo` bracket lets the entry
FILL before the stop has been admitted: the shrink runs against a sibling that
is not on the book, adjusts nothing, and the stop then arrives at full size
beside a position that is already open. The pair's aggregate fill is twice the
bracket quantity, which for a crossed slice reverses the account. So:

**A LINKED ORDER SENT AS A BARE `SubmitOrder` IS REFUSED.** Not deprecated -
refused, at the protocol boundary, naming the group frame as the remedy. A venue
that served both routes could only promise atomicity for one code path, and a
consumer cannot build a safety argument on which path a consumer happened to take.

What a group frame must satisfy, refused at the protocol boundary and again by
the exchange core itself - the same validator, called from both, so the
guarantee below is a property of the venue rather than of the route a frame
took to reach it:

- **Self-contained.** Every `linked_order_ids` entry and every
  `parent_order_id` names another member. Admitting the group and admitting
  every sibling have to be the same statement.
- **One list, one symbol, unique ids.** Two list ids in one frame are two
  groups; a cross-symbol group would need two books at one instant.
- **Every member linked.** A standalone order in a group frame is asking for a
  guarantee that means nothing for it.
- **No `Ioc` or `Fok`.** A now-or-never order's fate is decided by the market
  rather than by admission.
- At most `MAX_GROUP_ORDERS` members, which is `MAX_LINKED_ORDERS` plus one, so
  a parent can travel with the maximum number of siblings.

**The one carve-out, and it is funds.** The admission pass judges every member
against the book as it is BEFORE the group runs, so it cannot see money an
earlier member's fill is about to spend. A member the venue can no longer fund
when its own turn comes is REJECTED, with its earlier siblings already accepted.
On that one axis the guarantee covers everything the venue can decide in
advance, and not a balance the group's own fills moved.

Whether your own group meets it is a question about YOUR orders, not about
brackets in general. A reduce-only order places no hold, so a group whose
exits are reduce-only never meets the carve-out. Whether an exit CAN be
reduce-only depends on the run's `oms_type` and is on your side of the wire:
under hedging an exit names the `position_id` it reduces and caps against that
position, while under netting the cap is taken against the account net, which
is a different number from a slice a consumer tracks locally. An exit that is not
reduce-only takes a hold like any other order - it takes initial margin per resting
contract at admission, and nothing clamps its fill to a position. See
[Netting and hedging](oms-types.md).

Size a group so its members are jointly affordable against the balance the venue
holds at submission and the carve-out is unreachable.

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
`QueryOrders`, scanned by nothing, and **placing no hold**. An order that
cannot execute must not tie up funds the parent's own fill needs.

Its parent's first fill RELEASES it: it takes the resting state it would have
been given at submit, draws a fresh fill-band trigger, starts its scan from the
release instant, and places its hold then. Release emits no wire frame -
the child was already accepted and its status has not changed.

A child of a parent that has ALREADY filled is live at once. That is the
fast-market bracket: a market entry that filled on arrival leaves its exits
nothing to wait for.

A parent that goes terminal WITHOUT filling takes its held children with it, in
the same batch. A child left waiting for a release that can never come would
rest for the life of the run. EVERY terminal path counts, not just the consumer's
own cancel and the clock's expiry: a reduce-only parent cancelled at its trigger
because there is nothing left to reduce, a post-only stop-limit rejected when it
would have taken liquidity, a resting or triggered order cancelled at its funds
check, and the control plane's silent out-of-band cancel all reap the same way.
The silent one reaps silently - the children leave the book and the truth store
records them cancelled, with no wire frame for either, which is the whole point
of that fault class.

AMENDING A HELD CHILD LEAVES IT HELD. A price amend moves the price the child
will rest at once it is released; it does not promote the child to a live limit,
does not give it a hold, and does not offer it any tape. A trigger amend
on a held conditional child is refused, and says which of the two it is - the
child is held, not triggered.

## What the venue refuses, and why

- A child that is a `Market` order, or `Ioc`/`Fok`. A released child RESTS, and
  a now-or-never child would be gone before its parent ever filled. Both are
  refused at the protocol boundary, so they are refused on every route a linked
  order can legally take: a linked bare `SubmitOrder` is refused for being bare,
  and every member of a `SubmitOrderGroup` is validated individually.
- `Oco` or `Ouo` naming nothing. It would silently behave like a standalone
  order, which a consumer discovers only by watching a stop it thought was reaped
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

A host submits an `OrderList` and the adapter sends its legs as ONE
`SubmitOrderGroup`, in the list's own order, each carrying its own linkage. A
leg the adapter cannot convert, or cannot resolve against the cache, aborts the
whole list before any leg is ANNOUNCED and before anything is dispatched: half a
bracket is worse than none, and a strategy that gets a rejection for its entry
can retry, while one whose stop silently never reached the venue cannot. Both
fallible passes run to completion first, so the pass that emits `OrderSubmitted`
and writes the mirror cannot fail partway and strand the legs behind it.

A group refusal names the LIST rather than a member, since a group is refused
whole; the adapter remembers which legs it dispatched under which list id and
fans the refusal back out as one `OrderRejected` per leg, because nautilus has
no order-list-scoped rejection event and a leg with no answer would wait on one
forever.

Nautilus's `ContingencyType` maps across unchanged, and a linked order must name
its `order_list_id` - the adapter refuses a contingency, a link or a parent
without one rather than passing an unkeyable linkage to the venue.
