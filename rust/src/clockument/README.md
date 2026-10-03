# 🕰️ Clockument Tools

Utilities for creating, storing, and sharing multi-document transactions with Automerge CRDTs.

## Background

CRDTs are extremely bulletproof when it comes to transactions. They're designed to be safely mutable, by multiple users, with immutable histories. They can be forked, merged, and edited at any point in history. 

This works great as long as you're just working with *one* CRDT. But what if one CRDT links another CRDT as a *dependency?* Then, we have problems:

- The root dependency might arrive before the dependency!
- The dependency might NEVER arrive, leaving the root waiting forever!
- Forking and merging the CRDTs might produce different behavior based on the synced version!

Conceptually, if the dependency must be tracked alongside the parent, the solution is just to jam it into a larger CRDT. But giant CRDTs are not performant, particularly during sync or many-user contention.

Instead, these problems can be resolved by using a clockument. 

### What is a Clockument?

A clockument is defined very simply: 

> *clockument*: A CRDT that pins a set of `dependencies(A)`, at a particular head `A`.

In `dependencies(A)` is a list of tuples containing another CRDT's document ID, and the `heads` the dependency document is pinned at.

As an example, consider an Address Book represented with CRDTs. The root document `address-book` contains a list of links to each contact:

```yaml
# document ID: "address-book"
# current heads: ROOT_HEADS
contacts:
    "Miles O'Brien": obrien @ OBRIEN_HEADS
    "Julian Bashir": bashir @ BASHIR_HEADS
```

Here, each file path within `address-book` links to a separate CRDT, `obrien` and `bashir`. The `obrien` dependency is expected to have heads `OBRIEN_HEADS`, at the root heads `ROOT_HEADS`.

Here is the dependency `obrien`: 
```yaml
# document ID: "obrien"
# current heads: OBRIEN_HEADS
name: "Miles O'Brien"
```

Now, let's say we add a phone number to `obrien`. Because it is a dependency, we must `transact_at(OBRIEN_HEADS)`, which results in:

```yaml
# document ID: "obrien"
# current heads: OBRIEN_HEADS_2
name: "Miles O'Brien"
phone: "+12345" 
```

The current heads of `obrien` are now `OBRIEN_HEADS_2`! But our `root` clockument hasn't been updated, so its current heads `ROOT_HEADS` still points to `obrien @ OBRIEN_HEADS`. 

Because `root` is a clockument, we now need to explicitly update the link: 

```yaml
# document ID: "address-book"
# current heads: ROOT_HEADS_2
contacts:
    "Miles O'Brien": obrien @ OBRIEN_HEADS_2
    "Julian Bashir": bashir @ BASHIR_HEADS
```

We can look at the clockument `dependencies` function for our changes:

```yaml
dependencies(ROOT_HEADS)   : [obrien@OBRIEN_HEADS,   bashir@BASHIR_HEADS]
dependencies(ROOT_HEADS_2) : [obrien@OBRIEN_HEADS_2, bashir@BASHIR_HEADS]
```

That is all a *clockument* is: a root document that pins its dependencies at each of its heads.

When we want to make a change, we explicitly track that change. When we commit on a specific dependency, we always `transact_at` at some pinned heads. 

This conceptually links the two CRDTs in lockstep: a dependency cannot be updated without informing the root. If a dependency *is* updated without informing the root, that update is ignored, because the new head is never pinned. 

In other words, `obrien` could have *overall* heads `[OBRIEN_HEADS_2, SOME_NONSENSE, UTTER_GARBAGE]`. Because `OBRIEN_HEADS_2` is the only one pinned, and we always use `transact_at`, `SOME_NONSENSE` and `UTTER_GARBAGE` are ignored forever and are never merged in.

### Clockument Syncing

This is where it gets hairy. It's easy to atomically sync a single document. But how can we atomically sync a *whole dependency network?*

In `automerge-repo`, `samod`, and similar repo-based CRDT synchronization frameworks, sync is always done *greedily:* each document is synced as soon as it is available. This strategy doesn't work for clockuments.

Take our update from before, as an example. Let's say there are two peers, Jadzia and Nerys. Both have `address-book @ ROOT_HEADS`, and `obrien @ OBRIEN_HEADS`, and `bashir @ BASHIR_HEADS`. 

