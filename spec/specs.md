# Subspace Specification

Specification Version: 0.1.0

Authors: Jeremiah Wagstaff & Nazar Mokrynskyi

License: Subspace specification (this document) is hereby placed in the public domain

## Contents

1. Ledger
2. Farmer
3. Network
4. State
5. Transactions

## Ledger

### Introduction

The ledger module implements the core logic and purpose of the protocol: arriving at a canonical ordering of blocks and transactions. The protocol remains secure as long as honest farmers control greater than one-half of the pledged storage.

### Glossary

* Farmer: a consensus node who pledges disk space to the network and attempts to pass the storage audit.
* Storage Audit: a unique challenge issued in each timeslot that specifies a target hash and acceptable solution range.
* Solution Range: specifies the maximum allowed distance between a challenge and a solution, for the solution to be valid.
* Timeslot: a fixed unit of time (i.e. 5 seconds) for which there is a single challenge.
* Shared Clock: a global clock such as NTP or NTS that allows all nodes to synchronize their local clocks.
* Epoch: a set of timeslots which share the same base randomness, from which timeslot challenges are derived.
* Full Block: A block consists of an immutable proof-of-storage and a mutable set of content pointers.
* Proof Block: The proof block is canonical for a given challenge, node id, and plot and is used to verify the farmer passed the storage audit.
* Content Block: The content block points to a proof block, a few previous content blocks, and a set of new transactions.
* Reference: an immutable pointer to the SHA-256 hash (id) of some ledger data structure, such as a block or transaction.
* Parent Pointer: A reference to the content block on the tip of the longest observed proof chain from a previous timeslot.
* Uncle Pointers: A reference to all remaining unreferenced content blocks from previous timeslots.
* Transaction Pointers: A reference to all remaining unreferenced transactions present in the memory pool.

### Consensus

* Honest network convergence
* Fork with single late block (accidental or malicious) -- may only solve on one branch
* Fork with many late blocks (short re-org) -- may solve on all branches (but only publish one)
* Fork with different randomness


1. Begin with genesis time and genesis blocks
2. Derive next epoch randomness from the genesis epoch (will depend on the genesis farmer)
3. For each timeslot, derive the slot challenge and solve
4. Expected solution count follows a binomial distribution
5. We begin by assuming the honest setting where the timeslot is sufficient for all blocks to be seen in each round.
6. Each block references a single parent and many uncles
7. Once blocks are referenced we may derive a total ordering apply transactions.

#### Managing Time

In a proof-of-work blockchain the mining process introduces a delay between blocks that is tuned such that in most cases, the latest block is able to propagate to all nodes on the network before the next block is found. In the Subspace blockchain, proofs-of-storage may be solved almost instantly, requiring an external notion of time to allow blocks to fully propagate before solving the next challenge. Similar to many other proof-of-stake and proof-of-capacity blockchains, Subspace relies on a global clock to synchronize the local clocks for all farmers. This may be implemented insecurely using unauthenticated Network Time Protocol (NTP) or securely using authenticated Network Time Security (NTS).

In the Subspace blockchain time is divided into fixed-duration timeslots (currently configured at five seconds each). The timeslot duration is chosen such that it is large enough to ensure that a new block published at the beginning of the timeslot is seen by all farmers on the network before the next timeslot arrives, with high probability. Timeslots are then grouped into epochs (currently configured at 32 timeslots per epoch), which have a shared base randomness derived from the previous epoch. This is required in order to constrain the advantage an attacker may gain through costless simulation, more commonly known as the nothing-at-stake problem. Epochs are further grouped into eons (currently configured at 63 epochs or 2016 timeslots per eon), which share the same self-adjusting range for a valid solution for a given challenge, similar to the self-adjusting work difficulty setting in a proof-of-work blockchain. 

#### Initializing from Genesis

All that is needed to initialize the ledger from genesis is a shared seed for the initial randomness and a shared timestamp for genesis time. 

In order to initialize a new node from genesis 

Should have a seed for some genesis data
Then solve the genesis blocks according to the timer



#### Deriving the base randomness for an epoch

From each epoch, we derive the base randomness for the next epoch, from the block on the longest chain with the most confirmations. Specifically, starting from the first timeslot in the epoch with at least one valid block, take the block with the smallest proof hash (the block on the longest chain), then count the number of direct descendants it has (children which are also on the longest chain). It follows that a single block will then determine the randomness for an epoch and any attacker who attempts to withhold that block will 

#### Determining the slot challenge

#### Ordering Blocks and Transactions

#### Managing Forks

1. Late blocks
2. A late string of blocks
3. A string of blocks that results in a different randomness












When a fork occurs the honest network must pick a single branch to mine on, due to the conservation of work inherent in proof-of-work.


* genesis challenge
* genesis block
* staged blocks
* pending blocks
* referencing blocks
* applied blocks
* choosing a parent
* choosing an uncle

### Sync Process

How do we sync new nodes from genesis