Then, Jadzia makes the change: she updates her local copy of `obrien @ OBRIEN_HEADS` to `obrien @ OBRIEN_HEADS_2`, and pins it inside `address-book @ ROOT_HEADS_2`. 

If the peers are using a greedy sync framework, Jadzia's computer might sync `address-book` before `obrien`. Then, Nerys would receive `address-book @ ROOT_HEADS_2`, and attempt to checkout `obrien @ OBRIEN_HEADS_2`. But Nerys wouldn't have the correct heads of `obrien`, because Jadzia's computer hasn't synced them yet!

Worse, if Jadzia's computer explodes before it can send `obrien`, Nerys might be waiting on `OBRIEN_HEADS_2` *forever.* In that case, `address-book` is permanently corrupted: it will have heads `ROOT_HEADS_2` that can *never* have its `dependencies(ROOT_HEADS_2)` resolve. 

(What is Nerys supposed to do, here? She could wait forever, or decide to time-out after ten seconds, ten minutes, ten hours...)

A similar issue occurs even without any peers at all. If Jadzia's repo greedily writes `address-book` to *disk*, but crashes before writing `obrien`, the disk copy of the clockument is corrupted as well!

But we don't *have* to sync documents greedily with peers, or write documents greedily to disk. Let's maintain an invariant:

> *Invariant 1*: In well-behaved storage, if heads `Z` are persisted, all `dependencies(Z)` must also be persisted.

(Here, "storage" means "any persistent storage" -- a peer, a server, a hard disk. "Persisted," here, means "written to the storage.")

With this sync invariant, Jadzia can be sure that her `address-book` is uploaded to Nerys only *after* `obrien` is also uploaded. And Nerys can be sure that if she is able to access `address-book @ ROOT_HEADS_2` off her local hard disk, that `obrien @ OBRIEN_HEADS_2` is also present. 

Great! We've created a reliable, bulletproof, perfectly atomic, immutable CRDT network. Now, nothing could *ever* go wrong. 

### Error Detection

Unfortunately, computers are bad at being reliable. Let's take a look at our invariant again: 

> *Invariant 1*: In well-behaved storage, if heads `Z` are persisted, all `dependencies(Z)` must also be persisted.

"Well-behaved" is doing a lot of heavy-lifting here. We *can't* assume that Jadzia's hard drive is well-behaved! If a storage isn't behaving well, we'll call it *negligent*.

Take the case where an ultra-high-energy cosmic ray hits Jadzia's hard drive, which spontaneously deletes `obrien` entirely. A new peer, Ben, connects to Jadzia, and downloads `address-book @ ROOT_HEADS_2`. But, he can't *use* the document, because the `obrien` dependency is not available anymore!

However, now that we maintain *Invariant 1*, another property emerges:

> If heads `A` are persisted, but not all `dependencies(A)` are persisted, the storage is negligent. 

Before, Nerys was stuck waiting forever, with zero knowledge on whether Jadzia's computer would ever provide `obrien`. Now, because the network is maintaining *Invariant 1*, Ben is able to immediately notice that Jadzia is negligent, just by asking her if she has `obrien` at all.

### Error Handling

Once Ben notices that Jadzia is negligent, he can decide what to do. To be safe, we assume the following:

> If a required dependency `document @ A` is not present on a negligent peer, `document @ A` MIGHT NEVER become available on the negligent peer.

Ben has two options:

1. Continue with the sync, even knowing that `obrien` is completely unavailable. 
2. Reject Jadzia's copy of `address-book` entirely. 

In most cases, #1 is the realistic choice. Jadzia doesn't deserve to be kicked out of the address book, just because a cosmic ray hit her hard drive!

In future commits, Ben can repair `address-book` by rolling-back or removing `obrien`, or he can just leave it as a broken reference. Because CRDTs have immutable history, Ben can't remove the broken reference from the past.

All together, with option #1, we can promise the following unified invariant to Ben: 

> If `document @ A` is available for transactions, each of `dependencies(A)` is either available, or is permanently (or semi-permanently) unavailable. 

### Edge Cases

#### Nested Dependencies

What if `obrien` has dependencies of its own? 

As a result of Invariant 1, we can actually greedily sync both `obrien` and all of *its* pinned dependencies! Because `obrien` is pinned in an ancestor clockument (`address-book`), all of its dependencies must eventually bubble-up to `address-book` itself. 

As such, we can slightly adjust the definition of a clockument, within our specific sync system:

> *clockument*: A CRDT that pins a set of `dependencies(A)`, at a particular head `A`, AND is not pinned itself by a parent clockument.

In other words, it is always safe to greedily sync a document, given the following conditions:

1. It is pinned by some ancestor document(s), AND
2. Whenever we transact on the document, we transact on the pinned heads, AND
3. We never read the document at its *current* heads. 

Of course, a root clockument can never have heads pinned in a sharable manner. The canonical heads must *always* be `doc.get_heads()`. Therefore, it cannot be greedily synced, and we must wait for its dependencies to sync. 

#### Diamond-Shaped Dependency Graphs

In theory, a dependency-graph which is diamond shaped (two separate documents have a shared dependency) should be OK. In practice, it becomes tricky with search depths -- see Limitations below. 

In general, consider this an antipattern. 

#### Dependency Cycles

Cycles of dependencies should be fine, as we simply store dependencies in a `visited` set while visiting. However, resolving the dependencies could become extremely expensive, especially if the dependencies are pinned at different versions and therefore count as slightly different dependencies. 

In general, consider this an antipattern. 

#### Historical Dependencies

Take a very close look at Invariant 1: 

> *Invariant 1*: In well-behaved storage, if heads `Z` are persisted, all `dependencies(Z)` must also be persisted.

This means that, for obsolete heads, all dependencies still must be persisted. So, for full correctness against Invariant 1, it isn't enough to check the persistence of `dependencies(Z)` before syncing: we must also check the persistence of `dependencies(Y)`, `dependencies(X)`, and so on.

Realistically, this is infeasible (TODO: probably?), so this isn't actually enforced in this implementation. The user may simply provide the current dependencies, and old, removed dependencies may naturally cause the targets to become negligent over time according to Invariant 1. 

Instead, if full historical accuracy in regards to Invariant 1 is required, we ask that the user never remove a dependency. The heads of dependencies can be updated (as long as all previous pinned heads exist as ancestors to the current pinned heads), but removed dependencies should be serialized in the Automerge data.

Of course, this causes the size of the network to increase monotonically. The user might desire, then, to implement some sort of pruning.

## Rust Implementation

TODO: Document specifics. For now, see the code at the top of persistence.rs for a usage example.


### Limitations & Future Work


#### Dynamic Clockuments

The biggest issue is that currently, `ClockumentCoordinator` takes a single, static document ID, representing the root clockument. Dynamically adding root clockuments are not supported.

We *need* to allow dynamic clockuments. However, this comes with problems. If we explicitly track a set of root clockument IDs, we may break the invariant if a clockument is treated as a dependency before it's explicitly inserted as an ID. 

Instead, the solution here shall be to specify *two* methods on the dependency resolver: 

```
get_dependencies()
identify_root_clockument()
```

Then, it is up to the user to explicitly tag their root clockuments as being roots, ensuring that this can never change for the document's lifetime. 

We'll also need a new method on the coordinator:

```
discover(id)
```

This would allow users to explicitly search the storage/peers for a document matching the ID, rather than specifying it in the constructor. 

(Aside: In an ideal world, we'd be able to simply define a clockument as a document where `get_dependencies()` returns a non-empty set -- but that removes the ability to greedily persist non-roots, since there's no longer any explicitly-specified root.)


#### Nested Clockuments

Backstitch doesn't require nested clockuments, but something like Patchwork might. I haven't thought through the implications of a document that is both explicitly pinned by parent(s), and *also* can behave as a root of its own. For now, consider it unsupported -- but it might work, once Dynamic Clockuments is solved.

#### Unreliable Diamond-Shaped Graphs

Diamond-shaped dependency graphs are not supported with a specified dependency depth limit, because deep dependencies may resolve the dependency without looking further, when the dependency is also specified shallowly and should have its dependencies explored. 

For now, diamond-shaped graphs are not supported.


#### Reliance on Tokio, Subduction, Automerge

Ideally, we can genericize these tools (like how Subduction does it) to work with nearly anything. Right now, for simplicity, I'm using the Backstitch stack.


#### No End-to-End Tests

My next task will be testing this thing end-to-end -- making a Subduction persistence target for peers as well as disk, making sure it actually works, and integrating it into the Backstitch Subduction draft. 